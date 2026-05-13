//! MCP resource catalogue.
//!
//! MCP resources are URIs the client can `resources/list`,
//! `resources/read`, or `resources/subscribe` to. spotuify exposes
//! live application state as subscribable resources backed by
//! [`spotuify_protocol::DaemonEvent`] notifications.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resource {
    pub uri: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub mime_type: &'static str,
    pub subscribable: bool,
}

pub const RESOURCES: &[Resource] = &[
    Resource {
        uri: "spotuify://playback",
        name: "Current playback",
        description: "Track, device, progress, and play state. Refreshes on DaemonEvent::PlaybackChanged.",
        mime_type: "application/json",
        subscribable: true,
    },
    Resource {
        uri: "spotuify://devices",
        name: "Available devices",
        description: "Spotify Connect devices currently visible. Refreshes on DaemonEvent::DevicesChanged.",
        mime_type: "application/json",
        subscribable: true,
    },
    Resource {
        uri: "spotuify://playlists",
        name: "User playlists",
        description: "All playlists the user owns or follows. Refreshes on DaemonEvent::PlaylistsChanged.",
        mime_type: "application/json",
        subscribable: true,
    },
    Resource {
        uri: "spotuify://now_playing/lyrics",
        name: "Lyrics stream",
        description: "Synced lyrics for the current track. Requires Phase 9 (embedded librespot backend). Phase 16 wires the LRCLIB fallback.",
        mime_type: "application/json",
        subscribable: true,
    },
    Resource {
        uri: "spotuify://doctor",
        name: "Doctor report",
        description: "Latest health-check report. Refreshes when the daemon recomputes it.",
        mime_type: "application/json",
        subscribable: false,
    },
];

pub struct ResourceCatalogue;

impl ResourceCatalogue {
    pub fn all() -> &'static [Resource] {
        RESOURCES
    }
    pub fn by_uri(uri: &str) -> Option<&'static Resource> {
        RESOURCES.iter().find(|r| r.uri == uri)
    }
}

/// Mapping from a [`spotuify_protocol::DaemonEvent`] tag to the resource
/// URI it invalidates. Used by the rmcp shim to translate the daemon
/// event stream into MCP `notifications/resources/updated` messages.
pub fn resource_uris_invalidated_by(event_tag: &str) -> Vec<&'static str> {
    match event_tag {
        "playback-changed" => vec!["spotuify://playback"],
        "devices-changed" => vec!["spotuify://devices"],
        "playlists-changed" => vec!["spotuify://playlists"],
        "library-changed" => vec!["spotuify://playlists"],
        _ => vec![],
    }
}
