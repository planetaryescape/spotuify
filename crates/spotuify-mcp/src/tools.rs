//! MCP tool catalogue.
//!
//! Each daemon Request that's safe to expose to an LLM appears here as
//! a [`Tool`]. The catalogue is the source of truth for the manifest
//! served to MCP clients via `tools/list`.
//!
//! Tools are classified by [`ToolKind`]:
//! - `Read`: query-only, never mutates provider state.
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
use serde_json::Value;
use spotuify_core::{
    MediaKind, ProviderCaps, ProviderCatalog, ProviderDescriptor, ProviderId, ResourceUri,
};

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
        description: "Search the configured music provider (tracks/albums/artists/playlists/episodes). Hybrid local+remote by default.",
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
        description: "Start playback of an exact provider URI returned by search.",
        kind: ToolKind::Transport,
        destructive: false,
    },
    Tool {
        name: "play_uri",
        description: "Start playback of an exact provider URI.",
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
        description: "Get synced lyrics for the current or specified track, using provider or LRCLIB data when available.",
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
        description: "Resolve a radio station seeded by a provider URI; queues it onto the active device unless dry_run is set.",
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

    /// Tools visible for the currently registered providers.
    ///
    /// `None` is the additive-compatibility state: an older daemon did not
    /// expose a catalog, so the legacy full catalogue remains visible. A
    /// present-but-empty catalog is authoritative and hides provider-backed
    /// tools.
    pub fn available(
        catalog: Option<&ProviderCatalog>,
    ) -> impl Iterator<Item = &'static Tool> + '_ {
        TOOLS
            .iter()
            .filter(move |tool| tool_visible(tool.name, catalog))
    }
}

#[derive(Clone, Copy)]
enum ToolRequirement {
    Local,
    Provider(fn(&ProviderCaps) -> bool),
}

fn requirement(tool: &str) -> ToolRequirement {
    use ToolRequirement::{Local, Provider};

    match tool {
        "search" => Provider(|caps| caps.search.remote),
        "now_playing" => Provider(|caps| {
            caps.transport
                .as_ref()
                .is_some_and(|transport| transport.playback_state)
        }),
        "devices_list" => Provider(|caps| {
            caps.transport
                .as_ref()
                .is_some_and(|transport| transport.devices)
        }),
        "queue_show" => Provider(|caps| {
            caps.transport
                .as_ref()
                .is_some_and(|transport| transport.queue_read)
        }),
        "playlists_list" => Provider(|caps| caps.playlists.list),
        "playlist_tracks" => Provider(|caps| caps.playlists.item_read),
        "library_list" => Provider(|caps| !caps.library.read_kinds.is_empty()),
        "playlist_resolve_tracks" => {
            Provider(|caps| caps.search.remote && caps.search.kinds.contains(&MediaKind::Track))
        }
        "play" | "play_uri" => Provider(|caps| {
            caps.transport
                .as_ref()
                .is_some_and(|transport| transport.play)
        }),
        "radio_start" => Provider(|caps| caps.extras.radio),
        "pause" => Provider(|caps| {
            caps.transport
                .as_ref()
                .is_some_and(|transport| transport.pause)
        }),
        "resume" => Provider(|caps| {
            caps.transport
                .as_ref()
                .is_some_and(|transport| transport.resume)
        }),
        "next" => Provider(|caps| {
            caps.transport
                .as_ref()
                .is_some_and(|transport| transport.next)
        }),
        "previous" => Provider(|caps| {
            caps.transport
                .as_ref()
                .is_some_and(|transport| transport.previous)
        }),
        "seek" => Provider(|caps| {
            caps.transport
                .as_ref()
                .is_some_and(|transport| transport.seek)
        }),
        "volume" => Provider(|caps| {
            caps.transport
                .as_ref()
                .is_some_and(|transport| transport.volume)
        }),
        "shuffle" => Provider(|caps| {
            caps.transport
                .as_ref()
                .is_some_and(|transport| transport.shuffle)
        }),
        "repeat" => Provider(|caps| {
            caps.transport
                .as_ref()
                .is_some_and(|transport| transport.repeat)
        }),
        "queue_add" => Provider(|caps| {
            caps.transport
                .as_ref()
                .is_some_and(|transport| transport.queue_add)
        }),
        "transfer_device" => Provider(|caps| {
            caps.transport
                .as_ref()
                .is_some_and(|transport| transport.transfer)
        }),
        "playlist_create" => Provider(|caps| caps.playlists.create),
        "playlist_add" => Provider(|caps| caps.playlists.add),
        "playlist_remove" => Provider(|caps| caps.playlists.remove),
        "playlist_unfollow" => Provider(|caps| caps.playlists.unfollow),
        "playlist_set_image" => Provider(|caps| caps.playlists.image),
        // Artist likes route to follow, so the tool stays available when the
        // provider can save OR follow (Spotify puts Artist in follow_kinds).
        "library_save" | "library_unsave" => Provider(|caps| {
            !caps.library.save_kinds.is_empty() || !caps.library.follow_kinds.is_empty()
        }),
        "related_artists" => Provider(|caps| caps.extras.related_artists),
        // Lyrics has an LRCLIB fallback that needs no provider, so it is always
        // available even when the provider catalog is explicitly empty.
        "lyrics" => Local,
        _ => Local,
    }
}

/// Whether a tool should appear in `tools/list`.
pub fn tool_visible(tool: &str, catalog: Option<&ProviderCatalog>) -> bool {
    // Search always has a local-cache mode, even with no remote providers.
    if tool == "search" {
        return true;
    }
    let Some(catalog) = catalog else {
        return true;
    };
    match requirement(tool) {
        ToolRequirement::Local => true,
        ToolRequirement::Provider(supported)
            if provider_scoped(tool) || resource_arg_name(tool).is_some() =>
        {
            catalog
                .providers
                .iter()
                .any(|provider| supported(&provider.capabilities))
        }
        ToolRequirement::Provider(supported) if active_transport_routed(tool) => catalog
            .providers
            .iter()
            .any(|provider| supported(&provider.capabilities)),
        ToolRequirement::Provider(supported) => descriptor_for_provider(catalog, None)
            .is_some_and(|provider| supported(&provider.capabilities)),
    }
}

/// Enforce the same capability predicate used by `tools/list` at call time.
///
/// Provider-scoped calls may select a registered provider via their optional
/// `provider` argument. Resource calls use the URI owner. Current-transport
/// calls can be rejected only when no registered provider supports them,
/// because the daemon's active provider is transient and authoritative.
/// Unknown catalogs retain old-daemon behavior.
pub fn ensure_tool_available(
    tool: &str,
    args: &Value,
    catalog: Option<&ProviderCatalog>,
) -> Result<(), String> {
    let Some(catalog) = catalog else {
        if args.get("provider").is_some() {
            return Err(
                "explicit provider selection requires a daemon provider catalog; upgrade the daemon or omit `provider`"
                    .to_string(),
            );
        }
        return Ok(());
    };
    if tool == "search" && search_uses_local_source(args, catalog) {
        // A provider scope still has to name a registered cache partition,
        // but an unscoped local search works with an explicitly empty
        // provider registry.
        provider_for_args(args, Some(catalog))?;
        return Ok(());
    }
    let ToolRequirement::Provider(supported) = requirement(tool) else {
        return Ok(());
    };

    let explicit_provider = provider_scoped(tool)
        .then(|| provider_for_args(args, Some(catalog)))
        .transpose()?
        .flatten();
    let resource_uri = resource_arg(tool, args).and_then(|value| ResourceUri::parse(value).ok());
    if explicit_provider.is_none() && resource_uri.is_none() && active_transport_routed(tool) {
        return catalog
            .providers
            .iter()
            .any(|provider| supported(&provider.capabilities))
            .then_some(())
            .ok_or_else(|| format!("no configured provider supports MCP tool `{tool}`"));
    }
    let descriptor = if let Some(provider) = explicit_provider.as_ref() {
        let descriptor = catalog
            .providers
            .iter()
            .find(|descriptor| &descriptor.id == provider);
        if let (Some(descriptor), Some(uri)) = (descriptor, resource_uri.as_ref()) {
            if &descriptor.uri_scheme != uri.scheme() {
                return Err(format!(
                    "provider `{provider}` conflicts with URI scheme `{}`",
                    uri.scheme()
                ));
            }
        }
        descriptor
    } else if let Some(uri) = resource_uri.as_ref() {
        Some(
            catalog
                .providers
                .iter()
                .find(|descriptor| &descriptor.uri_scheme == uri.scheme())
                .ok_or_else(|| format!("unknown provider URI scheme `{}`", uri.scheme()))?,
        )
    } else {
        descriptor_for_provider(catalog, None)
    };

    match descriptor {
        Some(descriptor) if supported(&descriptor.capabilities) => {
            ensure_argument_capability(tool, args, descriptor)
        }
        Some(descriptor) => Err(format!(
            "provider `{}` does not support MCP tool `{tool}`",
            descriptor.id
        )),
        None => Err(format!(
            "MCP tool `{tool}` is unavailable because no default provider is configured"
        )),
    }
}

/// Reject malformed tool arguments without consulting daemon state.
///
/// Call this before provider-catalog discovery so client input errors cannot
/// be masked by a daemon connection failure.
pub fn validate_tool_arguments(tool: &str, args: &Value) -> Result<(), String> {
    if matches!(
        tool,
        "playlist_create" | "playlist_add" | "playlist_remove" | "radio_start"
    ) && args.get("dry_run").is_some_and(|value| !value.is_boolean())
    {
        return Err(format!(
            "MCP tool `{tool}` argument `dry_run` must be boolean"
        ));
    }
    Ok(())
}

/// Whether a concrete call needs provider discovery before it can be gated.
pub fn tool_needs_provider_catalog(tool: &str, args: &Value) -> bool {
    if tool == "search" && args.get("source").and_then(Value::as_str) == Some("local") {
        return args.get("provider").is_some();
    }
    matches!(requirement(tool), ToolRequirement::Provider(_))
}

/// Static source default annotation when every selectable provider route has
/// the same omission behavior. Mixed catalogs intentionally return `None`:
/// the concrete `provider` argument determines the real default at call time.
pub fn search_default_source(catalog: Option<&ProviderCatalog>) -> Option<&'static str> {
    let Some(catalog) = catalog else {
        return Some("hybrid");
    };
    let default_remote = provider_supports_remote_search(catalog, None);
    let mut has_remote = default_remote;
    let mut has_local = !default_remote;
    for descriptor in &catalog.providers {
        if descriptor.capabilities.search.remote {
            has_remote = true;
        } else {
            has_local = true;
        }
    }
    match (has_local, has_remote) {
        (true, false) => Some("local"),
        (false, true) => Some("hybrid"),
        _ => None,
    }
}

fn search_uses_local_source(args: &Value, catalog: &ProviderCatalog) -> bool {
    match args.get("source").and_then(Value::as_str) {
        Some("local") => true,
        Some(_) => false,
        None => {
            let provider = args
                .get("provider")
                .and_then(Value::as_str)
                .and_then(|provider| ProviderId::new(provider).ok());
            !provider_supports_remote_search(catalog, provider.as_ref())
        }
    }
}

fn provider_supports_remote_search(
    catalog: &ProviderCatalog,
    provider: Option<&ProviderId>,
) -> bool {
    descriptor_for_provider(catalog, provider)
        .is_some_and(|descriptor| descriptor.capabilities.search.remote)
}

fn provider_scoped(tool: &str) -> bool {
    matches!(
        tool,
        "search"
            | "playlists_list"
            | "library_list"
            | "playlist_resolve_tracks"
            | "playlist_create"
            | "playlist_tracks"
            | "playlist_add"
            | "playlist_remove"
            | "playlist_unfollow"
            | "playlist_set_image"
            | "related_artists"
    )
}

/// Calls whose daemon route follows the active transport provider rather than
/// the catalog default. The catalog does not expose that transient identity,
/// so clients may reject only when no configured provider supports the call;
/// the daemon remains authoritative for the provider active at dispatch time.
fn active_transport_routed(tool: &str) -> bool {
    matches!(
        tool,
        "now_playing"
            | "devices_list"
            | "queue_show"
            | "pause"
            | "resume"
            | "next"
            | "previous"
            | "seek"
            | "volume"
            | "shuffle"
            | "repeat"
            | "transfer_device"
    )
}

fn resource_arg_name(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "play" | "play_uri" | "queue_add" | "library_save" | "library_unsave" => "uri",
        "playlist_tracks" | "playlist_add" | "playlist_remove" | "playlist_unfollow"
        | "playlist_set_image" => "playlist",
        "lyrics" => "track_uri",
        "related_artists" => "artist",
        "radio_start" => "seed_uri",
        _ => return None,
    })
}

fn resource_arg<'a>(tool: &str, args: &'a Value) -> Option<&'a str> {
    args.get(resource_arg_name(tool)?).and_then(Value::as_str)
}

fn ensure_argument_capability(
    tool: &str,
    args: &Value,
    descriptor: &ProviderDescriptor,
) -> Result<(), String> {
    let radio_dry_run = if tool == "radio_start" {
        Some(
            args.get("dry_run")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        )
    } else {
        None
    };
    if radio_dry_run == Some(false) {
        let transport = descriptor.capabilities.transport.as_ref();
        if !transport.is_some_and(|transport| transport.queue_add) {
            return Err(format!(
                "provider `{}` does not support radio queue additions",
                descriptor.id
            ));
        }
    }
    if tool == "search" {
        let supported = match args.get("kind") {
            None => descriptor
                .capabilities
                .search
                .kinds
                .contains(&MediaKind::Track),
            Some(value) => match value
                .as_str()
                .ok_or_else(|| "search kind must be a string".to_string())?
            {
                "track" => descriptor
                    .capabilities
                    .search
                    .kinds
                    .contains(&MediaKind::Track),
                "episode" => descriptor
                    .capabilities
                    .search
                    .kinds
                    .contains(&MediaKind::Episode),
                "show" | "podcast" | "podcasts" => descriptor
                    .capabilities
                    .search
                    .kinds
                    .contains(&MediaKind::Show),
                "album" => descriptor
                    .capabilities
                    .search
                    .kinds
                    .contains(&MediaKind::Album),
                "artist" => descriptor
                    .capabilities
                    .search
                    .kinds
                    .contains(&MediaKind::Artist),
                "playlist" => descriptor
                    .capabilities
                    .search
                    .kinds
                    .contains(&MediaKind::Playlist),
                // `all` means the provider's non-empty advertised
                // intersection. The daemon owns the canonical pruning.
                "all" => !descriptor.capabilities.search.kinds.is_empty(),
                kind => return Err(format!("unknown search media kind `{kind}`")),
            },
        };
        if !supported {
            return Err(format!(
                "provider `{}` does not support searching the requested media kind",
                descriptor.id
            ));
        }
    }
    if matches!(tool, "library_save" | "library_unsave") {
        if let Some(uri) = resource_arg(tool, args).and_then(|value| ResourceUri::parse(value).ok())
        {
            // Artist likes route to follow/unfollow, so accept the item when
            // the provider can save OR follow that kind.
            let kind = uri.kind();
            let library = &descriptor.capabilities.library;
            if !(library.can_save(&kind) || library.can_follow(&kind)) {
                return Err(format!(
                    "provider `{}` does not support saving `{kind}` items",
                    descriptor.id
                ));
            }
        }
    }
    Ok(())
}

pub fn provider_for_args(
    args: &Value,
    catalog: Option<&ProviderCatalog>,
) -> Result<Option<ProviderId>, String> {
    let explicit = match args.get("provider") {
        None => None,
        Some(value) => Some(
            ProviderId::new(
                value
                    .as_str()
                    .ok_or_else(|| "provider must be a string".to_string())?,
            )
            .map_err(|error| error.to_string())?,
        ),
    };
    if let (Some(provider), Some(catalog)) = (&explicit, catalog) {
        if !catalog
            .providers
            .iter()
            .any(|descriptor| &descriptor.id == provider)
        {
            return Err(format!("unknown provider `{provider}`"));
        }
    }
    Ok(explicit)
}

pub fn descriptor_for_provider<'a>(
    catalog: &'a ProviderCatalog,
    provider: Option<&ProviderId>,
) -> Option<&'a ProviderDescriptor> {
    let provider = provider.or(catalog.default_provider.as_ref())?;
    catalog
        .providers
        .iter()
        .find(|descriptor| &descriptor.id == provider)
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
