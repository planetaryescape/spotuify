//! Shared MCP request execution used by stdio and HTTP transports.

use serde_json::{json, Value};
use spotuify_core::{MediaKind, ProviderCatalog, ProviderId, ResourceUri};
use spotuify_protocol::{
    IpcErrorKind, MutationId, Request, Response, ResponseData, SearchScopeData, SearchSourceData,
};
use url::Url;

use crate::{
    bridge::{translate_playlist_preview_with_catalog, translate_with_catalog, TranslatedCall},
    confirm::{decide, Authorized},
    daemon_client::{
        default_socket_path, is_post_send_compatibility_error, round_trip,
        round_trip_with_mutation_id, round_trip_with_timeout, DISCOVERY_TIMEOUT,
    },
    dispatch,
    rpc::dispatch_with_catalog,
    tools::{ensure_tool_available, tool_needs_provider_catalog, validate_tool_arguments},
    RpcError, RpcRequest, RpcResponse,
};

pub async fn handle_request(request: RpcRequest) -> RpcResponse {
    if request.method == "tools/list" {
        let socket = default_socket_path();
        // Fail open: MCP hosts call `tools/list` during initialization, before
        // the daemon is guaranteed up. Discovery failure serves the full static
        // manifest (tool calls surface daemon errors at call time) and a short
        // deadline keeps a hung daemon from blocking client startup.
        let catalog = discover_provider_catalog_with_timeout(&socket, DISCOVERY_TIMEOUT)
            .await
            .unwrap_or_default();
        return dispatch_with_catalog(request, catalog.as_ref());
    }

    if request.method == "resources/read" {
        let id = request.id.clone().unwrap_or(Value::Null);
        let params = request.params.clone();
        let Some(uri) = params.get("uri").and_then(Value::as_str) else {
            return dispatch(request);
        };
        match resource_request_for_uri(uri) {
            Ok(Some(route)) => {
                let socket = default_socket_path();
                let catalog = if route.capability_tool.is_some() {
                    match discover_provider_catalog(&socket).await {
                        Ok(catalog) => catalog,
                        Err(error) => {
                            return rpc_error(id, RpcError::internal_error(error.to_string()));
                        }
                    }
                } else {
                    None
                };
                if let Some(tool) = route.capability_tool {
                    if let Err(message) =
                        ensure_tool_available(tool, &route.capability_args, catalog.as_ref())
                    {
                        return rpc_error(id, RpcError::invalid_request(message));
                    }
                }
                let outcome = round_trip(&socket, route.request).await;
                return resource_outcome_to_rpc(id, uri, outcome);
            }
            Ok(None) => {}
            Err(message) => return rpc_error(id, RpcError::invalid_params(message)),
        }
        return dispatch(request);
    }

    if request.method == "tools/call" {
        let id = request.id.clone().unwrap_or(Value::Null);
        let params = request.params.clone();
        let name = params
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        if let Err(message) = validate_tool_arguments(&name, &args) {
            return rpc_error(id, RpcError::invalid_params(message));
        }
        let socket = default_socket_path();
        let catalog = if tool_needs_provider_catalog(&name, &args) {
            match discover_provider_catalog(&socket).await {
                Ok(catalog) => catalog,
                Err(error) => return rpc_error(id, RpcError::internal_error(error.to_string())),
            }
        } else {
            None
        };
        if let Err(message) = ensure_tool_available(&name, &args, catalog.as_ref()) {
            return rpc_error(id, RpcError::invalid_request(message));
        }
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
                if args.get("mutation_id").is_some() {
                    return rpc_error(
                        id,
                        RpcError::invalid_params(
                            "`mutation_id` is only valid for live execution, not a preview",
                        ),
                    );
                }
                return match translate_playlist_preview_with_catalog(&name, &args, catalog.as_ref())
                {
                    Ok(Some(request)) => {
                        let outcome = round_trip(&socket, request).await;
                        daemon_preview_outcome_to_rpc(id, &name, &args, outcome)
                    }
                    Ok(None) => crate::rpc::preview_response(id, &name, &args),
                    Err(err) => rpc_error(id, RpcError::invalid_params(err.to_string())),
                };
            }
            Ok(Authorized::Execute) => match translate_with_catalog(&name, &args, catalog.as_ref())
            {
                Ok(TranslatedCall::Request(req)) => {
                    let preview = crate::rpc::is_playlist_preview_request(&req);
                    if preview {
                        if args.get("mutation_id").is_some() {
                            return rpc_error(
                                id,
                                RpcError::invalid_params(
                                    "`mutation_id` is only valid for a live mutation, not a preview",
                                ),
                            );
                        }
                        let outcome = round_trip(&socket, req).await;
                        return daemon_preview_outcome_to_rpc(id, &name, &args, outcome);
                    }
                    if req.requires_mutation_id() {
                        let mutation_id = match live_mutation_id(&args) {
                            Ok(mutation_id) => mutation_id,
                            Err(message) => {
                                return rpc_error(id, RpcError::invalid_params(message));
                            }
                        };
                        let outcome =
                            round_trip_with_mutation_id(&socket, req, Some(mutation_id)).await;
                        return daemon_mutation_outcome_to_rpc(id, outcome, mutation_id);
                    }
                    if args.get("mutation_id").is_some() {
                        return rpc_error(
                            id,
                            RpcError::invalid_params(
                                "`mutation_id` is only valid for requests that perform a durable mutation",
                            ),
                        );
                    }
                    let outcome = round_trip(&socket, req).await;
                    return daemon_outcome_to_rpc(id, outcome);
                }
                Ok(TranslatedCall::LocalJson(value)) => {
                    return local_json_to_rpc(id, "local_json", value);
                }
                Ok(TranslatedCall::PlaylistResolveTracks { plan, provider }) => {
                    return playlist_resolve_outcome_to_rpc(
                        id,
                        resolve_playlist_tracks(&socket, plan, provider, catalog.as_ref()).await,
                    );
                }
                Ok(TranslatedCall::RelatedArtists { artist, provider }) => {
                    return daemon_outcome_to_rpc(
                        id,
                        resolve_related_artists(&socket, artist, provider).await,
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
    provider: Option<ProviderId>,
    catalog: Option<&ProviderCatalog>,
) -> anyhow::Result<PlaylistResolveOutcome> {
    let effective_provider = provider
        .clone()
        .or_else(|| catalog.and_then(|catalog| catalog.default_provider.clone()));
    let mut results = Vec::with_capacity(plan.candidate_searches.len());
    for query in &plan.candidate_searches {
        match round_trip(
            socket,
            Request::Search {
                query: query.clone(),
                scope: SearchScopeData::Track,
                // Agent plan resolution = catalog discovery.
                source: effective_provider.clone().map_or_else(
                    SearchSourceData::legacy_default_remote,
                    SearchSourceData::Remote,
                ),
                limit: 50,
                provider: provider.clone(),
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
            response @ Response::Error { .. } => {
                return Ok(PlaylistResolveOutcome::DaemonError(Box::new(response)));
            }
        }
    }
    Ok(PlaylistResolveOutcome::Candidates(
        spotuify_protocol::agent_playlists::resolve_plan_candidates(&plan, results),
    ))
}

enum PlaylistResolveOutcome {
    Candidates(Vec<spotuify_protocol::ResolvedTrackCandidate>),
    DaemonError(Box<Response>),
}

async fn resolve_related_artists(
    socket: &std::path::Path,
    artist: String,
    provider: Option<ProviderId>,
) -> anyhow::Result<Response> {
    let artist = match ResourceUri::parse(&artist) {
        Ok(uri) if uri.kind() == MediaKind::Artist => uri.as_uri(),
        Ok(uri) => anyhow::bail!("expected artist URI, got {}", uri.kind()),
        Err(_) => match round_trip(
            socket,
            Request::ResolveTarget {
                input: artist.clone(),
                provider,
                expected_kinds: Some(vec![MediaKind::Artist]),
            },
        )
        .await
        {
            Ok(Response::Ok {
                data:
                    ResponseData::TargetResolved {
                        target: Some(target),
                    },
            }) if target.uri.kind() == MediaKind::Artist => target.uri.as_uri(),
            Ok(Response::Ok {
                data:
                    ResponseData::TargetResolved {
                        target: Some(target),
                    },
            }) => anyhow::bail!(
                "provider resolved artist reference `{artist}` as `{}`",
                target.uri.kind()
            ),
            Ok(Response::Ok {
                data: ResponseData::TargetResolved { target: None },
            }) => anyhow::bail!("unrecognized artist reference `{artist}`"),
            Ok(response @ Response::Error { .. }) => return Ok(response),
            Ok(Response::Ok { data }) => {
                anyhow::bail!("expected resolved target for `{artist}`, got {data:?}")
            }
            // Older daemons predate `ResolveTarget` and close/mis-decode the
            // request. Fall back to the legacy client-side prefixing that
            // worked before provider routing, matching the catalog-discovery
            // compatibility path.
            Err(error) if is_post_send_compatibility_error(&error) => {
                format!("spotify:artist:{artist}")
            }
            Err(error) => return Err(error),
        },
    };
    round_trip(socket, Request::RelatedArtists { artist }).await
}

fn playlist_resolve_outcome_to_rpc(
    id: Value,
    outcome: anyhow::Result<PlaylistResolveOutcome>,
) -> RpcResponse {
    match outcome {
        Ok(PlaylistResolveOutcome::Candidates(candidates)) => {
            local_json_to_rpc(id, "playlist_resolve_tracks", json!(candidates))
        }
        Ok(PlaylistResolveOutcome::DaemonError(response)) => {
            daemon_outcome_to_rpc(id, Ok(*response))
        }
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

struct ResourceRoute {
    request: Request,
    capability_tool: Option<&'static str>,
    capability_args: Value,
}

fn resource_request_for_uri(uri: &str) -> Result<Option<ResourceRoute>, String> {
    let parsed = match Url::parse(uri) {
        Ok(parsed) if parsed.scheme() == "spotuify" => parsed,
        Ok(_) | Err(_) => return Ok(None),
    };
    if parsed.username() != ""
        || parsed.password().is_some()
        || parsed.port().is_some()
        || parsed.fragment().is_some()
    {
        return Err(format!("invalid spotuify resource URI `{uri}`"));
    }

    let mut provider = None;
    for (key, value) in parsed.query_pairs() {
        if key != "provider" || provider.is_some() {
            return Err(format!("unsupported or repeated resource query `{key}`"));
        }
        provider = Some(ProviderId::new(value.as_ref()).map_err(|error| error.to_string())?);
    }
    let capability_args = provider
        .as_ref()
        .map_or_else(|| json!({}), |provider| json!({ "provider": provider }));

    let route = match (parsed.host_str(), parsed.path()) {
        (Some("playback"), "" | "/") if provider.is_none() => ResourceRoute {
            request: Request::PlaybackGet,
            capability_tool: Some("now_playing"),
            capability_args,
        },
        (Some("devices"), "" | "/") if provider.is_none() => ResourceRoute {
            request: Request::DevicesList,
            capability_tool: Some("devices_list"),
            capability_args,
        },
        (Some("playlists"), "" | "/") => ResourceRoute {
            request: Request::PlaylistsList { provider },
            capability_tool: Some("playlists_list"),
            capability_args,
        },
        (Some("now_playing"), "/lyrics") if provider.is_none() => ResourceRoute {
            request: Request::LyricsGet {
                track_uri: None,
                force_refresh: false,
            },
            capability_tool: Some("lyrics"),
            capability_args,
        },
        (Some("doctor"), "" | "/") if provider.is_none() => ResourceRoute {
            request: Request::GetDoctorReport,
            capability_tool: None,
            capability_args,
        },
        _ => return Ok(None),
    };
    Ok(Some(route))
}

async fn discover_provider_catalog(
    socket: &std::path::Path,
) -> anyhow::Result<Option<ProviderCatalog>> {
    provider_catalog_from_outcome(round_trip(socket, Request::ProvidersList).await)
}

async fn discover_provider_catalog_with_timeout(
    socket: &std::path::Path,
    timeout: std::time::Duration,
) -> anyhow::Result<Option<ProviderCatalog>> {
    provider_catalog_from_outcome(
        round_trip_with_timeout(socket, Request::ProvidersList, timeout).await,
    )
}

fn provider_catalog_from_outcome(
    outcome: anyhow::Result<Response>,
) -> anyhow::Result<Option<ProviderCatalog>> {
    match outcome {
        Ok(Response::Ok {
            data:
                ResponseData::ProviderList {
                    default_provider,
                    providers,
                },
        }) => {
            let catalog = ProviderCatalog {
                default_provider,
                providers,
            };
            catalog
                .validate()
                .map_err(|message| anyhow::anyhow!(message))?;
            Ok(Some(catalog))
        }
        Ok(Response::Error {
            kind: IpcErrorKind::Unsupported | IpcErrorKind::InvalidRequest,
            ..
        }) => Ok(None),
        // Older daemons commonly close the connection while decoding a new
        // request. Only a typed post-send close/decode may use compatibility;
        // connect, missing-socket, send, and timeout failures stay fail-closed.
        Err(error) if is_post_send_compatibility_error(&error) => Ok(None),
        Err(error) => Err(error),
        Ok(Response::Error { message, code, .. }) => {
            anyhow::bail!("provider catalog discovery failed [{code}]: {message}")
        }
        Ok(Response::Ok { data }) => {
            anyhow::bail!("expected provider list, got {data:?}")
        }
    }
}

fn rpc_error(id: Value, error: RpcError) -> RpcResponse {
    RpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(error),
    }
}

fn live_mutation_id(args: &Value) -> Result<MutationId, String> {
    let Some(value) = args.get("mutation_id") else {
        return Ok(MutationId::new_v7());
    };
    let raw = value
        .as_str()
        .ok_or_else(|| "`mutation_id` must be a UUIDv7 string".to_string())?;
    let mutation_id = raw
        .parse::<MutationId>()
        .map_err(|_| "`mutation_id` must be a UUIDv7 string".to_string())?;
    let bytes = mutation_id.0.as_bytes();
    if bytes[6] >> 4 != 7 || bytes[8] & 0b1100_0000 != 0b1000_0000 {
        return Err("`mutation_id` must be a UUIDv7 string".to_string());
    }
    Ok(mutation_id)
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
        Ok(Response::Error {
            message,
            code,
            kind,
            retryable,
            provider,
            detail,
        }) => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "application/json",
                    "text": format!("Daemon error [{code}]: {message}"),
                }],
                "_meta": daemon_error_metadata(
                    kind,
                    retryable,
                    provider.as_ref(),
                    detail.as_deref(),
                    None,
                ),
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

fn daemon_error_metadata(
    kind: IpcErrorKind,
    retryable: bool,
    provider: Option<&ProviderId>,
    detail: Option<&str>,
    retry_after_secs: Option<u64>,
) -> Value {
    let mut metadata = serde_json::Map::new();
    insert_daemon_error_metadata(
        &mut metadata,
        kind,
        retryable,
        provider,
        detail,
        retry_after_secs,
    );
    Value::Object(metadata)
}

fn insert_daemon_error_metadata(
    metadata: &mut serde_json::Map<String, Value>,
    kind: IpcErrorKind,
    retryable: bool,
    provider: Option<&ProviderId>,
    detail: Option<&str>,
    retry_after_secs: Option<u64>,
) {
    metadata.insert(
        "spotuify_error_kind".to_string(),
        serde_json::to_value(kind).expect("IPC error kind must serialize"),
    );
    metadata.insert(
        "spotuify_error_retryable".to_string(),
        Value::Bool(retryable),
    );
    if let Some(provider) = provider {
        metadata.insert(
            "spotuify_error_provider".to_string(),
            Value::String(provider.to_string()),
        );
    }
    if let Some(detail) = detail {
        metadata.insert(
            "spotuify_error_detail".to_string(),
            Value::String(detail.to_string()),
        );
    }
    if let Some(retry_after_secs) = retry_after_secs {
        metadata.insert(
            "spotuify_error_retry_after_secs".to_string(),
            Value::from(retry_after_secs),
        );
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
        Ok(Response::Error {
            message,
            code,
            kind,
            retryable,
            provider,
            detail,
        }) => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({
                "content": [{
                    "type": "text",
                    "text": format!("Daemon error [{code}]: {message}"),
                }],
                "_meta": daemon_error_metadata(
                    kind,
                    retryable,
                    provider.as_ref(),
                    detail.as_deref(),
                    None,
                ),
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

fn daemon_mutation_outcome_to_rpc(
    id: Value,
    outcome: anyhow::Result<Response>,
    mutation_id: MutationId,
) -> RpcResponse {
    let transport_ambiguous = outcome.is_err();
    let receipt_pending = matches!(
        &outcome,
        Ok(Response::Ok {
            data: ResponseData::Mutation {
                receipt: spotuify_protocol::CommandReceipt {
                    status: Some(spotuify_protocol::ReceiptStatus::Pending),
                    ..
                },
            },
        })
    );
    let receipt_metadata = match &outcome {
        Ok(Response::Ok {
            data: ResponseData::Mutation { receipt },
        }) => Some((
            receipt.receipt_id.map(|id| id.to_string()),
            receipt
                .status
                .as_ref()
                .and_then(|status| serde_json::to_value(status).ok()),
            receipt.replayed,
            receipt.error.clone(),
        )),
        Ok(Response::Ok {
            data: ResponseData::PlaylistCreate { receipt },
        }) => Some((
            receipt.receipt_id.map(|id| id.to_string()),
            Some(Value::String("confirmed".to_string())),
            receipt.replayed,
            None,
        )),
        _ => None,
    };
    let mut response = daemon_outcome_to_rpc(id, outcome);
    let Some(result) = response.result.as_mut() else {
        return response;
    };
    let Some(object) = result.as_object_mut() else {
        return response;
    };
    let metadata = object
        .entry("_meta")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("MCP result metadata must be an object");
    metadata.insert(
        "spotuify_mutation_id".to_string(),
        Value::String(mutation_id.to_string()),
    );
    metadata.insert(
        "spotuify_retry_disposition".to_string(),
        Value::String(
            if transport_ambiguous {
                "retry-same-id"
            } else if receipt_pending {
                "poll-same-id"
            } else {
                "do-not-retry"
            }
            .to_string(),
        ),
    );
    if let Some((receipt_id, status, replayed, error)) = receipt_metadata {
        if let Some(receipt_id) = receipt_id {
            metadata.insert("spotuify_receipt_id".to_string(), Value::String(receipt_id));
        }
        if let Some(status) = status {
            metadata.insert("spotuify_receipt_status".to_string(), status);
        }
        metadata.insert("spotuify_replayed".to_string(), Value::Bool(replayed));
        if let Some(error) = error {
            insert_daemon_error_metadata(
                metadata,
                error.kind,
                error.kind.is_retryable(),
                error.provider.as_ref(),
                error.detail.as_deref(),
                error.retry_after_secs,
            );
        }
    }
    if transport_ambiguous {
        if let Some(text) = object
            .get_mut("content")
            .and_then(Value::as_array_mut)
            .and_then(|content| content.first_mut())
            .and_then(Value::as_object_mut)
            .and_then(|content| content.get_mut("text"))
            .and_then(|value| value.as_str())
            .map(str::to_string)
        {
            object["content"][0]["text"] = Value::String(format!(
                "{text} Retry only with the same `mutation_id`: {mutation_id}."
            ));
        }
    } else if receipt_pending {
        if let Some(text) = object
            .get_mut("content")
            .and_then(Value::as_array_mut)
            .and_then(|content| content.first_mut())
            .and_then(Value::as_object_mut)
            .and_then(|content| content.get_mut("text"))
            .and_then(|value| value.as_str())
            .map(str::to_string)
        {
            object["content"][0]["text"] = Value::String(format!(
                "{text} The mutation is still pending; poll only with the same `mutation_id`: {mutation_id}."
            ));
        }
    }
    response
}

fn preview_error_metadata(
    preview: Value,
    kind: IpcErrorKind,
    retryable: bool,
    provider: Option<&ProviderId>,
    detail: Option<&str>,
) -> Value {
    let mut metadata = daemon_error_metadata(kind, retryable, provider, detail, None);
    let object = metadata
        .as_object_mut()
        .expect("daemon error metadata must be an object");
    object.insert("spotuify_preview_only".to_string(), Value::Bool(true));
    object.insert("spotuify_preview".to_string(), preview);
    metadata
}

fn daemon_preview_outcome_to_rpc(
    id: Value,
    name: &str,
    args: &Value,
    outcome: anyhow::Result<Response>,
) -> RpcResponse {
    let preview = crate::rpc::destructive_preview(name, args);
    match outcome {
        Ok(Response::Ok { data }) => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({
                "content": [{
                    "type": "text",
                    "text": format!(
                        "Daemon validated the read-only `{name}` preview. No mutation was executed: {data:?}. Re-invoke with `confirm: true` and `dry_run: false` after the user approves."
                    ),
                }],
                "_meta": {
                    "spotuify_preview_only": true,
                    "spotuify_preview": preview,
                    "spotuify_response_kind": kind_label(&data),
                }
            })),
            error: None,
        },
        Ok(Response::Error {
            message,
            code,
            kind,
            retryable,
            provider,
            detail,
        }) => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({
                "content": [{
                    "type": "text",
                    "text": format!("Daemon rejected the read-only `{name}` preview [{code}]: {message}"),
                }],
                "_meta": preview_error_metadata(
                    preview,
                    kind,
                    retryable,
                    provider.as_ref(),
                    detail.as_deref(),
                ),
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
                        "Daemon unreachable while validating the read-only `{name}` preview: {err}. Start it with `spotuify daemon start`."
                    ),
                }],
                "_meta": {
                    "spotuify_preview_only": true,
                    "spotuify_preview": preview,
                },
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
        D::ProviderList { .. } => "provider_list",
        D::TargetResolved { .. } => "target_resolved",
        D::AudioOutputs { .. } => "audio_outputs",
        D::AuthSession { .. } => "auth_session",
        D::AuthStatus { .. } => "auth_status",
        D::AuthLogout { .. } => "auth_logout",
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
        D::SavedTracksPage { .. } => "saved_tracks_page",
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
    #![allow(clippy::panic, clippy::unwrap_used)]

    use super::*;

    #[tokio::test]
    async fn malformed_tool_arguments_fail_before_provider_discovery() {
        for tool in [
            "playlist_create",
            "playlist_add",
            "playlist_remove",
            "radio_start",
        ] {
            let response = handle_request(RpcRequest {
                jsonrpc: "2.0".to_string(),
                id: Some(json!(1)),
                method: "tools/call".to_string(),
                params: json!({
                    "name": tool,
                    "arguments": { "dry_run": "true" }
                }),
            })
            .await;
            let error = response.error.expect("malformed arguments must fail");
            assert_eq!(error.code, -32602);
            assert!(error.message.contains("`dry_run` must be boolean"));
        }
    }

    #[test]
    fn resource_uri_maps_to_daemon_request() {
        assert!(matches!(
            resource_request_for_uri("spotuify://playback"),
            Ok(Some(ResourceRoute {
                request: Request::PlaybackGet,
                ..
            }))
        ));
        assert!(matches!(
            resource_request_for_uri("spotuify://devices"),
            Ok(Some(ResourceRoute {
                request: Request::DevicesList,
                ..
            }))
        ));
        assert!(matches!(
            resource_request_for_uri("spotuify://playlists"),
            Ok(Some(ResourceRoute {
                request: Request::PlaylistsList { provider: None },
                ..
            }))
        ));
        assert!(matches!(
            resource_request_for_uri("spotuify://playlists?provider=music"),
            Ok(Some(ResourceRoute {
                request: Request::PlaylistsList {
                    provider: Some(provider)
                },
                ..
            })) if provider.as_str() == "music"
        ));
        assert!(matches!(
            resource_request_for_uri("spotuify://now_playing/lyrics"),
            Ok(Some(ResourceRoute {
                request: Request::LyricsGet {
                    track_uri: None,
                    force_refresh: false
                },
                ..
            }))
        ));
        assert!(matches!(
            resource_request_for_uri("spotuify://doctor"),
            Ok(Some(ResourceRoute {
                request: Request::GetDoctorReport,
                ..
            }))
        ));
        assert!(matches!(
            resource_request_for_uri("spotuify://missing"),
            Ok(None)
        ));
        assert!(resource_request_for_uri("spotuify://playlists?provider=NotValid").is_err());
    }

    #[test]
    fn every_explicit_provider_call_is_rejected_without_catalog() {
        let args = json!({ "provider": "spotify" });
        for tool in ["library_list", "playlist_remove", "search"] {
            let error = ensure_tool_available(tool, &args, None).unwrap_err();
            assert!(error.contains("requires a daemon provider catalog"));
            assert!(ensure_tool_available(tool, &json!({}), None).is_ok());
        }

        let route = resource_request_for_uri("spotuify://playlists?provider=spotify")
            .unwrap()
            .unwrap();
        let error =
            ensure_tool_available(route.capability_tool.unwrap(), &route.capability_args, None)
                .unwrap_err();
        assert!(error.contains("requires a daemon provider catalog"));
    }

    #[test]
    fn provider_discovery_only_falls_back_for_typed_post_send_compatibility() {
        let compatibility = provider_catalog_from_outcome(Err(
            crate::daemon_client::test_post_send_compatibility_error(),
        ))
        .expect("old-daemon post-send close should use legacy compatibility");
        assert!(compatibility.is_none());

        // Connect/timeout failures stay hard errors from the discovery helper.
        // The `tools/call` path keeps these fail-closed; only `tools/list`
        // interprets a discovery error as "serve the static manifest".
        let connect = provider_catalog_from_outcome(Err(anyhow::anyhow!(
            "connect to daemon IPC: socket missing"
        )));
        assert!(
            connect.is_err(),
            "missing daemon must stay a hard error for tool-call discovery"
        );

        let timeout = provider_catalog_from_outcome(Err(anyhow::anyhow!(
            "daemon did not respond within 40s"
        )));
        assert!(
            timeout.is_err(),
            "timeouts must stay a hard error for tool-call discovery"
        );
    }

    #[tokio::test]
    async fn tools_list_fails_open_to_static_manifest_when_daemon_unreachable() {
        // MCP hosts call tools/list during initialization, before the daemon
        // is guaranteed up. An unreachable daemon must serve the full static
        // manifest instead of a JSON-RPC error with zero tools.
        let bogus = std::env::temp_dir().join(format!(
            "spotuify-mcp-absent-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        std::env::set_var("SPOTUIFY_SOCKET", &bogus);
        let response = handle_request(RpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: "tools/list".to_string(),
            params: json!({}),
        })
        .await;
        std::env::remove_var("SPOTUIFY_SOCKET");

        assert!(
            response.error.is_none(),
            "tools/list must fail open, got {:?}",
            response.error
        );
        let tools = response
            .result
            .expect("tools/list has a result")
            .get("tools")
            .and_then(Value::as_array)
            .expect("tools array")
            .len();
        assert_eq!(
            tools,
            crate::tools::ToolCatalogue::all().len(),
            "unreachable daemon serves the full static catalogue"
        );
    }

    #[tokio::test]
    async fn related_artists_falls_back_to_legacy_prefixing_on_old_daemon() {
        use futures::{SinkExt as _, StreamExt as _};
        use spotuify_protocol::{IpcCodec, IpcMessage, IpcPayload};
        use tokio_util::codec::Framed;

        let unique = format!(
            "spotuify-mcp-related-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        );
        #[cfg(unix)]
        let socket = std::env::temp_dir().join(format!("{unique}.sock"));
        #[cfg(windows)]
        let socket = std::path::PathBuf::from(format!(r"\\.\pipe\{unique}"));

        let mut listener = spotuify_protocol::ipc_stream::IpcListener::bind(&socket)
            .expect("bind test IPC listener");
        let server = tokio::spawn(async move {
            // First connection: a pre-`ResolveTarget` daemon reads the request
            // then closes without responding (post-send compatibility close).
            let first = listener.accept().await.expect("accept ResolveTarget");
            let mut framed = Framed::new(first, IpcCodec::new());
            let message = framed.next().await.expect("frame").expect("valid frame");
            assert!(matches!(
                message.payload,
                IpcPayload::Request(Request::ResolveTarget { .. })
            ));
            drop(framed);

            // Second connection: the fallback re-issues RelatedArtists with the
            // legacy client-side prefixed URI.
            let second = listener.accept().await.expect("accept RelatedArtists");
            let mut framed = Framed::new(second, IpcCodec::new());
            let message = framed.next().await.expect("frame").expect("valid frame");
            let IpcPayload::Request(Request::RelatedArtists { artist }) = message.payload else {
                panic!("expected RelatedArtists request");
            };
            framed
                .send(IpcMessage {
                    id: message.id,
                    source: None,
                    mutation_id: None,
                    payload: IpcPayload::Response(Response::Ok {
                        data: ResponseData::Pong,
                    }),
                })
                .await
                .expect("send response");
            artist
        });

        let response = resolve_related_artists(&socket, "some artist".to_string(), None)
            .await
            .expect("fallback resolves without error");
        assert!(matches!(
            response,
            Response::Ok {
                data: ResponseData::Pong
            }
        ));
        let artist = server.await.expect("server task");
        assert_eq!(artist, "spotify:artist:some artist");
        let _ = std::fs::remove_file(&socket);
    }

    #[test]
    fn mutation_id_parser_requires_uuid_v7() {
        let supplied = json!({
            "mutation_id": "018f2f76-7c5d-7b1d-8000-000000000001"
        });
        assert_eq!(
            live_mutation_id(&supplied).unwrap().to_string(),
            "018f2f76-7c5d-7b1d-8000-000000000001"
        );
        assert!(live_mutation_id(&json!({
            "mutation_id": "018f2f76-7c5d-4b1d-8000-000000000001"
        }))
        .is_err());
        assert!(live_mutation_id(&json!({ "mutation_id": 7 })).is_err());
    }

    #[test]
    fn mutation_outcome_preserves_retry_id_on_transport_error() {
        let mutation_id: MutationId = "018f2f76-7c5d-7b1d-8000-000000000001".parse().unwrap();
        let response = daemon_mutation_outcome_to_rpc(
            json!(1),
            Err(anyhow::anyhow!("daemon did not respond")),
            mutation_id,
        );
        let result = response.result.unwrap();
        assert_eq!(
            result["_meta"]["spotuify_mutation_id"],
            mutation_id.to_string()
        );
        assert_eq!(
            result["_meta"]["spotuify_retry_disposition"],
            "retry-same-id"
        );
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Retry only with the same `mutation_id`"));
    }

    #[test]
    fn mutation_outcome_exposes_receipt_replay_metadata() {
        let mutation_id: MutationId = "018f2f76-7c5d-7b1d-8000-000000000001".parse().unwrap();
        let receipt_id = spotuify_protocol::ReceiptId::new_v7();
        let response = daemon_mutation_outcome_to_rpc(
            json!(1),
            Ok(Response::Ok {
                data: ResponseData::Mutation {
                    receipt: spotuify_protocol::CommandReceipt {
                        ok: true,
                        action: "queue-add".to_string(),
                        message: "already applied".to_string(),
                        receipt_id: Some(receipt_id),
                        mutation_id: Some(mutation_id),
                        status: Some(spotuify_protocol::ReceiptStatus::Confirmed),
                        error: None,
                        replayed: true,
                    },
                },
            }),
            mutation_id,
        );
        let result = response.result.unwrap();
        assert_eq!(
            result["_meta"]["spotuify_receipt_id"],
            receipt_id.to_string()
        );
        assert_eq!(result["_meta"]["spotuify_receipt_status"], "confirmed");
        assert_eq!(result["_meta"]["spotuify_replayed"], true);
        assert_eq!(
            result["_meta"]["spotuify_retry_disposition"],
            "do-not-retry"
        );
        assert!(!result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Retry only"));
    }

    #[test]
    fn pending_mutation_outcome_requires_polling_with_the_same_id() {
        let mutation_id: MutationId = "018f2f76-7c5d-7b1d-8000-000000000001".parse().unwrap();
        let receipt_id = spotuify_protocol::ReceiptId::new_v7();
        let response = daemon_mutation_outcome_to_rpc(
            json!(1),
            Ok(Response::Ok {
                data: ResponseData::Mutation {
                    receipt: spotuify_protocol::CommandReceipt {
                        ok: true,
                        action: "queue-add".to_string(),
                        message: "queued".to_string(),
                        receipt_id: Some(receipt_id),
                        mutation_id: Some(mutation_id),
                        status: Some(spotuify_protocol::ReceiptStatus::Pending),
                        error: None,
                        replayed: false,
                    },
                },
            }),
            mutation_id,
        );
        let result = response.result.unwrap();
        assert_eq!(result["_meta"]["spotuify_receipt_status"], "pending");
        assert_eq!(
            result["_meta"]["spotuify_retry_disposition"],
            "poll-same-id"
        );
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("poll only with the same `mutation_id`"));
    }

    #[test]
    fn typed_mutation_error_is_not_marked_transport_ambiguous() {
        let mutation_id: MutationId = "018f2f76-7c5d-7b1d-8000-000000000001".parse().unwrap();
        let response = daemon_mutation_outcome_to_rpc(
            json!(1),
            Ok(Response::error_with_kind(
                "provider rejected request",
                IpcErrorKind::Provider,
            )),
            mutation_id,
        );
        let result = response.result.unwrap();
        assert_eq!(
            result["_meta"]["spotuify_retry_disposition"],
            "do-not-retry"
        );
        assert!(!result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Retry only"));
    }

    #[test]
    fn daemon_error_exposes_structured_provider_metadata() {
        let provider = ProviderId::new("nondefault").unwrap();
        let response = daemon_outcome_to_rpc(
            json!(1),
            Ok(Response::Error {
                message: "provider throttled".to_string(),
                kind: IpcErrorKind::RateLimited,
                code: IpcErrorKind::RateLimited.as_code().to_string(),
                retryable: true,
                provider: Some(provider),
                detail: Some("catalog quota".to_string()),
            }),
        );
        let result = response.result.unwrap();
        assert_eq!(result["_meta"]["spotuify_error_kind"], "rate_limited");
        assert_eq!(result["_meta"]["spotuify_error_retryable"], true);
        assert_eq!(result["_meta"]["spotuify_error_provider"], "nondefault");
        assert_eq!(result["_meta"]["spotuify_error_detail"], "catalog quota");
    }

    #[test]
    fn playlist_resolve_preserves_daemon_error_envelope_metadata() {
        let provider = ProviderId::new("nondefault").unwrap();
        let response = playlist_resolve_outcome_to_rpc(
            json!(1),
            Ok(PlaylistResolveOutcome::DaemonError(Box::new(
                Response::Error {
                    message: "authorization revoked".to_string(),
                    kind: IpcErrorKind::AuthRevoked,
                    code: IpcErrorKind::AuthRevoked.as_code().to_string(),
                    retryable: false,
                    provider: Some(provider),
                    detail: Some("refresh token rejected".to_string()),
                },
            ))),
        );
        let result = response.result.unwrap();

        assert_eq!(result["_meta"]["spotuify_error_kind"], "auth_revoked");
        assert_eq!(result["_meta"]["spotuify_error_retryable"], false);
        assert_eq!(result["_meta"]["spotuify_error_provider"], "nondefault");
        assert_eq!(
            result["_meta"]["spotuify_error_detail"],
            "refresh token rejected"
        );
        assert_eq!(result["isError"], true);
    }

    #[test]
    fn failed_receipt_exposes_retry_after_metadata() {
        let mutation_id: MutationId = "018f2f76-7c5d-7b1d-8000-000000000001".parse().unwrap();
        let response = daemon_mutation_outcome_to_rpc(
            json!(1),
            Ok(Response::Ok {
                data: ResponseData::Mutation {
                    receipt: spotuify_protocol::CommandReceipt {
                        ok: false,
                        action: "queue-add".to_string(),
                        message: "provider throttled".to_string(),
                        receipt_id: Some(spotuify_protocol::ReceiptId::new_v7()),
                        mutation_id: Some(mutation_id),
                        status: Some(spotuify_protocol::ReceiptStatus::Failed),
                        error: Some(spotuify_protocol::ApiErrorSummary {
                            kind: IpcErrorKind::RateLimited,
                            message: "provider throttled".to_string(),
                            retry_after_secs: Some(23),
                            provider: Some(ProviderId::new("nondefault").unwrap()),
                            detail: Some("catalog quota".to_string()),
                        }),
                        replayed: false,
                    },
                },
            }),
            mutation_id,
        );
        let result = response.result.unwrap();
        assert_eq!(result["_meta"]["spotuify_error_retry_after_secs"], 23);
        assert_eq!(result["_meta"]["spotuify_error_kind"], "rate_limited");
    }

    #[test]
    fn playlist_create_receipt_is_terminal_confirmed_metadata() {
        let mutation_id: MutationId = "018f2f76-7c5d-7b1d-8000-000000000001".parse().unwrap();
        let response = daemon_mutation_outcome_to_rpc(
            json!(1),
            Ok(Response::Ok {
                data: ResponseData::PlaylistCreate {
                    receipt: spotuify_protocol::PlaylistCreateReceipt {
                        ok: true,
                        action: "playlist-create".to_string(),
                        playlist_id: "nondefault:playlist:one".to_string(),
                        playlist_uri: "nondefault:playlist:one".to_string(),
                        name: "One".to_string(),
                        added_item_count: 0,
                        message: "created".to_string(),
                        receipt_id: Some(spotuify_protocol::ReceiptId::new_v7()),
                        mutation_id: Some(mutation_id),
                        replayed: false,
                    },
                },
            }),
            mutation_id,
        );
        let result = response.result.unwrap();
        assert_eq!(result["_meta"]["spotuify_receipt_status"], "confirmed");
        assert_eq!(
            result["_meta"]["spotuify_retry_disposition"],
            "do-not-retry"
        );
    }
}
