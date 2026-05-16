use std::sync::OnceLock;
use std::time::{Duration, Instant};

use reqwest::header::{HeaderValue, RETRY_AFTER};
use serde::Deserialize;
use spotuify_core::{LyricsProvider, MediaItem, SyncedLyrics};
use tokio::sync::Mutex;

use crate::{parse_lrc, plain_text_lines, LyricsError};

static NEXT_LRCLIB_REQUEST: OnceLock<Mutex<Instant>> = OnceLock::new();

#[derive(Clone)]
pub struct LrclibProvider {
    http: reqwest::Client,
    base_url: String,
}

impl Default for LrclibProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl LrclibProvider {
    pub fn new() -> Self {
        let base_url = std::env::var("SPOTUIFY_LRCLIB_BASE_URL")
            .unwrap_or_else(|_| "https://lrclib.net".to_string());
        Self::with_base_url(base_url)
    }

    pub fn with_base_url(base_url: String) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(format!(
                "spotuify/{} ({})",
                env!("CARGO_PKG_VERSION"),
                env!("CARGO_PKG_REPOSITORY")
            ))
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { http, base_url }
    }

    pub async fn fetch(
        &self,
        item: &MediaItem,
        fetched_at_ms: i64,
    ) -> Result<Option<SyncedLyrics>, LyricsError> {
        match self.fetch_exact(item, fetched_at_ms).await? {
            Some(lyrics) => Ok(Some(lyrics)),
            None => self.fetch_search(item, fetched_at_ms).await,
        }
    }

    async fn fetch_exact(
        &self,
        item: &MediaItem,
        fetched_at_ms: i64,
    ) -> Result<Option<SyncedLyrics>, LyricsError> {
        let duration = (item.duration_ms / 1000).to_string();
        let resp = self
            .send_with_backoff(|| {
                self.http.get(format!("{}/api/get", self.base_url)).query(&[
                    ("track_name", item.name.as_str()),
                    ("artist_name", item.subtitle.as_str()),
                    ("album_name", item.context.as_str()),
                    ("duration", duration.as_str()),
                ])
            })
            .await?;
        response_to_lyrics(resp, &item.uri, fetched_at_ms).await
    }

    async fn fetch_search(
        &self,
        item: &MediaItem,
        fetched_at_ms: i64,
    ) -> Result<Option<SyncedLyrics>, LyricsError> {
        let resp = self
            .send_with_backoff(|| {
                self.http
                    .get(format!("{}/api/search", self.base_url))
                    .query(&[("q", format!("{} {}", item.name, item.subtitle))])
            })
            .await?;
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(LyricsError::RateLimited);
        }
        if !resp.status().is_success() {
            return Ok(None);
        }
        let rows: Vec<LrclibResponse> = resp.json().await?;
        Ok(rows
            .into_iter()
            .find_map(|row| lrclib_row_to_lyrics(row, &item.uri, fetched_at_ms)))
    }

    async fn send_with_backoff<F>(
        &self,
        mut make_request: F,
    ) -> Result<reqwest::Response, LyricsError>
    where
        F: FnMut() -> reqwest::RequestBuilder,
    {
        for attempt in 0..=1 {
            wait_rate_limit().await;
            let resp = make_request().send().await?;
            if resp.status() != reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Ok(resp);
            }
            apply_rate_limit_backoff(retry_after_duration(resp.headers().get(RETRY_AFTER))).await;
            if attempt == 1 {
                return Err(LyricsError::RateLimited);
            }
        }
        Err(LyricsError::RateLimited)
    }
}

async fn wait_rate_limit() {
    let mut next = NEXT_LRCLIB_REQUEST
        .get_or_init(|| Mutex::new(Instant::now()))
        .lock()
        .await;
    let now = Instant::now();
    if *next > now {
        tokio::time::sleep(*next - now).await;
    }
    *next = Instant::now() + Duration::from_millis(500);
}

async fn apply_rate_limit_backoff(delay: Duration) {
    let mut next = NEXT_LRCLIB_REQUEST
        .get_or_init(|| Mutex::new(Instant::now()))
        .lock()
        .await;
    let retry_at = Instant::now() + delay;
    if *next < retry_at {
        *next = retry_at;
    }
}

fn retry_after_duration(header: Option<&HeaderValue>) -> Duration {
    header
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(1))
}

async fn response_to_lyrics(
    resp: reqwest::Response,
    track_uri: &str,
    fetched_at_ms: i64,
) -> Result<Option<SyncedLyrics>, LyricsError> {
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(LyricsError::RateLimited);
    }
    if !resp.status().is_success() {
        return Ok(None);
    }
    let row: LrclibResponse = resp.json().await?;
    Ok(lrclib_row_to_lyrics(row, track_uri, fetched_at_ms))
}

fn lrclib_row_to_lyrics(
    row: LrclibResponse,
    track_uri: &str,
    fetched_at_ms: i64,
) -> Option<SyncedLyrics> {
    if row.instrumental.unwrap_or(false) {
        return None;
    }
    let (lines, synced) = if let Some(synced) = row.synced_lyrics.as_deref() {
        (parse_lrc(synced), true)
    } else if let Some(plain) = row.plain_lyrics.as_deref() {
        (plain_text_lines(plain), false)
    } else {
        return None;
    };
    (!lines.is_empty()).then(|| SyncedLyrics {
        provider: LyricsProvider::Lrclib,
        track_uri: track_uri.to_string(),
        lines,
        fetched_at_ms,
        synced,
        language: None,
        source_url: Some("https://lrclib.net".to_string()),
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LrclibResponse {
    instrumental: Option<bool>,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use spotuify_core::MediaKind;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn exact_synced_row_maps_to_synced_lyrics() {
        let row = LrclibResponse {
            instrumental: Some(false),
            plain_lyrics: None,
            synced_lyrics: Some("[00:01.00]hello".to_string()),
        };
        let lyrics = lrclib_row_to_lyrics(row, "spotify:track:abc", 9)
            .expect("synced row should map to lyrics");
        assert_eq!(lyrics.provider, LyricsProvider::Lrclib);
        assert!(lyrics.synced);
        assert_eq!(lyrics.lines[0].start_ms, 1_000);
    }

    #[test]
    fn plain_row_maps_to_unsynced_lines() {
        let row = LrclibResponse {
            instrumental: Some(false),
            plain_lyrics: Some("one\ntwo".to_string()),
            synced_lyrics: None,
        };
        let lyrics = lrclib_row_to_lyrics(row, "spotify:track:abc", 9)
            .expect("plain row should map to lyrics");
        assert!(!lyrics.synced);
        assert_eq!(lyrics.lines.len(), 2);
    }

    #[test]
    fn instrumental_rows_are_not_lyrics() {
        let row = LrclibResponse {
            instrumental: Some(true),
            plain_lyrics: Some("ignored".to_string()),
            synced_lyrics: None,
        };
        assert!(lrclib_row_to_lyrics(row, "spotify:track:abc", 9).is_none());
    }

    #[tokio::test]
    async fn exact_request_retries_once_after_429_retry_after() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/get"))
            .and(query_param("track_name", "Track"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/get"))
            .and(query_param("track_name", "Track"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "instrumental": false,
                "plainLyrics": "hello"
            })))
            .mount(&server)
            .await;

        let provider = LrclibProvider::with_base_url(server.uri());
        let lyrics = provider
            .fetch(&media_item(), 9)
            .await
            .expect("429 with retry-after 0 should retry once")
            .expect("second response should provide lyrics");

        assert_eq!(lyrics.lines[0].text, "hello");
        let requests = server
            .received_requests()
            .await
            .expect("wiremock should expose received requests");
        assert_eq!(requests.len(), 2);
    }

    #[allow(dead_code)]
    fn media_item() -> MediaItem {
        MediaItem {
            id: Some("abc".to_string()),
            uri: "spotify:track:abc".to_string(),
            name: "Track".to_string(),
            subtitle: "Artist".to_string(),
            context: "Album".to_string(),
            duration_ms: 180_000,
            image_url: None,
            kind: MediaKind::Track,
            source: None,
            freshness: None,
            explicit: None,
            is_playable: None,
        }
    }
}
