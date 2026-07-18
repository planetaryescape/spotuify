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

/// The invalidation tag for a daemon event, if it touches a resource the
/// MCP exposes. Used by the stdio transport to push
/// `notifications/resources/updated`.
pub fn event_invalidation_tag(event: &spotuify_protocol::DaemonEvent) -> Option<&'static str> {
    use spotuify_protocol::DaemonEvent as E;
    match event {
        E::PlaybackChanged { .. } | E::QueueChanged { .. } => Some("playback-changed"),
        E::DevicesChanged { .. } => Some("devices-changed"),
        E::PlaylistsChanged { .. } => Some("playlists-changed"),
        E::LibraryChanged { .. } => Some("library-changed"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spotuify_protocol::DaemonEvent;

    #[test]
    fn events_map_to_their_invalidation_tags_and_uris() {
        let playback = DaemonEvent::PlaybackChanged {
            action: "started music:track:1".into(),
            playback: None,
        };
        assert_eq!(event_invalidation_tag(&playback), Some("playback-changed"));
        assert_eq!(
            resource_uris_invalidated_by("playback-changed"),
            vec!["spotuify://playback"]
        );

        let devices = DaemonEvent::DevicesChanged {
            action: "refresh".into(),
            devices: None,
        };
        assert_eq!(event_invalidation_tag(&devices), Some("devices-changed"));

        // Events that don't touch an exposed resource map to nothing.
        let lagged = DaemonEvent::EventStreamLagged { skipped: 1 };
        assert_eq!(event_invalidation_tag(&lagged), None);
    }
}
