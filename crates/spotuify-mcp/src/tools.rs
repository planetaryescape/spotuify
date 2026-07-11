//! MCP tool catalogue.
//!
//! Each daemon Request that's safe to expose to an LLM appears here as
//! a [`Tool`]. The catalogue is the source of truth for the manifest
//! served to MCP clients via `tools/list`.
//!
//! Tools are classified by [`ToolKind`]:
//! - `Read`: query-only, never mutates Spotify state. Always allowed.
//! - `Transport`: playback control (play/pause/seek/etc). User-visible
//!   but trivially reversible. Allowed without confirmation.
//! - `Destructive`: mutates persistent state (playlist add/remove,
//!   library save/unsave, playlist create, device transfer). Requires
//!   explicit `confirm: true` in the args.
//! - `Mercury`: librespot mercury/local provider surfaces such as
//!   lyrics. Only implemented tools appear in the live manifest.
//! - `Analytics`: Phase 10 derivations (top, habits, rediscovery).
//! - `Ops`: Phase 12 operation log + undo.

use serde::{Deserialize, Serialize};

/// One MCP tool entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub kind: ToolKind,
    pub destructive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Read,
    Transport,
    Destructive,
    Mercury,
    Analytics,
    Ops,
}

/// The full catalogue of tools spotuify-mcp exposes.
///
/// This is a static const so the manifest is identical across runs --
/// the manifest golden test (`mcp_manifest_matches_snapshot`) locks it
/// down so adding/removing/renaming a tool is always a code-review event.
pub const TOOLS: &[Tool] = &[
    // Read-only
    Tool {
        name: "search",
        description: "Search Spotify (tracks/albums/artists/playlists/episodes). Hybrid local+remote by default.",
        kind: ToolKind::Read,
        destructive: false,
    },
    Tool {
        name: "now_playing",
        description: "Return the current playback state (track, device, progress, lyrics if available).",
        kind: ToolKind::Read,
        destructive: false,
    },
    Tool {
        name: "devices_list",
        description: "List currently visible Spotify Connect devices.",
        kind: ToolKind::Read,
        destructive: false,
    },
    Tool {
        name: "queue_show",
        description: "Return the current playback queue.",
        kind: ToolKind::Read,
        destructive: false,
    },
    Tool {
        name: "playlists_list",
        description: "List the user's playlists.",
        kind: ToolKind::Read,
        destructive: false,
    },
    Tool {
        name: "playlist_tracks",
        description: "List the tracks in a specific playlist.",
        kind: ToolKind::Read,
        destructive: false,
    },
    Tool {
        name: "library_list",
        description: "List saved (liked) tracks from the user's library.",
        kind: ToolKind::Read,
        destructive: false,
    },
    Tool {
        name: "playlist_plan",
        description: "Build a deterministic playlist-plan scaffold from a user brief.",
        kind: ToolKind::Read,
        destructive: false,
    },
    Tool {
        name: "playlist_resolve_tracks",
        description: "Resolve a playlist plan's candidate searches into track candidates via daemon search.",
        kind: ToolKind::Read,
        destructive: false,
    },
    // Transport -- reversible, no confirm needed
    Tool {
        name: "play",
        description: "Start playback of an exact Spotify URI returned by search.",
        kind: ToolKind::Transport,
        destructive: false,
    },
    Tool {
        name: "play_uri",
        description: "Start playback of an exact Spotify URI.",
        kind: ToolKind::Transport,
        destructive: false,
    },
    Tool {
        name: "pause",
        description: "Pause playback.",
        kind: ToolKind::Transport,
        destructive: false,
    },
    Tool {
        name: "resume",
        description: "Resume playback.",
        kind: ToolKind::Transport,
        destructive: false,
    },
    Tool {
        name: "next",
        description: "Skip to the next track.",
        kind: ToolKind::Transport,
        destructive: false,
    },
    Tool {
        name: "previous",
        description: "Skip to the previous track.",
        kind: ToolKind::Transport,
        destructive: false,
    },
    Tool {
        name: "seek",
        description: "Seek to a position in the current track (milliseconds, or +/- delta).",
        kind: ToolKind::Transport,
        destructive: false,
    },
    Tool {
        name: "volume",
        description: "Set the volume percent (0-100).",
        kind: ToolKind::Transport,
        destructive: false,
    },
    Tool {
        name: "shuffle",
        description: "Toggle shuffle on/off.",
        kind: ToolKind::Transport,
        destructive: false,
    },
    Tool {
        name: "repeat",
        description: "Set repeat mode (off/track/context).",
        kind: ToolKind::Transport,
        destructive: false,
    },
    // Destructive -- require confirm: true
    Tool {
        name: "queue_add",
        description: "Queue a URI for the current playback. Reversible via `undo_last`.",
        kind: ToolKind::Destructive,
        destructive: true,
    },
    Tool {
        name: "transfer_device",
        description: "Transfer active playback to another Spotify Connect device.",
        kind: ToolKind::Destructive,
        destructive: true,
    },
    Tool {
        name: "playlist_create",
        description: "Create a new playlist. Without confirm:true returns a preview.",
        kind: ToolKind::Destructive,
        destructive: true,
    },
    Tool {
        name: "playlist_add",
        description: "Add tracks to an existing playlist. Without confirm:true returns a preview.",
        kind: ToolKind::Destructive,
        destructive: true,
    },
    Tool {
        name: "playlist_remove",
        description: "Remove tracks from an existing playlist. Without confirm:true returns a preview.",
        kind: ToolKind::Destructive,
        destructive: true,
    },
    Tool {
        name: "playlist_unfollow",
        description: "Unfollow (effectively delete) a playlist the user owns. Not reversible. Without confirm:true returns a preview.",
        kind: ToolKind::Destructive,
        destructive: true,
    },
    Tool {
        name: "playlist_set_image",
        description: "Replace a playlist's cover art with a base64-encoded JPEG (256 KB max).",
        kind: ToolKind::Destructive,
        destructive: true,
    },
    Tool {
        name: "library_save",
        description: "Save a track/album to the user's library.",
        kind: ToolKind::Destructive,
        destructive: true,
    },
    Tool {
        name: "library_unsave",
        description: "Remove a track/album from the user's library.",
        kind: ToolKind::Destructive,
        destructive: true,
    },
    // Mercury / provider-backed read tools.
    Tool {
        name: "lyrics",
        description: "Get synced lyrics for the current or specified track, using cached Spotify mercury/LRCLIB data when available.",
        kind: ToolKind::Mercury,
        destructive: false,
    },
    // Discovery (Mercury-backed)
    Tool {
        name: "related_artists",
        description: "Artists related to a given artist (Mercury-backed; needs the daemon's librespot session).",
        kind: ToolKind::Read,
        destructive: false,
    },
    Tool {
        name: "radio_start",
        description: "Resolve a radio station seeded by any Spotify URI; queues it onto the active device unless dry_run is set.",
        kind: ToolKind::Transport,
        destructive: false,
    },
    // Analytics (Phase 10)
    Tool {
        name: "analytics_top",
        description: "Top tracks/artists/albums by listening time (local data, no API call).",
        kind: ToolKind::Analytics,
        destructive: false,
    },
    Tool {
        name: "analytics_habits",
        description: "Listening habits by day/week/month (local data).",
        kind: ToolKind::Analytics,
        destructive: false,
    },
    Tool {
        name: "analytics_search",
        description: "Search history with normalized/raw query mode (local data).",
        kind: ToolKind::Analytics,
        destructive: false,
    },
    Tool {
        name: "analytics_rediscovery",
        description: "Tracks worth re-discovering — qualified listens older than the gap window (local data).",
        kind: ToolKind::Analytics,
        destructive: false,
    },
    Tool {
        name: "analytics_import_lastfm",
        description: "Preview/apply Last.fm historical scrobble import. Dry-run unless apply=true.",
        kind: ToolKind::Destructive,
        destructive: true,
    },
    Tool {
        name: "analytics_import_status",
        description: "Show status for an analytics import run.",
        kind: ToolKind::Analytics,
        destructive: false,
    },
    Tool {
        name: "analytics_import_unresolved",
        description: "List unresolved scrobbles for an analytics import run.",
        kind: ToolKind::Analytics,
        destructive: false,
    },
    Tool {
        name: "analytics_import_undo",
        description: "Undo promoted analytics facts for an import run while preserving raw scrobble audit rows.",
        kind: ToolKind::Destructive,
        destructive: true,
    },
    // Ops (Phase 12) -- undo bypasses confirm because it IS the safety net
    Tool {
        name: "ops_log",
        description: "Show the recent operation log (mutations with reversal plans).",
        kind: ToolKind::Ops,
        destructive: false,
    },
    Tool {
        name: "undo_last",
        description: "Reverse the most recent destructive operation. No confirm required -- this is the safety net.",
        kind: ToolKind::Ops,
        destructive: false,
    },
];

/// Read-only view of the catalogue.
pub struct ToolCatalogue;

impl ToolCatalogue {
    pub fn all() -> &'static [Tool] {
        TOOLS
    }

    pub fn by_name(name: &str) -> Option<&'static Tool> {
        TOOLS.iter().find(|t| t.name == name)
    }

    pub fn destructive() -> impl Iterator<Item = &'static Tool> {
        TOOLS.iter().filter(|t| t.destructive)
    }

    pub fn by_kind(kind: ToolKind) -> impl Iterator<Item = &'static Tool> {
        TOOLS.iter().filter(move |t| t.kind == kind)
    }
}

/// Snapshot manifest for MCP's `tools/list` reply and for the golden test.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolManifest {
    pub spec_version: &'static str,
    pub server_name: &'static str,
    pub tools: Vec<Tool>,
}

impl ToolManifest {
    pub fn build() -> Self {
        Self {
            spec_version: "2024-11-05",
            server_name: "spotuify-mcp",
            tools: TOOLS.to_vec(),
        }
    }
}
