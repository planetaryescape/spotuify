//! MCP tool → spotuify-protocol Request bridge.
//!
//! Translates JSON-shaped MCP tool calls into the typed Request enum the
//! daemon already understands. Pure functions, trivially testable. The
//! actual MCP transport (rmcp stdio/HTTP) is a thin wrapper around
//! these.

use serde_json::{json, Value};

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
    },
}

/// Translate `(tool_name, args)` into either a daemon Request or a local
/// read-only workflow.
pub fn translate(tool: &str, args: &Value) -> Result<TranslatedCall, BridgeError> {
    use spotuify_protocol::PlaybackCommand;
    use spotuify_protocol::Request as R;

    match tool {
        "search" => {
            let query = required_str(args, tool, "query")?.to_string();
            let scope = parse_scope(optional_str(args, "kind"));
            let source = parse_source(optional_str(args, "source"));
            let limit = optional_u64(args, "limit").map_or(20, |n| n.min(50) as u32);
            Ok(TranslatedCall::Request(R::Search {
                query,
                scope,
                source,
                limit,
                kinds: None,
                sort: None,
            }))
        }
        "now_playing" => Ok(TranslatedCall::Request(R::PlaybackGet)),
        "devices_list" => Ok(TranslatedCall::Request(R::DevicesList)),
        "queue_show" => Ok(TranslatedCall::Request(R::QueueGet)),
        "playlists_list" => Ok(TranslatedCall::Request(R::PlaylistsList)),
        "playlist_tracks" => {
            let playlist = required_str(args, tool, "playlist")?.to_string();
            Ok(TranslatedCall::Request(R::PlaylistTracks {
                playlist,
                wait: true,
            }))
        }
        "library_list" => {
            let limit = optional_u64(args, "limit").map_or(100, |n| n.min(500) as u32);
            Ok(TranslatedCall::Request(R::LibraryList { limit }))
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
            Ok(TranslatedCall::PlaylistResolveTracks { plan })
        }
        "play" | "play_uri" => {
            // The MCP "play" tool requires a URI -- LLMs are expected to
            // call `search` first when they have a name. That keeps the
            // flow predictable and avoids LLM hallucination of URIs that
            // get treated as "best match".
            let uri = required_str(args, tool, "uri")?.to_string();
            Ok(TranslatedCall::Request(R::PlaybackCommand {
                command: PlaybackCommand::PlayUri { uri },
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
            let state = required_str(args, tool, "mode")?.to_string();
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
            let description = optional_str(args, "description").map(str::to_string);
            let uris = args
                .get("uris")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Ok(TranslatedCall::Request(R::PlaylistCreate {
                name,
                description,
                uris,
            }))
        }
        "playlist_add" => {
            let playlist = required_str(args, tool, "playlist")?.to_string();
            let uris = args
                .get("uris")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .ok_or_else(|| BridgeError::MissingArg {
                    tool: tool.into(),
                    arg: "uris".into(),
                })?;
            Ok(TranslatedCall::Request(R::PlaylistAddItems {
                playlist,
                uris,
            }))
        }
        "playlist_remove" => {
            let playlist = required_str(args, tool, "playlist")?.to_string();
            let uris = args
                .get("uris")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .ok_or_else(|| BridgeError::MissingArg {
                    tool: tool.into(),
                    arg: "uris".into(),
                })?;
            Ok(TranslatedCall::Request(R::PlaylistRemoveItems {
                playlist,
                uris,
            }))
        }
        "playlist_unfollow" => {
            let playlist = required_str(args, tool, "playlist")?.to_string();
            Ok(TranslatedCall::Request(R::PlaylistUnfollow { playlist }))
        }
        "playlist_set_image" => {
            let playlist = required_str(args, tool, "playlist")?.to_string();
            let image_base64 = required_str(args, tool, "image_base64")?.to_string();
            Ok(TranslatedCall::Request(R::PlaylistSetImage {
                playlist,
                image_base64,
            }))
        }
        "library_save" => {
            let uri = required_str(args, tool, "uri")?.to_string();
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
            Ok(TranslatedCall::Request(R::LibraryUnsave { uri }))
        }
        // Phase 10 — analytics tools route to typed daemon Requests.
        "related_artists" => {
            let artist = required_str(args, tool, "artist")?;
            let artist = if artist.starts_with("spotify:") {
                artist.to_string()
            } else {
                format!("spotify:artist:{artist}")
            };
            Ok(TranslatedCall::Request(R::RelatedArtists { artist }))
        }
        "radio_start" => {
            let seed_uri = required_str(args, tool, "seed_uri")?.to_string();
            let dry_run = optional_bool(args, "dry_run").unwrap_or(false);
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

fn parse_scope(raw: Option<&str>) -> spotuify_protocol::SearchScopeData {
    use spotuify_protocol::SearchScopeData as S;
    match raw.unwrap_or("track") {
        "track" => S::Track,
        "episode" => S::Episode,
        "show" | "podcast" | "podcasts" => S::Show,
        "album" => S::Album,
        "artist" => S::Artist,
        "playlist" => S::Playlist,
        _ => S::All,
    }
}

fn parse_source(raw: Option<&str>) -> spotuify_protocol::SearchSourceData {
    use spotuify_protocol::SearchSourceData as S;
    match raw.unwrap_or("hybrid") {
        "local" => S::Local,
        "spotify" => S::Spotify,
        _ => S::Hybrid,
    }
}
