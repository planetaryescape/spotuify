//! Shared MCP request execution used by stdio and HTTP transports.

use serde_json::{json, Value};
use spotuify_protocol::{Request, Response, ResponseData, SearchScopeData, SearchSourceData};

use crate::{
    bridge::{translate, TranslatedCall},
    confirm::{decide, Authorized},
    daemon_client::{default_socket_path, round_trip},
    dispatch, RpcError, RpcRequest, RpcResponse,
};

pub async fn handle_request(request: RpcRequest) -> RpcResponse {
    if request.method == "resources/read" {
        let id = request.id.clone().unwrap_or(Value::Null);
        let params = request.params.clone();
        let Some(uri) = params.get("uri").and_then(Value::as_str) else {
            return dispatch(request);
        };
        if let Some(req) = resource_request_for_uri(uri) {
            let socket = default_socket_path();
            let outcome = round_trip(&socket, req).await;
            return resource_outcome_to_rpc(id, uri, outcome);
        }
        return dispatch(request);
    }

    if request.method == "tools/call" {
        let id = request.id.clone().unwrap_or(Value::Null);
        let params = request.params.clone();
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let confirm = args.get("confirm").and_then(Value::as_bool);

        match decide(&name, confirm) {
            Err(err) => {
                return RpcResponse {
                    jsonrpc: "2.0",
                    id,
                    result: None,
                    error: Some(RpcError::invalid_request(err.to_string())),
                };
            }
            Ok(Authorized::PreviewOnly) => {
                return crate::rpc::preview_response(id, &name, &args);
            }
            Ok(Authorized::Execute) => match translate(&name, &args) {
                Ok(TranslatedCall::Request(req)) => {
                    let socket = default_socket_path();
                    let outcome = round_trip(&socket, req).await;
                    return daemon_outcome_to_rpc(id, outcome);
                }
                Ok(TranslatedCall::LocalJson(value)) => {
                    return local_json_to_rpc(id, "local_json", value);
                }
                Ok(TranslatedCall::PlaylistResolveTracks { plan }) => {
                    let socket = default_socket_path();
                    return playlist_resolve_outcome_to_rpc(
                        id,
                        resolve_playlist_tracks(&socket, plan).await,
                    );
                }
                Err(err) => {
                    return RpcResponse {
                        jsonrpc: "2.0",
                        id,
                        result: None,
                        error: Some(RpcError::invalid_params(err.to_string())),
                    };
                }
            },
        }
    }

    dispatch(request)
}

async fn resolve_playlist_tracks(
    socket: &std::path::Path,
    plan: spotuify_protocol::PlaylistPlan,
) -> anyhow::Result<Vec<spotuify_protocol::ResolvedTrackCandidate>> {
    let mut results = Vec::with_capacity(plan.candidate_searches.len());
    for query in &plan.candidate_searches {
        match round_trip(
            socket,
            Request::Search {
                query: query.clone(),
                scope: SearchScopeData::Track,
                // Agent plan resolution = catalog discovery.
                source: SearchSourceData::Spotify,
                limit: 50,
                kinds: None,
                sort: None,
            },
        )
        .await?
        {
            Response::Ok {
                data: ResponseData::SearchResults { items },
            } => results.push(items),
            Response::Ok { data } => {
                anyhow::bail!("expected search results while resolving `{query}`, got {data:?}");
            }
            Response::Error { message, code, .. } => {
                anyhow::bail!("daemon error [{code}] while resolving `{query}`: {message}");
            }
        }
    }
    Ok(spotuify_protocol::agent_playlists::resolve_plan_candidates(
        &plan, results,
    ))
}

fn playlist_resolve_outcome_to_rpc(
    id: Value,
    outcome: anyhow::Result<Vec<spotuify_protocol::ResolvedTrackCandidate>>,
) -> RpcResponse {
    match outcome {
        Ok(candidates) => local_json_to_rpc(id, "playlist_resolve_tracks", json!(candidates)),
        Err(err) => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({
                "content": [{
                    "type": "text",
                    "text": format!(
                        "Daemon unreachable or failed while resolving playlist tracks: {err}. Start it with `spotuify daemon start`."
                    ),
                }],
                "isError": true,
            })),
            error: None,
        },
    }
}

fn local_json_to_rpc(id: Value, kind: &'static str, value: Value) -> RpcResponse {
    let text = serde_json::to_string_pretty(&value)
        .unwrap_or_else(|err| format!("{{\"error\":\"serialization failed: {err}\"}}"));
    RpcResponse {
        jsonrpc: "2.0",
        id,
        result: Some(json!({
            "content": [{
                "type": "text",
                "text": text,
            }],
            "_meta": {
                "spotuify_response_kind": kind,
            }
        })),
        error: None,
    }
}

fn resource_request_for_uri(uri: &str) -> Option<Request> {
    match uri {
        "spotuify://playback" => Some(Request::PlaybackGet),
        "spotuify://devices" => Some(Request::DevicesList),
        "spotuify://playlists" => Some(Request::PlaylistsList),
        "spotuify://now_playing/lyrics" => Some(Request::LyricsGet {
            track_uri: None,
            force_refresh: false,
        }),
        "spotuify://doctor" => Some(Request::GetDoctorReport),
        _ => None,
    }
}

fn resource_outcome_to_rpc(id: Value, uri: &str, outcome: anyhow::Result<Response>) -> RpcResponse {
    match outcome {
        Ok(Response::Ok { data }) => {
            let text = serde_json::to_string_pretty(&data)
                .unwrap_or_else(|err| format!("{{\"error\":\"serialization failed: {err}\"}}"));
            RpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(json!({
                    "contents": [{
                        "uri": uri,
                        "mimeType": "application/json",
                        "text": text,
                    }]
                })),
                error: None,
            }
        }
        Ok(Response::Error { message, code, .. }) => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "application/json",
                    "text": format!("Daemon error [{code}]: {message}"),
                }],
                "isError": true,
            })),
            error: None,
        },
        Err(err) => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "application/json",
                    "text": format!(
                        "Daemon unreachable: {err}. Start it with `spotuify daemon start`."
                    ),
                }],
                "isError": true,
            })),
            error: None,
        },
    }
}

fn daemon_outcome_to_rpc(id: Value, outcome: anyhow::Result<Response>) -> RpcResponse {
    match outcome {
        Ok(Response::Ok { data }) => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({
                "content": [{
                    "type": "text",
                    "text": format!("Daemon ok: {:?}", data),
                }],
                "_meta": {
                    "spotuify_response_kind": kind_label(&data),
                }
            })),
            error: None,
        },
        Ok(Response::Error { message, code, .. }) => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({
                "content": [{
                    "type": "text",
                    "text": format!("Daemon error [{code}]: {message}"),
                }],
                "isError": true,
            })),
            error: None,
        },
        Err(err) => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({
                "content": [{
                    "type": "text",
                    "text": format!(
                        "Daemon unreachable: {err}. Start it with `spotuify daemon start`."
                    ),
                }],
                "isError": true,
            })),
            error: None,
        },
    }
}

fn kind_label(data: &spotuify_protocol::ResponseData) -> &'static str {
    use spotuify_protocol::ResponseData as D;
    match data {
        D::Pong => "pong",
        D::Shutdown => "shutdown",
        D::DaemonStatus { .. } => "daemon_status",
        D::DoctorReport { .. } => "doctor_report",
        D::Playback { .. } => "playback",
        D::Devices { .. } => "devices",
        D::SearchResults { .. } => "search_results",
        D::SearchStarted { .. } => "search_started",
        D::CacheStatus { .. } => "cache_status",
        D::Reindex { .. } => "reindex",
        D::Sync { .. } => "sync",
        D::Image { .. } => "image",
        D::CoverArt { .. } => "cover_art",
        D::Queue { .. } => "queue",
        D::ClientSeed { .. } => "client_seed",
        D::Playlists { .. } => "playlists",
        D::MediaItems { .. } => "media_items",
        D::ListenSessions { .. } => "listen_sessions",
        D::Logs { .. } => "logs",
        D::Mutation { .. } => "mutation",
        D::PlaylistCreate { .. } => "playlist_create",
        D::Lyrics { .. } => "lyrics",
        D::LyricsOffset { .. } => "lyrics_offset",
        D::AnalyticsTop { .. } => "analytics_top",
        D::AnalyticsHabits { .. } => "analytics_habits",
        D::AnalyticsSearch { .. } => "analytics_search",
        D::AnalyticsRediscovery { .. } => "analytics_rediscovery",
        D::AnalyticsRebuildReport { .. } => "analytics_rebuild_report",
        D::AnalyticsPruneReport { .. } => "analytics_prune_report",
        D::AnalyticsImportSummary { .. } => "analytics_import_summary",
        D::AnalyticsImportRunStatus { .. } => "analytics_import_status",
        D::AnalyticsImportUnresolved { .. } => "analytics_import_unresolved",
        D::AnalyticsImportUndoSummary { .. } => "analytics_import_undo_summary",
        D::Operations { .. } => "operations",
        D::OperationDetail { .. } => "operation_detail",
        D::OperationUndoResult { .. } => "operation_undo_result",
        D::Ack { .. } => "ack",
        D::WebApiToken { .. } => "web_api_token",
        D::SearchCachePruned { .. } => "search_cache_pruned",
        D::VizStatus { .. } => "viz_status",
        D::Reminders { .. } => "reminders",
        D::Notifications { .. } => "notifications",
        D::ReminderCreated { .. } => "reminder_created",
        D::UpdateStatus { .. } => "update_status",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_uri_maps_to_daemon_request() {
        assert!(matches!(
            resource_request_for_uri("spotuify://playback"),
            Some(Request::PlaybackGet)
        ));
        assert!(matches!(
            resource_request_for_uri("spotuify://devices"),
            Some(Request::DevicesList)
        ));
        assert!(matches!(
            resource_request_for_uri("spotuify://playlists"),
            Some(Request::PlaylistsList)
        ));
        assert!(matches!(
            resource_request_for_uri("spotuify://now_playing/lyrics"),
            Some(Request::LyricsGet {
                track_uri: None,
                force_refresh: false
            })
        ));
        assert!(matches!(
            resource_request_for_uri("spotuify://doctor"),
            Some(Request::GetDoctorReport)
        ));
        assert!(resource_request_for_uri("spotuify://missing").is_none());
    }
}
