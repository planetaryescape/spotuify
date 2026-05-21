use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result as AnyResult};
use reqwest::{Client, Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use tokio::sync::Mutex;

use spotuify_core::{now_ms, spotify_api_finished_event, AnalyticsEvent, AnalyticsSource};

use crate::auth::{self, StoredToken};
use crate::compat::{compat_normalize, NormalizeHint};
use crate::config::Config;
use crate::error::{SpotifyError, SpotifyResult};
use crate::rate_limit::{Priority, RateLimitedClient};

// Re-export domain types from spotuify-core so existing call sites
// (`crate::spotify::Playback`, etc.) keep working.
pub use spotuify_core::{Device, MediaItem, MediaKind, Playback, Playlist, Queue, TrackId};

const API: &str = "https://api.spotify.com/v1";

pub trait SchemaCompatReporter: Send + Sync {
    fn report_schema_compat(&self, endpoint: &str, missing_keys: &[String]);
}

#[derive(Clone, Debug)]
pub struct SavedTracksPage {
    pub total: u64,
    pub items: Vec<MediaItem>,
}

/// Phase 13 (P13-E) — canonical User-Agent attached to every outbound
/// HTTP request. The OS+arch suffix lets Spotify operations triage
/// platform-specific issues; the GitHub URL is etiquette for any
/// third-party endpoints we hit (LRCLIB, image CDNs, etc.).
pub fn user_agent_string() -> String {
    format!(
        "spotuify/{version} ({os}; {arch}; +https://github.com/planetaryescape/spotuify)",
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
    )
}

#[derive(Clone)]
pub struct SpotifyClient {
    config: Config,
    api_base: String,
    http: Client,
    rate_limiter: RateLimitedClient,
    /// Decoupled via `spotuify_core::AnalyticsSink` so any
    /// Send+Sync+Debug impl works -- the binary's `AnalyticsStore`
    /// is one; tests and future crates can supply their own.
    analytics: Option<Arc<dyn spotuify_core::AnalyticsSink>>,
    schema_compat_reporter: Option<Arc<dyn SchemaCompatReporter>>,
    analytics_source: AnalyticsSource,
    default_priority: Priority,
    fake: bool,
    token_cache: Arc<Mutex<Option<StoredToken>>>,
    /// SHA-1-hex device_id our embedded librespot publishes (deterministic,
    /// derived from the registered device name). Optional because pure
    /// CLI / tests construct clients without an embedded session.
    /// Threaded through to `preferred_device` so device selection prefers
    /// our own live entry over stale namesakes in `/v1/me/player/devices`.
    own_device_id: Option<String>,
}

fn fake_config() -> Config {
    Config {
        client_id: "fake-client-id".to_string(),
        client_secret: Some("fake-client-secret".to_string()),
        redirect_uri: "http://127.0.0.1:8888/callback".to_string(),
        config_path: PathBuf::from("fake-spotuify.toml"),
        player: crate::config::PlayerConfig::default(),
        cache: crate::config::CacheConfig::default(),
        analytics: crate::config::AnalyticsConfig::default(),
        notifications: crate::config::NotificationsConfig::default(),
        discord: crate::config::DiscordConfig::default(),
        viz: crate::config::VizConfig::default(),
    }
}

impl SpotifyClient {
    pub fn new(config: Config) -> SpotifyResult<Self> {
        Ok(Self::new_with_rate_limiter(
            config,
            Self::default_rate_limiter()?,
        ))
    }

    /// Build the default shared HTTP/backpressure runtime. Clones of
    /// the returned value share reqwest pools, semaphores, and backoff.
    pub fn default_rate_limiter() -> SpotifyResult<RateLimitedClient> {
        let http = Client::builder()
            .user_agent(user_agent_string())
            .local_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
            .connect_timeout(Duration::from_secs(4))
            .read_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(8))
            .build()
            .context("failed to build HTTP client")?;
        Ok(RateLimitedClient::new(
            http,
            Some(rate_limit_bucket_path()),
            // Foreground permits — user-facing mutations.
            4,
            // Background permits. Previously 1, which serialized
            // every sync call: when the slow scheduler's `/me/playlists`
            // stalled (10s+ on a slow Spotify response), the fast
            // scheduler's `/me/player`/queue/devices/recent all queued
            // behind it and the TUI saw no live updates. 4 lets the
            // fast and slow loops run concurrently; Spotify's per-app
            // rate budget is far higher than this allows.
            4,
        ))
    }

    pub fn new_with_rate_limiter(config: Config, rate_limiter: RateLimitedClient) -> Self {
        let http = rate_limiter.inner().clone();
        Self {
            config,
            api_base: API.to_string(),
            http,
            rate_limiter,
            analytics: None,
            schema_compat_reporter: None,
            analytics_source: AnalyticsSource::Cli,
            default_priority: Priority::Foreground,
            fake: false,
            token_cache: Arc::new(Mutex::new(None)),
            own_device_id: None,
        }
    }

    pub fn fake() -> SpotifyResult<Self> {
        Ok(Self::new(fake_config())?.with_fake_backend())
    }

    pub fn fake_with_rate_limiter(rate_limiter: RateLimitedClient) -> Self {
        Self::new_with_rate_limiter(fake_config(), rate_limiter).with_fake_backend()
    }

    fn with_fake_backend(mut self) -> Self {
        self.fake = true;
        self
    }

    pub fn with_analytics(
        mut self,
        analytics: Arc<dyn spotuify_core::AnalyticsSink>,
        source: AnalyticsSource,
    ) -> Self {
        self.analytics = Some(analytics);
        self.analytics_source = source;
        self
    }

    pub fn with_schema_compat_reporter(mut self, reporter: Arc<dyn SchemaCompatReporter>) -> Self {
        self.schema_compat_reporter = Some(reporter);
        self
    }

    pub fn with_token_cache(mut self, token_cache: Arc<Mutex<Option<StoredToken>>>) -> Self {
        self.token_cache = token_cache;
        self
    }

    pub fn with_default_priority(mut self, priority: Priority) -> Self {
        self.default_priority = priority;
        self
    }

    /// Annotate this client with the deterministic SHA-1-hex device_id
    /// our embedded librespot publishes. Threaded to `preferred_device`
    /// so device selection prefers our live entry over stale namesakes.
    pub fn with_own_device_id(mut self, own_device_id: Option<String>) -> Self {
        self.own_device_id = own_device_id;
        self
    }

    pub fn own_device_id(&self) -> Option<&str> {
        self.own_device_id.as_deref()
    }

    #[doc(hidden)]
    pub fn with_api_base_for_tests(mut self, api_base: String) -> Self {
        self.api_base = api_base;
        self
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn analytics_source(&self) -> AnalyticsSource {
        self.analytics_source
    }

    pub async fn access_token(&self) -> SpotifyResult<String> {
        auth::access_token_cached(&self.config, &self.http, &self.token_cache).await
    }

    pub async fn record_analytics_event(&self, event: AnalyticsEvent) {
        let Some(analytics) = &self.analytics else {
            return;
        };
        // AnalyticsSink::record swallows failures inside the impl per
        // the trait contract -- analytics is best-effort and must not
        // block the producer.
        analytics.record(&event).await;
    }

    async fn record_spotify_api_finished(
        &self,
        method: &Method,
        path: &str,
        status: Option<StatusCode>,
        elapsed_ms: u128,
        error_class: Option<&str>,
    ) {
        self.record_analytics_event(spotify_api_finished_event(
            AnalyticsSource::SpotifyApi,
            method.as_str(),
            path,
            status.map(|status| status.as_u16()),
            elapsed_ms,
            error_class,
            now_ms(),
        ))
        .await;
    }

    pub async fn playback(&mut self) -> SpotifyResult<Playback> {
        if self.fake {
            return Ok(fake_playback());
        }
        match self
            .request_json::<PlaybackResponse>(Method::GET, "/me/player", None::<()>)
            .await
        {
            Ok(Some(raw)) => Ok(raw.into_playback()),
            Ok(None) => Ok(Playback::default()),
            Err(err) => Err(err.into()),
        }
    }

    pub async fn devices(&mut self) -> SpotifyResult<Vec<Device>> {
        if self.fake {
            return Ok(vec![fake_device()]);
        }
        let response = self
            .request_json::<DevicesResponse>(Method::GET, "/me/player/devices", None::<()>)
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no devices response"))?;
        Ok(response.devices)
    }

    pub async fn queue(&mut self) -> SpotifyResult<Queue> {
        if self.fake {
            return Ok(Queue {
                currently_playing: Some(fake_track()),
                items: vec![fake_second_track()],
                session_active: true,
                as_of_ms: now_ms(),
            });
        }
        let response = self
            .request_json::<QueueResponse>(Method::GET, "/me/player/queue", None::<()>)
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no queue response"))?;
        let currently_playing = response
            .currently_playing
            .and_then(RawPlayable::into_media_item);
        let items: Vec<_> = response
            .queue
            .into_iter()
            .filter_map(RawPlayable::into_media_item)
            .collect();
        // Spotify returns `{ currently_playing: null, queue: [] }` when
        // no device has an active session. Treat that as the only
        // negative signal — any item in either field means a live
        // session existed at the moment of the probe.
        let session_active = currently_playing.is_some() || !items.is_empty();
        Ok(Queue {
            currently_playing,
            items,
            session_active,
            as_of_ms: now_ms(),
        })
    }

    pub async fn search_with_limit(
        &self,
        query: &str,
        kinds: &[MediaKind],
        limit: u8,
    ) -> SpotifyResult<Vec<MediaItem>> {
        if self.fake {
            return Ok(fake_search_results(query, kinds, limit));
        }
        if query.trim().is_empty() || kinds.is_empty() {
            return Ok(Vec::new());
        }

        // Spotify's /v1/search rejects `limit > 20` when more than one
        // type is requested in a single call. To get the documented
        // per-type max of 50 while supporting scope=All, we fan out
        // into one request per `MediaKind`. The shared rate-limiter's
        // `Arc<Semaphore>` caps in-flight concurrency, so up to its
        // permit count run truly in parallel and the rest queue.
        //
        // Spotify can return the same item across multiple type
        // queries (e.g. an album's lead single appearing in both the
        // `track` and `album` responses), so dedup by URI on the way
        // out with first-occurrence-wins to preserve per-type
        // relevance ordering.
        let futures = kinds
            .iter()
            .cloned()
            .map(|kind| self.search_single_type(query, kind, limit, 0));
        let batches: Vec<Vec<MediaItem>> = futures::future::try_join_all(futures).await?;

        let mut items = Vec::new();
        let mut seen_uris: std::collections::HashSet<String> = std::collections::HashSet::new();
        for batch in batches {
            for item in batch {
                if seen_uris.insert(item.uri.clone()) {
                    items.push(item);
                }
            }
        }
        Ok(items)
    }

    async fn search_single_type(
        &self,
        query: &str,
        kind: MediaKind,
        limit: u8,
        offset: u32,
    ) -> SpotifyResult<Vec<MediaItem>> {
        let path = search_path(query, std::slice::from_ref(&kind), limit, offset);
        let response = match self
            .request_json::<SearchResponse>(Method::GET, &path, None::<()>)
            .await
        {
            Ok(Some(r)) => r,
            Ok(None) => return Err(anyhow!("Spotify returned no search response").into()),
            Err(err) => {
                // Spotify caps `limit + offset` at 1000. When the caller paginates
                // past the wall we treat it as an exhausted pane, not an error —
                // the streaming TUI uses empty-page as its stop signal.
                if let Some(SpotifyError::Api {
                    status: 400, body, ..
                }) = err.downcast_ref::<SpotifyError>()
                {
                    if body.contains("exceeds maximum of 1000") {
                        return Ok(Vec::new());
                    }
                }
                return Err(err.into());
            }
        };
        let mut items: Vec<MediaItem> = Vec::new();
        if let Some(tracks) = response.tracks {
            items.extend(tracks.items.into_iter().map(RawTrack::into_media_item));
        }
        if let Some(episodes) = response.episodes {
            items.extend(episodes.items.into_iter().map(RawEpisode::into_media_item));
        }
        if let Some(shows) = response.shows {
            items.extend(shows.items.into_iter().map(RawShow::into_media_item));
        }
        if let Some(albums) = response.albums {
            items.extend(albums.items.into_iter().map(RawAlbum::into_media_item));
        }
        if let Some(artists) = response.artists {
            // Spotify's `/v1/search?type=artist` returns artist objects
            // but the `followers.total` is frequently `0` (varies per
            // account / per call — Spotify doesn't backfill the real
            // count for every search hit). Hydrate via `/v1/artists?ids=…`
            // which is a single batched call and always returns the real
            // count. Falls back gracefully if the hydration fails.
            let mut raws = artists.items;
            if raws
                .iter()
                .any(|a| a.followers.as_ref().is_none_or(|f| f.total == 0))
                && !raws.is_empty()
            {
                self.hydrate_artist_followers(&mut raws).await;
            }
            items.extend(raws.into_iter().map(RawArtist::into_media_item));
        }
        if let Some(playlists) = response.playlists {
            items.extend(
                playlists
                    .items
                    .into_iter()
                    .flatten()
                    .filter_map(RawPlaylist::into_media_item),
            );
        }
        Ok(items)
    }

    /// Hydrate `followers.total` for the given artists from
    /// `/v1/artists?ids=…`. The search response's `followers.total` is
    /// often `0` for reasons internal to Spotify; this batch call
    /// returns the real count for up to 50 artist IDs in a single
    /// round-trip.
    ///
    /// Failures are swallowed (caller keeps the stub data) — followers
    /// is a cosmetic subtitle, not load-bearing for playback.
    async fn hydrate_artist_followers(&self, raws: &mut [RawArtist]) {
        if self.fake {
            return;
        }
        // Spotify caps the batched endpoint at 50 ids per call. Filter
        // to artists with a non-empty id; truncate at 50.
        let ids: Vec<String> = raws
            .iter()
            .filter_map(|raw| raw.id.clone())
            .take(50)
            .collect();
        if ids.is_empty() {
            return;
        }
        let path = format!("/artists?ids={}", encode_component(&ids.join(",")));
        let response = match self
            .request_json::<ArtistsBatchResponse>(Method::GET, &path, None::<()>)
            .await
        {
            Ok(Some(r)) => r,
            Ok(None) => return,
            Err(err) => {
                tracing::debug!(error = %err, "artist followers hydration failed");
                return;
            }
        };
        // Build an id → follower count map, then patch the raws in
        // place. Items whose id isn't in the response keep their
        // (possibly bogus) follower count from search.
        let counts: std::collections::HashMap<String, u64> = response
            .artists
            .into_iter()
            .flatten()
            .filter_map(|raw| {
                let id = raw.id?;
                let total = raw.followers.map(|f| f.total).unwrap_or(0);
                Some((id, total))
            })
            .collect();
        for raw in raws.iter_mut() {
            if let Some(id) = raw.id.as_ref() {
                if let Some(&total) = counts.get(id) {
                    raw.followers = Some(Followers { total });
                }
            }
        }
    }

    /// Single-page search for one `MediaKind` at a given offset. Used by
    /// the streaming search fanout in the daemon. Returns an empty Vec
    /// when the caller has paginated past Spotify's `limit + offset
    /// <= 1000` wall (treated as exhausted, not an error).
    pub async fn search_page(
        &self,
        query: &str,
        kind: MediaKind,
        offset: u32,
    ) -> SpotifyResult<Vec<MediaItem>> {
        if self.fake {
            // Match search_with_limit's fake path: limit clamps internally.
            return Ok(fake_search_results(query, &[kind], 10));
        }
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        self.search_single_type(query, kind, 10, offset).await
    }

    pub async fn media_item_by_uri(&mut self, uri: &str) -> SpotifyResult<Option<MediaItem>> {
        let Some(track_id) = TrackId::from_uri(uri) else {
            return Ok(None);
        };
        if self.fake {
            return Ok(fake_catalog()
                .into_iter()
                .find(|item| item.uri == uri && item.kind == MediaKind::Track));
        }

        let path = format!("/tracks/{}", encode_component(track_id.as_str()));
        Ok(self
            .request_json::<RawTrack>(Method::GET, &path, None::<()>)
            .await?
            .map(RawTrack::into_media_item))
    }

    pub async fn playlists(&mut self) -> SpotifyResult<Vec<Playlist>> {
        if self.fake {
            return Ok(fake_playlists());
        }
        let mut offset = 0;
        let mut playlists = Vec::new();
        loop {
            let path = format!("/me/playlists?limit=50&offset={offset}");
            let response = self
                .request_json::<Paging<Option<RawPlaylist>>>(Method::GET, &path, None::<()>)
                .await?
                .ok_or_else(|| anyhow!("Spotify returned no playlists response"))?;
            let total = response.total;
            playlists.extend(
                response
                    .items
                    .into_iter()
                    .flatten()
                    .filter_map(RawPlaylist::into_playlist),
            );
            offset += 50;
            if offset >= total {
                break;
            }
        }
        Ok(playlists)
    }

    pub async fn current_user_id(&mut self) -> SpotifyResult<String> {
        if self.fake {
            return Ok("fake-user".to_string());
        }
        let response = self
            .request_json::<CurrentUserResponse>(Method::GET, "/me", None::<()>)
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no current user response"))?;
        Ok(response.id)
    }

    pub async fn create_playlist(
        &mut self,
        name: &str,
        description: Option<&str>,
        public: bool,
    ) -> SpotifyResult<Playlist> {
        if self.fake {
            return Ok(Playlist {
                id: fake_playlist_id(name),
                name: name.to_string(),
                owner: "Fake User".to_string(),
                tracks_total: 0,
                image_url: None,
                snapshot_id: None,
            });
        }
        let user_id = self.current_user_id().await?;
        let user_id = encode_component(&user_id);
        let body = serde_json::json!({
            "name": name,
            "description": description.unwrap_or("Created by spotuify"),
            "public": public,
        });
        Ok(self
            .request_json::<RawPlaylist>(
                Method::POST,
                &format!("/users/{user_id}/playlists"),
                Some(body),
            )
            .await?
            .and_then(RawPlaylist::into_playlist)
            .ok_or_else(|| anyhow!("Spotify returned no created playlist"))?)
    }

    pub async fn recently_played(&mut self) -> SpotifyResult<Vec<MediaItem>> {
        if self.fake {
            return Ok(vec![fake_track(), fake_second_track()]);
        }
        let response = self
            .request_json::<RecentlyPlayedResponse>(
                Method::GET,
                "/me/player/recently-played?limit=20",
                None::<()>,
            )
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no recently played response"))?;
        Ok(response
            .items
            .into_iter()
            .map(|item| item.track.into_media_item())
            .collect())
    }

    pub async fn saved_tracks(&mut self) -> SpotifyResult<Vec<MediaItem>> {
        let mut offset = 0;
        let mut items = Vec::new();
        loop {
            let page = self.saved_tracks_page(50, offset).await?;
            let total = page.total;
            items.extend(page.items);
            offset += 50;
            if offset >= total {
                break;
            }
        }
        Ok(items)
    }

    pub async fn saved_tracks_page(
        &mut self,
        limit: u8,
        offset: u64,
    ) -> SpotifyResult<SavedTracksPage> {
        if self.fake {
            let all = vec![fake_track(), fake_second_track()];
            let items = all
                .into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .collect::<Vec<_>>();
            return Ok(SavedTracksPage { total: 2, items });
        }
        let path = format!("/me/tracks?limit={limit}&offset={offset}");
        let response = self
            .request_json::<Paging<SavedTrackItem>>(Method::GET, &path, None::<()>)
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no saved tracks response"))?;
        Ok(SavedTracksPage {
            total: response.total,
            items: response
                .items
                .into_iter()
                .map(|item| item.track.into_media_item())
                .collect(),
        })
    }

    pub async fn saved_albums(&mut self) -> SpotifyResult<Vec<MediaItem>> {
        if self.fake {
            return Ok(vec![fake_album()]);
        }
        let mut offset = 0;
        let mut items = Vec::new();
        loop {
            let path = format!("/me/albums?limit=50&offset={offset}");
            let response = self
                .request_json::<Paging<SavedAlbumItem>>(Method::GET, &path, None::<()>)
                .await?
                .ok_or_else(|| anyhow!("Spotify returned no saved albums response"))?;
            let total = response.total;
            items.extend(
                response
                    .items
                    .into_iter()
                    .map(|item| item.album.into_media_item()),
            );
            offset += 50;
            if offset >= total {
                break;
            }
        }
        Ok(items)
    }

    /// Fetch the user's saved podcasts (Spotify's `/me/shows`).
    /// Paginated 50 at a time. Callers that need an interactive preview
    /// should cap at their own boundary; the daemon sync path hydrates
    /// the full library.
    pub async fn saved_shows(&mut self) -> SpotifyResult<Vec<MediaItem>> {
        if self.fake {
            return Ok(Vec::new());
        }
        let mut offset = 0;
        let mut items = Vec::new();
        loop {
            let path = format!("/me/shows?limit=50&offset={offset}");
            let response = self
                .request_json::<Paging<SavedShowItem>>(Method::GET, &path, None::<()>)
                .await?
                .ok_or_else(|| anyhow!("Spotify returned no saved shows response"))?;
            let total = response.total;
            items.extend(
                response
                    .items
                    .into_iter()
                    .map(|item| item.show.into_media_item()),
            );
            offset += 50;
            if offset >= total {
                break;
            }
        }
        Ok(items)
    }

    pub async fn playlist_tracks(&mut self, playlist_id: &str) -> SpotifyResult<Vec<MediaItem>> {
        if self.fake {
            if fake_playlists()
                .iter()
                .any(|playlist| playlist.id == playlist_id)
            {
                return Ok(vec![fake_track(), fake_second_track()]);
            }
            return Err(SpotifyError::NotFound);
        }
        let mut offset = 0;
        let mut tracks = Vec::new();
        loop {
            let path = format!("/playlists/{playlist_id}/items?limit=50&offset={offset}");
            let response = self
                .request_json::<Paging<PlaylistTrackItem>>(Method::GET, &path, None::<()>)
                .await?
                .ok_or_else(|| anyhow!("Spotify returned no playlist tracks response"))?;
            let total = response.total;
            tracks.extend(
                response
                    .items
                    .into_iter()
                    .filter_map(|item| item.track.and_then(RawPlayable::into_media_item))
                    .filter(|item| item.is_playable != Some(false)),
            );
            offset += 50;
            if offset >= total {
                break;
            }
        }
        Ok(tracks)
    }

    pub async fn album_tracks(&mut self, album_id: &str) -> SpotifyResult<Vec<MediaItem>> {
        if self.fake {
            let album_id = album_id.trim_start_matches("spotify:album:");
            if fake_album().id.as_deref() == Some(album_id) {
                return Ok(vec![fake_track(), fake_second_track()]);
            }
            return Err(SpotifyError::NotFound);
        }
        let album_id = encode_component(album_id.trim_start_matches("spotify:album:"));
        let mut offset = 0;
        let mut tracks = Vec::new();
        loop {
            let path = format!("/albums/{album_id}/tracks?limit=50&offset={offset}");
            let response = self
                .request_json::<Paging<RawAlbumTrack>>(Method::GET, &path, None::<()>)
                .await?
                .ok_or_else(|| anyhow!("Spotify returned no album tracks response"))?;
            let total = response.total;
            tracks.extend(
                response
                    .items
                    .into_iter()
                    .map(RawAlbumTrack::into_media_item),
            );
            offset += 50;
            if offset >= total {
                break;
            }
        }
        Ok(tracks)
    }

    /// Albums for a given artist (Spotify's `/v1/artists/{id}/albums`).
    /// Includes singles + albums; excludes appears-on so the user
    /// sees the artist's own releases.
    pub async fn artist_albums(&mut self, artist_id: &str) -> SpotifyResult<Vec<MediaItem>> {
        if self.fake {
            return Ok(vec![fake_album()]);
        }
        let artist_id = encode_component(artist_id.trim_start_matches("spotify:artist:"));
        let mut offset = 0u32;
        let mut albums = Vec::new();
        // Empirical cap for this account/app: limit>10 → 400 "Invalid limit".
        // Same quirk as /v1/search (see commit c99e576). Docs claim 50 max.
        const PAGE: u32 = 10;
        loop {
            let path = format!(
                "/artists/{artist_id}/albums?include_groups=album%2Csingle&limit={PAGE}&offset={offset}"
            );
            let response = self
                .request_json::<Paging<RawAlbum>>(Method::GET, &path, None::<()>)
                .await?
                .ok_or_else(|| anyhow!("Spotify returned no artist albums response"))?;
            let total = response.total;
            albums.extend(response.items.into_iter().map(RawAlbum::into_media_item));
            offset += PAGE;
            if u64::from(offset) >= total {
                break;
            }
        }
        Ok(albums)
    }

    pub async fn play_pause(&mut self, is_playing: bool) -> SpotifyResult<()> {
        if self.fake {
            let _ = is_playing;
            return Ok(());
        }
        if is_playing {
            self.empty(Method::PUT, "/me/player/pause", None::<()>)
                .await?;
        } else {
            self.empty(Method::PUT, "/me/player/play", Some(serde_json::json!({})))
                .await?;
        }
        Ok(())
    }

    pub async fn play_uri(&mut self, uri: &str, kind: &MediaKind) -> SpotifyResult<()> {
        if self.fake {
            let _ = (uri, kind);
            return Ok(());
        }
        let body = match kind {
            MediaKind::Album | MediaKind::Artist | MediaKind::Playlist | MediaKind::Show => {
                serde_json::json!({ "context_uri": uri })
            }
            _ => serde_json::json!({ "uris": [uri] }),
        };
        self.empty(Method::PUT, "/me/player/play", Some(body))
            .await?;
        Ok(())
    }

    pub async fn next(&mut self) -> SpotifyResult<()> {
        if self.fake {
            return Ok(());
        }
        self.empty(Method::POST, "/me/player/next", None::<()>)
            .await?;
        Ok(())
    }

    pub async fn previous(&mut self) -> SpotifyResult<()> {
        if self.fake {
            return Ok(());
        }
        self.empty(Method::POST, "/me/player/previous", None::<()>)
            .await?;
        Ok(())
    }

    pub async fn seek(&mut self, position_ms: u64) -> SpotifyResult<()> {
        if self.fake {
            let _ = position_ms;
            return Ok(());
        }
        self.empty(
            Method::PUT,
            &format!("/me/player/seek?position_ms={position_ms}"),
            None::<()>,
        )
        .await?;
        Ok(())
    }

    pub async fn volume(&mut self, volume_percent: u8) -> SpotifyResult<()> {
        if self.fake {
            let _ = volume_percent;
            return Ok(());
        }
        let volume_percent = volume_percent.min(100);
        self.empty(
            Method::PUT,
            &format!("/me/player/volume?volume_percent={volume_percent}"),
            None::<()>,
        )
        .await?;
        Ok(())
    }

    pub async fn shuffle(&mut self, state: bool) -> SpotifyResult<()> {
        if self.fake {
            let _ = state;
            return Ok(());
        }
        self.empty(
            Method::PUT,
            &format!("/me/player/shuffle?state={state}"),
            None::<()>,
        )
        .await?;
        Ok(())
    }

    pub async fn repeat(&mut self, state: &str) -> SpotifyResult<()> {
        if self.fake {
            let _ = state;
            return Ok(());
        }
        self.empty(
            Method::PUT,
            &format!("/me/player/repeat?state={state}"),
            None::<()>,
        )
        .await?;
        Ok(())
    }

    pub async fn add_to_queue(&mut self, uri: &str) -> SpotifyResult<()> {
        if self.fake {
            selection_like_uri_check(uri)?;
            return Ok(());
        }
        let encoded = url::form_urlencoded::byte_serialize(uri.as_bytes()).collect::<String>();
        self.empty(
            Method::POST,
            &format!("/me/player/queue?uri={encoded}"),
            None::<()>,
        )
        .await?;
        Ok(())
    }

    pub async fn transfer(&mut self, device_id: &str, play: bool) -> SpotifyResult<()> {
        if self.fake {
            let _ = play;
            if fake_device().id.as_deref() == Some(device_id) || device_id == "spotuify-fake" {
                return Ok(());
            }
            return Err(SpotifyError::NotFound);
        }
        self.empty(
            Method::PUT,
            "/me/player",
            Some(serde_json::json!({ "device_ids": [device_id], "play": play })),
        )
        .await?;
        Ok(())
    }

    pub async fn add_to_playlist(&mut self, playlist_id: &str, uri: &str) -> SpotifyResult<()> {
        self.add_items_to_playlist(playlist_id, &[uri.to_string()])
            .await
    }

    pub async fn add_items_to_playlist(
        &mut self,
        playlist_id: &str,
        uris: &[String],
    ) -> SpotifyResult<()> {
        if self.fake {
            if fake_playlists()
                .iter()
                .any(|playlist| playlist.id == playlist_id)
            {
                for uri in uris {
                    selection_like_uri_check(uri)?;
                }
                return Ok(());
            }
            return Err(SpotifyError::NotFound);
        }
        if uris.is_empty() {
            return Ok(());
        }
        let playlist_id = encode_component(playlist_id);
        for chunk in uris.chunks(100) {
            self.empty(
                Method::POST,
                &format!("/playlists/{playlist_id}/tracks"),
                Some(serde_json::json!({ "uris": chunk })),
            )
            .await?;
        }
        Ok(())
    }

    pub async fn save_item(&mut self, item: &MediaItem) -> SpotifyResult<()> {
        if self.fake {
            selection_like_uri_check(&item.uri)?;
            return Ok(());
        }
        match item.kind {
            MediaKind::Track | MediaKind::Episode | MediaKind::Show => {
                self.library_save_by_uri(&item.uri).await
            }
            _ => Err(SpotifyError::InvalidInput {
                message: "only tracks, episodes, and shows can be saved from now playing"
                    .to_string(),
            }),
        }
    }

    // ---------------------------------------------------------------
    // Phase 12 (P12-A) — inverse mutators used by `apply_reversal`.
    //
    // Each method delegates URL+body shape to a pure helper at the
    // bottom of the file so the wire format stays unit-testable.
    // ---------------------------------------------------------------

    /// `DELETE /v1/playlists/{id}/tracks` with `tracks[].uri` and
    /// optional `snapshot_id` precondition. Returns the new
    /// `snapshot_id` Spotify hands back so the caller can persist it.
    pub async fn remove_playlist_items(
        &mut self,
        playlist_id: &str,
        uris: &[String],
        snapshot_id: Option<&str>,
    ) -> SpotifyResult<String> {
        if self.fake {
            if fake_playlists().iter().any(|p| p.id == playlist_id) {
                return Ok("fake-snap-after-remove".to_string());
            }
            return Err(SpotifyError::NotFound);
        }
        if uris.is_empty() {
            // No-op remove still needs a snapshot to return; surface the
            // caller's stored one (best-effort) or empty so the caller
            // can decide not to persist.
            return Ok(snapshot_id.unwrap_or_default().to_string());
        }
        let encoded = encode_component(playlist_id);
        let mut current_snapshot = snapshot_id.map(str::to_string);
        for chunk in uris.chunks(100) {
            let body = playlist_remove_items_body(chunk, current_snapshot.as_deref());
            let resp = self
                .request_json::<SnapshotResponse>(
                    Method::DELETE,
                    &format!("/playlists/{encoded}/tracks"),
                    Some(body),
                )
                .await?
                .ok_or_else(|| anyhow!("Spotify returned no response for playlist-remove"))?;
            current_snapshot = Some(resp.snapshot_id);
        }
        Ok(current_snapshot.ok_or_else(|| anyhow!("Spotify returned no snapshot_id"))?)
    }

    /// Re-add items at their original positions (undo of a previous
    /// remove). Groups by position so each unique position becomes one
    /// `POST /v1/playlists/{id}/tracks?position={p}` call carrying the
    /// URIs that landed at that position.
    pub async fn add_items_to_playlist_at_positions(
        &mut self,
        playlist_id: &str,
        items: &[(String, u32)],
        snapshot_id: Option<&str>,
    ) -> SpotifyResult<String> {
        let _ = snapshot_id; // Spotify's add endpoint ignores snapshot_id.
        if self.fake {
            if fake_playlists().iter().any(|p| p.id == playlist_id) {
                return Ok("fake-snap-after-readd".to_string());
            }
            return Err(SpotifyError::NotFound);
        }
        if items.is_empty() {
            return Ok(String::new());
        }
        let encoded = encode_component(playlist_id);
        let groups = group_items_by_position(items);
        let mut last_snapshot = String::new();
        for (position, uris) in groups {
            for chunk in uris.chunks(100) {
                let body = serde_json::json!({ "uris": chunk });
                let resp = self
                    .request_json::<SnapshotResponse>(
                        Method::POST,
                        &format!("/playlists/{encoded}/tracks?position={position}"),
                        Some(body),
                    )
                    .await?
                    .ok_or_else(|| anyhow!("Spotify returned no response for playlist-add"))?;
                last_snapshot = resp.snapshot_id;
            }
        }
        Ok(last_snapshot)
    }

    /// Reorder a contiguous range of items in a playlist.
    /// `PUT /v1/playlists/{id}/tracks` with `{range_start, range_length,
    /// insert_before, snapshot_id?}`.
    pub async fn reorder_playlist_items(
        &mut self,
        playlist_id: &str,
        range_start: u32,
        insert_before: u32,
        range_length: u32,
        snapshot_id: Option<&str>,
    ) -> SpotifyResult<String> {
        if self.fake {
            if fake_playlists().iter().any(|p| p.id == playlist_id) {
                return Ok("fake-snap-after-reorder".to_string());
            }
            return Err(SpotifyError::NotFound);
        }
        let encoded = encode_component(playlist_id);
        let body = playlist_reorder_body(range_start, insert_before, range_length, snapshot_id);
        let resp = self
            .request_json::<SnapshotResponse>(
                Method::PUT,
                &format!("/playlists/{encoded}/tracks"),
                Some(body),
            )
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no response for playlist-reorder"))?;
        Ok(resp.snapshot_id)
    }

    /// Unfollow / delete a playlist. Spotify models playlist deletion
    /// as the owner unfollowing it. `DELETE /v1/playlists/{id}/followers`.
    pub async fn unfollow_playlist(&mut self, playlist_id: &str) -> SpotifyResult<()> {
        if self.fake {
            if fake_playlists().iter().any(|p| p.id == playlist_id) {
                return Ok(());
            }
            return Err(SpotifyError::NotFound);
        }
        let encoded = encode_component(playlist_id);
        self.empty(
            Method::DELETE,
            &format!("/playlists/{encoded}/followers"),
            None::<()>,
        )
        .await?;
        Ok(())
    }

    /// Save (=like) an item by URI. Routes to the correct
    /// `/me/{tracks,albums,episodes,shows}` endpoint based on the URI
    /// kind and uses Spotify's `?ids=` query syntax.
    pub async fn library_save_by_uri(&mut self, uri: &str) -> SpotifyResult<()> {
        if self.fake {
            selection_like_uri_check(uri)?;
            return Ok(());
        }
        let (path, _id) = library_endpoint_for_uri(uri)?;
        self.empty(Method::PUT, &path, None::<()>).await?;
        Ok(())
    }

    /// Inverse of `library_save_by_uri`. `DELETE` against the same
    /// endpoint family.
    pub async fn library_unsave_by_uri(&mut self, uri: &str) -> SpotifyResult<()> {
        if self.fake {
            selection_like_uri_check(uri)?;
            return Ok(());
        }
        let (path, _id) = library_endpoint_for_uri(uri)?;
        self.empty(Method::DELETE, &path, None::<()>).await?;
        Ok(())
    }

    pub async fn image(&self, url: &str) -> SpotifyResult<Vec<u8>> {
        let response = self
            .http
            .get(url)
            .send()
            .await
            .context("image request failed")?;
        let status = response.status();
        if !status.is_success() {
            return Err(SpotifyError::Api {
                status: status.as_u16(),
                endpoint: "GET image".to_string(),
                message: format!("image request failed with {status}"),
                body: String::new(),
            });
        }
        Ok(response
            .bytes()
            .await
            .context("failed to read image")?
            .to_vec())
    }

    async fn empty<T: Serialize>(
        &mut self,
        method: Method,
        path: &str,
        body: Option<T>,
    ) -> AnyResult<()> {
        let token = auth::access_token_cached(&self.config, &self.http, &self.token_cache).await?;
        let url = format!("{}{path}", self.api_base);
        let body = body.map(serde_json::to_value).transpose()?;
        let priority = request_priority(&method, path, self.default_priority);
        let scope = endpoint_scope(&method, path);
        let started = Instant::now();
        tracing::debug!(method = %method, path, "Spotify request start");
        let response = match self
            .rate_limiter
            .send_with_retry(priority, &scope, || {
                let mut request = self
                    .rate_limiter
                    .inner()
                    .request(method.clone(), url.clone())
                    .bearer_auth(token.clone());
                if let Some(body) = &body {
                    request = request.json(body);
                } else if method_accepts_empty_body(&method) {
                    // Spotify's edge layer occasionally responds with
                    // HTTP 411 ("Length Required") for bodyless PUT/POST
                    // even when `Content-Length: 0` is set explicitly
                    // — `seanmonstar/reqwest#838` documents the
                    // header-stripping path. Sending an empty JSON
                    // object lets reqwest compute Content-Length from
                    // the body and pins Content-Type, which the edge
                    // accepts uniformly.
                    request = request.json(&serde_json::json!({}));
                }
                request
            })
            .await
        {
            Ok(response) => response,
            Err(err) => {
                self.record_spotify_api_finished(
                    &method,
                    path,
                    None,
                    started.elapsed().as_millis(),
                    Some(spotify_error_class(&err)),
                )
                .await;
                tracing::warn!(method = %method, path, error = %err, "Spotify request send failed");
                return Err(anyhow!(err))
                    .with_context(|| format!("Spotify {method} {path} request failed"));
            }
        };
        let status = response.status();
        self.record_spotify_api_finished(
            &method,
            path,
            Some(status),
            started.elapsed().as_millis(),
            if status.is_success() {
                None
            } else {
                Some("http")
            },
        )
        .await;
        tracing::debug!(
            method = %method,
            path,
            status = %status,
            elapsed_ms = started.elapsed().as_millis(),
            "Spotify request finished"
        );
        handle_empty_response(&method, path, response).await
    }

    async fn request_json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<impl Serialize>,
    ) -> AnyResult<Option<T>> {
        let mut token =
            auth::access_token_cached(&self.config, &self.http, &self.token_cache).await?;
        let url = format!("{}{path}", self.api_base);
        let body = body.map(serde_json::to_value).transpose()?;
        let priority = request_priority(&method, path, self.default_priority);
        let scope = endpoint_scope(&method, path);
        let started = Instant::now();
        tracing::debug!(method = %method, path, "Spotify request start");
        let mut auth_attempt = 0_u8;
        let response = loop {
            match self
                .rate_limiter
                .send_with_retry(priority, &scope, || {
                    let mut request = self
                        .rate_limiter
                        .inner()
                        .request(method.clone(), url.clone())
                        .bearer_auth(token.clone());
                    if let Some(body) = &body {
                        request = request.json(body);
                    } else if method_accepts_empty_body(&method) {
                        // See note in `empty()` — Spotify's edge requires a
                        // body for bodyless PUT/POST/PATCH/DELETE writes.
                        request = request.json(&serde_json::json!({}));
                    }
                    request
                })
                .await
            {
                Ok(response) => break response,
                Err(SpotifyError::AuthExpired) if auth_attempt == 0 => {
                    auth_attempt += 1;
                    token = auth::refresh_access_token_cached(
                        &self.config,
                        &self.http,
                        &self.token_cache,
                    )
                    .await?;
                }
                Err(err) => {
                    self.record_spotify_api_finished(
                        &method,
                        path,
                        None,
                        started.elapsed().as_millis(),
                        Some(spotify_error_class(&err)),
                    )
                    .await;
                    tracing::warn!(method = %method, path, error = %err, "Spotify request send failed");
                    return Err(anyhow!(err))
                        .with_context(|| format!("Spotify {method} {path} request failed"));
                }
            }
        };
        let status = response.status();
        self.record_spotify_api_finished(
            &method,
            path,
            Some(status),
            started.elapsed().as_millis(),
            if status.is_success() {
                None
            } else {
                Some("http")
            },
        )
        .await;
        tracing::debug!(
            method = %method,
            path,
            status = %status,
            elapsed_ms = started.elapsed().as_millis(),
            "Spotify request finished"
        );
        let (result, compat_keys) = handle_json_response(&method, path, response).await?;
        if !compat_keys.is_empty() {
            if let Some(reporter) = &self.schema_compat_reporter {
                reporter.report_schema_compat(path, &compat_keys);
            }
        }
        Ok(result)
    }
}

fn search_path(query: &str, kinds: &[MediaKind], limit: u8, offset: u32) -> String {
    let encoded = encode_component(query);
    let types = kinds
        .iter()
        .map(MediaKind::label)
        .collect::<Vec<_>>()
        .join(",");
    // Empirical /v1/search ceiling is 10, even though the docs and
    // every other Spotify TUI report 50 as "the max per type". Each
    // type-fan request beyond 10 returns:
    //   400 {"error":{"status":400,"message":"Invalid limit"}}
    // Bisected against a real Premium account 2026-05-17. The
    // discrepancy is likely a recent tier change or app-config quirk
    // that Spotify hasn't documented; raising this requires
    // verifying against the live API again.
    //
    // `offset` is paginatable up to `limit + offset <= 1000`. Beyond
    // that Spotify returns 400 "Limit + Offset exceeds maximum of 1000"
    // — handled in `search_single_type` as an exhausted pane.
    let limit = limit.min(10);
    format!("/search?q={encoded}&type={types}&limit={limit}&offset={offset}")
}

fn encode_component(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>()
}

fn rate_limit_bucket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("SPOTUIFY_RUNTIME_DIR") {
        return PathBuf::from(path).join("spotify-rate-limit.json");
    }
    dirs::cache_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".cache")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("spotuify")
        .join("spotify-rate-limit.json")
}

fn endpoint_scope(method: &Method, path: &str) -> String {
    let path = path.split('?').next().unwrap_or(path);
    format!("{method} {path}")
}

fn request_priority(method: &Method, path: &str, default_priority: Priority) -> Priority {
    if path.starts_with("/me/player") && *method != Method::GET {
        Priority::PlaybackControl
    } else {
        default_priority
    }
}

fn method_accepts_empty_body(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

fn spotify_error_class(error: &SpotifyError) -> &'static str {
    match error {
        SpotifyError::RateLimited { .. } => "rate_limit",
        SpotifyError::AuthRequired
        | SpotifyError::AuthExpired
        | SpotifyError::AuthRevoked
        | SpotifyError::Forbidden { .. } => "auth",
        SpotifyError::Network { .. } => "transport",
        SpotifyError::Decode { .. } => "decode",
        SpotifyError::NotFound | SpotifyError::Deprecated { .. } | SpotifyError::Api { .. } => {
            "http"
        }
        SpotifyError::InvalidInput { .. } | SpotifyError::Client { .. } => "client",
    }
}

async fn handle_empty_response(
    method: &Method,
    path: &str,
    response: reqwest::Response,
) -> AnyResult<()> {
    let status = response.status();
    if status.is_success() || status == StatusCode::NO_CONTENT || status == StatusCode::NOT_MODIFIED
    {
        return Ok(());
    }

    let retry = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = response.text().await.unwrap_or_default();
    if let Some(retry) = retry {
        bail!("Spotify {method} {path} was rate limited; retry after {retry}s");
    }
    let message = spotify_error_message(&body);
    tracing::warn!(method = %method, path, status = %status, body = %trim_body(&body), "Spotify request failed");
    bail!("Spotify {method} {path} failed ({status}): {message}")
}

async fn handle_json_response<T: DeserializeOwned>(
    method: &Method,
    path: &str,
    response: reqwest::Response,
) -> AnyResult<(Option<T>, Vec<String>)> {
    let status = response.status();
    if status == StatusCode::NO_CONTENT || status == StatusCode::NOT_MODIFIED {
        return Ok((None, Vec::new()));
    }
    if !status.is_success() {
        let retry = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = response.text().await.unwrap_or_default();
        if let Some(retry) = retry {
            bail!("Spotify {method} {path} was rate limited; retry after {retry}s");
        }
        let message = spotify_error_message(&body);
        tracing::warn!(method = %method, path, status = %status, body = %trim_body(&body), "Spotify request failed");
        bail!(
            "Spotify {method} {path} failed ({status}): {message} [body: {}]",
            trim_body(&body)
        );
    }
    let body = response
        .text()
        .await
        .with_context(|| format!("failed to read Spotify {method} {path} response"))?;
    let mut value = serde_json::from_str::<serde_json::Value>(&body)
        .with_context(|| format!("failed to decode Spotify {method} {path} response"))?;
    let patched = normalize_spotify_response(path, &mut value);
    if !patched.is_empty() {
        tracing::debug!(
            method = %method,
            path,
            missing_key_count = patched.len(),
            sample_missing_keys = ?patched.iter().take(8).collect::<Vec<_>>(),
            "normalized Spotify response payload"
        );
    }
    match serde_json::from_value::<T>(value) {
        Ok(value) => Ok((Some(value), patched)),
        Err(err) => {
            tracing::warn!(
                method = %method,
                path,
                error = %err,
                body = %trim_body(&body),
                "failed to decode Spotify response"
            );
            Err(err).with_context(|| format!("failed to decode Spotify {method} {path} response"))
        }
    }
}

fn normalize_spotify_response(path: &str, value: &mut serde_json::Value) -> Vec<String> {
    let endpoint = path.split('?').next().unwrap_or(path);
    let mut patched = Vec::new();
    match endpoint {
        "/me/player" => {
            normalize_child(value, "item", "item", normalize_playable, &mut patched);
        }
        "/me/player/queue" => {
            normalize_child(
                value,
                "currently_playing",
                "currently_playing",
                normalize_playable,
                &mut patched,
            );
            normalize_array_child(value, "queue", "queue", normalize_playable, &mut patched);
        }
        "/me/player/recently-played" => {
            normalize_paging(
                value,
                NormalizeHint::Unknown,
                "recently_played",
                &mut patched,
            );
            normalize_array_child(
                value,
                "items",
                "items.track",
                normalize_recent_item,
                &mut patched,
            );
        }
        "/me/tracks" => {
            normalize_paging(value, NormalizeHint::PagingTrack, "paging", &mut patched);
            normalize_array_child(
                value,
                "items",
                "items.track",
                normalize_saved_track,
                &mut patched,
            );
        }
        "/me/albums" => {
            normalize_paging(value, NormalizeHint::PagingAlbum, "paging", &mut patched);
            normalize_array_child(
                value,
                "items",
                "items.album",
                normalize_saved_album,
                &mut patched,
            );
        }
        "/me/playlists" => {
            normalize_paging(value, NormalizeHint::PagingPlaylist, "paging", &mut patched);
            normalize_array_child(
                value,
                "items",
                "items",
                normalize_playlist_option,
                &mut patched,
            );
        }
        _ if endpoint.starts_with("/tracks/") => {
            normalize_track(value, "track", &mut patched);
        }
        _ if endpoint.starts_with("/users/") && endpoint.ends_with("/playlists") => {
            normalize_playlist(value, "playlist", &mut patched);
        }
        _ if endpoint.starts_with("/playlists/") && endpoint.ends_with("/tracks") => {
            normalize_paging(value, NormalizeHint::PagingTrack, "paging", &mut patched);
            normalize_array_child(
                value,
                "items",
                "items.track",
                normalize_playlist_track,
                &mut patched,
            );
        }
        _ if endpoint == "/search" => {
            normalize_child(
                value,
                "tracks",
                "tracks",
                normalize_track_paging,
                &mut patched,
            );
            normalize_child(
                value,
                "episodes",
                "episodes",
                normalize_episode_paging,
                &mut patched,
            );
            normalize_child(
                value,
                "albums",
                "albums",
                normalize_album_paging,
                &mut patched,
            );
            normalize_child(
                value,
                "artists",
                "artists",
                normalize_artist_paging,
                &mut patched,
            );
            normalize_child(
                value,
                "playlists",
                "playlists",
                normalize_playlist_paging,
                &mut patched,
            );
        }
        _ => {}
    }
    patched
}

fn normalize_child(
    value: &mut serde_json::Value,
    key: &str,
    label: &str,
    normalize: fn(&mut serde_json::Value, &str, &mut Vec<String>),
    patched: &mut Vec<String>,
) {
    if let Some(child) = value.get_mut(key) {
        if !child.is_null() {
            normalize(child, label, patched);
        }
    }
}

fn normalize_array_child(
    value: &mut serde_json::Value,
    key: &str,
    label: &str,
    normalize: fn(&mut serde_json::Value, &str, &mut Vec<String>),
    patched: &mut Vec<String>,
) {
    let Some(items) = value.get_mut(key).and_then(serde_json::Value::as_array_mut) else {
        return;
    };
    for item in items {
        normalize(item, label, patched);
    }
}

fn normalize_paging(
    value: &mut serde_json::Value,
    hint: NormalizeHint,
    label: &str,
    patched: &mut Vec<String>,
) {
    record_patched(label, compat_normalize(value, hint), patched);
}

fn normalize_track(value: &mut serde_json::Value, label: &str, patched: &mut Vec<String>) {
    record_patched(
        label,
        compat_normalize(value, NormalizeHint::Track),
        patched,
    );
    normalize_child(value, "album", "album", normalize_album, patched);
}

fn normalize_album(value: &mut serde_json::Value, label: &str, patched: &mut Vec<String>) {
    record_patched(
        label,
        compat_normalize(value, NormalizeHint::Album),
        patched,
    );
}

fn normalize_artist(value: &mut serde_json::Value, label: &str, patched: &mut Vec<String>) {
    record_patched(
        label,
        compat_normalize(value, NormalizeHint::Artist),
        patched,
    );
}

fn normalize_playlist(value: &mut serde_json::Value, label: &str, patched: &mut Vec<String>) {
    record_patched(
        label,
        compat_normalize(value, NormalizeHint::Playlist),
        patched,
    );
}

fn normalize_episode(value: &mut serde_json::Value, label: &str, patched: &mut Vec<String>) {
    record_patched(
        label,
        compat_normalize(value, NormalizeHint::Episode),
        patched,
    );
}

fn normalize_playable(value: &mut serde_json::Value, label: &str, patched: &mut Vec<String>) {
    match value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
    {
        "track" => normalize_track(value, label, patched),
        "episode" => normalize_episode(value, label, patched),
        _ => {}
    }
}

fn normalize_saved_track(value: &mut serde_json::Value, label: &str, patched: &mut Vec<String>) {
    normalize_child(value, "track", label, normalize_track, patched);
}

fn normalize_saved_album(value: &mut serde_json::Value, label: &str, patched: &mut Vec<String>) {
    normalize_child(value, "album", label, normalize_album, patched);
}

fn normalize_recent_item(value: &mut serde_json::Value, label: &str, patched: &mut Vec<String>) {
    normalize_child(value, "track", label, normalize_track, patched);
}

fn normalize_playlist_track(value: &mut serde_json::Value, label: &str, patched: &mut Vec<String>) {
    normalize_child(value, "track", label, normalize_playable, patched);
}

fn normalize_playlist_option(
    value: &mut serde_json::Value,
    label: &str,
    patched: &mut Vec<String>,
) {
    if !value.is_null() {
        normalize_playlist(value, label, patched);
    }
}

fn normalize_track_paging(value: &mut serde_json::Value, label: &str, patched: &mut Vec<String>) {
    normalize_paging(value, NormalizeHint::PagingTrack, label, patched);
    normalize_array_child(value, "items", "tracks.items", normalize_track, patched);
}

fn normalize_episode_paging(value: &mut serde_json::Value, label: &str, patched: &mut Vec<String>) {
    normalize_paging(value, NormalizeHint::PagingEpisode, label, patched);
    normalize_array_child(value, "items", "episodes.items", normalize_episode, patched);
}

fn normalize_album_paging(value: &mut serde_json::Value, label: &str, patched: &mut Vec<String>) {
    normalize_paging(value, NormalizeHint::PagingAlbum, label, patched);
    normalize_array_child(value, "items", "albums.items", normalize_album, patched);
}

fn normalize_artist_paging(value: &mut serde_json::Value, label: &str, patched: &mut Vec<String>) {
    normalize_paging(value, NormalizeHint::PagingArtist, label, patched);
    normalize_array_child(value, "items", "artists.items", normalize_artist, patched);
}

fn normalize_playlist_paging(
    value: &mut serde_json::Value,
    label: &str,
    patched: &mut Vec<String>,
) {
    normalize_paging(value, NormalizeHint::PagingPlaylist, label, patched);
    normalize_array_child(
        value,
        "items",
        "playlists.items",
        normalize_playlist_option,
        patched,
    );
}

fn record_patched(label: &str, keys: Vec<&'static str>, patched: &mut Vec<String>) {
    patched.extend(keys.into_iter().map(|key| format!("{label}.{key}")));
}

fn spotify_error_message(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .and_then(|message| message.as_str())
                .or_else(|| {
                    value
                        .get("error_description")
                        .and_then(|message| message.as_str())
                })
                .map(str::to_string)
        })
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| trim_body(body))
}

fn trim_body(body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        return "empty response body".to_string();
    }
    const MAX: usize = 500;
    if body.len() <= MAX {
        body.to_string()
    } else {
        format!("{}...", &body[..MAX])
    }
}

#[derive(Debug, Deserialize)]
struct PlaybackResponse {
    device: Option<Device>,
    repeat_state: Option<String>,
    shuffle_state: Option<bool>,
    progress_ms: Option<u64>,
    is_playing: Option<bool>,
    item: Option<RawPlayable>,
    /// Phase 4 — Spotify's last state-transition timestamp (Unix epoch ms).
    /// Not the response time. Optional in the API response.
    #[serde(default)]
    timestamp: Option<i64>,
}

impl PlaybackResponse {
    fn into_playback(self) -> Playback {
        Playback {
            item: self.item.and_then(RawPlayable::into_media_item),
            device: self.device,
            is_playing: self.is_playing.unwrap_or(false),
            progress_ms: self.progress_ms.unwrap_or_default(),
            shuffle: self.shuffle_state.unwrap_or(false),
            repeat: self.repeat_state.unwrap_or_else(|| "off".to_string()),
            sampled_at_ms: Some(now_ms()),
            provider_timestamp_ms: self.timestamp,
            source: Some(spotuify_core::PlaybackStateSource::WebApiPoll),
        }
    }
}

#[derive(Debug, Deserialize)]
struct DevicesResponse {
    devices: Vec<Device>,
}

#[derive(Debug, Deserialize)]
struct QueueResponse {
    currently_playing: Option<RawPlayable>,
    queue: Vec<RawPlayable>,
}

#[derive(Debug, Deserialize)]
struct CurrentUserResponse {
    id: String,
}

#[derive(Debug, Deserialize)]
struct RecentlyPlayedResponse {
    items: Vec<RecentlyPlayedItem>,
}

#[derive(Debug, Deserialize)]
struct RecentlyPlayedItem {
    track: RawTrack,
}

#[derive(Debug, Deserialize)]
struct SavedTrackItem {
    track: RawTrack,
}

#[derive(Debug, Deserialize)]
struct SavedAlbumItem {
    album: RawAlbum,
}

#[derive(Debug, Deserialize)]
struct SavedShowItem {
    show: RawShow,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    tracks: Option<Paging<RawTrack>>,
    episodes: Option<Paging<RawEpisode>>,
    shows: Option<Paging<RawShow>>,
    albums: Option<Paging<RawAlbum>>,
    artists: Option<Paging<RawArtist>>,
    playlists: Option<Paging<Option<RawPlaylist>>>,
}

#[derive(Debug, Deserialize)]
struct Paging<T> {
    items: Vec<T>,
    total: u64,
}

#[derive(Debug, Deserialize)]
struct PlaylistTrackItem {
    #[serde(alias = "item")]
    track: Option<RawPlayable>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type")]
enum RawPlayable {
    #[serde(rename = "track")]
    Track(RawTrack),
    #[serde(rename = "episode")]
    Episode(RawEpisode),
    #[serde(other)]
    Other,
}

impl RawPlayable {
    fn into_media_item(self) -> Option<MediaItem> {
        match self {
            Self::Track(track) => Some(track.into_media_item()),
            Self::Episode(episode) => Some(episode.into_media_item()),
            Self::Other => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct RawTrack {
    id: Option<String>,
    uri: String,
    name: String,
    duration_ms: u64,
    explicit: Option<bool>,
    is_playable: Option<bool>,
    #[serde(default, deserialize_with = "null_to_default")]
    artists: Vec<SimpleNamed>,
    album: RawAlbum,
}

impl RawTrack {
    fn into_media_item(self) -> MediaItem {
        let artists = join_names(&self.artists);
        MediaItem {
            id: self.id,
            uri: self.uri,
            name: self.name,
            subtitle: artists,
            context: self.album.name.clone(),
            duration_ms: self.duration_ms,
            image_url: image_url(&self.album.images),
            kind: MediaKind::Track,
            source: Some("spotify".to_string()),
            freshness: None,
            explicit: self.explicit,
            is_playable: self.is_playable,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct RawAlbumTrack {
    id: Option<String>,
    uri: String,
    name: String,
    duration_ms: u64,
    explicit: Option<bool>,
    is_playable: Option<bool>,
    #[serde(default, deserialize_with = "null_to_default")]
    artists: Vec<SimpleNamed>,
}

impl RawAlbumTrack {
    fn into_media_item(self) -> MediaItem {
        MediaItem {
            id: self.id,
            uri: self.uri,
            name: self.name,
            subtitle: join_names(&self.artists),
            context: String::new(),
            duration_ms: self.duration_ms,
            image_url: None,
            kind: MediaKind::Track,
            source: Some("spotify".to_string()),
            freshness: None,
            explicit: self.explicit,
            is_playable: self.is_playable,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct RawEpisode {
    id: Option<String>,
    uri: String,
    name: String,
    duration_ms: u64,
    show: Option<SimpleShow>,
    #[serde(default, deserialize_with = "null_to_default")]
    images: Vec<ImageRef>,
}

impl RawEpisode {
    fn into_media_item(self) -> MediaItem {
        let show = self
            .show
            .map(|show| show.name)
            .unwrap_or_else(|| "Podcast episode".to_string());
        MediaItem {
            id: self.id,
            uri: self.uri,
            name: self.name,
            subtitle: show.clone(),
            context: show,
            duration_ms: self.duration_ms,
            image_url: image_url(&self.images),
            kind: MediaKind::Episode,
            source: Some("spotify".to_string()),
            freshness: None,
            explicit: None,
            is_playable: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct RawShow {
    id: Option<String>,
    uri: String,
    name: String,
    publisher: Option<String>,
    #[serde(default, deserialize_with = "null_to_default")]
    images: Vec<ImageRef>,
    total_episodes: Option<u64>,
}

impl RawShow {
    fn into_media_item(self) -> MediaItem {
        MediaItem {
            id: self.id,
            uri: self.uri,
            name: self.name,
            subtitle: self.publisher.unwrap_or_else(|| "Podcast".to_string()),
            context: self
                .total_episodes
                .map(|count| format!("{count} episodes"))
                .unwrap_or_default(),
            duration_ms: 0,
            image_url: image_url(&self.images),
            kind: MediaKind::Show,
            source: Some("spotify".to_string()),
            freshness: None,
            explicit: None,
            is_playable: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct RawAlbum {
    id: Option<String>,
    uri: String,
    name: String,
    #[serde(default, deserialize_with = "null_to_default")]
    artists: Vec<SimpleNamed>,
    #[serde(default, deserialize_with = "null_to_default")]
    images: Vec<ImageRef>,
    total_tracks: Option<u64>,
}

impl RawAlbum {
    fn into_media_item(self) -> MediaItem {
        let artists = join_names(&self.artists);
        MediaItem {
            id: self.id,
            uri: self.uri,
            name: self.name,
            subtitle: artists,
            context: self
                .total_tracks
                .map(|n| format!("{n} tracks"))
                .unwrap_or_default(),
            duration_ms: 0,
            image_url: image_url(&self.images),
            kind: MediaKind::Album,
            source: Some("spotify".to_string()),
            freshness: None,
            explicit: None,
            is_playable: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct RawArtist {
    id: Option<String>,
    uri: String,
    name: String,
    #[serde(default, deserialize_with = "null_to_default")]
    images: Vec<ImageRef>,
    followers: Option<Followers>,
}

impl RawArtist {
    fn into_media_item(self) -> MediaItem {
        MediaItem {
            id: self.id,
            uri: self.uri,
            name: self.name,
            subtitle: "Artist".to_string(),
            context: self
                .followers
                .map(|followers| format_followers(followers.total))
                .unwrap_or_default(),
            duration_ms: 0,
            image_url: image_url(&self.images),
            kind: MediaKind::Artist,
            source: Some("spotify".to_string()),
            freshness: None,
            explicit: None,
            is_playable: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct RawPlaylist {
    id: Option<String>,
    uri: Option<String>,
    name: Option<String>,
    owner: Option<PlaylistOwner>,
    #[serde(alias = "items")]
    tracks: Option<PlaylistTracks>,
    #[serde(default, deserialize_with = "null_to_default")]
    images: Vec<ImageRef>,
    /// Spotify's playlist-version token. Phase 6.5 sync refetch gate
    /// reads this to skip /playlists/{id}/tracks when unchanged.
    #[serde(default)]
    snapshot_id: Option<String>,
}

impl RawPlaylist {
    fn into_playlist(self) -> Option<Playlist> {
        let id = self.id?;
        let tracks_total = self.tracks.as_ref().map(|tracks| tracks.total).unwrap_or(0);
        let snapshot_id = self.snapshot_id.clone();
        Some(Playlist {
            id,
            name: self.name.unwrap_or_else(|| "Untitled playlist".to_string()),
            owner: playlist_owner_name(self.owner),
            tracks_total,
            image_url: image_url(&self.images),
            snapshot_id,
        })
    }

    fn into_media_item(self) -> Option<MediaItem> {
        let id = self.id?;
        let tracks_total = self.tracks.as_ref().map(|tracks| tracks.total).unwrap_or(0);
        Some(MediaItem {
            uri: self.uri.unwrap_or_else(|| format!("spotify:playlist:{id}")),
            id: Some(id),
            name: self.name.unwrap_or_else(|| "Untitled playlist".to_string()),
            subtitle: playlist_owner_name(self.owner),
            context: format!("{tracks_total} tracks"),
            duration_ms: 0,
            image_url: image_url(&self.images),
            kind: MediaKind::Playlist,
            source: Some("spotify".to_string()),
            freshness: None,
            explicit: None,
            is_playable: None,
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
struct SimpleNamed {
    name: String,
}

#[derive(Clone, Debug, Deserialize)]
struct SimpleShow {
    name: String,
}

#[derive(Clone, Debug, Deserialize)]
struct PlaylistOwner {
    id: Option<String>,
    display_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct PlaylistTracks {
    total: u64,
}

#[derive(Clone, Debug, Deserialize)]
struct Followers {
    total: u64,
}

/// Render a follower count with k/M suffixes. `0` returns an empty
/// string so we don't lie about an artist having zero followers
/// (Spotify's search response is unreliable on this field; hydration
/// fills it in but if even that fails we'd rather show nothing).
fn format_followers(total: u64) -> String {
    if total == 0 {
        return String::new();
    }
    if total >= 1_000_000 {
        let m = total as f64 / 1_000_000.0;
        if m >= 10.0 {
            format!("{:.0}M followers", m)
        } else {
            format!("{:.1}M followers", m)
        }
    } else if total >= 1_000 {
        let k = total as f64 / 1_000.0;
        if k >= 100.0 {
            format!("{:.0}K followers", k)
        } else {
            format!("{:.1}K followers", k)
        }
    } else {
        format!("{} followers", total)
    }
}

/// Response shape for `GET /v1/artists?ids=…`. Items can be `null`
/// when an id wasn't found, so use `Option<RawArtist>`.
#[derive(Clone, Debug, Deserialize)]
struct ArtistsBatchResponse {
    artists: Vec<Option<RawArtist>>,
}

#[derive(Clone, Debug, Deserialize)]
struct ImageRef {
    url: Option<String>,
    width: Option<u32>,
}

/// Spotify returns `{ "snapshot_id": "..." }` on playlist mutations
/// (add/remove/reorder/replace). The new snapshot is the concurrency
/// token for the next mutation — the daemon persists it so the next
/// undo can compare against it.
#[derive(Debug, Deserialize)]
struct SnapshotResponse {
    snapshot_id: String,
}

// --- Phase 12 (P12-A) URL/body helpers (pure, unit-testable) ---

/// Build the JSON body for `DELETE /playlists/{id}/tracks`.
/// Spotify expects `{ "tracks": [{ "uri": "..." }, ...], "snapshot_id"? }`.
fn playlist_remove_items_body(uris: &[String], snapshot_id: Option<&str>) -> serde_json::Value {
    let tracks: Vec<serde_json::Value> = uris
        .iter()
        .map(|uri| serde_json::json!({ "uri": uri }))
        .collect();
    match snapshot_id {
        Some(snap) => serde_json::json!({ "tracks": tracks, "snapshot_id": snap }),
        None => serde_json::json!({ "tracks": tracks }),
    }
}

/// Build the JSON body for `PUT /playlists/{id}/tracks` reorder.
fn playlist_reorder_body(
    range_start: u32,
    insert_before: u32,
    range_length: u32,
    snapshot_id: Option<&str>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "range_start": range_start,
        "range_length": range_length,
        "insert_before": insert_before,
    });
    if let Some(snap) = snapshot_id {
        body["snapshot_id"] = serde_json::Value::String(snap.to_string());
    }
    body
}

/// Group `(uri, position)` items into `BTreeMap<position, Vec<uri>>`
/// so re-adds use the fewest possible API calls. BTreeMap keeps
/// positions sorted; smallest position first means later inserts
/// don't shift earlier ones.
fn group_items_by_position(
    items: &[(String, u32)],
) -> std::collections::BTreeMap<u32, Vec<String>> {
    let mut grouped: std::collections::BTreeMap<u32, Vec<String>> =
        std::collections::BTreeMap::new();
    for (uri, position) in items {
        grouped.entry(*position).or_default().push(uri.clone());
    }
    grouped
}

/// Resolve a Spotify URI to its library endpoint path and id.
///
/// Spotify deprecated the type-specific save/remove endpoints such as
/// `/me/tracks` in favor of `/me/library?uris=spotify%3Atrack%3A...`.
/// Artists still use the follow endpoint because the new library write
/// endpoint does not accept artist URIs.
fn library_endpoint_for_uri(uri: &str) -> AnyResult<(String, String)> {
    let id = uri
        .rsplit(':')
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("malformed Spotify URI `{uri}`"))?
        .to_string();
    let path = match crate::selection::media_kind_from_uri(uri)? {
        MediaKind::Track | MediaKind::Album | MediaKind::Episode | MediaKind::Show => {
            let encoded_uri = encode_component(uri);
            format!("/me/library?uris={encoded_uri}")
        }
        MediaKind::Artist => format!("/me/following?type=artist&ids={id}"),
        MediaKind::Playlist => bail!(
            "playlists are saved/unsaved via /playlists/{{id}}/followers, \
             not /me/{{tracks,albums,episodes,artists}}"
        ),
    };
    Ok((path, id))
}

fn playlist_owner_name(owner: Option<PlaylistOwner>) -> String {
    owner
        .and_then(|owner| owner.display_name.or(owner.id))
        .unwrap_or_else(|| "Unknown owner".to_string())
}

fn null_to_default<'de, D, T>(deserializer: D) -> std::result::Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

fn join_names(items: &[SimpleNamed]) -> String {
    items
        .iter()
        .map(|item| item.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn image_url(images: &[ImageRef]) -> Option<String> {
    images
        .iter()
        .filter(|image| image.url.is_some())
        .min_by_key(|image| image.width.unwrap_or(u32::MAX).abs_diff(300))
        .and_then(|image| image.url.clone())
}

fn fake_playback() -> Playback {
    Playback {
        item: Some(fake_track()),
        device: Some(fake_device()),
        is_playing: true,
        progress_ms: 42_000,
        shuffle: false,
        repeat: "off".to_string(),
        sampled_at_ms: Some(now_ms()),
        provider_timestamp_ms: None,
        source: Some(spotuify_core::PlaybackStateSource::WebApiPoll),
    }
}

fn fake_device() -> Device {
    Device {
        id: Some("fake-device".to_string()),
        name: "spotuify-fake".to_string(),
        kind: "Computer".to_string(),
        is_active: true,
        is_restricted: false,
        volume_percent: Some(70),
        supports_volume: true,
    }
}

fn fake_search_results(query: &str, kinds: &[MediaKind], limit: u8) -> Vec<MediaItem> {
    if query.trim().is_empty() {
        return Vec::new();
    }

    fake_catalog()
        .into_iter()
        .filter(|item| kinds.iter().any(|kind| kind == &item.kind))
        .filter(|item| fake_matches_query(item, query))
        .take(limit as usize)
        .collect()
}

fn fake_matches_query(item: &MediaItem, query: &str) -> bool {
    let haystack = format!("{} {} {}", item.name, item.subtitle, item.context).to_ascii_lowercase();
    query
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .all(|token| haystack.contains(&token))
}

fn fake_catalog() -> Vec<MediaItem> {
    vec![
        fake_track(),
        fake_second_track(),
        fake_album(),
        fake_artist(),
        fake_playlist_media_item(),
    ]
}

fn fake_track() -> MediaItem {
    MediaItem {
        id: Some("never-too-much".to_string()),
        uri: "spotify:track:never-too-much".to_string(),
        name: "Never Too Much".to_string(),
        subtitle: "Luther Vandross".to_string(),
        context: "Never Too Much".to_string(),
        duration_ms: 221_000,
        image_url: None,
        kind: MediaKind::Track,
        source: Some("fake".to_string()),
        freshness: None,
        explicit: Some(false),
        is_playable: Some(true),
    }
}

fn fake_second_track() -> MediaItem {
    MediaItem {
        id: Some("sweet-thing".to_string()),
        uri: "spotify:track:sweet-thing".to_string(),
        name: "Sweet Thing".to_string(),
        subtitle: "Chaka Khan".to_string(),
        context: "Rufus featuring Chaka Khan".to_string(),
        duration_ms: 199_000,
        image_url: None,
        kind: MediaKind::Track,
        source: Some("fake".to_string()),
        freshness: None,
        explicit: Some(false),
        is_playable: Some(true),
    }
}

fn fake_album() -> MediaItem {
    MediaItem {
        id: Some("never-too-much-album".to_string()),
        uri: "spotify:album:never-too-much-album".to_string(),
        name: "Never Too Much".to_string(),
        subtitle: "Luther Vandross".to_string(),
        context: "7 tracks".to_string(),
        duration_ms: 0,
        image_url: None,
        kind: MediaKind::Album,
        source: Some("fake".to_string()),
        freshness: None,
        explicit: None,
        is_playable: None,
    }
}

fn fake_artist() -> MediaItem {
    MediaItem {
        id: Some("luther-vandross".to_string()),
        uri: "spotify:artist:luther-vandross".to_string(),
        name: "Luther Vandross".to_string(),
        subtitle: "Artist".to_string(),
        context: "1000000 followers".to_string(),
        duration_ms: 0,
        image_url: None,
        kind: MediaKind::Artist,
        source: Some("fake".to_string()),
        freshness: None,
        explicit: None,
        is_playable: None,
    }
}

fn fake_playlist_media_item() -> MediaItem {
    MediaItem {
        id: Some("quiet-storm".to_string()),
        uri: "spotify:playlist:quiet-storm".to_string(),
        name: "Quiet Storm".to_string(),
        subtitle: "Fake User".to_string(),
        context: "2 tracks".to_string(),
        duration_ms: 0,
        image_url: None,
        kind: MediaKind::Playlist,
        source: Some("fake".to_string()),
        freshness: None,
        explicit: None,
        is_playable: None,
    }
}

fn fake_playlists() -> Vec<Playlist> {
    vec![Playlist {
        id: "quiet-storm".to_string(),
        name: "Quiet Storm".to_string(),
        owner: "Fake User".to_string(),
        tracks_total: 2,
        image_url: None,
        snapshot_id: Some("fake-snap-1".to_string()),
    }]
}

fn fake_playlist_id(name: &str) -> String {
    name.to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

fn selection_like_uri_check(uri: &str) -> AnyResult<()> {
    if uri.starts_with("spotify:track:")
        || uri.starts_with("spotify:episode:")
        || uri.starts_with("spotify:album:")
        || uri.starts_with("spotify:artist:")
        || uri.starts_with("spotify:playlist:")
    {
        Ok(())
    } else {
        bail!("unsupported Spotify URI `{uri}`")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        format_followers, group_items_by_position, library_endpoint_for_uri,
        normalize_spotify_response, playlist_remove_items_body, playlist_reorder_body, search_path,
        Config, MediaKind, SpotifyClient,
    };
    use reqwest::Method;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_config() -> Config {
        Config {
            client_id: "test-client-id".to_string(),
            client_secret: Some("test-client-secret".to_string()),
            redirect_uri: "http://127.0.0.1:8888/callback".to_string(),
            config_path: PathBuf::from("test-spotuify.toml"),
            player: crate::config::PlayerConfig::default(),
            cache: crate::config::CacheConfig::default(),
            analytics: crate::config::AnalyticsConfig::default(),
            notifications: crate::config::NotificationsConfig::default(),
            discord: crate::config::DiscordConfig::default(),
            viz: crate::config::VizConfig::default(),
        }
    }

    fn token_cache() -> Arc<Mutex<Option<super::StoredToken>>> {
        Arc::new(Mutex::new(Some(super::StoredToken {
            access_token: "test-access".to_string(),
            refresh_token: "test-refresh".to_string(),
            expires_at: 4_000_000_000,
            scope: "user-modify-playback-state user-library-modify user-follow-modify".to_string(),
            token_type: "Bearer".to_string(),
        })))
    }

    async fn test_client(server: &MockServer) -> SpotifyClient {
        SpotifyClient::new(test_config())
            .expect("test client should build")
            .with_api_base_for_tests(format!("{}/v1", server.uri()))
            .with_token_cache(token_cache())
    }

    // Bodyless PUT/POST/DELETE behavior — verifying the JSON-object
    // body contract that Spotify's edge accepts — lives in
    // `tests/client_empty_body.rs`. Earlier inline tests that asserted
    // `content-length: 0` exactly are removed: they pinned an
    // implementation detail (the literal header value the helper set),
    // not the user-facing behavior, and locked the codebase into a
    // contract that real Spotify rejects with HTTP 411.

    #[tokio::test]
    async fn album_tracks_fetches_track_uris_for_queue_expansion() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/albums/album-one/tracks"))
            .and(query_param("limit", "50"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total": 1,
                "items": [{
                    "id": "track-one",
                    "uri": "spotify:track:track-one",
                    "name": "Track One",
                    "duration_ms": 123000,
                    "artists": [{"name": "Artist One"}]
                }]
            })))
            .mount(&server)
            .await;

        let mut client = test_client(&server).await;
        let tracks = client
            .album_tracks("spotify:album:album-one")
            .await
            .expect("album tracks should load");

        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].uri, "spotify:track:track-one");
        assert_eq!(tracks[0].name, "Track One");
        assert_eq!(tracks[0].subtitle, "Artist One");
    }

    #[test]
    fn search_path_uses_valid_spotify_type_and_limit_params() {
        assert_eq!(
            search_path("luther vandross", &[MediaKind::Track], 10, 0),
            "/search?q=luther+vandross&type=track&limit=10&offset=0"
        );
    }

    #[test]
    fn search_path_clamps_to_empirical_per_type_max() {
        // 10 is the empirical cap — Spotify rejects anything above it
        // with 400 "Invalid limit" despite docs claiming a 50 max.
        // Verified against the live API on 2026-05-17.
        assert_eq!(
            search_path("jazz", &[MediaKind::Track], 50, 0),
            "/search?q=jazz&type=track&limit=10&offset=0"
        );
        assert_eq!(
            search_path("jazz", &[MediaKind::Track], 200, 0),
            "/search?q=jazz&type=track&limit=10&offset=0"
        );
        // Values below the cap pass through unchanged.
        assert_eq!(
            search_path("jazz", &[MediaKind::Track], 5, 0),
            "/search?q=jazz&type=track&limit=5&offset=0"
        );
    }

    #[test]
    fn search_path_includes_offset_for_pagination() {
        assert_eq!(
            search_path("love", &[MediaKind::Track], 10, 30),
            "/search?q=love&type=track&limit=10&offset=30"
        );
    }

    #[test]
    fn format_followers_renders_human_readable_counts() {
        // Zero collapses to empty so we don't render a misleading
        // "0 followers" subtitle when Spotify omits the real count.
        assert_eq!(format_followers(0), "");
        // Single digits and small numbers pass through verbatim.
        assert_eq!(format_followers(7), "7 followers");
        assert_eq!(format_followers(999), "999 followers");
        // Thousands use K with one decimal when small, no decimal when large.
        assert_eq!(format_followers(1_234), "1.2K followers");
        assert_eq!(format_followers(99_500), "99.5K followers");
        assert_eq!(format_followers(150_000), "150K followers");
        // Millions use M.
        assert_eq!(format_followers(1_200_000), "1.2M followers");
        assert_eq!(format_followers(45_700_000), "46M followers");
    }

    // --- Phase 12 (P12-A) inverse mutator shape tests ---

    #[test]
    fn playlist_remove_items_body_emits_tracks_array_with_uri_field_per_spotify_api() {
        let uris = vec!["spotify:track:1".to_string(), "spotify:track:2".to_string()];
        let body = playlist_remove_items_body(&uris, None);
        let tracks = body["tracks"]
            .as_array()
            .expect("body must contain a tracks array");
        assert_eq!(tracks.len(), 2);
        assert_eq!(
            tracks[0]["uri"].as_str().expect("track 0 should have uri"),
            "spotify:track:1"
        );
        assert_eq!(
            tracks[1]["uri"].as_str().expect("track 1 should have uri"),
            "spotify:track:2"
        );
        // snapshot_id is absent when not provided; presence forces
        // Spotify's optimistic-concurrency precondition which we only
        // want when the daemon captured one.
        assert!(body.get("snapshot_id").is_none());
    }

    #[test]
    fn playlist_remove_items_body_includes_snapshot_id_when_present() {
        let body = playlist_remove_items_body(&["spotify:track:x".to_string()], Some("snap-A"));
        assert_eq!(
            body["snapshot_id"]
                .as_str()
                .expect("body should contain snapshot_id"),
            "snap-A"
        );
    }

    #[test]
    fn playlist_reorder_body_carries_all_three_position_fields_and_snapshot() {
        let body = playlist_reorder_body(2, 0, 1, Some("snap-Z"));
        assert_eq!(
            body["range_start"]
                .as_u64()
                .expect("body should contain range_start"),
            2
        );
        assert_eq!(
            body["range_length"]
                .as_u64()
                .expect("body should contain range_length"),
            1
        );
        assert_eq!(
            body["insert_before"]
                .as_u64()
                .expect("body should contain insert_before"),
            0
        );
        assert_eq!(
            body["snapshot_id"]
                .as_str()
                .expect("body should contain snapshot_id"),
            "snap-Z"
        );
    }

    #[test]
    fn playlist_reorder_body_omits_snapshot_when_unknown() {
        // Spotify rejects requests where snapshot_id is the literal
        // empty string, so we must omit the field entirely when None.
        let body = playlist_reorder_body(0, 5, 3, None);
        assert!(body.get("snapshot_id").is_none());
    }

    #[test]
    fn group_items_by_position_collapses_repeats_and_orders_ascending() {
        let items = vec![
            ("spotify:track:a".to_string(), 3),
            ("spotify:track:b".to_string(), 0),
            ("spotify:track:c".to_string(), 3),
        ];
        let grouped = group_items_by_position(&items);
        let positions: Vec<u32> = grouped.keys().copied().collect();
        // BTreeMap ordering means we process the lowest-position
        // bucket first; that prevents later inserts from shifting
        // earlier indices in the playlist.
        assert_eq!(positions, vec![0, 3]);
        assert_eq!(grouped[&0], vec!["spotify:track:b".to_string()]);
        assert_eq!(
            grouped[&3],
            vec!["spotify:track:a".to_string(), "spotify:track:c".to_string()]
        );
    }

    #[test]
    fn compat_wiring_normalizes_search_paging_and_nested_tracks() {
        let mut value = json!({
            "tracks": {
                "items": [{
                    "type": "track",
                    "id": "t1",
                    "uri": "spotify:track:t1",
                    "name": "Track One",
                    "duration_ms": 100,
                    "artists": [{"name": "Artist"}],
                    "album": {
                        "id": "a1",
                        "uri": "spotify:album:a1",
                        "name": "Album One",
                        "artists": [{"name": "Artist"}],
                        "images": []
                    }
                }]
            }
        });

        let patched = normalize_spotify_response("/search?q=x&type=track&limit=10", &mut value);

        assert!(patched.contains(&"tracks.total".to_string()));
        assert!(patched.contains(&"tracks.items.popularity".to_string()));
        assert!(patched.contains(&"album.popularity".to_string()));
        let response: super::SearchResponse =
            serde_json::from_value(value).expect("normalized search should deserialize");
        assert_eq!(
            response
                .tracks
                .expect("tracks page")
                .items
                .into_iter()
                .next()
                .expect("track")
                .uri,
            "spotify:track:t1"
        );
    }

    #[test]
    fn compat_wiring_normalizes_playlist_listing_paging() {
        let mut value = json!({
            "items": [{
                "id": "p1",
                "name": "Playlist One",
                "owner": {"display_name": "Owner"},
                "items": {"total": 7}
            }]
        });

        let patched = normalize_spotify_response("/me/playlists?limit=50&offset=0", &mut value);

        assert!(patched.contains(&"paging.total".to_string()));
        assert!(patched.contains(&"items.followers".to_string()));
        let page: super::Paging<Option<super::RawPlaylist>> =
            serde_json::from_value(value).expect("normalized playlists should deserialize");
        assert_eq!(page.total, 1);
        assert_eq!(
            page.items
                .into_iter()
                .flatten()
                .next()
                .expect("playlist")
                .into_playlist()
                .expect("playlist output")
                .tracks_total,
            7
        );
    }

    #[test]
    fn playlist_items_endpoint_shape_deserializes() {
        let value = json!({
            "total": 2,
            "items": [
                {
                    "item": {
                        "type": "track",
                        "id": "t1",
                        "uri": "spotify:track:t1",
                        "name": "Playable",
                        "duration_ms": 123000,
                        "explicit": false,
                        "is_playable": true,
                        "artists": [{"name": "Artist"}],
                        "album": {
                            "id": "a1",
                            "uri": "spotify:album:a1",
                            "name": "Album",
                            "images": []
                        }
                    }
                },
                {"item": null}
            ]
        });

        let page: super::Paging<super::PlaylistTrackItem> =
            serde_json::from_value(value).expect("playlist items should deserialize");
        assert_eq!(page.total, 2);
        let tracks = page
            .items
            .into_iter()
            .filter_map(|item| item.track.and_then(super::RawPlayable::into_media_item))
            .collect::<Vec<_>>();

        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].uri, "spotify:track:t1");
    }

    #[test]
    fn library_endpoint_for_uri_routes_each_media_kind_to_correct_spotify_endpoint() {
        let cases = [
            (
                "spotify:track:abc",
                "/me/library?uris=spotify%3Atrack%3Aabc",
            ),
            (
                "spotify:album:xyz",
                "/me/library?uris=spotify%3Aalbum%3Axyz",
            ),
            (
                "spotify:episode:e1",
                "/me/library?uris=spotify%3Aepisode%3Ae1",
            ),
            ("spotify:show:s1", "/me/library?uris=spotify%3Ashow%3As1"),
            ("spotify:artist:a1", "/me/following?type=artist&ids=a1"),
        ];
        for (uri, expected_path) in cases {
            let (path, _id) = library_endpoint_for_uri(uri)
                .expect("supported library URI should map to endpoint");
            assert_eq!(path, expected_path, "wrong endpoint for {uri}");
        }
    }

    #[test]
    fn user_agent_string_carries_version_os_arch_and_github_url() {
        // Operators triaging Spotify API logs need at least the
        // version, OS, and arch fields to be present and machine-
        // parseable. The GitHub URL is etiquette for third-party
        // services like LRCLIB.
        let ua = super::user_agent_string();
        assert!(ua.starts_with(&format!("spotuify/{}", env!("CARGO_PKG_VERSION"))));
        assert!(ua.contains(std::env::consts::OS));
        assert!(ua.contains(std::env::consts::ARCH));
        assert!(ua.contains("https://github.com/planetaryescape/spotuify"));
    }

    #[test]
    fn empty_body_methods_include_spotify_playback_puts() {
        assert!(super::method_accepts_empty_body(&Method::PUT));
        assert!(super::method_accepts_empty_body(&Method::POST));
        assert!(!super::method_accepts_empty_body(&Method::GET));
    }

    #[test]
    fn library_endpoint_for_uri_rejects_playlists() {
        // Playlists are followed/unfollowed via /playlists/{id}/followers,
        // not the generic /me/* family. Calling library_save on a
        // playlist URI by accident would silently 404; we'd rather
        // bail with a clear error.
        let err = library_endpoint_for_uri("spotify:playlist:p1")
            .expect_err("playlist URIs should not map to library endpoints");
        assert!(
            err.to_string().contains("playlists"),
            "expected playlist-specific error, got `{err}`"
        );
    }
}
