use serde::Deserialize;
use spotuify_core::{LyricLine, LyricsProvider, SyncedLyrics};

use crate::LyricsError;

pub fn mercury_uri_for_track_uri(track_uri: &str) -> Option<String> {
    let track_id = track_uri.strip_prefix("spotify:track:")?;
    (!track_id.is_empty()).then(|| format!("hm://lyrics/v1/track/{track_id}"))
}

pub fn parse_spotify_mercury(
    bytes: bytes::Bytes,
    track_uri: &str,
    fetched_at_ms: i64,
) -> Result<Option<SyncedLyrics>, LyricsError> {
    let response: SpotifyLyricsResponse = serde_json::from_slice(&bytes)?;
    let Some(lyrics) = response.lyrics else {
        return Ok(None);
    };
    let mut lines = Vec::new();
    for line in lyrics.lines {
        let start_ms = line.start_time_ms.parse::<u64>().map_err(|err| {
            LyricsError::InvalidPayload(format!("invalid Spotify startTimeMs: {err}"))
        })?;
        lines.push(LyricLine {
            start_ms,
            is_rtl: crate::parser::is_rtl(&line.words),
            text: line.words,
        });
    }
    if lines.is_empty() {
        return Ok(None);
    }
    Ok(Some(SyncedLyrics {
        provider: LyricsProvider::SpotifyMercury,
        track_uri: track_uri.to_string(),
        lines,
        fetched_at_ms,
        synced: lyrics.sync_type.as_deref() != Some("UNSYNCED"),
        language: lyrics.language,
        source_url: None,
    }))
}

#[derive(Debug, Deserialize)]
struct SpotifyLyricsResponse {
    lyrics: Option<SpotifyLyricsBody>,
}

#[derive(Debug, Deserialize)]
struct SpotifyLyricsBody {
    #[serde(default, rename = "syncType")]
    sync_type: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    lines: Vec<SpotifyLine>,
}

#[derive(Debug, Deserialize)]
struct SpotifyLine {
    #[serde(rename = "startTimeMs")]
    start_time_ms: String,
    words: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_mercury_uri_from_track_uri() {
        assert_eq!(
            mercury_uri_for_track_uri("spotify:track:abc").as_deref(),
            Some("hm://lyrics/v1/track/abc")
        );
        assert!(mercury_uri_for_track_uri("spotify:album:abc").is_none());
    }

    #[test]
    fn parses_spotify_mercury_json() {
        let raw = serde_json::json!({
            "lyrics": {
                "syncType": "LINE_SYNCED",
                "language": "en",
                "lines": [{"startTimeMs": "1234", "words": "hello"}]
            }
        });
        let lyrics = parse_spotify_mercury(
            bytes::Bytes::from(
                serde_json::to_vec(&raw).expect("test mercury payload should serialize"),
            ),
            "spotify:track:abc",
            7,
        )
        .expect("valid mercury JSON should parse")
        .expect("payload with lyrics should produce lyrics");
        assert_eq!(lyrics.provider, LyricsProvider::SpotifyMercury);
        assert!(lyrics.synced);
        assert_eq!(lyrics.lines[0].start_ms, 1_234);
    }
}
