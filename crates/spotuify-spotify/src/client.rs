use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result as AnyResult};
use base64::Engine as _;
use reqwest::{Client, Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use tokio::sync::Mutex;

use spotuify_core::{
    now_ms, provider_api_finished_event, AlbumGroup, AnalyticsEvent, AnalyticsSource, Page,
    ProviderId, ReleaseDate, RepeatMode, ResourceUri, UriScheme,
};

use crate::auth::{self, StoredToken};
use crate::compat::{compat_normalize, NormalizeHint};
use crate::config::Config;
use crate::endpoints;
use crate::error::{SpotifyError, SpotifyResult};
use crate::rate_limit::{Priority, RateLimitedClient};

// Re-export domain types used by the adapter's public client API.
pub use spotuify_core::{
    ArtistRef, Device, MediaItem, MediaKind, Playback, Playlist, Queue, TrackId,
};

const API: &str = "https://api.spotify.com/v1";
const LOCAL_TRACK_SURROGATE_PREFIX: &str = "local~";

pub trait SchemaCompatReporter: Send + Sync {
    fn report_schema_compat(&self, endpoint: &str, missing_keys: &[String]);
}

/// Source of the Web API bearer when running in first-party (keymaster)
/// mode. The daemon implements this by minting via `login5` over the
/// live librespot session (with an OAuth-refresh bootstrap/fallback).
///
/// When a provider is attached, the client takes the bearer from it
/// instead of the dev-app PKCE refresh path in [`crate::auth`]. This is
/// the cutover seam: it leaves the entire legacy dev-app flow intact and
/// untouched (provider `None` == legacy behaviour), so a user who sets
/// `SPOTUIFY_CLIENT_ID` still gets their own-app token.
#[async_trait::async_trait]
pub trait WebApiBearerProvider: Send + Sync {
    /// Return a Web API bearer. `force_refresh` asks for a freshly
    /// minted token (used after a 401 so a stale bearer is replaced).
    async fn bearer(&self, force_refresh: bool) -> SpotifyResult<String>;
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
    provider_id: ProviderId,
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
    token_cache: Arc<Mutex<Option<StoredToken>>>,
    /// When set, the Web API bearer is minted by this provider
    /// (first-party / login5) instead of the dev-app PKCE refresh path.
    bearer_provider: Option<Arc<dyn WebApiBearerProvider>>,
    /// Hybrid auth: when set, playlist/library WRITE endpoints (the ones a
    /// Development-Mode dev app 403s on — see [`endpoint_needs_first_party`])
    /// take their bearer from THIS provider (first-party / login5) while
    /// every other request keeps using the primary source above. Only
    /// attached in dev-app-primary mode when a first-party credential also
    /// exists on disk; `None` leaves the single-source behaviour intact.
    write_bearer_provider: Option<Arc<dyn WebApiBearerProvider>>,
    /// SHA-1-hex device_id our embedded librespot publishes (deterministic,
    /// derived from the registered device name). Optional because pure
    /// CLI / tests construct clients without an embedded session.
    /// Threaded through to `preferred_device` so device selection prefers
    /// our own live entry over stale namesakes in `/v1/me/player/devices`.
    own_device_id: Option<String>,
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
            provider_id: ProviderId::new("spotify").expect("built-in provider id is valid"),
            config,
            api_base: API.to_string(),
            http,
            rate_limiter,
            analytics: None,
            schema_compat_reporter: None,
            analytics_source: AnalyticsSource::Cli,
            default_priority: Priority::Foreground,
            token_cache: Arc::new(Mutex::new(None)),
            bearer_provider: None,
            write_bearer_provider: None,
            own_device_id: None,
        }
    }

    pub fn with_provider_id(mut self, provider_id: ProviderId) -> Self {
        self.provider_id = provider_id;
        self
    }

    pub(crate) fn provider_id(&self) -> &ProviderId {
        &self.provider_id
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

    /// Attach a first-party bearer provider (login5). When set, the
    /// client mints its bearer through `provider` instead of the dev-app
    /// PKCE refresh path.
    pub fn with_bearer_provider(mut self, provider: Arc<dyn WebApiBearerProvider>) -> Self {
        self.bearer_provider = Some(provider);
        self
    }

    /// Attach a first-party WRITE bearer provider for hybrid auth. When
    /// set, only the playlist/library write endpoints that a Development-
    /// Mode dev app 403s on ([`endpoint_needs_first_party`]) take their
    /// bearer from `provider`; reads, polling, and playback control keep
    /// using the primary source. Leaving it unset keeps the single-source
    /// behaviour, so the two legacy modes are byte-for-byte unchanged.
    pub fn with_write_bearer_provider(mut self, provider: Arc<dyn WebApiBearerProvider>) -> Self {
        self.write_bearer_provider = Some(provider);
        self
    }

    /// Current Web API bearer for a request to `method path`. In hybrid
    /// mode a playlist/library WRITE routes to the first-party write
    /// provider; everything else falls through to the primary source
    /// (first-party provider when attached, otherwise the legacy dev-app
    /// PKCE cache/refresh path).
    async fn current_bearer(&self, method: &Method, path: &str) -> SpotifyResult<String> {
        if let Some(write_provider) = &self.write_bearer_provider {
            if endpoint_needs_first_party(method, path) {
                tracing::debug!(%method, path, "routing write to first-party bearer");
                return write_provider.bearer(false).await;
            }
        }
        match &self.bearer_provider {
            Some(provider) => provider.bearer(false).await,
            None => {
                auth::access_token_cached_for(
                    self.provider_id.as_str(),
                    &self.config,
                    &self.http,
                    &self.token_cache,
                )
                .await
            }
        }
    }

    /// Force a freshly minted bearer after a 401, routed the same way as
    /// [`Self::current_bearer`]: a hybrid write re-mints via the write
    /// provider; otherwise the primary source (first-party provider or
    /// dev-app refresh).
    async fn refresh_bearer(&self, method: &Method, path: &str) -> SpotifyResult<String> {
        if let Some(write_provider) = &self.write_bearer_provider {
            if endpoint_needs_first_party(method, path) {
                return write_provider.bearer(true).await;
            }
        }
        match &self.bearer_provider {
            Some(provider) => provider.bearer(true).await,
            None => {
                auth::refresh_access_token_cached_for(
                    self.provider_id.as_str(),
                    &self.config,
                    &self.http,
                    &self.token_cache,
                )
                .await
            }
        }
    }

    fn cooldown_scope(&self, method: &Method, path: &str, endpoint_scope: &str) -> String {
        let uses_first_party = (self.write_bearer_provider.is_some()
            && endpoint_needs_first_party(method, path))
            || self.bearer_provider.is_some();
        let bearer = if uses_first_party {
            "first-party"
        } else {
            "dev-app"
        };
        format!("{bearer} {endpoint_scope}")
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
        // Non-request accessor (diagnostics, player-slot publish): always
        // the PRIMARY bearer. A GET read path can never route to the
        // hybrid write provider, so this keeps returning the dev-app
        // (or first-party-primary) token regardless of hybrid attachment.
        self.current_bearer(&Method::GET, "/me").await
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
        self.record_analytics_event(provider_api_finished_event(
            AnalyticsSource::ProviderApi,
            method.as_str(),
            path,
            status.map(|status| status.as_u16()),
            // Narrow to u64 for the serde/IPC payload: u128 can't be
            // serialized by serde_json. Elapsed milliseconds never overflow u64.
            elapsed_ms as u64,
            error_class,
            now_ms(),
        ))
        .await;
    }

    pub async fn playback(&mut self) -> SpotifyResult<Playback> {
        match self
            .request_json::<PlaybackResponse>(Method::GET, endpoints::PLAYBACK, None::<()>)
            .await
        {
            Ok(Some(raw)) => Ok(raw.into_playback(self.provider_id.as_str())),
            Ok(None) => Ok(Playback::default()),
            Err(err) => Err(err.into()),
        }
    }

    pub async fn devices(&mut self) -> SpotifyResult<Vec<Device>> {
        let response = self
            .request_json::<DevicesResponse>(Method::GET, endpoints::DEVICES, None::<()>)
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no devices response"))?;
        Ok(response.devices)
    }

    pub async fn queue(&mut self) -> SpotifyResult<Queue> {
        let response = self
            .request_json::<QueueResponse>(Method::GET, endpoints::QUEUE, None::<()>)
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no queue response"))?;
        let currently_playing = response
            .currently_playing
            .and_then(|item| item.into_media_item(self.provider_id.as_str()));
        let items: Vec<_> = response
            .queue
            .into_iter()
            .filter_map(|item| item.into_media_item(self.provider_id.as_str()))
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
        let batches = futures::future::try_join_all(futures).await?;

        let mut items = Vec::new();
        let mut seen_uris: std::collections::HashSet<String> = std::collections::HashSet::new();
        for batch in batches {
            for item in batch.items {
                if seen_uris.insert(item.uri.clone()) {
                    items.push(item);
                }
            }
        }
        Ok(items)
    }

    pub(crate) async fn search_single_type(
        &self,
        query: &str,
        kind: MediaKind,
        limit: u8,
        offset: u32,
    ) -> SpotifyResult<SearchPageResult> {
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
                        return Ok(SearchPageResult {
                            items: Vec::new(),
                            total: Some(u64::from(offset)),
                            consumed: 0,
                        });
                    }
                }
                return Err(err.into());
            }
        };
        let mut items: Vec<MediaItem> = Vec::new();
        let mut total = None;
        let mut consumed = 0;
        if let Some(tracks) = response.tracks {
            total = Some(tracks.total);
            consumed = tracks.items.len() as u64;
            items.extend(
                tracks
                    .items
                    .into_iter()
                    .map(|item| item.into_media_item(self.provider_id.as_str())),
            );
        }
        if let Some(episodes) = response.episodes {
            total = Some(episodes.total);
            consumed = episodes.items.len() as u64;
            items.extend(
                episodes
                    .items
                    .into_iter()
                    .map(|item| item.into_media_item(self.provider_id.as_str())),
            );
        }
        if let Some(shows) = response.shows {
            total = Some(shows.total);
            consumed = shows.items.len() as u64;
            items.extend(
                shows
                    .items
                    .into_iter()
                    .map(|item| item.into_media_item(self.provider_id.as_str())),
            );
        }
        if let Some(albums) = response.albums {
            total = Some(albums.total);
            consumed = albums.items.len() as u64;
            items.extend(
                albums
                    .items
                    .into_iter()
                    .map(|item| item.into_media_item(self.provider_id.as_str())),
            );
        }
        if let Some(artists) = response.artists {
            total = Some(artists.total);
            consumed = artists.items.len() as u64;
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
            items.extend(
                raws.into_iter()
                    .map(|item| item.into_media_item(self.provider_id.as_str())),
            );
        }
        if let Some(playlists) = response.playlists {
            total = Some(playlists.total);
            consumed = playlists.items.len() as u64;
            items.extend(
                playlists
                    .items
                    .into_iter()
                    .flatten()
                    .filter_map(|item| item.into_media_item(self.provider_id.as_str())),
            );
        }
        Ok(SearchPageResult {
            items,
            total,
            consumed,
        })
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
        let path = format!(
            "{}?ids={}",
            endpoints::ARTISTS_LOOKUP,
            encode_component(&ids.join(","))
        );
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
                let total = raw.followers.map_or(0, |f| f.total);
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
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        Ok(self
            .search_single_type(query, kind, 10, offset)
            .await?
            .items)
    }

    pub async fn media_item_by_uri(&mut self, uri: &str) -> SpotifyResult<Option<MediaItem>> {
        let resource = spotify_uri(uri)?;
        if resource.kind() != MediaKind::Track {
            return Ok(None);
        }
        let Some(track_id) = TrackId::from_uri(uri) else {
            return Ok(None);
        };
        let path = endpoints::track(track_id.as_str());
        Ok(self
            .request_json::<RawTrack>(Method::GET, &path, None::<()>)
            .await?
            .map(|item| item.into_media_item(self.provider_id.as_str())))
    }

    pub async fn playlists_page(
        &mut self,
        limit: u8,
        offset: u64,
    ) -> SpotifyResult<Page<Playlist>> {
        let path = format!("{}?limit={limit}&offset={offset}", endpoints::MY_PLAYLISTS);
        let response = self
            .request_json::<Paging<Option<RawPlaylist>>>(Method::GET, &path, None::<()>)
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no playlists response"))?;
        Ok(Page {
            total: response.total,
            offset,
            items: response
                .items
                .into_iter()
                .flatten()
                .filter_map(RawPlaylist::into_playlist)
                .collect(),
        })
    }

    /// Fetch one playlist's metadata without walking the user's paginated
    /// playlist collection. Used when a terminal item-access response needs
    /// the exact observed version token.
    pub async fn playlist_metadata(&mut self, playlist_id: &str) -> SpotifyResult<Playlist> {
        let playlist_id = spotify_resource_id(playlist_id, MediaKind::Playlist)?;
        let path = endpoints::playlist(&playlist_id);
        self.request_json::<RawPlaylist>(Method::GET, &path, None::<()>)
            .await?
            .and_then(RawPlaylist::into_playlist)
            .ok_or(SpotifyError::NotFound)
    }

    pub async fn current_user_id(&mut self) -> SpotifyResult<String> {
        let response = self
            .request_json::<CurrentUserResponse>(Method::GET, endpoints::ME, None::<()>)
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
        // Use `POST /me/playlists` (current per Spotify docs), NOT the
        // older `POST /users/{user_id}/playlists` — the latter appears
        // to require Extended Quota Mode (returns 403 on dev-mode apps)
        // and was the silent cause of every playlist-create 403 we
        // diagnosed. `/me/playlists` works for any authenticated user
        // with `playlist-modify-public`/`playlist-modify-private` and
        // needs no user_id, so we also drop the prerequisite `GET /me`.
        let body = serde_json::json!({
            "name": name,
            "description": description.unwrap_or("Created by spotuify"),
            "public": public,
        });
        Ok(self
            .request_json::<RawPlaylist>(Method::POST, endpoints::MY_PLAYLISTS, Some(body))
            .await?
            .and_then(RawPlaylist::into_playlist)
            .ok_or_else(|| anyhow!("Spotify returned no created playlist"))?)
    }

    pub async fn recently_played(&mut self) -> SpotifyResult<Vec<MediaItem>> {
        let response = self
            .request_json::<RecentlyPlayedResponse>(
                Method::GET,
                format!("{}?limit=20", endpoints::RECENTLY_PLAYED).as_str(),
                None::<()>,
            )
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no recently played response"))?;
        Ok(response
            .items
            .into_iter()
            .map(|item| item.track.into_media_item(self.provider_id.as_str()))
            .collect())
    }

    pub async fn saved_tracks(&mut self) -> SpotifyResult<Vec<MediaItem>> {
        let mut offset = 0;
        let mut items = Vec::new();
        loop {
            let page = self.saved_tracks_page(50, offset).await?;
            let total = page.total;
            let fetched = page.items.len();
            items.extend(page.items);
            offset += 50;
            // Stop on an exhausted (empty) page, once we've reached the reported
            // `total`, or at Spotify's `limit + offset` 1000 wall — a
            // >1000-track library returns what it can instead of failing the
            // whole fetch (`/me/tracks` cannot paginate past offset 1000).
            if fetched == 0 || offset >= total || offset >= 1000 {
                break;
            }
        }
        Ok(items)
    }

    pub async fn saved_tracks_page(
        &mut self,
        limit: u8,
        offset: u64,
    ) -> SpotifyResult<Page<MediaItem>> {
        let path = format!("{}?limit={limit}&offset={offset}", endpoints::SAVED_TRACKS);
        let response = match self
            .request_json::<Paging<SavedTrackItem>>(Method::GET, &path, None::<()>)
            .await
        {
            Ok(Some(r)) => r,
            Ok(None) => return Err(anyhow!("Spotify returned no saved tracks response").into()),
            Err(err) => {
                // Spotify caps `limit + offset` at 1000. Past the wall we return
                // an exhausted (empty) page rather than erroring — the same
                // signal `search_single_type` uses. `total` reflects the wall
                // so the caller's paging loop stops cleanly.
                if let Some(SpotifyError::Api {
                    status: 400, body, ..
                }) = err.downcast_ref::<SpotifyError>()
                {
                    if body.contains("exceeds maximum of 1000") {
                        return Ok(Page {
                            items: Vec::new(),
                            total: offset,
                            offset,
                        });
                    }
                }
                return Err(err.into());
            }
        };
        Ok(Page {
            total: response.total,
            offset,
            items: response
                .items
                .into_iter()
                .map(|item| {
                    let added_at_ms = item.added_at.as_deref().and_then(parse_rfc3339_ms);
                    let mut media = item.track.into_media_item(self.provider_id.as_str());
                    media.added_at_ms = added_at_ms;
                    media
                })
                .collect(),
        })
    }

    pub async fn saved_albums_page(
        &mut self,
        limit: u8,
        offset: u64,
    ) -> SpotifyResult<Page<MediaItem>> {
        let path = format!("{}?limit={limit}&offset={offset}", endpoints::SAVED_ALBUMS);
        let response = self
            .request_json::<Paging<SavedAlbumItem>>(Method::GET, &path, None::<()>)
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no saved albums response"))?;
        Ok(Page {
            total: response.total,
            offset,
            items: response
                .items
                .into_iter()
                .map(|item| item.album.into_media_item(self.provider_id.as_str()))
                .collect(),
        })
    }

    /// Fetch the user's saved podcast episodes (`/me/episodes`).
    pub async fn saved_episodes_page(
        &mut self,
        limit: u8,
        offset: u64,
    ) -> SpotifyResult<Page<MediaItem>> {
        let path = format!(
            "{}?limit={limit}&offset={offset}",
            endpoints::SAVED_EPISODES
        );
        let response = self
            .request_json::<Paging<SavedEpisodeItem>>(Method::GET, &path, None::<()>)
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no saved episodes response"))?;
        Ok(Page {
            total: response.total,
            offset,
            items: response
                .items
                .into_iter()
                .map(|item| item.episode.into_media_item(self.provider_id.as_str()))
                .collect(),
        })
    }

    /// Fetch the user's saved podcasts (Spotify's `/me/shows`).
    /// The caller owns pagination so provider-level page requests map to one
    /// upstream request.
    pub async fn saved_shows_page(
        &mut self,
        limit: u8,
        offset: u64,
    ) -> SpotifyResult<Page<MediaItem>> {
        let path = format!("{}?limit={limit}&offset={offset}", endpoints::SAVED_SHOWS);
        let response = self
            .request_json::<Paging<SavedShowItem>>(Method::GET, &path, None::<()>)
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no saved shows response"))?;
        Ok(Page {
            total: response.total,
            offset,
            items: response
                .items
                .into_iter()
                .map(|item| item.show.into_media_item(self.provider_id.as_str()))
                .collect(),
        })
    }

    /// Artists the user follows (Spotify's `/me/following?type=artist`).
    /// Cursor-paginated: each page yields the next `after` artist id until
    /// `next` is null. The payload nests the page under an `artists` key.
    pub async fn followed_artists_page(
        &mut self,
        limit: u8,
        after: Option<&str>,
    ) -> SpotifyResult<(Vec<MediaItem>, Option<String>)> {
        let mut path = format!("{}?type=artist&limit={limit}", endpoints::FOLLOWING);
        if let Some(cursor) = after {
            path.push_str("&after=");
            path.push_str(cursor);
        }
        let response = self
            .request_json::<FollowingPage>(Method::GET, &path, None::<()>)
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no followed artists response"))?;
        let page = response.artists;
        let next = page
            .next
            .is_some()
            .then(|| page.cursors.and_then(|cursors| cursors.after))
            .flatten();
        Ok((
            page.items
                .into_iter()
                .map(|item| item.into_media_item(self.provider_id.as_str()))
                .collect(),
            next,
        ))
    }

    pub async fn playlist_tracks_page(
        &mut self,
        playlist_id: &str,
        limit: u8,
        offset: u64,
    ) -> SpotifyResult<Page<MediaItem>> {
        let playlist_id = spotify_resource_id(playlist_id, MediaKind::Playlist)?;
        let path = format!(
            "{}?limit={limit}&offset={offset}",
            endpoints::playlist_items(&playlist_id)
        );
        let response = self
            .request_json::<Paging<PlaylistTrackItem>>(Method::GET, &path, None::<()>)
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no playlist tracks response"))?;
        Ok(Page {
            total: response.total,
            offset,
            items: response
                .items
                .into_iter()
                .enumerate()
                .map(|(index, item)| {
                    item.into_media_item(
                        self.provider_id.as_str(),
                        &playlist_id,
                        offset.saturating_add(index as u64),
                    )
                })
                .collect(),
        })
    }

    pub async fn album_tracks_page(
        &mut self,
        album_id: &str,
        limit: u8,
        offset: u64,
    ) -> SpotifyResult<Page<MediaItem>> {
        let album_id = spotify_resource_id(album_id, MediaKind::Album)?;
        let path = format!(
            "{}?limit={limit}&offset={offset}",
            endpoints::album_tracks(&album_id)
        );
        let response = self
            .request_json::<Paging<RawAlbumTrack>>(Method::GET, &path, None::<()>)
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no album tracks response"))?;
        Ok(Page {
            total: response.total,
            offset,
            items: response
                .items
                .into_iter()
                .map(|item| item.into_media_item(self.provider_id.as_str()))
                .collect(),
        })
    }

    /// Albums for a given artist (Spotify's `/v1/artists/{id}/albums`).
    /// Fetches one page across all four groups (albums, singles, compilations,
    /// appears-on); the per-item `album_group` lets clients split sections.
    pub async fn artist_albums_page(
        &mut self,
        artist_id: &str,
        limit: u8,
        offset: u64,
    ) -> SpotifyResult<Page<MediaItem>> {
        let artist_id = spotify_resource_id(artist_id, MediaKind::Artist)?;
        let path = format!(
            "{}?include_groups=album%2Csingle%2Ccompilation%2Cappears_on&market=from_token&limit={limit}&offset={offset}",
            endpoints::artist_albums(&artist_id)
        );
        let response = self
            .request_json::<Paging<RawAlbum>>(Method::GET, &path, None::<()>)
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no artist albums response"))?;
        Ok(Page {
            total: response.total,
            offset,
            items: response
                .items
                .into_iter()
                .map(|item| item.into_media_item(self.provider_id.as_str()))
                .collect(),
        })
    }

    /// Episodes of a show (Spotify's `/v1/shows/{id}/episodes`). Single page;
    /// the caller paginates via `offset`. Episodes carry `resume_point`
    /// (listened state) when the token has `user-read-playback-position`.
    pub async fn show_episodes(
        &mut self,
        show_id: &str,
        limit: u8,
        offset: u64,
    ) -> SpotifyResult<Vec<MediaItem>> {
        let show_id = spotify_resource_id(show_id, MediaKind::Show)?;
        let path = format!(
            "{}?limit={}&offset={offset}",
            endpoints::show_episodes(&show_id),
            limit.min(50)
        );
        let response = self
            .request_json::<Paging<RawEpisode>>(Method::GET, &path, None::<()>)
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no show episodes response"))?;
        Ok(response
            .items
            .into_iter()
            .map(|item| item.into_media_item(self.provider_id.as_str()))
            .collect())
    }

    pub async fn play_pause(&mut self, is_playing: bool) -> SpotifyResult<()> {
        if is_playing {
            self.empty(Method::PUT, endpoints::PAUSE, None::<()>)
                .await?;
        } else {
            self.empty(Method::PUT, endpoints::PLAY, Some(serde_json::json!({})))
                .await?;
        }
        Ok(())
    }

    pub async fn play_uri(&mut self, uri: &str, kind: &MediaKind) -> SpotifyResult<()> {
        let resource = spotify_uri(uri)?;
        if &resource.kind() != kind {
            return Err(SpotifyError::InvalidInput {
                message: format!("expected a {kind} URI, got {}", resource.kind()),
            });
        }
        let body = match kind {
            MediaKind::Album | MediaKind::Artist | MediaKind::Playlist | MediaKind::Show => {
                serde_json::json!({ "context_uri": uri })
            }
            _ => serde_json::json!({ "uris": [uri] }),
        };
        self.empty(Method::PUT, endpoints::PLAY, Some(body)).await?;
        Ok(())
    }

    /// Start playback of `start_uri` inside a collection context.
    ///
    /// - `context_uri` present (album/playlist/…): `{ context_uri, offset:
    ///   { uri: start_uri }, position_ms }` so Spotify owns natural
    ///   progression and starts at the tapped track.
    /// - `tracks` present (Liked Songs): Spotify has no offset-accepting
    ///   collection context, so send a bounded `uris` window (≤100, the
    ///   Web API cap) beginning at `start_uri`. The daemon supplies the
    ///   list already ordered from the tapped track.
    /// - neither: fall back to the lone `start_uri`.
    ///
    /// This is the remote-Connect fallback; the embedded librespot path is
    /// primary and preferred.
    pub async fn play_context(
        &mut self,
        start_uri: &str,
        context_uri: Option<&str>,
        tracks: Option<&[String]>,
        position_ms: u64,
    ) -> SpotifyResult<()> {
        /// Spotify Web API caps `uris` at 100 entries per play request.
        const MAX_URIS: usize = 100;
        selection_like_uri_check(start_uri)?;
        if let Some(context_uri) = context_uri {
            let context = spotify_uri(context_uri)?;
            if !matches!(
                context.kind(),
                MediaKind::Album | MediaKind::Artist | MediaKind::Playlist | MediaKind::Show
            ) {
                return Err(SpotifyError::InvalidInput {
                    message: format!("unsupported Spotify playback context `{context_uri}`"),
                });
            }
        }
        if let Some(tracks) = tracks {
            for uri in tracks {
                selection_like_uri_check(uri)?;
            }
        }
        let body = if let Some(context_uri) = context_uri {
            if context_uri == start_uri {
                serde_json::json!({
                    "context_uri": context_uri,
                    "position_ms": position_ms,
                })
            } else {
                serde_json::json!({
                    "context_uri": context_uri,
                    "offset": { "uri": start_uri },
                    "position_ms": position_ms,
                })
            }
        } else if let Some(tracks) = tracks.filter(|t| !t.is_empty()) {
            // Window the ordered list so it begins at the tapped track,
            // then cap at the API's 100-URI limit. "Next" past the window
            // stops on the Web-API fallback — the embedded path (full list
            // + autoplay) is the one that continues through the collection.
            let start = tracks.iter().position(|u| u == start_uri).unwrap_or(0);
            let uris: Vec<&String> = tracks.iter().skip(start).take(MAX_URIS).collect();
            serde_json::json!({ "uris": uris, "position_ms": position_ms })
        } else {
            serde_json::json!({ "uris": [start_uri], "position_ms": position_ms })
        };
        self.empty(Method::PUT, endpoints::PLAY, Some(body)).await?;
        Ok(())
    }

    /// Start a single track on a specific device at a position. Used to heal
    /// transfers that land on a silent target: when the source was playing a
    /// contextless track (our embedded librespot loads single tracks via
    /// `from_tracks`, so the Spotify transfer state has no resolvable
    /// context), the target receives nothing to play. Re-asserting the track
    /// with `device_id` forces it to actually start.
    pub async fn play_uri_on_device(
        &mut self,
        device_id: &str,
        uri: &str,
        position_ms: u64,
    ) -> SpotifyResult<()> {
        let resource = spotify_uri(uri)?;
        if !matches!(resource.kind(), MediaKind::Track | MediaKind::Episode) {
            return Err(SpotifyError::InvalidInput {
                message: format!("unsupported Spotify playback URI `{uri}`"),
            });
        }
        let encoded_id =
            url::form_urlencoded::byte_serialize(device_id.as_bytes()).collect::<String>();
        let path = format!("{}?device_id={encoded_id}", endpoints::PLAY);
        let body = serde_json::json!({ "uris": [uri], "position_ms": position_ms });
        self.empty(Method::PUT, &path, Some(body)).await?;
        Ok(())
    }

    /// Start a provider context or explicit URI window on a selected Spotify
    /// Connect device. Unlike `play_uri_on_device`, this preserves collection
    /// offsets and ordered playback windows.
    pub(crate) async fn play_context_on_device(
        &mut self,
        device_id: &str,
        start_uri: &str,
        context_uri: Option<&str>,
        tracks: Option<&[String]>,
        position_ms: u64,
    ) -> SpotifyResult<()> {
        const MAX_URIS: usize = 100;
        selection_like_uri_check(start_uri)?;
        if let Some(context_uri) = context_uri {
            let context = spotify_uri(context_uri)?;
            if !matches!(
                context.kind(),
                MediaKind::Album | MediaKind::Artist | MediaKind::Playlist | MediaKind::Show
            ) {
                return Err(SpotifyError::InvalidInput {
                    message: format!("unsupported Spotify playback context `{context_uri}`"),
                });
            }
        }
        if let Some(tracks) = tracks {
            for uri in tracks {
                selection_like_uri_check(uri)?;
            }
        }
        let encoded_id = encode_component(device_id);
        let path = format!("{}?device_id={encoded_id}", endpoints::PLAY);
        let body = if let Some(context_uri) = context_uri {
            if context_uri == start_uri {
                serde_json::json!({
                    "context_uri": context_uri,
                    "position_ms": position_ms,
                })
            } else {
                serde_json::json!({
                    "context_uri": context_uri,
                    "offset": { "uri": start_uri },
                    "position_ms": position_ms,
                })
            }
        } else if let Some(tracks) = tracks.filter(|tracks| !tracks.is_empty()) {
            let start = tracks.iter().position(|uri| uri == start_uri).unwrap_or(0);
            let uris = tracks.iter().skip(start).take(MAX_URIS).collect::<Vec<_>>();
            serde_json::json!({ "uris": uris, "position_ms": position_ms })
        } else {
            serde_json::json!({ "uris": [start_uri], "position_ms": position_ms })
        };
        self.empty(Method::PUT, &path, Some(body)).await?;
        Ok(())
    }

    pub async fn next(&mut self) -> SpotifyResult<()> {
        self.empty(Method::POST, endpoints::NEXT, None::<()>)
            .await?;
        Ok(())
    }

    pub async fn previous(&mut self) -> SpotifyResult<()> {
        self.empty(Method::POST, endpoints::PREVIOUS, None::<()>)
            .await?;
        Ok(())
    }

    pub async fn seek(&mut self, position_ms: u64) -> SpotifyResult<()> {
        self.empty(
            Method::PUT,
            &format!("{}?position_ms={position_ms}", endpoints::SEEK),
            None::<()>,
        )
        .await?;
        Ok(())
    }

    pub async fn volume(&mut self, volume_percent: u8) -> SpotifyResult<()> {
        let volume_percent = volume_percent.min(100);
        self.empty(
            Method::PUT,
            &format!("{}?volume_percent={volume_percent}", endpoints::VOLUME),
            None::<()>,
        )
        .await?;
        Ok(())
    }

    pub async fn shuffle(&mut self, state: bool) -> SpotifyResult<()> {
        self.empty(
            Method::PUT,
            &format!("{}?state={state}", endpoints::SHUFFLE),
            None::<()>,
        )
        .await?;
        Ok(())
    }

    pub async fn repeat(&mut self, state: RepeatMode) -> SpotifyResult<()> {
        self.empty(
            Method::PUT,
            &format!("{}?state={}", endpoints::REPEAT, state.label()),
            None::<()>,
        )
        .await?;
        Ok(())
    }

    pub async fn add_to_queue(&mut self, uri: &str) -> SpotifyResult<()> {
        selection_like_uri_check(uri)?;
        let encoded = url::form_urlencoded::byte_serialize(uri.as_bytes()).collect::<String>();
        self.empty(
            Method::POST,
            &format!("{}?uri={encoded}", endpoints::QUEUE),
            None::<()>,
        )
        .await?;
        Ok(())
    }

    pub(crate) async fn add_to_queue_on_device(
        &mut self,
        uri: &str,
        device_id: &str,
    ) -> SpotifyResult<()> {
        selection_like_uri_check(uri)?;
        let uri = encode_component(uri);
        let device_id = encode_component(device_id);
        self.empty(
            Method::POST,
            &format!("{}?uri={uri}&device_id={device_id}", endpoints::QUEUE),
            None::<()>,
        )
        .await?;
        Ok(())
    }

    pub async fn transfer(&mut self, device_id: &str, play: bool) -> SpotifyResult<()> {
        self.empty(
            Method::PUT,
            endpoints::PLAYBACK,
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
        let playlist_id = spotify_resource_id(playlist_id, MediaKind::Playlist)?;
        for uri in uris {
            selection_like_uri_check(uri)?;
        }
        if uris.is_empty() {
            return Ok(());
        }
        let path = endpoints::playlist_items(&playlist_id);
        for chunk in uris.chunks(100) {
            self.empty(
                Method::POST,
                &path,
                Some(serde_json::json!({ "uris": chunk })),
            )
            .await?;
        }
        Ok(())
    }

    /// Add one provider-adapter batch and retain Spotify's resulting snapshot.
    /// The legacy public helper intentionally keeps returning `()` for wire
    /// compatibility; mutation receipts use this lossless path.
    pub(crate) async fn add_playlist_items_with_snapshot(
        &mut self,
        playlist_id: &str,
        uris: &[String],
        position: Option<u32>,
    ) -> SpotifyResult<String> {
        let playlist_id = spotify_resource_id(playlist_id, MediaKind::Playlist)?;
        for uri in uris {
            selection_like_uri_check(uri)?;
        }
        if uris.is_empty() {
            return Ok(String::new());
        }
        let mut path = endpoints::playlist_items(&playlist_id);
        if let Some(position) = position {
            path.push_str(&format!("?position={position}"));
        }
        let response = self
            .request_json::<SnapshotResponse>(
                Method::POST,
                &path,
                Some(serde_json::json!({ "uris": uris })),
            )
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no response for playlist-add"))?;
        Ok(response.snapshot_id)
    }

    pub async fn save_item(&mut self, item: &MediaItem) -> SpotifyResult<()> {
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

    /// `DELETE /v1/playlists/{id}/items` with `items[].uri` and
    /// optional `snapshot_id` precondition. Returns the new
    /// `snapshot_id` Spotify hands back so the caller can persist it.
    pub async fn remove_playlist_items(
        &mut self,
        playlist_id: &str,
        uris: &[String],
        snapshot_id: Option<&str>,
    ) -> SpotifyResult<String> {
        let playlist_id = spotify_resource_id(playlist_id, MediaKind::Playlist)?;
        for uri in uris {
            selection_like_uri_check(uri)?;
        }
        if uris.is_empty() {
            // No-op remove still needs a snapshot to return; surface the
            // caller's stored one (best-effort) or empty so the caller
            // can decide not to persist.
            return Ok(snapshot_id.unwrap_or_default().to_string());
        }
        let path = endpoints::playlist_items(&playlist_id);
        let mut current_snapshot = snapshot_id.map(str::to_string);
        for chunk in uris.chunks(100) {
            let items = chunk
                .iter()
                .cloned()
                .map(|uri| (uri, Vec::new()))
                .collect::<Vec<_>>();
            let body = playlist_remove_item_refs_body(&items, current_snapshot.as_deref());
            let resp = self
                .request_json::<SnapshotResponse>(Method::DELETE, &path, Some(body))
                .await?
                .ok_or_else(|| anyhow!("Spotify returned no response for playlist-remove"))?;
            current_snapshot = Some(resp.snapshot_id);
        }
        Ok(current_snapshot.ok_or_else(|| anyhow!("Spotify returned no snapshot_id"))?)
    }

    /// Remove URI occurrences in one snapshot-relative request. Spotify
    /// interprets every `positions` array against the supplied snapshot, so
    /// callers must not split or reorder this batch.
    pub(crate) async fn remove_playlist_item_refs(
        &mut self,
        playlist_id: &str,
        items: &[(String, Vec<u32>)],
        snapshot_id: Option<&str>,
    ) -> SpotifyResult<String> {
        let playlist_id = spotify_resource_id(playlist_id, MediaKind::Playlist)?;
        for (uri, _) in items {
            selection_like_uri_check(uri)?;
        }
        if items.is_empty() {
            return Ok(snapshot_id.unwrap_or_default().to_string());
        }
        let path = endpoints::playlist_items(&playlist_id);
        let body = playlist_remove_item_refs_body(items, snapshot_id);
        let response = self
            .request_json::<SnapshotResponse>(Method::DELETE, &path, Some(body))
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no response for playlist-remove"))?;
        Ok(response.snapshot_id)
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
        let playlist_id = spotify_resource_id(playlist_id, MediaKind::Playlist)?;
        for (uri, _) in items {
            selection_like_uri_check(uri)?;
        }
        if items.is_empty() {
            return Ok(String::new());
        }
        let base = endpoints::playlist_items(&playlist_id);
        let groups = group_items_by_position(items);
        let mut last_snapshot = String::new();
        for (position, uris) in groups {
            for chunk in uris.chunks(100) {
                let body = serde_json::json!({ "uris": chunk });
                let resp = self
                    .request_json::<SnapshotResponse>(
                        Method::POST,
                        &format!("{base}?position={position}"),
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
        let playlist_id = spotify_resource_id(playlist_id, MediaKind::Playlist)?;
        let path = endpoints::playlist_items(&playlist_id);
        let body = playlist_reorder_body(range_start, insert_before, range_length, snapshot_id);
        let resp = self
            .request_json::<SnapshotResponse>(Method::PUT, &path, Some(body))
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no response for playlist-reorder"))?;
        Ok(resp.snapshot_id)
    }

    /// Unfollow / delete a playlist. Spotify models playlist deletion
    /// as the owner unfollowing it. `DELETE /v1/playlists/{id}/followers`.
    pub async fn unfollow_playlist(&mut self, playlist_id: &str) -> SpotifyResult<()> {
        let playlist_id = spotify_resource_id(playlist_id, MediaKind::Playlist)?;
        let path = endpoints::playlist_followers(&playlist_id);
        self.empty(Method::DELETE, &path, None::<()>).await?;
        Ok(())
    }

    /// Upload a custom cover image for a playlist. `image_base64` must
    /// be the base64-encoded contents of a JPEG no larger than 256 KB
    /// after encoding — Spotify rejects anything else.
    /// `PUT /v1/playlists/{id}/images`, scope `ugc-image-upload`.
    pub async fn set_playlist_image(
        &mut self,
        playlist_id: &str,
        image_base64: &str,
    ) -> SpotifyResult<()> {
        let playlist_id = spotify_resource_id(playlist_id, MediaKind::Playlist)?;
        // Spotify wants the base64 as a plain-text body with
        // `Content-Type: image/jpeg`, NOT a JSON wrapper. None of the
        // request helpers in this file handle that shape — they all
        // serialize via reqwest's `.json(...)`. Inline the call so we
        // can set the raw body + content-type without generalising the
        // helpers for one caller.
        let path = endpoints::playlist_image(&playlist_id);
        let token = self.current_bearer(&Method::PUT, &path).await?;
        let url = format!("{}{path}", self.api_base);
        let priority = request_priority(&Method::PUT, &path, self.default_priority);
        let scope = endpoint_scope(&Method::PUT, &path);
        let cooldown_scope = self.cooldown_scope(&Method::PUT, &path, &scope);
        let started = Instant::now();
        let body = image_base64.to_owned();
        tracing::debug!(method = %Method::PUT, path, body_bytes = body.len(), "Spotify request start");
        let response = match self
            .rate_limiter
            .send_with_retry_in_bucket(priority, &cooldown_scope, &scope, || {
                self.rate_limiter
                    .inner()
                    .request(Method::PUT, url.clone())
                    .bearer_auth(token.clone())
                    .header(reqwest::header::CONTENT_TYPE, "image/jpeg")
                    .body(body.clone())
            })
            .await
        {
            Ok(response) => response,
            Err(err) => {
                self.record_spotify_api_finished(
                    &Method::PUT,
                    &path,
                    None,
                    started.elapsed().as_millis(),
                    Some(spotify_error_class(&err)),
                )
                .await;
                tracing::warn!(method = %Method::PUT, path, error = %err, "Spotify request send failed");
                return Err(anyhow!(err))
                    .with_context(|| format!("Spotify PUT {path} request failed"))?;
            }
        };
        let status = response.status();
        self.record_spotify_api_finished(
            &Method::PUT,
            &path,
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
            method = %Method::PUT,
            path,
            status = %status,
            elapsed_ms = started.elapsed().as_millis(),
            "Spotify request finished"
        );
        handle_empty_response(&Method::PUT, &path, response).await?;
        Ok(())
    }

    /// Save (=like) an item by URI. Routes to the correct
    /// `/me/{tracks,albums,episodes,shows}` endpoint based on the URI
    /// kind and uses Spotify's `?ids=` query syntax.
    pub async fn library_save_by_uri(&mut self, uri: &str) -> SpotifyResult<()> {
        let (path, _id) = library_endpoint_for_uri(uri)?;
        self.empty(Method::PUT, &path, None::<()>).await?;
        Ok(())
    }

    /// Inverse of `library_save_by_uri`. `DELETE` against the same
    /// endpoint family.
    pub async fn library_unsave_by_uri(&mut self, uri: &str) -> SpotifyResult<()> {
        let (path, _id) = library_endpoint_for_uri(uri)?;
        self.empty(Method::DELETE, &path, None::<()>).await?;
        Ok(())
    }

    /// Follow an artist (`PUT /me/following?type=artist&ids={id}`). A thin,
    /// self-documenting wrapper over the library-save routing, which already
    /// maps `spotify:artist:…` URIs to the follow endpoint.
    pub async fn follow_artist(&mut self, uri: &str) -> SpotifyResult<()> {
        self.library_save_by_uri(uri).await
    }

    /// Unfollow an artist (`DELETE /me/following?type=artist&ids={id}`).
    pub async fn unfollow_artist(&mut self, uri: &str) -> SpotifyResult<()> {
        self.library_unsave_by_uri(uri).await
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
        let mut token = self.current_bearer(&method, path).await?;
        let url = format!("{}{path}", self.api_base);
        let body = body.map(serde_json::to_value).transpose()?;
        let priority = request_priority(&method, path, self.default_priority);
        let scope = endpoint_scope(&method, path);
        let cooldown_scope = self.cooldown_scope(&method, path, &scope);
        let started = Instant::now();
        tracing::debug!(method = %method, path, "Spotify request start");
        let mut auth_attempt = 0_u8;
        let response = loop {
            match self
                .rate_limiter
                .send_with_retry_in_bucket(priority, &cooldown_scope, &scope, || {
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
                Ok(response) => break response,
                Err(SpotifyError::AuthExpired) if auth_attempt == 0 => {
                    auth_attempt += 1;
                    token = self.refresh_bearer(&method, path).await?;
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
        handle_empty_response(&method, path, response).await
    }

    async fn request_json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<impl Serialize>,
    ) -> AnyResult<Option<T>> {
        let mut token = self.current_bearer(&method, path).await?;
        let url = format!("{}{path}", self.api_base);
        let body = body.map(serde_json::to_value).transpose()?;
        let priority = request_priority(&method, path, self.default_priority);
        let scope = endpoint_scope(&method, path);
        let cooldown_scope = self.cooldown_scope(&method, path, &scope);
        let started = Instant::now();
        tracing::debug!(method = %method, path, "Spotify request start");
        let mut auth_attempt = 0_u8;
        let response = loop {
            match self
                .rate_limiter
                .send_with_retry_in_bucket(priority, &cooldown_scope, &scope, || {
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
                    token = self.refresh_bearer(&method, path).await?;
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
    format!(
        "{}?q={encoded}&type={types}&limit={limit}&offset={offset}",
        endpoints::SEARCH
    )
}

/// URL-encode a path segment (or query value) the way Spotify expects.
/// Exposed `pub(crate)` so `crate::endpoints` can compose paths with
/// safely-encoded ids in one place.
pub(crate) fn encode_component(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>()
}

fn rate_limit_bucket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("SPOTUIFY_RUNTIME_DIR") {
        return PathBuf::from(path).join("spotify-rate-limit.json");
    }
    spotuify_protocol::paths::runtime_dir().join("spotify-rate-limit.json")
}

fn endpoint_scope(method: &Method, path: &str) -> String {
    let path = path.split('?').next().unwrap_or(path);
    format!("{method} {path}")
}

/// Playlist/library WRITE endpoints that a Spotify Development-Mode dev
/// app 403s on (needs Extended Quota Mode OR the first-party bearer). In
/// hybrid auth these are the ONLY calls routed to the first-party write
/// provider; everything else uses the primary (dev-app) bearer.
///
/// Matches a WRITE verb (POST/PUT/DELETE) AND a library-write path. Reads
/// (GET) never match, and playback writes (`/me/player/...`) never match —
/// they work on a dev app. The path set is shared with
/// [`crate::error::is_library_write_path`] (which drives `dev_app_write_hint`)
/// so the router and the hint cannot drift apart.
pub(crate) fn endpoint_needs_first_party(method: &Method, path: &str) -> bool {
    let is_write_verb = matches!(*method, Method::POST | Method::PUT | Method::DELETE);
    is_write_verb && crate::error::is_library_write_path(path)
}

fn request_priority(method: &Method, path: &str, default_priority: Priority) -> Priority {
    if path.starts_with(endpoints::PLAYBACK) && *method != Method::GET {
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
    let patched = normalize_spotify_response(method, path, &mut value);
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

fn normalize_spotify_response(
    method: &Method,
    path: &str,
    value: &mut serde_json::Value,
) -> Vec<String> {
    let endpoint = path.split('?').next().unwrap_or(path);
    let mut patched = Vec::new();
    match endpoint {
        endpoints::PLAYBACK => {
            normalize_child(value, "item", "item", normalize_playable, &mut patched);
        }
        endpoints::QUEUE => {
            normalize_child(
                value,
                "currently_playing",
                "currently_playing",
                normalize_playable,
                &mut patched,
            );
            normalize_array_child(value, "queue", "queue", normalize_playable, &mut patched);
        }
        endpoints::RECENTLY_PLAYED => {
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
        endpoints::SAVED_TRACKS => {
            normalize_paging(value, NormalizeHint::PagingTrack, "paging", &mut patched);
            normalize_array_child(
                value,
                "items",
                "items.track",
                normalize_saved_track,
                &mut patched,
            );
        }
        endpoints::SAVED_ALBUMS => {
            normalize_paging(value, NormalizeHint::PagingAlbum, "paging", &mut patched);
            normalize_array_child(
                value,
                "items",
                "items.album",
                normalize_saved_album,
                &mut patched,
            );
        }
        endpoints::SAVED_EPISODES => {
            normalize_paging(value, NormalizeHint::PagingEpisode, "paging", &mut patched);
            normalize_array_child(
                value,
                "items",
                "items.episode",
                normalize_saved_episode,
                &mut patched,
            );
        }
        endpoints::MY_PLAYLISTS if method == Method::POST => {
            normalize_playlist(value, "playlist", &mut patched);
        }
        endpoints::MY_PLAYLISTS => {
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
        // Legacy `POST /users/{user_id}/playlists` response shape — we
        // no longer emit this endpoint (create now uses MY_PLAYLISTS),
        // but the normalizer keeps the arm so historical fixtures /
        // upstream weirdness still round-trip cleanly.
        _ if endpoint.starts_with("/users/") && endpoint.ends_with("/playlists") => {
            normalize_playlist(value, "playlist", &mut patched);
        }
        // Both the modern `/items` form and the deprecated `/tracks`
        // form return the same paging-of-track shape; keep both so
        // older recorded responses still normalize.
        _ if endpoint.starts_with("/playlists/")
            && (endpoint.ends_with("/items") || endpoint.ends_with("/tracks")) =>
        {
            normalize_paging(value, NormalizeHint::PagingTrack, "paging", &mut patched);
            normalize_array_child(
                value,
                "items",
                "items.track",
                normalize_playlist_track,
                &mut patched,
            );
        }
        endpoints::SEARCH => {
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

fn normalize_saved_episode(value: &mut serde_json::Value, label: &str, patched: &mut Vec<String>) {
    normalize_child(value, "episode", label, normalize_episode, patched);
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
    fn into_playback(self, source: &str) -> Playback {
        Playback {
            item: self.item.and_then(|item| item.into_media_item(source)),
            device: self.device,
            is_playing: self.is_playing.unwrap_or(false),
            progress_ms: self.progress_ms.unwrap_or_default(),
            shuffle: self.shuffle_state.unwrap_or(false),
            repeat: self
                .repeat_state
                .as_deref()
                .and_then(|state| RepeatMode::parse(state).ok())
                .unwrap_or_default(),
            sampled_at_ms: Some(now_ms()),
            provider_timestamp_ms: self.timestamp,
            source: Some(spotuify_core::PlaybackStateSource::RemotePoll),
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
    /// RFC3339 timestamp of when the user saved the track (`/me/tracks`).
    #[serde(default)]
    added_at: Option<String>,
}

/// Parse a Spotify RFC3339 timestamp (e.g. `2024-03-01T12:00:00Z`) to epoch ms.
fn parse_rfc3339_ms(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

#[derive(Debug, Deserialize)]
struct SavedAlbumItem {
    album: RawAlbum,
}

#[derive(Debug, Deserialize)]
struct SavedEpisodeItem {
    episode: RawEpisode,
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

pub(crate) struct SearchPageResult {
    pub(crate) items: Vec<MediaItem>,
    pub(crate) total: Option<u64>,
    /// Raw upstream result slots consumed, including nullable or invalid
    /// playlist rows filtered out during canonicalization.
    pub(crate) consumed: u64,
}

#[derive(Debug, Deserialize)]
struct Paging<T> {
    items: Vec<T>,
    total: u64,
}

/// `/me/following?type=artist` nests its cursor page under `artists`.
#[derive(Debug, Deserialize)]
struct FollowingPage {
    artists: CursorPage<RawArtist>,
}

#[derive(Debug, Deserialize)]
struct CursorPage<T> {
    #[serde(default = "Vec::new")]
    items: Vec<T>,
    #[serde(default)]
    next: Option<String>,
    #[serde(default)]
    cursors: Option<Cursors>,
}

#[derive(Debug, Deserialize)]
struct Cursors {
    #[serde(default)]
    after: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PlaylistTrackItem {
    #[serde(alias = "item")]
    track: Option<RawPlayable>,
}

impl PlaylistTrackItem {
    fn into_media_item(self, source: &str, playlist_id: &str, position: u64) -> MediaItem {
        self.track.map_or_else(
            || unavailable_playlist_item(source, playlist_id, position),
            |item| item.into_playlist_media_item(source, playlist_id, position),
        )
    }
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
    fn into_media_item(self, source: &str) -> Option<MediaItem> {
        match self {
            // Spotify marks local-file playlist entries with `is_local` and
            // may return a non-canonical URI plus `album.uri: null`. They
            // cannot be addressed through the Web API, so keep them below
            // the provider URI boundary.
            Self::Track(track) if !track.is_local => Some(track.into_media_item(source)),
            Self::Track(_) => None,
            Self::Episode(episode) => Some(episode.into_media_item(source)),
            Self::Other => None,
        }
    }

    fn into_playlist_media_item(self, source: &str, playlist_id: &str, position: u64) -> MediaItem {
        match self {
            Self::Track(track) if track.is_local => track.into_local_playlist_item(source),
            Self::Track(track) => track.into_media_item(source),
            Self::Episode(episode) => episode.into_media_item(source),
            Self::Other => unavailable_playlist_item(source, playlist_id, position),
        }
    }
}

fn unavailable_playlist_item(source: &str, playlist_id: &str, position: u64) -> MediaItem {
    let uri = ResourceUri::spotify(
        MediaKind::Track,
        format!("unavailable~{playlist_id}~{position}"),
    )
    .expect("Spotify playlist IDs and positions form canonical surrogate IDs")
    .as_uri();
    MediaItem {
        uri,
        name: "Unavailable playlist item".to_string(),
        subtitle: "Unavailable on Spotify".to_string(),
        kind: MediaKind::Track,
        source: Some(spotuify_core::ItemSource::Provider(source.to_string())),
        is_playable: Some(false),
        ..Default::default()
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
    #[serde(default)]
    is_local: bool,
    #[serde(default, deserialize_with = "null_to_default")]
    artists: Vec<SimpleNamed>,
    album: RawAlbum,
}

impl RawTrack {
    fn into_media_item(self, source: &str) -> MediaItem {
        let subtitle = join_names(&self.artists);
        let artists = artist_refs(&self.artists);
        MediaItem {
            id: self.id,
            uri: self.uri,
            name: self.name,
            subtitle,
            context: self.album.name.clone(),
            duration_ms: self.duration_ms,
            image_url: image_url(&self.album.images),
            kind: MediaKind::Track,
            source: Some(spotuify_core::ItemSource::Provider(source.to_string())),
            freshness: None,
            explicit: self.explicit,
            is_playable: self.is_playable,
            album: Some(self.album.name),
            album_uri: self.album.uri,
            artists,
            ..Default::default()
        }
    }

    fn into_local_playlist_item(self, source: &str) -> MediaItem {
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.uri.as_bytes());
        let uri = ResourceUri::spotify(
            MediaKind::Track,
            format!("{LOCAL_TRACK_SURROGATE_PREFIX}{encoded}"),
        )
        .expect("URL-safe base64 forms a canonical local-track surrogate ID")
        .as_uri();
        MediaItem {
            id: self.id,
            uri,
            name: self.name,
            subtitle: join_names(&self.artists),
            context: self.album.name.clone(),
            duration_ms: self.duration_ms,
            image_url: image_url(&self.album.images),
            kind: MediaKind::Track,
            source: Some(spotuify_core::ItemSource::Provider(source.to_string())),
            freshness: None,
            explicit: self.explicit,
            is_playable: Some(false),
            album: Some(self.album.name),
            album_uri: self.album.uri,
            artists: artist_refs(&self.artists),
            ..Default::default()
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
    fn into_media_item(self, source: &str) -> MediaItem {
        let subtitle = join_names(&self.artists);
        let artists = artist_refs(&self.artists);
        MediaItem {
            id: self.id,
            uri: self.uri,
            name: self.name,
            subtitle,
            context: String::new(),
            duration_ms: self.duration_ms,
            image_url: None,
            kind: MediaKind::Track,
            source: Some(spotuify_core::ItemSource::Provider(source.to_string())),
            freshness: None,
            explicit: self.explicit,
            is_playable: self.is_playable,
            artists,
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct ResumePoint {
    #[serde(default)]
    fully_played: Option<bool>,
    #[serde(default)]
    resume_position_ms: Option<u64>,
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
    /// Listened state — present on user-context endpoints with the
    /// `user-read-playback-position` scope; absent on plain search results.
    #[serde(default)]
    resume_point: Option<ResumePoint>,
    #[serde(default)]
    release_date: Option<String>,
}

impl RawEpisode {
    fn into_media_item(self, source: &str) -> MediaItem {
        let show = self
            .show
            .map_or_else(|| "Podcast episode".to_string(), |show| show.name);
        let resume = self.resume_point.unwrap_or(ResumePoint {
            fully_played: None,
            resume_position_ms: None,
        });
        MediaItem {
            id: self.id,
            uri: self.uri,
            name: self.name,
            subtitle: show.clone(),
            context: show,
            duration_ms: self.duration_ms,
            image_url: image_url(&self.images),
            kind: MediaKind::Episode,
            source: Some(spotuify_core::ItemSource::Provider(source.to_string())),
            freshness: None,
            explicit: None,
            is_playable: None,
            resume_position_ms: resume.resume_position_ms,
            fully_played: resume.fully_played,
            release_date: self
                .release_date
                .and_then(|date| date.parse::<ReleaseDate>().ok()),
            ..Default::default()
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
    fn into_media_item(self, source: &str) -> MediaItem {
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
            source: Some(spotuify_core::ItemSource::Provider(source.to_string())),
            freshness: None,
            explicit: None,
            is_playable: None,
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct RawAlbum {
    id: Option<String>,
    #[serde(default)]
    uri: Option<String>,
    name: String,
    #[serde(default, deserialize_with = "null_to_default")]
    artists: Vec<SimpleNamed>,
    #[serde(default, deserialize_with = "null_to_default")]
    images: Vec<ImageRef>,
    total_tracks: Option<u64>,
    #[serde(default)]
    release_date: Option<String>,
    /// The artist-relative grouping (`/v1/artists/{id}/albums` only).
    #[serde(default)]
    album_group: Option<String>,
    /// The intrinsic album type; fallback when `album_group` is absent
    /// (e.g. on plain album objects).
    #[serde(default)]
    album_type: Option<String>,
}

impl RawAlbum {
    fn into_media_item(self, source: &str) -> MediaItem {
        let subtitle = join_names(&self.artists);
        let artists = artist_refs(&self.artists);
        let album_group = self.album_group.or(self.album_type).map(AlbumGroup::from);
        MediaItem {
            id: self.id,
            uri: self.uri.unwrap_or_default(),
            name: self.name,
            subtitle,
            context: self
                .total_tracks
                .map(|n| format!("{n} tracks"))
                .unwrap_or_default(),
            duration_ms: 0,
            image_url: image_url(&self.images),
            kind: MediaKind::Album,
            source: Some(spotuify_core::ItemSource::Provider(source.to_string())),
            freshness: None,
            explicit: None,
            is_playable: None,
            release_date: self
                .release_date
                .as_deref()
                .and_then(|date| date.parse::<ReleaseDate>().ok()),
            album_group,
            artists,
            ..Default::default()
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
    fn into_media_item(self, source: &str) -> MediaItem {
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
            source: Some(spotuify_core::ItemSource::Provider(source.to_string())),
            freshness: None,
            explicit: None,
            is_playable: None,
            ..Default::default()
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
        let tracks_total = self.tracks.as_ref().map_or(0, |tracks| tracks.total);
        let version_token = self.snapshot_id.clone();
        Some(Playlist {
            id,
            name: self.name.unwrap_or_else(|| "Untitled playlist".to_string()),
            owner: playlist_owner_name(self.owner),
            tracks_total,
            image_url: image_url(&self.images),
            version_token,
        })
    }

    fn into_media_item(self, source: &str) -> Option<MediaItem> {
        let id = self.id?;
        let tracks_total = self.tracks.as_ref().map_or(0, |tracks| tracks.total);
        let id_resource = ResourceUri::spotify(MediaKind::Playlist, &id).ok()?;
        let uri = match self.uri {
            Some(uri) => {
                let resource = ResourceUri::parse(&uri).ok()?;
                (resource.kind() == MediaKind::Playlist).then_some(resource)?
            }
            None => id_resource,
        };
        Some(MediaItem {
            uri: uri.as_uri(),
            id: Some(id),
            name: self.name.unwrap_or_else(|| "Untitled playlist".to_string()),
            subtitle: playlist_owner_name(self.owner),
            context: format!("{tracks_total} tracks"),
            duration_ms: 0,
            image_url: image_url(&self.images),
            kind: MediaKind::Playlist,
            source: Some(spotuify_core::ItemSource::Provider(source.to_string())),
            freshness: None,
            explicit: None,
            is_playable: None,
            ..Default::default()
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
struct SimpleNamed {
    name: String,
    /// Artist URI (`spotify:artist:…`); present on track/album artist objects,
    /// used to build navigable `ArtistRef`s.
    #[serde(default)]
    uri: Option<String>,
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
            format!("{m:.0}M followers")
        } else {
            format!("{m:.1}M followers")
        }
    } else if total >= 1_000 {
        let k = total as f64 / 1_000.0;
        if k >= 100.0 {
            format!("{k:.0}K followers")
        } else {
            format!("{k:.1}K followers")
        }
    } else {
        format!("{total} followers")
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

/// Build the JSON body for `DELETE /playlists/{id}/items` without changing
/// the item or position order supplied by the caller.
fn playlist_remove_item_refs_body(
    refs: &[(String, Vec<u32>)],
    snapshot_id: Option<&str>,
) -> serde_json::Value {
    let items: Vec<serde_json::Value> = refs
        .iter()
        .map(|(uri, positions)| {
            if positions.is_empty() {
                serde_json::json!({ "uri": uri })
            } else {
                serde_json::json!({ "uri": uri, "positions": positions })
            }
        })
        .collect();
    match snapshot_id {
        Some(snap) => serde_json::json!({ "items": items, "snapshot_id": snap }),
        None => serde_json::json!({ "items": items }),
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
/// Artists still use the follow endpoint because library writes do not accept
/// artist URIs.
fn library_endpoint_for_uri(uri: &str) -> SpotifyResult<(String, String)> {
    let resource = spotify_uri(uri)?;
    let id = resource.bare_id().to_string();
    let path = match resource.kind() {
        MediaKind::Track => format!("{}?ids={}", endpoints::SAVED_TRACKS, encode_component(&id)),
        MediaKind::Album => format!("{}?ids={}", endpoints::SAVED_ALBUMS, encode_component(&id)),
        MediaKind::Episode => {
            format!(
                "{}?ids={}",
                endpoints::SAVED_EPISODES,
                encode_component(&id)
            )
        }
        MediaKind::Show => format!("{}?ids={}", endpoints::SAVED_SHOWS, encode_component(&id)),
        MediaKind::Artist => {
            format!(
                "{}?type=artist&ids={}",
                endpoints::FOLLOWING,
                encode_component(&id)
            )
        }
        MediaKind::Playlist => {
            return Err(SpotifyError::InvalidInput {
                message: "playlists are saved/unsaved via /playlists/{id}/followers, not /me/{tracks,albums,episodes,artists}"
                    .to_string(),
            });
        }
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

/// Build navigable artist references from raw artist objects, preserving order.
/// Artists missing a URI keep an empty `uri` (clients leave those non-clickable).
fn artist_refs(items: &[SimpleNamed]) -> Vec<ArtistRef> {
    items
        .iter()
        .map(|item| ArtistRef {
            name: item.name.clone(),
            uri: item.uri.clone().unwrap_or_default(),
        })
        .collect()
}

fn image_url(images: &[ImageRef]) -> Option<String> {
    images
        .iter()
        .filter(|image| image.url.is_some())
        .min_by_key(|image| image.width.unwrap_or(u32::MAX).abs_diff(300))
        .and_then(|image| image.url.clone())
}

fn selection_like_uri_check(uri: &str) -> SpotifyResult<()> {
    let resource = spotify_uri(uri)?;
    if matches!(
        resource.kind(),
        MediaKind::Track
            | MediaKind::Episode
            | MediaKind::Album
            | MediaKind::Artist
            | MediaKind::Playlist
    ) {
        Ok(())
    } else {
        Err(SpotifyError::InvalidInput {
            message: format!("unsupported Spotify URI `{uri}`"),
        })
    }
}

fn spotify_uri(uri: &str) -> SpotifyResult<ResourceUri> {
    let resource = ResourceUri::parse(uri).map_err(|err| SpotifyError::InvalidInput {
        message: format!("malformed Spotify URI `{uri}`: {err}"),
    })?;
    if resource.scheme() != &UriScheme::Spotify {
        return Err(SpotifyError::InvalidInput {
            message: format!("unsupported Spotify URI scheme in `{uri}`"),
        });
    }
    Ok(resource)
}

fn spotify_resource_id(input: &str, expected_kind: MediaKind) -> SpotifyResult<String> {
    ResourceUri::spotify_from_uri_or_id(expected_kind, input)
        .map(|resource| resource.bare_id().to_string())
        .map_err(|error| SpotifyError::InvalidInput {
            message: format!("invalid Spotify resource `{input}`: {error}"),
        })
}

#[cfg(test)]
mod tests {
    use reqwest::Method;

    use super::{
        format_followers, group_items_by_position, library_endpoint_for_uri,
        normalize_spotify_response, parse_rfc3339_ms, playlist_remove_item_refs_body,
        playlist_reorder_body, search_path, Config, MediaKind, RawEpisode, RawPlaylist,
        ResourceUri, SpotifyClient,
    };

    #[test]
    fn episode_resume_point_and_release_date_map_into_media_item() {
        let raw: RawEpisode = serde_json::from_value(json!({
            "id": "ep1",
            "uri": "spotify:episode:ep1",
            "name": "Episode 1",
            "duration_ms": 1_800_000,
            "show": { "name": "My Show" },
            "release_date": "2024-03-01",
            "resume_point": { "fully_played": true, "resume_position_ms": 12_000 }
        }))
        .expect("episode should deserialize");
        let item = raw.into_media_item("spotify");
        assert_eq!(item.kind, MediaKind::Episode);
        assert_eq!(item.fully_played, Some(true));
        assert_eq!(item.resume_position_ms, Some(12_000));
        assert_eq!(
            item.release_date.map(|date| date.to_string()).as_deref(),
            Some("2024-03-01")
        );
    }

    #[test]
    fn playlist_without_uri_drops_malformed_id_instead_of_panicking() {
        let malformed: RawPlaylist = serde_json::from_value(json!({
            "id": "bad/id",
            "name": "Broken"
        }))
        .expect("playlist should deserialize");
        assert!(malformed.into_media_item("spotify").is_none());
    }

    #[test]
    fn playlist_without_uri_builds_valid_canonical_uri() {
        let raw: RawPlaylist = serde_json::from_value(json!({
            "id": "mix_1",
            "name": "Mix"
        }))
        .expect("playlist should deserialize");
        assert_eq!(
            raw.into_media_item("spotify").map(|item| item.uri),
            Some("spotify:playlist:mix_1".to_string())
        );
    }

    #[test]
    fn rfc3339_parses_to_epoch_ms() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:01Z"), Some(1_000));
        assert!(parse_rfc3339_ms("not-a-date").is_none());
    }
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

        let mut client = test_client(&server)
            .await
            .with_provider_id(spotuify_core::ProviderId::new("spotify-work").expect("provider id"));
        let tracks = client
            .album_tracks_page("spotify:album:album-one", 50, 0)
            .await
            .expect("album tracks should load");

        assert_eq!(tracks.items.len(), 1);
        assert_eq!(tracks.items[0].uri, "spotify:track:track-one");
        assert_eq!(tracks.items[0].name, "Track One");
        assert_eq!(tracks.items[0].subtitle, "Artist One");
        assert_eq!(
            tracks.items[0]
                .source
                .as_ref()
                .map(spotuify_core::ItemSource::as_str),
            Some("spotify-work")
        );
    }

    #[tokio::test]
    async fn search_items_use_the_configured_provider_id_as_source() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/search"))
            .and(query_param("q", "needle"))
            .and(query_param("type", "track"))
            .and(query_param("limit", "10"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "tracks": {
                    "total": 1,
                    "items": [{
                        "id": "track-one",
                        "uri": "spotify:track:track-one",
                        "name": "Track One",
                        "duration_ms": 123000,
                        "artists": [{"name": "Artist One"}],
                        "album": {
                            "id": "album-one",
                            "uri": "spotify:album:album-one",
                            "name": "Album One"
                        }
                    }]
                }
            })))
            .mount(&server)
            .await;

        let client = test_client(&server)
            .await
            .with_provider_id(spotuify_core::ProviderId::new("spotify-work").expect("provider id"));
        let result = client
            .search_single_type("needle", MediaKind::Track, 10, 0)
            .await
            .expect("search should load");

        assert_eq!(result.items.len(), 1);
        assert!(matches!(
            &result.items[0].source,
            Some(spotuify_core::ItemSource::Provider(provider)) if provider == "spotify-work"
        ));
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
    fn playlist_remove_items_body_emits_items_array_with_uri_field_per_spotify_api() {
        let refs = vec![
            ("spotify:track:1".to_string(), Vec::new()),
            ("spotify:track:2".to_string(), Vec::new()),
        ];
        let body = playlist_remove_item_refs_body(&refs, None);
        let items = body["items"]
            .as_array()
            .expect("body must contain an items array");
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0]["uri"].as_str().expect("item 0 should have uri"),
            "spotify:track:1"
        );
        assert_eq!(
            items[1]["uri"].as_str().expect("item 1 should have uri"),
            "spotify:track:2"
        );
        // snapshot_id is absent when not provided; presence forces
        // Spotify's optimistic-concurrency precondition which we only
        // want when the daemon captured one.
        assert!(body.get("snapshot_id").is_none());
    }

    #[test]
    fn playlist_remove_items_body_includes_snapshot_id_when_present() {
        let body = playlist_remove_item_refs_body(
            &[("spotify:track:x".to_string(), Vec::new())],
            Some("snap-A"),
        );
        assert_eq!(
            body["snapshot_id"]
                .as_str()
                .expect("body should contain snapshot_id"),
            "snap-A"
        );
    }

    #[test]
    fn playlist_remove_item_refs_preserves_item_and_position_order() {
        let refs = vec![
            ("spotify:track:second".to_string(), vec![9, 2]),
            ("spotify:track:first".to_string(), vec![7]),
            ("spotify:episode:last".to_string(), Vec::new()),
        ];
        let body = playlist_remove_item_refs_body(&refs, Some("snap-A"));

        assert_eq!(
            body,
            serde_json::json!({
                "items": [
                    { "uri": "spotify:track:second", "positions": [9, 2] },
                    { "uri": "spotify:track:first", "positions": [7] },
                    { "uri": "spotify:episode:last" }
                ],
                "snapshot_id": "snap-A"
            })
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

        let patched =
            normalize_spotify_response(&Method::GET, "/search?q=x&type=track&limit=10", &mut value);

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

        let patched =
            normalize_spotify_response(&Method::GET, "/me/playlists?limit=50&offset=0", &mut value);

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
    fn compat_wiring_treats_playlist_create_as_one_playlist_not_paging() {
        let mut value = json!({
            "id": "created-1",
            "name": "Created",
            "owner": {"display_name": "Owner"},
            "tracks": {"total": 0},
            "images": [],
            "snapshot_id": "v1"
        });

        normalize_spotify_response(&Method::POST, "/me/playlists", &mut value);

        assert!(value.get("items").is_none());
        let playlist: RawPlaylist =
            serde_json::from_value(value).expect("created playlist should deserialize");
        assert_eq!(
            playlist.into_playlist().expect("playlist output").id,
            "created-1"
        );
    }

    #[test]
    fn me_playlists_mapping_keeps_followed_playlist_metadata() {
        let value = json!({
            "id": "followed-playlist",
            "name": "Followed Playlist",
            "owner": {"id": "not-current-user", "display_name": "Third Party"},
            "tracks": {"total": 42},
            "snapshot_id": "snap-followed"
        });

        let playlist = serde_json::from_value::<super::RawPlaylist>(value)
            .expect("followed playlist metadata should deserialize")
            .into_playlist()
            .expect("followed playlist should not be owner-filtered");

        assert_eq!(playlist.id, "followed-playlist");
        assert_eq!(playlist.name, "Followed Playlist");
        assert_eq!(playlist.owner, "Third Party");
        assert_eq!(playlist.tracks_total, 42);
        assert_eq!(playlist.version_token.as_deref(), Some("snap-followed"));
    }

    #[test]
    fn playlist_items_endpoint_shape_deserializes() {
        let value = json!({
            "total": 3,
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
                {
                    "item": {
                        "type": "track",
                        "id": null,
                        "uri": "spotify:local:Artist:Album:Local Song:123",
                        "name": "Local Song",
                        "duration_ms": 123000,
                        "is_local": true,
                        "artists": [{"name": "Artist"}],
                        "album": {
                            "id": null,
                            "uri": null,
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
        assert_eq!(page.total, 3);
        let tracks = page
            .items
            .into_iter()
            .enumerate()
            .map(|(position, item)| item.into_media_item("spotify", "playlist-1", position as u64))
            .collect::<Vec<_>>();

        assert_eq!(tracks.len(), 3);
        assert_eq!(tracks[0].uri, "spotify:track:t1");
        assert!(ResourceUri::parse(&tracks[1].uri)
            .expect("local surrogate should be canonical")
            .bare_id()
            .starts_with(super::LOCAL_TRACK_SURROGATE_PREFIX));
        assert_eq!(tracks[1].is_playable, Some(false));
        assert!(ResourceUri::parse(&tracks[1].uri).is_ok());
        assert_eq!(tracks[2].uri, "spotify:track:unavailable~playlist-1~2");
    }

    #[test]
    fn library_endpoint_for_uri_routes_each_media_kind_to_correct_spotify_endpoint() {
        let cases = [
            ("spotify:track:abc", "/me/tracks?ids=abc"),
            ("spotify:album:xyz", "/me/albums?ids=xyz"),
            ("spotify:episode:e1", "/me/episodes?ids=e1"),
            ("spotify:show:s1", "/me/shows?ids=s1"),
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

    /// Fixed test double for [`super::WebApiBearerProvider`]: returns a
    /// known token so a routing test can tell which source produced the
    /// bearer without touching the network.
    struct FixedBearer(&'static str);

    #[async_trait::async_trait]
    impl super::WebApiBearerProvider for FixedBearer {
        async fn bearer(&self, _force_refresh: bool) -> super::SpotifyResult<String> {
            Ok(self.0.to_string())
        }
    }

    #[test]
    fn endpoint_needs_first_party_truth_table() {
        use super::endpoint_needs_first_party;
        // Playlist/library WRITES (POST/PUT/DELETE) → first-party.
        assert!(endpoint_needs_first_party(
            &Method::POST,
            "/playlists/pl1/tracks"
        ));
        assert!(endpoint_needs_first_party(
            &Method::DELETE,
            "/playlists/pl1/tracks"
        ));
        assert!(endpoint_needs_first_party(
            &Method::PUT,
            "/playlists/pl1/tracks"
        ));
        assert!(endpoint_needs_first_party(
            &Method::PUT,
            "/me/tracks?ids=abc"
        ));
        assert!(endpoint_needs_first_party(
            &Method::PUT,
            "/me/following?type=artist&ids=a1"
        ));
        // Reads never route to first-party, even on the write families.
        assert!(!endpoint_needs_first_party(&Method::GET, "/me/player"));
        assert!(!endpoint_needs_first_party(&Method::GET, "/playlists/pl1"));
        assert!(!endpoint_needs_first_party(&Method::GET, "/me/tracks"));
        // Playback writes stay on the dev-app bearer.
        assert!(!endpoint_needs_first_party(&Method::PUT, "/me/player/play"));
        assert!(!endpoint_needs_first_party(
            &Method::POST,
            "/me/player/queue"
        ));
    }

    #[tokio::test]
    async fn current_bearer_routes_writes_to_first_party_write_provider() {
        // Case 3 (HYBRID): dev-app primary + first-party WRITE provider.
        let client = SpotifyClient::new(test_config())
            .expect("test client should build")
            .with_token_cache(token_cache())
            .with_write_bearer_provider(Arc::new(FixedBearer("write-token")));

        // Playlist / library writes take the first-party write provider.
        assert_eq!(
            client
                .current_bearer(&Method::PUT, "/playlists/p/tracks")
                .await
                .expect("bearer"),
            "write-token"
        );
        assert_eq!(
            client
                .current_bearer(&Method::DELETE, "/me/tracks?ids=x")
                .await
                .expect("bearer"),
            "write-token"
        );
        // Reads and playback control keep the dev-app (primary) bearer.
        assert_eq!(
            client
                .current_bearer(&Method::GET, "/me/player")
                .await
                .expect("bearer"),
            "test-access"
        );
        assert_eq!(
            client
                .current_bearer(&Method::PUT, "/me/player/play")
                .await
                .expect("bearer"),
            "test-access"
        );
    }

    #[test]
    fn cooldown_scopes_are_isolated_by_bearer() {
        let hybrid = SpotifyClient::new(test_config())
            .expect("test client should build")
            .with_write_bearer_provider(Arc::new(FixedBearer("write-token")));
        let first_party = SpotifyClient::new(test_config())
            .expect("test client should build")
            .with_bearer_provider(Arc::new(FixedBearer("fp-primary")));

        assert_eq!(
            hybrid.cooldown_scope(&Method::PUT, "/me/tracks?ids=x", "PUT /me/tracks"),
            "first-party PUT /me/tracks"
        );
        assert_eq!(
            hybrid.cooldown_scope(&Method::GET, "/me/tracks", "GET /me/tracks"),
            "dev-app GET /me/tracks"
        );
        assert_eq!(
            first_party.cooldown_scope(&Method::GET, "/me/tracks", "GET /me/tracks"),
            "first-party GET /me/tracks"
        );
    }

    #[tokio::test]
    async fn current_bearer_dev_app_only_uses_primary_for_all_paths() {
        // Case 2: no write provider, no first-party provider → dev-app for
        // every request (writes still hit the dev-app bearer, 403 in Dev
        // Mode via the existing hint). Unchanged from today.
        let client = SpotifyClient::new(test_config())
            .expect("test client should build")
            .with_token_cache(token_cache());
        for (method, path) in [
            (Method::GET, "/me/player"),
            (Method::PUT, "/playlists/p/tracks"),
            (Method::PUT, "/me/tracks?ids=x"),
        ] {
            assert_eq!(
                client.current_bearer(&method, path).await.expect("bearer"),
                "test-access",
                "{method} {path} should use the dev-app bearer"
            );
        }
    }

    #[tokio::test]
    async fn current_bearer_first_party_only_uses_primary_provider_for_all_paths() {
        // Case 1: first-party provider primary, no write provider → the
        // first-party bearer for every request. Unchanged from today.
        let client = SpotifyClient::new(test_config())
            .expect("test client should build")
            .with_bearer_provider(Arc::new(FixedBearer("fp-primary")));
        for (method, path) in [
            (Method::GET, "/me/player"),
            (Method::PUT, "/playlists/p/tracks"),
            (Method::PUT, "/me/tracks?ids=x"),
        ] {
            assert_eq!(
                client.current_bearer(&method, path).await.expect("bearer"),
                "fp-primary",
                "{method} {path} should use the first-party primary bearer"
            );
        }
    }

    #[test]
    fn router_and_hint_agree_on_library_write_family() {
        // The hybrid router and the dev-app 403 hint must classify the
        // same endpoints so they never drift.
        use super::endpoint_needs_first_party;
        let write_paths = [
            "/playlists/p/tracks",
            "/me/tracks?ids=x",
            "/me/albums?ids=x",
            "/me/episodes?ids=x",
            "/me/shows?ids=x",
            "/me/following?type=artist&ids=x",
        ];
        for path in write_paths {
            assert!(
                endpoint_needs_first_party(&Method::PUT, path),
                "router should route {path} to first-party"
            );
            assert!(
                crate::error::is_library_write_path(path),
                "hint predicate should match {path}"
            );
        }
        // Playback + a read agree the other direction.
        for path in ["/me/player/play", "/me/player/queue"] {
            assert!(!endpoint_needs_first_party(&Method::PUT, path));
            assert!(!crate::error::is_library_write_path(path));
        }
        assert!(!endpoint_needs_first_party(&Method::GET, "/me/tracks"));
    }
}
