use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use reqwest::{Client, Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

use crate::analytics::{
    now_ms, spotify_api_finished_event, AnalyticsEvent, AnalyticsSource, AnalyticsStore,
};
use crate::auth;
use crate::config::Config;

const API: &str = "https://api.spotify.com/v1";

#[derive(Clone)]
pub struct SpotifyClient {
    config: Config,
    http: Client,
    analytics: Option<AnalyticsStore>,
    analytics_source: AnalyticsSource,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Playback {
    pub item: Option<MediaItem>,
    pub device: Option<Device>,
    pub is_playing: bool,
    pub progress_ms: u64,
    pub shuffle: bool,
    pub repeat: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Queue {
    pub currently_playing: Option<MediaItem>,
    pub items: Vec<MediaItem>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Track,
    Episode,
    Album,
    Artist,
    Playlist,
}

impl MediaKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Track => "track",
            Self::Episode => "episode",
            Self::Album => "album",
            Self::Artist => "artist",
            Self::Playlist => "playlist",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MediaItem {
    pub id: Option<String>,
    pub uri: String,
    pub name: String,
    pub subtitle: String,
    pub context: String,
    pub duration_ms: u64,
    pub image_url: Option<String>,
    pub kind: MediaKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Device {
    pub id: Option<String>,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub is_active: bool,
    pub is_restricted: bool,
    pub volume_percent: Option<u8>,
    pub supports_volume: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub owner: String,
    pub tracks_total: u64,
    pub image_url: Option<String>,
}

impl SpotifyClient {
    pub fn new(config: Config) -> Result<Self> {
        let http = Client::builder()
            .user_agent(format!("spotuify/{}", env!("CARGO_PKG_VERSION")))
            .local_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
            .connect_timeout(Duration::from_secs(4))
            .read_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(8))
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self {
            config,
            http,
            analytics: None,
            analytics_source: AnalyticsSource::Cli,
        })
    }

    pub fn with_analytics(mut self, analytics: AnalyticsStore, source: AnalyticsSource) -> Self {
        self.analytics = Some(analytics);
        self.analytics_source = source;
        self
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn analytics_source(&self) -> AnalyticsSource {
        self.analytics_source
    }

    pub async fn record_analytics_event(&self, event: AnalyticsEvent) {
        let Some(analytics) = &self.analytics else {
            return;
        };
        if let Err(err) = analytics.record_event(&event).await {
            tracing::warn!(error = %err, "failed to record analytics event");
        }
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

    pub async fn playback(&mut self) -> Result<Playback> {
        match self
            .request_json::<PlaybackResponse>(Method::GET, "/me/player", None::<()>)
            .await
        {
            Ok(Some(raw)) => Ok(raw.into_playback()),
            Ok(None) => Ok(Playback::default()),
            Err(err) => Err(err),
        }
    }

    pub async fn devices(&mut self) -> Result<Vec<Device>> {
        let response = self
            .request_json::<DevicesResponse>(Method::GET, "/me/player/devices", None::<()>)
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no devices response"))?;
        Ok(response.devices)
    }

    pub async fn queue(&mut self) -> Result<Queue> {
        let response = self
            .request_json::<QueueResponse>(Method::GET, "/me/player/queue", None::<()>)
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no queue response"))?;
        Ok(Queue {
            currently_playing: response
                .currently_playing
                .and_then(RawPlayable::into_media_item),
            items: response
                .queue
                .into_iter()
                .filter_map(RawPlayable::into_media_item)
                .collect(),
        })
    }

    pub async fn search_with_limit(
        &mut self,
        query: &str,
        kinds: &[MediaKind],
        limit: u8,
    ) -> Result<Vec<MediaItem>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }

        let path = search_path(query, kinds, limit);
        let response = self
            .request_json::<SearchResponse>(Method::GET, &path, None::<()>)
            .await?
            .ok_or_else(|| anyhow!("Spotify returned no search response"))?;

        let mut items = Vec::new();
        if let Some(tracks) = response.tracks {
            items.extend(tracks.items.into_iter().map(RawTrack::into_media_item));
        }
        if let Some(episodes) = response.episodes {
            items.extend(episodes.items.into_iter().map(RawEpisode::into_media_item));
        }
        if let Some(albums) = response.albums {
            items.extend(albums.items.into_iter().map(RawAlbum::into_media_item));
        }
        if let Some(artists) = response.artists {
            items.extend(artists.items.into_iter().map(RawArtist::into_media_item));
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

    pub async fn playlists(&mut self) -> Result<Vec<Playlist>> {
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
            if offset >= total || playlists.len() >= 250 {
                break;
            }
        }
        Ok(playlists)
    }

    pub async fn recently_played(&mut self) -> Result<Vec<MediaItem>> {
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

    pub async fn saved_tracks(&mut self) -> Result<Vec<MediaItem>> {
        let mut offset = 0;
        let mut items = Vec::new();
        loop {
            let path = format!("/me/tracks?limit=50&offset={offset}");
            let response = self
                .request_json::<Paging<SavedTrackItem>>(Method::GET, &path, None::<()>)
                .await?
                .ok_or_else(|| anyhow!("Spotify returned no saved tracks response"))?;
            let total = response.total;
            items.extend(
                response
                    .items
                    .into_iter()
                    .map(|item| item.track.into_media_item()),
            );
            offset += 50;
            if offset >= total || items.len() >= 250 {
                break;
            }
        }
        Ok(items)
    }

    pub async fn saved_albums(&mut self) -> Result<Vec<MediaItem>> {
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
            if offset >= total || items.len() >= 250 {
                break;
            }
        }
        Ok(items)
    }

    pub async fn playlist_tracks(&mut self, playlist_id: &str) -> Result<Vec<MediaItem>> {
        let mut offset = 0;
        let mut tracks = Vec::new();
        loop {
            let path = format!("/playlists/{playlist_id}/tracks?limit=50&offset={offset}");
            let response = self
                .request_json::<Paging<PlaylistTrackItem>>(Method::GET, &path, None::<()>)
                .await?
                .ok_or_else(|| anyhow!("Spotify returned no playlist tracks response"))?;
            let total = response.total;
            tracks.extend(
                response
                    .items
                    .into_iter()
                    .filter_map(|item| item.track.into_media_item()),
            );
            offset += 50;
            if offset >= total || tracks.len() >= 500 {
                break;
            }
        }
        Ok(tracks)
    }

    pub async fn play_pause(&mut self, is_playing: bool) -> Result<()> {
        if is_playing {
            self.empty(Method::PUT, "/me/player/pause", None::<()>)
                .await
        } else {
            self.empty(Method::PUT, "/me/player/play", Some(serde_json::json!({})))
                .await
        }
    }

    pub async fn play_uri(&mut self, uri: &str, kind: &MediaKind) -> Result<()> {
        let body = match kind {
            MediaKind::Album | MediaKind::Artist | MediaKind::Playlist => {
                serde_json::json!({ "context_uri": uri })
            }
            _ => serde_json::json!({ "uris": [uri] }),
        };
        self.empty(Method::PUT, "/me/player/play", Some(body)).await
    }

    pub async fn next(&mut self) -> Result<()> {
        self.empty(Method::POST, "/me/player/next", None::<()>)
            .await
    }

    pub async fn previous(&mut self) -> Result<()> {
        self.empty(Method::POST, "/me/player/previous", None::<()>)
            .await
    }

    pub async fn seek(&mut self, position_ms: u64) -> Result<()> {
        self.empty(
            Method::PUT,
            &format!("/me/player/seek?position_ms={position_ms}"),
            None::<()>,
        )
        .await
    }

    pub async fn volume(&mut self, volume_percent: u8) -> Result<()> {
        let volume_percent = volume_percent.min(100);
        self.empty(
            Method::PUT,
            &format!("/me/player/volume?volume_percent={volume_percent}"),
            None::<()>,
        )
        .await
    }

    pub async fn shuffle(&mut self, state: bool) -> Result<()> {
        self.empty(
            Method::PUT,
            &format!("/me/player/shuffle?state={state}"),
            None::<()>,
        )
        .await
    }

    pub async fn repeat(&mut self, state: &str) -> Result<()> {
        self.empty(
            Method::PUT,
            &format!("/me/player/repeat?state={state}"),
            None::<()>,
        )
        .await
    }

    pub async fn add_to_queue(&mut self, uri: &str) -> Result<()> {
        let encoded = url::form_urlencoded::byte_serialize(uri.as_bytes()).collect::<String>();
        self.empty(
            Method::POST,
            &format!("/me/player/queue?uri={encoded}"),
            None::<()>,
        )
        .await
    }

    pub async fn transfer(&mut self, device_id: &str, play: bool) -> Result<()> {
        self.empty(
            Method::PUT,
            "/me/player",
            Some(serde_json::json!({ "device_ids": [device_id], "play": play })),
        )
        .await
    }

    pub async fn add_to_playlist(&mut self, playlist_id: &str, uri: &str) -> Result<()> {
        self.empty(
            Method::POST,
            &format!("/playlists/{playlist_id}/tracks"),
            Some(serde_json::json!({ "uris": [uri] })),
        )
        .await
    }

    pub async fn save_item(&mut self, item: &MediaItem) -> Result<()> {
        let id = item
            .id
            .as_deref()
            .ok_or_else(|| anyhow!("selected item has no Spotify id"))?;
        match item.kind {
            MediaKind::Track => {
                self.empty(Method::PUT, &format!("/me/tracks?ids={id}"), None::<()>)
                    .await
            }
            MediaKind::Episode => {
                self.empty(Method::PUT, &format!("/me/episodes?ids={id}"), None::<()>)
                    .await
            }
            _ => bail!("only tracks and episodes can be saved from now playing"),
        }
    }

    pub async fn image(&self, url: &str) -> Result<Vec<u8>> {
        let response = self
            .http
            .get(url)
            .send()
            .await
            .context("image request failed")?;
        let status = response.status();
        if !status.is_success() {
            bail!("image request failed with {status}");
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
    ) -> Result<()> {
        let token = auth::access_token(&self.config, &self.http).await?;
        let url = format!("{API}{path}");
        let started = Instant::now();
        tracing::debug!(method = %method, path, "Spotify request start");
        let mut request = self.http.request(method.clone(), url).bearer_auth(token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(err) => {
                self.record_spotify_api_finished(
                    &method,
                    path,
                    None,
                    started.elapsed().as_millis(),
                    Some("transport"),
                )
                .await;
                tracing::warn!(method = %method, path, error = %err, "Spotify request send failed");
                return Err(err).with_context(|| format!("Spotify {method} {path} request failed"));
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
        &mut self,
        method: Method,
        path: &str,
        body: Option<impl Serialize>,
    ) -> Result<Option<T>> {
        let token = auth::access_token(&self.config, &self.http).await?;
        let url = format!("{API}{path}");
        let started = Instant::now();
        tracing::debug!(method = %method, path, "Spotify request start");
        let mut request = self.http.request(method.clone(), url).bearer_auth(token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(err) => {
                self.record_spotify_api_finished(
                    &method,
                    path,
                    None,
                    started.elapsed().as_millis(),
                    Some("transport"),
                )
                .await;
                tracing::warn!(method = %method, path, error = %err, "Spotify request send failed");
                return Err(err).with_context(|| format!("Spotify {method} {path} request failed"));
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
        handle_json_response(&method, path, response).await
    }
}

fn search_path(query: &str, kinds: &[MediaKind], limit: u8) -> String {
    let encoded = url::form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>();
    let types = kinds
        .iter()
        .map(MediaKind::label)
        .collect::<Vec<_>>()
        .join(",");
    let limit = limit.min(10);
    format!("/search?q={encoded}&type={types}&limit={limit}")
}

async fn handle_empty_response(
    method: &Method,
    path: &str,
    response: reqwest::Response,
) -> Result<()> {
    let status = response.status();
    if status.is_success() || status == StatusCode::NO_CONTENT {
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
) -> Result<Option<T>> {
    let status = response.status();
    if status == StatusCode::NO_CONTENT {
        return Ok(None);
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
        bail!("Spotify {method} {path} failed ({status}): {message}");
    }
    let body = response
        .text()
        .await
        .with_context(|| format!("failed to read Spotify {method} {path} response"))?;
    match serde_json::from_str::<T>(&body) {
        Ok(value) => Ok(Some(value)),
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
struct SearchResponse {
    tracks: Option<Paging<RawTrack>>,
    episodes: Option<Paging<RawEpisode>>,
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
    track: RawPlayable,
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
                .map(|followers| format!("{} followers", followers.total))
                .unwrap_or_default(),
            duration_ms: 0,
            image_url: image_url(&self.images),
            kind: MediaKind::Artist,
            source: Some("spotify".to_string()),
            freshness: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct RawPlaylist {
    id: Option<String>,
    uri: Option<String>,
    name: Option<String>,
    owner: Option<PlaylistOwner>,
    tracks: Option<PlaylistTracks>,
    #[serde(default, deserialize_with = "null_to_default")]
    images: Vec<ImageRef>,
}

impl RawPlaylist {
    fn into_playlist(self) -> Option<Playlist> {
        let id = self.id?;
        let tracks_total = self.tracks.as_ref().map(|tracks| tracks.total).unwrap_or(0);
        Some(Playlist {
            id,
            name: self.name.unwrap_or_else(|| "Untitled playlist".to_string()),
            owner: playlist_owner_name(self.owner),
            tracks_total,
            image_url: image_url(&self.images),
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

#[derive(Clone, Debug, Deserialize)]
struct ImageRef {
    url: Option<String>,
    width: Option<u32>,
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

#[cfg(test)]
mod tests {
    use super::{search_path, MediaKind};

    #[test]
    fn search_path_uses_valid_spotify_type_and_limit_params() {
        assert_eq!(
            search_path("luther vandross", &[MediaKind::Track], 10),
            "/search?q=luther+vandross&type=track&limit=10"
        );
    }

    #[test]
    fn search_path_clamps_to_spotify_documented_max_limit() {
        assert_eq!(
            search_path(
                "jazz",
                &[
                    MediaKind::Track,
                    MediaKind::Episode,
                    MediaKind::Album,
                    MediaKind::Artist,
                    MediaKind::Playlist,
                ],
                50,
            ),
            "/search?q=jazz&type=track,episode,album,artist,playlist&limit=10"
        );
    }
}
