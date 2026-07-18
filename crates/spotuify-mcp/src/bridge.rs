//! MCP tool → spotuify-protocol Request bridge.
//!
//! Translates JSON-shaped MCP tool calls into the typed Request enum the
//! daemon already understands. Pure functions, trivially testable. The
//! actual MCP transport (rmcp stdio/HTTP) is a thin wrapper around
//! these.

use serde_json::{json, Value};
use spotuify_core::{MediaKind, ProviderCatalog, ProviderId, RepeatMode, ResourceUri};

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("tool `{tool}` requires argument `{arg}`")]
    MissingArg { tool: String, arg: String },
    #[error("tool `{tool}` arg `{arg}` had wrong type")]
    BadArgType { tool: String, arg: String },
    #[error("tool `{tool}` arg `{arg}` is invalid: {message}")]
    InvalidArg {
        tool: String,
        arg: String,
        message: String,
    },
    #[error("tool `{0}` not implemented yet (gated on later phases)")]
    NotYetImplemented(String),
    #[error("tool `{0}` not in catalogue")]
    UnknownTool(String),
}

/// Pull a required string arg out of a tool-call args object.
pub fn required_str<'a>(args: &'a Value, tool: &str, key: &str) -> Result<&'a str, BridgeError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| match args.get(key) {
            None => BridgeError::MissingArg {
                tool: tool.into(),
                arg: key.into(),
            },
            Some(_) => BridgeError::BadArgType {
                tool: tool.into(),
                arg: key.into(),
            },
        })
}

pub fn optional_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

pub fn optional_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

pub fn optional_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

/// Result of a successful tool-call translation. The bridge layer wraps
/// this in MCP JSON-RPC framing; the daemon consumes the inner Request.
#[derive(Debug)]
pub enum TranslatedCall {
    /// Forward to the daemon as a typed Request.
    Request(spotuify_protocol::Request),
    /// Return local JSON without contacting the daemon.
    LocalJson(Value),
    /// Resolve plan candidates by issuing one daemon search per candidate.
    PlaylistResolveTracks {
        plan: spotuify_protocol::PlaylistPlan,
        provider: Option<ProviderId>,
    },
    /// Normalize an artist reference through the daemon before discovery.
    RelatedArtists {
        artist: String,
        provider: Option<ProviderId>,
    },
}

/// Translate `(tool_name, args)` into either a daemon Request or a local
/// read-only workflow.
pub fn translate(tool: &str, args: &Value) -> Result<TranslatedCall, BridgeError> {
    translate_with_context(tool, args, None, None)
}

/// Translate with the daemon catalog's default provider when discovery is
/// available. The plain [`translate`] entry point keeps legacy behavior for
/// callers that do not have a catalog.
pub fn translate_with_default_provider(
    tool: &str,
    args: &Value,
    default_provider: Option<&ProviderId>,
) -> Result<TranslatedCall, BridgeError> {
    translate_with_context(tool, args, default_provider, None)
}

/// Translate using the daemon's discovered provider catalog, including the
/// truthful omitted-source default for search.
pub fn translate_with_catalog(
    tool: &str,
    args: &Value,
    catalog: Option<&ProviderCatalog>,
) -> Result<TranslatedCall, BridgeError> {
    translate_with_context(
        tool,
        args,
        catalog.and_then(|catalog| catalog.default_provider.as_ref()),
        catalog,
    )
}

/// Translate a destructive playlist call into its distinct read-only daemon
/// preview command. Other destructive tools keep the local-only confirmation
/// preview used by the MCP layer.
pub fn translate_playlist_preview_with_catalog(
    tool: &str,
    args: &Value,
    catalog: Option<&ProviderCatalog>,
) -> Result<Option<spotuify_protocol::Request>, BridgeError> {
    if !matches!(tool, "playlist_create" | "playlist_add" | "playlist_remove") {
        return Ok(None);
    }
    let call = translate_with_catalog(tool, args, catalog)?;
    let TranslatedCall::Request(request) = call else {
        return Err(BridgeError::NotYetImplemented(format!(
            "{tool} daemon preview"
        )));
    };
    use spotuify_protocol::Request as R;
    let preview = match request {
        R::PlaylistCreate {
            name,
            description,
            uris,
            provider,
        }
        | R::PlaylistCreatePreview {
            name,
            description,
            uris,
            provider,
        } => R::PlaylistCreatePreview {
            name,
            description,
            uris,
            provider,
        },
        R::PlaylistAddItems {
            playlist,
            uris,
            provider,
        } => R::PlaylistItemsPreview {
            playlist,
            uris,
            action: spotuify_protocol::PlaylistItemMutationAction::Add,
            provider,
        },
        R::PlaylistRemoveItems {
            playlist,
            uris,
            provider,
        } => R::PlaylistItemsPreview {
            playlist,
            uris,
            action: spotuify_protocol::PlaylistItemMutationAction::Remove,
            provider,
        },
        request @ R::PlaylistItemsPreview { .. } => request,
        _ => {
            return Err(BridgeError::NotYetImplemented(format!(
                "{tool} daemon preview"
            )))
        }
    };
    Ok(Some(preview))
}

fn translate_with_context(
    tool: &str,
    args: &Value,
    default_provider: Option<&ProviderId>,
    catalog: Option<&ProviderCatalog>,
) -> Result<TranslatedCall, BridgeError> {
    use spotuify_protocol::PlaybackCommand;
    use spotuify_protocol::Request as R;

    match tool {
        "search" => {
            let query = required_str(args, tool, "query")?.to_string();
            let scope = parse_scope(optional_checked_str(args, tool, "kind")?, tool)?;
            let provider = parse_provider(args, tool)?;
            let omitted_source = catalog.map_or("hybrid", |catalog| {
                let selected = provider.as_ref().or(catalog.default_provider.as_ref());
                if selected
                    .and_then(|provider| {
                        catalog
                            .providers
                            .iter()
                            .find(|descriptor| &descriptor.id == provider)
                    })
                    .is_some_and(|descriptor| descriptor.capabilities.search.remote)
                {
                    "hybrid"
                } else {
                    "local"
                }
            });
            let (source, provider) = parse_source(
                optional_checked_str(args, tool, "source")?,
                provider,
                default_provider,
                omitted_source,
                tool,
            )?;
            let limit = optional_u64(args, "limit").map_or(20, |n| n.min(50) as u32);
            Ok(TranslatedCall::Request(R::Search {
                query,
                scope,
                source,
                limit,
                provider,
                kinds: None,
                sort: None,
            }))
        }
        "now_playing" => Ok(TranslatedCall::Request(R::PlaybackGet)),
        "devices_list" => Ok(TranslatedCall::Request(R::DevicesList)),
        "queue_show" => Ok(TranslatedCall::Request(R::QueueGet)),
        "playlists_list" => Ok(TranslatedCall::Request(R::PlaylistsList {
            provider: parse_provider(args, tool)?,
        })),
        "playlist_tracks" => {
            let playlist = required_str(args, tool, "playlist")?.to_string();
            Ok(TranslatedCall::Request(R::PlaylistTracks {
                playlist,
                wait: true,
                provider: parse_provider(args, tool)?,
            }))
        }
        "library_list" => {
            let limit = optional_u64(args, "limit").map_or(100, |n| n.min(500) as u32);
            Ok(TranslatedCall::Request(R::LibraryList {
                limit,
                provider: parse_provider(args, tool)?,
            }))
        }
        "playlist_plan" => {
            let brief = required_str(args, tool, "brief")?;
            let plan =
                spotuify_protocol::agent_playlists::build_playlist_plan(brief).map_err(|err| {
                    BridgeError::InvalidArg {
                        tool: tool.into(),
                        arg: "brief".into(),
                        message: err.to_string(),
                    }
                })?;
            Ok(TranslatedCall::LocalJson(json!(plan)))
        }
        "playlist_resolve_tracks" => {
            let plan = parse_playlist_plan_arg(args, tool)?;
            Ok(TranslatedCall::PlaylistResolveTracks {
                plan,
                provider: parse_provider(args, tool)?,
            })
        }
        "play" | "play_uri" => {
            // The MCP "play" tool requires a URI -- LLMs are expected to
            // call `search` first when they have a name. That keeps the
            // flow predictable and avoids LLM hallucination of URIs that
            // get treated as "best match".
            let uri = required_str(args, tool, "uri")?.to_string();
            Ok(TranslatedCall::Request(R::PlaybackCommand {
                command: PlaybackCommand::PlayUri {
                    uri,
                    context_uri: None,
                },
            }))
        }
        "pause" => Ok(TranslatedCall::Request(R::PlaybackCommand {
            command: PlaybackCommand::Pause,
        })),
        "resume" => Ok(TranslatedCall::Request(R::PlaybackCommand {
            command: PlaybackCommand::Resume,
        })),
        "next" => Ok(TranslatedCall::Request(R::PlaybackCommand {
            command: PlaybackCommand::Next,
        })),
        "previous" => Ok(TranslatedCall::Request(R::PlaybackCommand {
            command: PlaybackCommand::Previous,
        })),
        "seek" => {
            // Phase 5 — accept either `position_ms` (absolute) or
            // `offset_ms` (relative). The daemon resolves relative
            // offsets against its `PlaybackClock`.
            if let Some(offset_ms) = args.get("offset_ms").and_then(|v| v.as_i64()) {
                return Ok(TranslatedCall::Request(R::PlaybackCommand {
                    command: PlaybackCommand::SeekRelative { offset_ms },
                }));
            }
            let position_ms =
                optional_u64(args, "position_ms").ok_or_else(|| BridgeError::MissingArg {
                    tool: tool.into(),
                    arg: "position_ms".into(),
                })?;
            Ok(TranslatedCall::Request(R::PlaybackCommand {
                command: PlaybackCommand::Seek { position_ms },
            }))
        }
        "volume" => {
            let volume_percent = optional_u64(args, "percent")
                .ok_or_else(|| BridgeError::MissingArg {
                    tool: tool.into(),
                    arg: "percent".into(),
                })?
                .min(100) as u8;
            Ok(TranslatedCall::Request(R::PlaybackCommand {
                command: PlaybackCommand::Volume { volume_percent },
            }))
        }
        "shuffle" => {
            let state = optional_bool(args, "on").ok_or_else(|| BridgeError::MissingArg {
                tool: tool.into(),
                arg: "on".into(),
            })?;
            Ok(TranslatedCall::Request(R::PlaybackCommand {
                command: PlaybackCommand::Shuffle { state },
            }))
        }
        "repeat" => {
            let state = RepeatMode::parse(required_str(args, tool, "mode")?).map_err(|err| {
                BridgeError::InvalidArg {
                    tool: tool.into(),
                    arg: "mode".into(),
                    message: err.to_string(),
                }
            })?;
            Ok(TranslatedCall::Request(R::PlaybackCommand {
                command: PlaybackCommand::Repeat { state },
            }))
        }
        "queue_add" => {
            let uri = required_str(args, tool, "uri")?.to_string();
            Ok(TranslatedCall::Request(R::QueueAdd { uri }))
        }
        "transfer_device" => {
            let device = required_str(args, tool, "device")?.to_string();
            Ok(TranslatedCall::Request(R::DeviceTransfer { device }))
        }
        "playlist_create" => {
            let name = required_str(args, tool, "name")?.to_string();
            let description = optional_checked_str(args, tool, "description")?.map(str::to_string);
            let uris = optional_playlist_create_uris(args, tool)?;
            let provider = parse_provider(args, tool)?;
            if optional_checked_bool(args, tool, "dry_run")?.unwrap_or(false) {
                Ok(TranslatedCall::Request(R::PlaylistCreatePreview {
                    name,
                    description,
                    uris,
                    provider,
                }))
            } else {
                Ok(TranslatedCall::Request(R::PlaylistCreate {
                    name,
                    description,
                    uris,
                    provider,
                }))
            }
        }
        "playlist_add" => {
            let playlist = required_str(args, tool, "playlist")?.to_string();
            let uris = required_playlist_item_uris(args, tool)?;
            let provider = parse_provider(args, tool)?;
            if optional_checked_bool(args, tool, "dry_run")?.unwrap_or(false) {
                Ok(TranslatedCall::Request(R::PlaylistItemsPreview {
                    playlist,
                    uris,
                    action: spotuify_protocol::PlaylistItemMutationAction::Add,
                    provider,
                }))
            } else {
                Ok(TranslatedCall::Request(R::PlaylistAddItems {
                    playlist,
                    uris,
                    provider,
                }))
            }
        }
        "playlist_remove" => {
            let playlist = required_str(args, tool, "playlist")?.to_string();
            let uris = required_playlist_item_uris(args, tool)?;
            let provider = parse_provider(args, tool)?;
            if optional_checked_bool(args, tool, "dry_run")?.unwrap_or(false) {
                Ok(TranslatedCall::Request(R::PlaylistItemsPreview {
                    playlist,
                    uris,
                    action: spotuify_protocol::PlaylistItemMutationAction::Remove,
                    provider,
                }))
            } else {
                Ok(TranslatedCall::Request(R::PlaylistRemoveItems {
                    playlist,
                    uris,
                    provider,
                }))
            }
        }
        "playlist_unfollow" => {
            let playlist = required_str(args, tool, "playlist")?.to_string();
            Ok(TranslatedCall::Request(R::PlaylistUnfollow {
                playlist,
                provider: parse_provider(args, tool)?,
            }))
        }
        "playlist_set_image" => {
            let playlist = required_str(args, tool, "playlist")?.to_string();
            let image_base64 = required_str(args, tool, "image_base64")?.to_string();
            Ok(TranslatedCall::Request(R::PlaylistSetImage {
                playlist,
                image_base64,
                provider: parse_provider(args, tool)?,
            }))
        }
        "library_save" => {
            let uri = required_str(args, tool, "uri")?.to_string();
            // Artist saves are follows in provider terms: Spotify caps put
            // Artist in `follow_kinds`, not `save_kinds`. Route artist URIs to
            // the follow path so `library_save` of an artist follows them,
            // mirroring the CLI/TUI and the daemon/adapter routing.
            if is_artist_uri(&uri) {
                return Ok(TranslatedCall::Request(R::ArtistFollow { artist: uri }));
            }
            // The legacy LibrarySave request carries Option<String> because
            // it also supports "current track" mode. MCP always supplies
            // an explicit URI.
            Ok(TranslatedCall::Request(R::LibrarySave {
                uri: Some(uri),
                current: false,
            }))
        }
        "library_unsave" => {
            let uri = required_str(args, tool, "uri")?.to_string();
            if is_artist_uri(&uri) {
                return Ok(TranslatedCall::Request(R::ArtistUnfollow { artist: uri }));
            }
            Ok(TranslatedCall::Request(R::LibraryUnsave { uri }))
        }
        // Phase 10 — analytics tools route to typed daemon Requests.
        "related_artists" => {
            let artist = required_str(args, tool, "artist")?;
            match ResourceUri::parse(artist) {
                Ok(resource) if resource.kind() == MediaKind::Artist => {}
                Ok(resource) => {
                    return Err(BridgeError::InvalidArg {
                        tool: tool.into(),
                        arg: "artist".into(),
                        message: format!("expected artist URI, got {}", resource.kind()),
                    });
                }
                // Free-form references are normalized by the daemon's
                // provider registry; the MCP client never invents a URI.
                Err(_) => {}
            }
            Ok(TranslatedCall::RelatedArtists {
                artist: artist.to_string(),
                provider: parse_provider(args, tool)?,
            })
        }
        "radio_start" => {
            let seed_uri = required_str(args, tool, "seed_uri")?.to_string();
            let dry_run = optional_checked_bool(args, tool, "dry_run")?.unwrap_or(false);
            Ok(TranslatedCall::Request(R::RadioStart { seed_uri, dry_run }))
        }
        "analytics_top" => {
            use spotuify_protocol::{Request as R, SinceWindow, TopKind};
            let kind = match optional_str(args, "kind").unwrap_or("tracks") {
                "tracks" => TopKind::Tracks,
                "artists" => TopKind::Artists,
                "albums" => TopKind::Albums,
                "playlists" => TopKind::Playlists,
                _ => TopKind::Tracks,
            };
            let since_window = match optional_str(args, "since").unwrap_or("30d") {
                "all" => SinceWindow::All,
                raw => {
                    let n = raw
                        .strip_suffix('d')
                        .unwrap_or(raw)
                        .parse::<u32>()
                        .unwrap_or(30);
                    SinceWindow::Days(n)
                }
            };
            let limit = optional_u64(args, "limit").map_or(25, |n| n.min(100) as u32);
            Ok(TranslatedCall::Request(R::AnalyticsTop {
                kind,
                since_window,
                limit,
            }))
        }
        "analytics_habits" => {
            use spotuify_protocol::{HabitWindow, Request as R};
            let window = match optional_str(args, "window").unwrap_or("week") {
                "day" => HabitWindow::Day,
                "month" => HabitWindow::Month,
                _ => HabitWindow::Week,
            };
            Ok(TranslatedCall::Request(R::AnalyticsHabits {
                window,
                since_ms: None,
            }))
        }
        "analytics_search" => {
            use spotuify_protocol::{Request as R, SearchMode};
            let mode = match optional_str(args, "mode").unwrap_or("raw") {
                "normalized" => SearchMode::Normalized,
                _ => SearchMode::Raw,
            };
            let limit = optional_u64(args, "limit").map_or(50, |n| n.min(200) as u32);
            Ok(TranslatedCall::Request(R::AnalyticsSearch { mode, limit }))
        }
        "analytics_rediscovery" => {
            use spotuify_protocol::Request as R;
            let gap_days = optional_str(args, "gap")
                .and_then(|s| s.strip_suffix('d').unwrap_or(s).parse::<u32>().ok())
                .unwrap_or(90);
            Ok(TranslatedCall::Request(R::AnalyticsRediscovery {
                gap_days,
            }))
        }
        "analytics_import_lastfm" => {
            use spotuify_protocol::{ExportTarget, Request as R};
            Ok(TranslatedCall::Request(R::AnalyticsImport {
                target: ExportTarget::LastFm,
                username: optional_str(args, "user")
                    .or_else(|| optional_str(args, "username"))
                    .map(str::to_string),
                api_key: optional_str(args, "api_key").map(str::to_string),
                from_ms: optional_u64(args, "from_ms").map(|n| n as i64),
                to_ms: optional_u64(args, "to_ms").map(|n| n as i64),
                apply: optional_bool(args, "apply").unwrap_or(false),
            }))
        }
        "analytics_import_status" => {
            use spotuify_protocol::Request as R;
            let run_id = required_str(args, tool, "run_id")?.to_string();
            Ok(TranslatedCall::Request(R::AnalyticsImportStatus { run_id }))
        }
        "analytics_import_unresolved" => {
            use spotuify_protocol::Request as R;
            let run_id = required_str(args, tool, "run_id")?.to_string();
            Ok(TranslatedCall::Request(R::AnalyticsImportUnresolved {
                run_id,
            }))
        }
        "analytics_import_undo" => {
            use spotuify_protocol::Request as R;
            let run_id = required_str(args, tool, "run_id")?.to_string();
            Ok(TranslatedCall::Request(R::AnalyticsImportUndo {
                run_id,
                dry_run: optional_bool(args, "dry_run").unwrap_or(true),
                force: optional_bool(args, "yes")
                    .or_else(|| optional_bool(args, "force"))
                    .unwrap_or(false),
            }))
        }
        // Phase 12 — ops_log + undo_last route to typed daemon Requests.
        "ops_log" => {
            use spotuify_protocol::{OperationSource, Request as R};
            let limit = optional_u64(args, "limit").map_or(20, |n| n.min(200) as u32);
            let source = optional_str(args, "source")
                .and_then(|value| value.parse::<OperationSource>().ok());
            Ok(TranslatedCall::Request(R::OpsLog {
                limit,
                since_ms: None,
                source,
            }))
        }
        "undo_last" => {
            use spotuify_protocol::Request as R;
            Ok(TranslatedCall::Request(R::OpsUndo {
                operation_id: None,
                dry_run: false,
                force: optional_bool(args, "force").unwrap_or(false),
                bulk_since_ms: None,
            }))
        }
        "lyrics" => {
            use spotuify_protocol::Request as R;
            Ok(TranslatedCall::Request(R::LyricsGet {
                track_uri: optional_str(args, "track_uri")
                    .or_else(|| optional_str(args, "uri"))
                    .map(str::to_string),
                force_refresh: optional_bool(args, "force_refresh").unwrap_or(false),
            }))
        }
        other => Err(BridgeError::UnknownTool(other.to_string())),
    }
}

fn parse_playlist_plan_arg(
    args: &Value,
    tool: &str,
) -> Result<spotuify_protocol::PlaylistPlan, BridgeError> {
    let Some(raw) = args.get("plan") else {
        return Err(BridgeError::MissingArg {
            tool: tool.into(),
            arg: "plan".into(),
        });
    };
    if let Some(plan_json) = raw.as_str() {
        return serde_json::from_str(plan_json).map_err(|err| BridgeError::InvalidArg {
            tool: tool.into(),
            arg: "plan".into(),
            message: err.to_string(),
        });
    }
    serde_json::from_value(raw.clone()).map_err(|err| BridgeError::InvalidArg {
        tool: tool.into(),
        arg: "plan".into(),
        message: err.to_string(),
    })
}

fn optional_checked_str<'a>(
    args: &'a Value,
    tool: &str,
    key: &str,
) -> Result<Option<&'a str>, BridgeError> {
    match args.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| BridgeError::BadArgType {
                tool: tool.into(),
                arg: key.into(),
            }),
    }
}

fn optional_checked_bool(args: &Value, tool: &str, key: &str) -> Result<Option<bool>, BridgeError> {
    match args.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| BridgeError::BadArgType {
                tool: tool.into(),
                arg: key.into(),
            }),
    }
}

fn required_playlist_item_uris(args: &Value, tool: &str) -> Result<Vec<String>, BridgeError> {
    required_playlist_uris(
        args,
        tool,
        &[MediaKind::Track, MediaKind::Episode],
        "a track or episode URI",
    )
}

/// `playlist_create`'s `uris` seed is optional: absent or empty creates an
/// empty playlist. Accepted kinds match `playlist_add` (Track + Episode) so
/// create-with-episode isn't arbitrarily blocked.
fn optional_playlist_create_uris(args: &Value, tool: &str) -> Result<Vec<String>, BridgeError> {
    let Some(raw) = args.get("uris") else {
        return Ok(Vec::new());
    };
    let raw = raw.as_array().ok_or_else(|| BridgeError::BadArgType {
        tool: tool.into(),
        arg: "uris".into(),
    })?;
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    map_playlist_uris(
        raw,
        tool,
        &[MediaKind::Track, MediaKind::Episode],
        "a track or episode URI",
    )
}

fn required_playlist_uris(
    args: &Value,
    tool: &str,
    allowed_kinds: &[MediaKind],
    expected: &str,
) -> Result<Vec<String>, BridgeError> {
    let raw = args
        .get("uris")
        .ok_or_else(|| BridgeError::MissingArg {
            tool: tool.into(),
            arg: "uris".into(),
        })?
        .as_array()
        .ok_or_else(|| BridgeError::BadArgType {
            tool: tool.into(),
            arg: "uris".into(),
        })?;
    if raw.is_empty() {
        return Err(BridgeError::InvalidArg {
            tool: tool.into(),
            arg: "uris".into(),
            message: "at least one playlist item URI is required".into(),
        });
    }
    map_playlist_uris(raw, tool, allowed_kinds, expected)
}

/// True when `uri` parses as a provider artist reference.
fn is_artist_uri(uri: &str) -> bool {
    ResourceUri::parse(uri).is_ok_and(|resource| resource.kind() == MediaKind::Artist)
}

fn map_playlist_uris(
    raw: &[Value],
    tool: &str,
    allowed_kinds: &[MediaKind],
    expected: &str,
) -> Result<Vec<String>, BridgeError> {
    raw.iter()
        .enumerate()
        .map(|(index, value)| {
            let value = value.as_str().ok_or_else(|| BridgeError::BadArgType {
                tool: tool.into(),
                arg: "uris".into(),
            })?;
            let resource = ResourceUri::parse(value).map_err(|error| BridgeError::InvalidArg {
                tool: tool.into(),
                arg: "uris".into(),
                message: format!("item {} is not a resource URI: {error}", index + 1),
            })?;
            if !allowed_kinds.contains(&resource.kind()) {
                return Err(BridgeError::InvalidArg {
                    tool: tool.into(),
                    arg: "uris".into(),
                    message: format!(
                        "item {} must be {expected}, got {}",
                        index + 1,
                        resource.kind()
                    ),
                });
            }
            Ok(resource.as_uri())
        })
        .collect()
}

fn parse_scope(
    raw: Option<&str>,
    tool: &str,
) -> Result<spotuify_protocol::SearchScopeData, BridgeError> {
    use spotuify_protocol::SearchScopeData as S;
    Ok(match raw.unwrap_or("track") {
        "track" => S::Track,
        "episode" => S::Episode,
        "show" | "podcast" | "podcasts" => S::Show,
        "album" => S::Album,
        "artist" => S::Artist,
        "playlist" => S::Playlist,
        "all" => S::All,
        kind => {
            return Err(BridgeError::InvalidArg {
                tool: tool.into(),
                arg: "kind".into(),
                message: format!("unknown media kind `{kind}`"),
            });
        }
    })
}

fn parse_provider(args: &Value, tool: &str) -> Result<Option<ProviderId>, BridgeError> {
    let Some(raw) = args.get("provider") else {
        return Ok(None);
    };
    let value = raw.as_str().ok_or_else(|| BridgeError::BadArgType {
        tool: tool.into(),
        arg: "provider".into(),
    })?;
    ProviderId::new(value)
        .map(Some)
        .map_err(|error| BridgeError::InvalidArg {
            tool: tool.into(),
            arg: "provider".into(),
            message: error.to_string(),
        })
}

fn parse_source(
    raw: Option<&str>,
    provider: Option<ProviderId>,
    default_provider: Option<&ProviderId>,
    omitted_source: &str,
    tool: &str,
) -> Result<(spotuify_protocol::SearchSourceData, Option<ProviderId>), BridgeError> {
    use spotuify_protocol::SearchSourceData as S;
    match raw.unwrap_or(omitted_source) {
        "local" => Ok((S::Local, provider)),
        "hybrid" => Ok((S::Hybrid, provider)),
        // `"spotify"` is the documented pre-abstraction source value and the
        // protocol layer's legacy wire encoding for `Remote("spotify")`; keep
        // it working as an alias for `"remote"` so existing agent configs do
        // not break with -32602.
        "remote" | "spotify" => Ok((
            provider
                .clone()
                .or_else(|| default_provider.cloned())
                .map_or_else(S::legacy_default_remote, S::Remote),
            provider,
        )),
        source => Err(BridgeError::InvalidArg {
            tool: tool.into(),
            arg: "source".into(),
            message: format!("expected local, hybrid, or remote; got `{source}`"),
        }),
    }
}
