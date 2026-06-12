use std::io::{IsTerminal, Read};
use std::path::Path;

use crate::error::{SpotifyError, SpotifyResult};
use spotuify_core::{Device, MediaItem, MediaKind, Playlist};

fn invalid(message: impl Into<String>) -> SpotifyError {
    SpotifyError::InvalidInput {
        message: message.into(),
    }
}

fn client(message: impl Into<String>) -> SpotifyError {
    SpotifyError::Client {
        message: message.into(),
    }
}

pub fn media_item_at_index(
    items: Vec<MediaItem>,
    query: &str,
    index: usize,
) -> SpotifyResult<MediaItem> {
    if index == 0 {
        return Err(invalid(
            "search index is 1-based; pass --index 1 for the first result",
        ));
    }
    items
        .into_iter()
        .nth(index - 1)
        .ok_or_else(|| invalid(format!("no Spotify result #{index} for `{query}`")))
}

pub fn media_kind_from_uri(uri: &str) -> SpotifyResult<MediaKind> {
    if uri.starts_with("spotify:track:") {
        return Ok(MediaKind::Track);
    }
    if uri.starts_with("spotify:episode:") {
        return Ok(MediaKind::Episode);
    }
    if uri.starts_with("spotify:show:") {
        return Ok(MediaKind::Show);
    }
    if uri.starts_with("spotify:album:") {
        return Ok(MediaKind::Album);
    }
    if uri.starts_with("spotify:artist:") {
        return Ok(MediaKind::Artist);
    }
    if uri.starts_with("spotify:playlist:") {
        return Ok(MediaKind::Playlist);
    }

    Err(invalid(format!(
        "unsupported Spotify URI `{uri}`; expected spotify:track, episode, show, album, artist, or playlist"
    )))
}

/// Normalize a user-supplied target into a canonical `spotify:` URI.
/// Accepts `spotify:` URIs (any case, `?si=` junk stripped, empty IDs
/// rejected) and open.spotify.com share links — including
/// locale-prefixed (`/intl-fr/track/<id>`), `/embed/track/<id>`, and
/// legacy `/user/<u>/playlist/<id>` shapes. Returns `None` for
/// anything that isn't a recognizable Spotify target so callers can
/// fall back (search) or reject loudly.
pub fn normalize_spotify_target(arg: &str) -> Option<String> {
    let trimmed = arg.trim();
    // `spotify:` URIs, case-insensitively (the prefix checks in
    // `media_kind_from_uri` are case-sensitive on purpose — canonical
    // URIs are lowercase — but user input shouldn't silently fall
    // through to a literal text search).
    if trimmed.len() >= 8 && trimmed[..8].eq_ignore_ascii_case("spotify:") {
        let mut parts = trimmed.split(':');
        let _scheme = parts.next()?;
        let mut kind = parts.next()?.to_ascii_lowercase();
        let mut id = parts.next()?;
        // Legacy long form: spotify:user:<username>:playlist:<id>.
        if kind == "user" {
            let _username = id;
            kind = parts.next()?.to_ascii_lowercase();
            id = parts.next()?;
        }
        let id = id.split('?').next().unwrap_or(id);
        if id.is_empty() {
            return None;
        }
        let uri = format!("spotify:{kind}:{id}");
        return media_kind_from_uri(&uri).is_ok().then_some(uri);
    }
    let parsed = url::Url::parse(trimmed).ok()?;
    if parsed.host_str() != Some("open.spotify.com") {
        return None;
    }
    let mut segments: Vec<&str> = parsed.path_segments()?.filter(|s| !s.is_empty()).collect();
    // Locale-prefixed share links: /intl-fr/track/<id>.
    if segments.first().is_some_and(|s| s.starts_with("intl-")) {
        segments.remove(0);
    }
    // Embed links: /embed/track/<id>.
    if segments.first() == Some(&"embed") {
        segments.remove(0);
    }
    // Legacy user-scoped playlists: /user/<u>/playlist/<id>.
    if segments.first() == Some(&"user") && segments.len() >= 4 {
        segments.drain(..2);
    }
    let [kind, id, ..] = segments[..] else {
        return None;
    };
    if id.is_empty() {
        return None;
    }
    let uri = format!("spotify:{}:{id}", kind.to_ascii_lowercase());
    media_kind_from_uri(&uri).is_ok().then_some(uri)
}

pub fn playlist_uri(playlist_id: &str) -> String {
    if playlist_id.starts_with("spotify:playlist:") {
        playlist_id.to_string()
    } else {
        format!("spotify:playlist:{playlist_id}")
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UriSelection {
    pub uris: Vec<String>,
    pub used_ids_file: bool,
    pub used_stdin: bool,
}

impl UriSelection {
    pub fn requires_confirmation(&self) -> bool {
        self.uris.len() > 1 || self.used_ids_file || self.used_stdin
    }
}

pub fn resolve_uri_selection(
    positional: Vec<String>,
    ids_path: Option<&Path>,
    missing_message: &str,
) -> SpotifyResult<UriSelection> {
    match (positional.is_empty(), ids_path) {
        (false, Some(_)) => Err(invalid("provide URI(s) or --ids, not both")),
        (false, None) => selection_from_uris(positional, false, false),
        (true, Some(path)) => {
            let ids = read_ids_path(path)?;
            if ids.is_empty() {
                return Err(invalid(format!(
                    "no Spotify URIs provided by --ids {}",
                    path.display()
                )));
            }
            selection_from_uris(ids, true, path == Path::new("-"))
        }
        (true, None) => match read_piped_ids()? {
            Some(ids) if !ids.is_empty() => selection_from_uris(ids, false, true),
            _ => Err(invalid(missing_message)),
        },
    }
}

pub fn ensure_track_or_episode_uris(uris: &[String]) -> SpotifyResult<()> {
    for uri in uris {
        match media_kind_from_uri(uri)? {
            MediaKind::Track | MediaKind::Episode => {}
            _ => {
                return Err(invalid(format!(
                    "playlist add only accepts track or episode URIs: {uri}"
                )));
            }
        }
    }
    Ok(())
}

fn selection_from_uris(
    uris: Vec<String>,
    used_ids_file: bool,
    used_stdin: bool,
) -> SpotifyResult<UriSelection> {
    for uri in &uris {
        media_kind_from_uri(uri)?;
    }
    Ok(UriSelection {
        uris,
        used_ids_file,
        used_stdin,
    })
}

fn read_ids_path(path: &Path) -> SpotifyResult<Vec<String>> {
    let input = if path == Path::new("-") {
        let mut input = String::new();
        std::io::stdin()
            .read_to_string(&mut input)
            .map_err(|err| client(format!("failed to read stdin: {err}")))?;
        input
    } else {
        std::fs::read_to_string(path)
            .map_err(|err| client(format!("failed to read {}: {err}", path.display())))?
    };
    Ok(split_ids(&input))
}

fn read_piped_ids() -> SpotifyResult<Option<Vec<String>>> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Ok(None);
    }
    let mut input = String::new();
    stdin
        .read_to_string(&mut input)
        .map_err(|err| client(format!("failed to read stdin: {err}")))?;
    Ok(Some(split_ids(&input)))
}

fn split_ids(input: &str) -> Vec<String> {
    input
        .split_whitespace()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn resolve_playlist(playlists: &[Playlist], value: &str) -> SpotifyResult<Playlist> {
    playlists
        .iter()
        .find(|playlist| {
            playlist.id == value
                || playlist_uri(&playlist.id) == value
                || playlist.name.eq_ignore_ascii_case(value)
        })
        .cloned()
        .ok_or_else(|| invalid(format!("no playlist matching `{value}`")))
}

pub fn resolve_device(devices: &[Device], value: &str) -> SpotifyResult<Device> {
    devices
        .iter()
        .find(|device| {
            device.id.as_deref() == Some(value) || device.name.eq_ignore_ascii_case(value)
        })
        .cloned()
        .ok_or_else(|| invalid(format!("no device matching `{value}`")))
}

pub fn parse_seek_target(input: &str, current_ms: u64) -> SpotifyResult<u64> {
    match parse_seek_input(input)? {
        SeekInput::Absolute(ms) => Ok(ms),
        SeekInput::Relative(offset_ms) => {
            let current = current_ms as i64;
            Ok(current.saturating_add(offset_ms).max(0) as u64)
        }
    }
}

/// Phase 5 — typed parse of a user-supplied seek expression. CLI sends
/// the result to the daemon so the daemon (not the client) resolves
/// relative offsets against its `PlaybackClock`. Eliminates the
/// "relative seek lands somewhere surprising" symptom caused by the
/// client reading a stale cached progress before computing the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekInput {
    Absolute(u64),
    Relative(i64),
}

pub fn parse_seek_input(input: &str) -> SpotifyResult<SeekInput> {
    let input = input.trim();
    if input.is_empty() {
        return Err(invalid("seek target is required"));
    }

    let (sign, value) = match input.as_bytes()[0] {
        b'+' => (1_i64, &input[1..]),
        b'-' => (-1_i64, &input[1..]),
        _ => (0_i64, input),
    };
    let duration_ms = parse_duration_ms(value)?;
    if sign == 0 {
        Ok(SeekInput::Absolute(duration_ms))
    } else {
        Ok(SeekInput::Relative(sign.saturating_mul(duration_ms as i64)))
    }
}

fn parse_duration_ms(value: &str) -> SpotifyResult<u64> {
    let (number, multiplier) = if let Some(number) = value.strip_suffix("ms") {
        (number, 1)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1_000)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60_000)
    } else {
        (value, 1_000)
    };
    let amount = number
        .parse::<u64>()
        .map_err(|_| invalid(format!("invalid seek duration `{value}`; try +15s or -30s")))?;
    Ok(amount.saturating_mul(multiplier))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_seek_target_supports_relative_offsets() {
        assert_eq!(
            parse_seek_target("+15s", 30_000).expect("positive relative seek should parse"),
            45_000
        );
        assert_eq!(
            parse_seek_target("-45s", 30_000).expect("negative relative seek should parse"),
            0
        );
        assert_eq!(
            parse_seek_target("2m", 30_000).expect("absolute minute seek should parse"),
            120_000
        );
    }

    #[test]
    fn media_item_index_is_one_based() {
        let items = vec![media("spotify:track:1"), media("spotify:track:2")];

        assert_eq!(
            media_item_at_index(items, "q", 2)
                .expect("second item should be selectable")
                .uri,
            "spotify:track:2"
        );
    }

    #[test]
    fn uri_selection_accepts_multiple_positional_spotify_uris() {
        let selection = resolve_uri_selection(
            vec![
                "spotify:track:1".to_string(),
                "spotify:episode:2".to_string(),
            ],
            None,
            "missing",
        )
        .expect("multiple positional Spotify URIs should resolve");

        assert_eq!(selection.uris.len(), 2);
        assert!(selection.requires_confirmation());
    }

    #[test]
    fn playlist_uri_validation_rejects_non_track_or_episode() {
        let err = ensure_track_or_episode_uris(&["spotify:album:1".to_string()])
            .expect_err("album URI should be rejected for playlist add")
            .to_string();

        assert!(err.contains("playlist add only accepts track or episode URIs"));
    }

    fn media(uri: &str) -> MediaItem {
        MediaItem {
            id: None,
            uri: uri.to_string(),
            name: uri.to_string(),
            subtitle: String::new(),
            context: String::new(),
            duration_ms: 0,
            image_url: None,
            kind: MediaKind::Track,
            source: None,
            freshness: None,
            explicit: None,
            is_playable: None,
            ..Default::default()
        }
    }
}
