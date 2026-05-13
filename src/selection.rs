use anyhow::{bail, Result};

use crate::spotify::{Device, MediaItem, MediaKind, Playlist};

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
        }
    }
}
