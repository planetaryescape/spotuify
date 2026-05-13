use std::io::{IsTerminal, Read};
use std::path::Path;

use anyhow::{bail, Context, Result};

use spotuify_core::{Device, MediaItem, MediaKind, Playlist};

pub fn media_item_at_index(items: Vec<MediaItem>, query: &str, index: usize) -> Result<MediaItem> {
    if index == 0 {
        bail!("search index is 1-based; pass --index 1 for the first result");
    }
    items
        .into_iter()
        .nth(index - 1)
        .ok_or_else(|| anyhow::anyhow!("no Spotify result #{index} for `{query}`"))
}

pub fn media_kind_from_uri(uri: &str) -> Result<MediaKind> {
    if uri.starts_with("spotify:track:") {
        return Ok(MediaKind::Track);
    }
    if uri.starts_with("spotify:episode:") {
        return Ok(MediaKind::Episode);
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

    bail!("unsupported Spotify URI `{uri}`; expected spotify:track, episode, album, artist, or playlist")
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
) -> Result<UriSelection> {
    match (positional.is_empty(), ids_path) {
        (false, Some(_)) => bail!("provide URI(s) or --ids, not both"),
        (false, None) => selection_from_uris(positional, false, false),
        (true, Some(path)) => {
            let ids = read_ids_path(path)?;
            if ids.is_empty() {
                bail!("no Spotify URIs provided by --ids {}", path.display());
            }
            selection_from_uris(ids, true, path == Path::new("-"))
        }
        (true, None) => match read_piped_ids()? {
            Some(ids) if !ids.is_empty() => selection_from_uris(ids, false, true),
            _ => bail!("{missing_message}"),
        },
    }
}

pub fn ensure_track_or_episode_uris(uris: &[String]) -> Result<()> {
    for uri in uris {
        match media_kind_from_uri(uri)? {
            MediaKind::Track | MediaKind::Episode => {}
            _ => bail!("playlist add only accepts track or episode URIs: {uri}"),
        }
    }
    Ok(())
}

fn selection_from_uris(
    uris: Vec<String>,
    used_ids_file: bool,
    used_stdin: bool,
) -> Result<UriSelection> {
    for uri in &uris {
        media_kind_from_uri(uri)?;
    }
    Ok(UriSelection {
        uris,
        used_ids_file,
        used_stdin,
    })
}

fn read_ids_path(path: &Path) -> Result<Vec<String>> {
    let input = if path == Path::new("-") {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        input
    } else {
        std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?
    };
    Ok(split_ids(&input))
}

fn read_piped_ids() -> Result<Option<Vec<String>>> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Ok(None);
    }
    let mut input = String::new();
    stdin.read_to_string(&mut input)?;
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

pub fn resolve_playlist(playlists: &[Playlist], value: &str) -> Result<Playlist> {
    playlists
        .iter()
        .find(|playlist| {
            playlist.id == value
                || playlist_uri(&playlist.id) == value
                || playlist.name.eq_ignore_ascii_case(value)
        })
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no playlist matching `{value}`"))
}

pub fn resolve_device(devices: &[Device], value: &str) -> Result<Device> {
    devices
        .iter()
        .find(|device| {
            device.id.as_deref() == Some(value) || device.name.eq_ignore_ascii_case(value)
        })
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no device matching `{value}`"))
}

pub fn parse_seek_target(input: &str, current_ms: u64) -> Result<u64> {
    let input = input.trim();
    if input.is_empty() {
        bail!("seek target is required");
    }

    let (sign, value) = match input.as_bytes()[0] {
        b'+' => (1_i64, &input[1..]),
        b'-' => (-1_i64, &input[1..]),
        _ => (0_i64, input),
    };
    let duration_ms = parse_duration_ms(value)?;
    if sign == 0 {
        return Ok(duration_ms);
    }

    let current = current_ms as i64;
    Ok(current
        .saturating_add(sign.saturating_mul(duration_ms as i64))
        .max(0) as u64)
}

fn parse_duration_ms(value: &str) -> Result<u64> {
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
        .map_err(|_| anyhow::anyhow!("invalid seek duration `{value}`; try +15s or -30s"))?;
    Ok(amount.saturating_mul(multiplier))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_seek_target_supports_relative_offsets() {
        assert_eq!(parse_seek_target("+15s", 30_000).unwrap(), 45_000);
        assert_eq!(parse_seek_target("-45s", 30_000).unwrap(), 0);
        assert_eq!(parse_seek_target("2m", 30_000).unwrap(), 120_000);
    }

    #[test]
    fn media_item_index_is_one_based() {
        let items = vec![media("spotify:track:1"), media("spotify:track:2")];

        assert_eq!(
            media_item_at_index(items, "q", 2).unwrap().uri,
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
        .unwrap();

        assert_eq!(selection.uris.len(), 2);
        assert!(selection.requires_confirmation());
    }

    #[test]
    fn playlist_uri_validation_rejects_non_track_or_episode() {
        let err = ensure_track_or_episode_uris(&["spotify:album:1".to_string()])
            .unwrap_err()
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
        }
    }
}
