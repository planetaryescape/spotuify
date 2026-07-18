use std::io::{IsTerminal, Read};
use std::path::Path;

use anyhow::{bail, Context, Result};
use spotuify_core::{MediaItem, MediaKind, Playlist, ResourceUri};

/// Client-side normalization of a `spotify:` URI or open.spotify.com share
/// link into a canonical [`ResourceUri`]. Returns `None` for anything that
/// isn't a recognizable Spotify target so callers fall back (text search).
///
/// This duplicates the canonical `spotuify_spotify::selection::
/// normalize_spotify_target`; the CLI cannot depend on `spotuify-spotify`
/// (see tests/workspace_boundaries.rs), yet still needs to normalize share
/// links locally when talking to a released daemon that lacks ResolveTarget.
/// Keep the two in sync.
pub fn normalize_spotify_target(arg: &str) -> Option<ResourceUri> {
    let trimmed = arg.trim();
    // `spotify:` URIs, case-insensitively — canonical URIs are lowercase, but
    // user input shouldn't silently fall through to a literal text search.
    if trimmed
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("spotify:"))
    {
        let mut parts = trimmed.split(':');
        let _scheme = parts.next()?;
        let mut kind = parts.next()?.to_ascii_lowercase();
        let mut id = parts.next()?;
        // Legacy long form: spotify:user:<username>:playlist:<id>.
        if kind == "user" {
            let _username = id;
            kind = parts.next()?.to_ascii_lowercase();
            if kind != "playlist" {
                return None;
            }
            id = parts.next()?;
        }
        if parts.next().is_some() {
            return None;
        }
        let id = id.split('?').next().unwrap_or(id);
        if id.is_empty() {
            return None;
        }
        let kind = kind.parse::<MediaKind>().ok()?;
        return ResourceUri::spotify(kind, id).ok();
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
    if segments.first() == Some(&"user") {
        if segments.len() != 4 || !segments[2].eq_ignore_ascii_case("playlist") {
            return None;
        }
        segments.drain(..2);
    }
    let [kind, id] = segments[..] else {
        return None;
    };
    if id.is_empty() {
        return None;
    }
    let kind = kind.to_ascii_lowercase().parse::<MediaKind>().ok()?;
    ResourceUri::spotify(kind, id).ok()
}

pub fn media_item_at_index(items: Vec<MediaItem>, query: &str, index: usize) -> Result<MediaItem> {
    if index == 0 {
        bail!("search index is 1-based; pass --index 1 for the first result");
    }
    items
        .into_iter()
        .nth(index - 1)
        .with_context(|| format!("no result #{index} for `{query}`"))
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
        (false, Some(_)) => bail!("provide resource reference(s) or --ids, not both"),
        (false, None) => selection_from_inputs(positional, false, false),
        (true, Some(path)) => {
            let ids = read_ids_path(path)?;
            if ids.is_empty() {
                bail!(
                    "no resource references provided by --ids {}",
                    path.display()
                );
            }
            selection_from_inputs(ids, true, path == Path::new("-"))
        }
        (true, None) => match read_piped_ids()? {
            Some(ids) if !ids.is_empty() => selection_from_inputs(ids, false, true),
            _ => bail!(missing_message.to_string()),
        },
    }
}

pub fn ensure_track_or_episode_uris(uris: &[String]) -> Result<()> {
    for uri in uris {
        let parsed = ResourceUri::parse(uri)
            .with_context(|| format!("invalid canonical resource URI `{uri}`"))?;
        if !matches!(parsed.kind(), MediaKind::Track | MediaKind::Episode) {
            bail!("playlist item mutations only accept track or episode URIs: {uri}");
        }
    }
    Ok(())
}

fn selection_from_inputs(
    inputs: Vec<String>,
    used_ids_file: bool,
    used_stdin: bool,
) -> Result<UriSelection> {
    if inputs.iter().any(|input| input.trim().is_empty()) {
        bail!("resource references cannot be empty");
    }
    Ok(UriSelection {
        uris: inputs,
        used_ids_file,
        used_stdin,
    })
}

fn read_ids_path(path: &Path) -> Result<Vec<String>> {
    let input = if path == Path::new("-") {
        let mut input = String::new();
        std::io::stdin()
            .read_to_string(&mut input)
            .context("failed to read stdin")?;
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
    stdin
        .read_to_string(&mut input)
        .context("failed to read stdin")?;
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
    let value_id = ResourceUri::parse(value)
        .ok()
        .map_or_else(|| value.to_string(), |uri| uri.bare_id().to_string());
    playlists
        .iter()
        .find(|playlist| {
            let playlist_id = ResourceUri::parse(&playlist.id)
                .ok()
                .map_or_else(|| playlist.id.clone(), |uri| uri.bare_id().to_string());
            playlist.id == value
                || value_id == playlist_id
                || playlist.name.eq_ignore_ascii_case(value)
        })
        .cloned()
        .with_context(|| format!("no playlist matching `{value}`"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekInput {
    Absolute(u64),
    Relative(i64),
}

pub fn parse_seek_input(input: &str) -> Result<SeekInput> {
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
        Ok(SeekInput::Absolute(duration_ms))
    } else {
        Ok(SeekInput::Relative(sign.saturating_mul(duration_ms as i64)))
    }
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
        .with_context(|| format!("invalid seek duration `{value}`; try +15s or -30s"))?;
    Ok(amount.saturating_mul(multiplier))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic, clippy::unwrap_used)]

    use super::*;

    #[test]
    fn accepts_provider_neutral_inputs_for_daemon_resolution() {
        let selection = resolve_uri_selection(
            vec!["music:track:1".to_string(), "shared-link".to_string()],
            None,
            "missing",
        )
        .unwrap();
        assert_eq!(selection.uris.len(), 2);
        assert!(selection.requires_confirmation());
    }

    #[test]
    fn seek_parser_is_provider_independent() {
        assert_eq!(
            parse_seek_input("+15s").unwrap(),
            SeekInput::Relative(15_000)
        );
        assert_eq!(
            parse_seek_input("2m").unwrap(),
            SeekInput::Absolute(120_000)
        );
    }

    #[test]
    fn playlist_resolution_matches_canonical_uri_to_legacy_bare_id() {
        let playlist = Playlist {
            id: "abc".to_string(),
            name: "Favorites".to_string(),
            owner: String::new(),
            tracks_total: 0,
            image_url: None,
            version_token: None,
        };
        assert_eq!(
            resolve_playlist(&[playlist], "music:playlist:abc")
                .unwrap()
                .id,
            "abc"
        );

        let playlist = Playlist {
            id: "music:playlist:abc".to_string(),
            name: "Favorites".to_string(),
            owner: String::new(),
            tracks_total: 0,
            image_url: None,
            version_token: None,
        };
        assert_eq!(
            resolve_playlist(&[playlist], "abc").unwrap().id,
            "music:playlist:abc"
        );
    }
}
