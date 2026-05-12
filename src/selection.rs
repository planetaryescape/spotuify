use anyhow::{bail, Result};

use crate::spotify::{MediaItem, MediaKind};

pub fn first_media_item(items: Vec<MediaItem>, query: &str) -> Result<MediaItem> {
    items
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no Spotify results for `{query}`"))
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
    if uri.starts_with("spotify:playlist:") {
        return Ok(MediaKind::Playlist);
    }

    bail!("unsupported Spotify URI `{uri}`; expected spotify:track, episode, album, or playlist")
}
