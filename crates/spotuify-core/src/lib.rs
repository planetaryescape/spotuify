//! Core domain types for spotuify.
//!
//! Per `docs/blueprint/01-architecture.md` §"Dependency rules", this crate has
//! **no internal dependencies**. Every other workspace member may import from
//! it; it imports from nothing in the workspace.
//!
//! These types describe the music domain — what plays, what's queued, what
//! devices exist, what playlists hold. IPC framing, HTTP semantics, storage
//! schema, and TUI rendering belong in other crates.

use serde::{Deserialize, Serialize};

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explicit: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_playable: Option<bool>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_kind_round_trips_through_json_lowercase() {
        let kinds = [
            MediaKind::Track,
            MediaKind::Episode,
            MediaKind::Album,
            MediaKind::Artist,
            MediaKind::Playlist,
        ];
        for kind in kinds {
            let encoded = serde_json::to_string(&kind).unwrap();
            let decoded: MediaKind = serde_json::from_str(&encoded).unwrap();
            assert_eq!(kind, decoded);
            assert_eq!(encoded.trim_matches('"'), kind.label());
        }
    }

    #[test]
    fn media_item_omits_optional_fields_when_none() {
        let item = MediaItem {
            id: None,
            uri: "spotify:track:abc".to_string(),
            name: "Song".to_string(),
            subtitle: String::new(),
            context: String::new(),
            duration_ms: 1000,
            image_url: None,
            kind: MediaKind::Track,
            source: None,
            freshness: None,
            explicit: None,
            is_playable: None,
        };
        let json = serde_json::to_value(&item).unwrap();
        let obj = json.as_object().unwrap();
        assert!(!obj.contains_key("source"));
        assert!(!obj.contains_key("freshness"));
        assert!(!obj.contains_key("explicit"));
        assert!(!obj.contains_key("is_playable"));
    }

    #[test]
    fn playback_default_is_paused_empty() {
        let p = Playback::default();
        assert!(p.item.is_none());
        assert!(p.device.is_none());
        assert!(!p.is_playing);
        assert_eq!(p.progress_ms, 0);
    }

    #[test]
    fn device_renames_kind_to_type_in_json() {
        let device = Device {
            id: Some("dev1".to_string()),
            name: "Phone".to_string(),
            kind: "smartphone".to_string(),
            is_active: false,
            is_restricted: false,
            volume_percent: Some(50),
            supports_volume: true,
        };
        let json = serde_json::to_value(&device).unwrap();
        assert_eq!(json.get("type").and_then(|v| v.as_str()), Some("smartphone"));
        assert!(json.get("kind").is_none());
    }
}

#[cfg(test)]
mod dev_dependencies_imports {
    // Required because serde_json is a dev-dependency of this crate but not a
    // direct dependency. The test module uses it via `serde_json::*` paths.
    #[allow(unused_imports)]
    use serde_json as _;
}
