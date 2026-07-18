//! Mercury-backed discovery: related artists and radio stations.
//!
//! These ride the in-daemon librespot session's Mercury channel (see
//! `DaemonState::mercury_get`) because the equivalent Web API endpoints
//! were deprecated in Nov 2024. The `hm://` endpoints are
//! reverse-engineered and unversioned by Spotify, so the parsers here
//! are deliberately defensive: unknown/extra fields are ignored and a
//! shape mismatch yields an empty result rather than a hard error, and
//! the daemon surfaces a clear "endpoint may have changed" message.

use serde::Deserialize;
use spotuify_core::{MediaItem, MediaKind, ResourceUri};

const BASE62: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// Decode a 22-char Spotify base62 id into its 128-bit value.
fn base62_to_u128(id: &str) -> Option<u128> {
    if id.len() != 22 {
        return None;
    }
    let mut value: u128 = 0;
    for byte in id.bytes() {
        let digit = BASE62.iter().position(|&c| c == byte)? as u128;
        value = value.checked_mul(62)?.checked_add(digit)?;
    }
    Some(value)
}

/// Encode a 128-bit id back into the 22-char base62 form.
fn u128_to_base62(mut value: u128) -> String {
    let mut buf = [b'0'; 22];
    for slot in buf.iter_mut().rev() {
        *slot = BASE62[(value % 62) as usize];
        value /= 62;
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// `spotify:artist:<base62>` → the 32-char hex `gid` Mercury endpoints use.
pub fn artist_gid_from_uri(uri: &str) -> Option<String> {
    let resource = ResourceUri::parse(uri).ok()?;
    if resource.kind() != MediaKind::Artist {
        return None;
    }
    Some(format!("{:032x}", base62_to_u128(resource.bare_id())?))
}

/// 32-char hex `gid` → `spotify:artist:<base62>`.
fn artist_uri_from_gid(gid: &str) -> Option<String> {
    let value = u128::from_str_radix(gid, 16).ok()?;
    ResourceUri::spotify(MediaKind::Artist, u128_to_base62(value))
        .ok()
        .map(|resource| resource.as_uri())
}

/// Mercury URI for an artist's related artists.
pub fn related_artists_mercury_uri(artist_uri: &str) -> Option<String> {
    let gid = artist_gid_from_uri(artist_uri)?;
    Some(format!("hm://artist/v1/{gid}/desktop?format=json"))
}

/// Mercury URI for a radio station seeded by any Spotify URI.
pub fn radio_station_mercury_uri(seed_uri: &str) -> String {
    format!("hm://radio-apollo/v3/stations/{seed_uri}?autoplay=true&count=50")
}

// --- related artists ---

#[derive(Debug, Deserialize)]
struct RelatedArtistsEnvelope {
    #[serde(default)]
    related_artists: RelatedArtistsBlock,
}

#[derive(Debug, Default, Deserialize)]
struct RelatedArtistsBlock {
    #[serde(default)]
    artists: Vec<RelatedArtist>,
}

#[derive(Debug, Deserialize)]
struct RelatedArtist {
    #[serde(default)]
    name: String,
    /// Hex gid (older shape) and/or a `spotify:artist:` URI (newer).
    #[serde(default)]
    gid: Option<String>,
    #[serde(default)]
    uri: Option<String>,
}

/// Parse a related-artists Mercury response into artist `MediaItem`s.
/// Returns an empty vec (not an error) when the shape doesn't match, so
/// a rotated endpoint degrades to "no results" rather than a crash.
pub fn parse_related_artists(bytes: &[u8]) -> Vec<MediaItem> {
    let Ok(envelope) = serde_json::from_slice::<RelatedArtistsEnvelope>(bytes) else {
        return Vec::new();
    };
    envelope
        .related_artists
        .artists
        .into_iter()
        .filter_map(|artist| {
            let resource = artist
                .uri
                .as_deref()
                .and_then(|uri| ResourceUri::parse(uri).ok())
                .filter(|resource| resource.kind() == MediaKind::Artist)
                .or_else(|| {
                    artist
                        .gid
                        .as_deref()
                        .and_then(artist_uri_from_gid)
                        .and_then(|uri| ResourceUri::parse(&uri).ok())
                })?;
            (!artist.name.is_empty()).then(|| MediaItem {
                id: Some(resource.bare_id().to_string()),
                uri: resource.as_uri(),
                name: artist.name,
                kind: MediaKind::Artist,
                source: Some("mercury".into()),
                ..MediaItem::default()
            })
        })
        .collect()
}

// --- radio station ---

#[derive(Debug, Deserialize)]
struct RadioStationEnvelope {
    #[serde(default)]
    tracks: Vec<RadioTrack>,
}

#[derive(Debug, Deserialize)]
struct RadioTrack {
    /// Some responses carry a full `spotify:track:` URI, others a bare
    /// `original_gid`/`gid`; we normalise to a track URI.
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    original_gid: Option<String>,
    #[serde(default)]
    gid: Option<String>,
}

/// Parse a radio-station Mercury response into the seed playlist of
/// track URIs. Defensive: unknown shapes yield an empty list.
pub fn parse_radio_station(bytes: &[u8]) -> Vec<String> {
    let Ok(envelope) = serde_json::from_slice::<RadioStationEnvelope>(bytes) else {
        return Vec::new();
    };
    envelope
        .tracks
        .into_iter()
        .filter_map(|track| {
            if let Some(resource) = track
                .uri
                .as_deref()
                .and_then(|uri| ResourceUri::parse(uri).ok())
                .filter(|resource| resource.kind() == MediaKind::Track)
            {
                return Some(resource.as_uri());
            }
            let gid = track.original_gid.or(track.gid)?;
            let value = u128::from_str_radix(&gid, 16).ok()?;
            ResourceUri::spotify(MediaKind::Track, u128_to_base62(value))
                .ok()
                .map(|resource| resource.as_uri())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn base62_gid_roundtrips() {
        // 4uLU6hMCjMI75M1A2tKUQC is a well-known artist id; the exact gid
        // doesn't matter, only that the conversion is a clean round-trip.
        let uri = "spotify:artist:4uLU6hMCjMI75M1A2tKUQC";
        let gid = artist_gid_from_uri(uri).expect("gid");
        assert_eq!(gid.len(), 32);
        assert_eq!(artist_uri_from_gid(&gid).as_deref(), Some(uri));
        assert!(artist_gid_from_uri("spotify:track:4uLU6hMCjMI75M1A2tKUQC").is_none());
        assert!(artist_gid_from_uri("spotify:artist:").is_none());
    }

    #[test]
    fn related_artists_uri_uses_gid() {
        let uri = related_artists_mercury_uri("spotify:artist:4uLU6hMCjMI75M1A2tKUQC")
            .expect("mercury uri");
        assert!(uri.starts_with("hm://artist/v1/"));
        assert!(uri.ends_with("/desktop?format=json"));
    }

    #[test]
    fn parses_related_artists_from_uri_and_gid() {
        let json = br#"{
            "related_artists": { "artists": [
                { "name": "Artist One", "uri": "spotify:artist:4uLU6hMCjMI75M1A2tKUQC" },
                { "name": "Artist Two", "gid": "0000000000000000000000000000002a" },
                { "name": "", "uri": "spotify:artist:4uLU6hMCjMI75M1A2tKUQC" },
                { "name": "Wrong Kind", "uri": "spotify:track:4uLU6hMCjMI75M1A2tKUQC" },
                { "name": "Malformed", "uri": "spotify:artist:" }
            ] }
        }"#;
        let items = parse_related_artists(json);
        assert_eq!(items.len(), 2, "blank-name artist is dropped");
        assert_eq!(items[0].name, "Artist One");
        assert_eq!(items[0].kind, MediaKind::Artist);
        assert_eq!(
            ResourceUri::parse(&items[1].uri).unwrap().kind(),
            MediaKind::Artist
        );
    }

    #[test]
    fn parse_related_artists_tolerates_rotated_shape() {
        assert!(parse_related_artists(b"{\"unexpected\":true}").is_empty());
        assert!(parse_related_artists(b"not json").is_empty());
    }

    #[test]
    fn parses_radio_station_track_uris() {
        let json = br#"{ "tracks": [
            { "uri": "spotify:track:4uLU6hMCjMI75M1A2tKUQC" },
            { "uri": "spotify:artist:4uLU6hMCjMI75M1A2tKUQC" },
            { "uri": "spotify:track:" },
            { "original_gid": "0000000000000000000000000000002a" }
        ] }"#;
        let uris = parse_radio_station(json);
        assert_eq!(uris.len(), 2);
        assert!(uris
            .iter()
            .all(|uri| { ResourceUri::parse(uri).unwrap().kind() == MediaKind::Track }));
    }
}
