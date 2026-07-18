//! Phase 8.6/8.7 — JSON-RPC 2.0 + MCP protocol message handler.
//!
//! Pure-function core: parse a JSON-RPC request, dispatch to the
//! catalogue/bridge/confirm modules, build the response. The stdio
//! transport (read line, call this, write line) is a thin wrapper.
//!
//! Spec versions targeted:
//! - JSON-RPC 2.0
//! - MCP protocol revision 2024-11-05 (the version returned by
//!   `ToolManifest::build()`)
//!
//! Not all of MCP is implemented here. The Phase 8 scope is:
//! initialize, tools/list, tools/call (with destructive confirm
//! gating), resources/list, resources/read. Subscriptions are
//! advertised via `serverCapabilities.resources.subscribe = true`
//! but the actual streaming push is wired by the daemon-side adapter
//! in a follow-up.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spotuify_core::ProviderCatalog;

use crate::confirm::{decide, Authorized};
use crate::resources::ResourceCatalogue;
use crate::tools::{ensure_tool_available, validate_tool_arguments, ToolCatalogue, ToolManifest};

/// One JSON-RPC 2.0 request.
#[derive(Debug, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// One JSON-RPC 2.0 response. We always include `id` (even when null)
/// because MCP clients reject unidentified responses.
#[derive(Debug, Serialize)]
pub struct RpcResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn invalid_request(msg: impl Into<String>) -> Self {
        Self {
            code: -32600,
            message: msg.into(),
            data: None,
        }
    }
    pub fn method_not_found(msg: impl Into<String>) -> Self {
        Self {
            code: -32601,
            message: msg.into(),
            data: None,
        }
    }
    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: msg.into(),
            data: None,
        }
    }
    pub fn internal_error(msg: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: msg.into(),
            data: None,
        }
    }
}

/// Outcome of `dispatch`. The transport caller serialises this to
/// JSON-RPC over its preferred wire.
pub fn dispatch(request: RpcRequest) -> RpcResponse {
    dispatch_with_catalog(request, None)
}

/// Dispatch with a daemon-discovered provider catalog. `None` preserves the
/// additive behavior required when connected to a pre-catalog daemon.
pub fn dispatch_with_catalog(
    request: RpcRequest,
    catalog: Option<&ProviderCatalog>,
) -> RpcResponse {
    let id = request.id.unwrap_or(Value::Null);

    if request.jsonrpc != "2.0" {
        return error_response(
            id,
            RpcError::invalid_request("only JSON-RPC 2.0 is supported"),
        );
    }

    match request.method.as_str() {
        "initialize" => initialize(id, request.params),
        "tools/list" => tools_list(id, catalog),
        "tools/call" => tools_call(id, request.params, catalog),
        "resources/list" => resources_list(id),
        "resources/read" => resources_read(id, request.params),
        "resources/subscribe" => resources_subscribe(id, request.params),
        "resources/unsubscribe" => resources_unsubscribe(id, request.params),
        "ping" => ok_response(id, json!({})),
        other => error_response(
            id,
            RpcError::method_not_found(format!("unknown method `{other}`")),
        ),
    }
}

/// Process-global resource subscription set (stdio is single-client).
/// `resources/subscribe` adds, `unsubscribe` removes; the stdio event
/// thread reads it to decide which `notifications/resources/updated` to
/// push.
fn subscriptions() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static SUBS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    SUBS.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

/// Snapshot of currently-subscribed resource URIs.
pub fn subscribed_uris() -> std::collections::HashSet<String> {
    subscriptions()
        .lock()
        .map(|set| set.clone())
        .unwrap_or_default()
}

fn resources_subscribe(id: Value, params: Value) -> RpcResponse {
    let Some(uri) = params.get("uri").and_then(Value::as_str) else {
        return error_response(
            id,
            RpcError::invalid_params("resources/subscribe: missing `uri`"),
        );
    };
    if crate::resources::ResourceCatalogue::by_uri(uri).is_none() {
        return error_response(
            id,
            RpcError::invalid_params(format!("resources/subscribe: unknown uri `{uri}`")),
        );
    }
    if let Ok(mut set) = subscriptions().lock() {
        set.insert(uri.to_string());
    }
    ok_response(id, json!({}))
}

fn resources_unsubscribe(id: Value, params: Value) -> RpcResponse {
    let Some(uri) = params.get("uri").and_then(Value::as_str) else {
        return error_response(
            id,
            RpcError::invalid_params("resources/unsubscribe: missing `uri`"),
        );
    };
    if let Ok(mut set) = subscriptions().lock() {
        set.remove(uri);
    }
    ok_response(id, json!({}))
}

fn ok_response(id: Value, result: Value) -> RpcResponse {
    RpcResponse {
        jsonrpc: "2.0",
        id,
        result: Some(result),
        error: None,
    }
}

fn error_response(id: Value, error: RpcError) -> RpcResponse {
    RpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(error),
    }
}

fn initialize(id: Value, _params: Value) -> RpcResponse {
    let manifest = ToolManifest::build();
    let result = json!({
        "protocolVersion": manifest.spec_version,
        "serverInfo": {
            "name": manifest.server_name,
            "version": env!("CARGO_PKG_VERSION"),
        },
        "capabilities": {
            "tools": { "listChanged": false },
            "resources": { "subscribe": true, "listChanged": false },
            "prompts": { "listChanged": false },
        }
    });
    ok_response(id, result)
}

fn tools_list(id: Value, catalog: Option<&ProviderCatalog>) -> RpcResponse {
    let tools: Vec<Value> = ToolCatalogue::available(catalog)
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": tool_input_schema(t.name, catalog),
            })
        })
        .collect();
    ok_response(id, json!({ "tools": tools }))
}

fn tools_call(id: Value, params: Value, catalog: Option<&ProviderCatalog>) -> RpcResponse {
    let name = match params.get("name").and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => {
            return error_response(id, RpcError::invalid_params("tools/call: missing `name`"));
        }
    };
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    if let Err(message) = validate_tool_arguments(&name, &args) {
        return error_response(id, RpcError::invalid_params(message));
    }
    if let Err(message) = ensure_tool_available(&name, &args, catalog) {
        return error_response(id, RpcError::invalid_request(message));
    }
    let confirm = args.get("confirm").and_then(Value::as_bool);

    match decide(&name, confirm) {
        Err(err) => error_response(id, RpcError::invalid_request(err.to_string())),
        Ok(Authorized::PreviewOnly) => {
            match crate::bridge::translate_playlist_preview_with_catalog(&name, &args, catalog) {
                Ok(Some(request)) => translated_preview_response(id, &name, &args, &request),
                Ok(None) => preview_response(id, &name, &args),
                Err(err) => error_response(id, RpcError::invalid_params(err.to_string())),
            }
        }
        Ok(Authorized::Execute) => {
            // The bridge translates to a daemon Request; the wire
            // layer (Phase 8 follow-up) actually dispatches it. For
            // now we report the translated form so MCP clients see a
            // consistent envelope.
            match crate::bridge::translate_with_catalog(&name, &args, catalog) {
                Ok(crate::bridge::TranslatedCall::Request(req)) => {
                    if is_playlist_preview_request(&req) {
                        translated_preview_response(id, &name, &args, &req)
                    } else {
                        ok_response(
                            id,
                            json!({
                                "content": [{
                                    "type": "text",
                                    "text": format!("Translated to daemon request: {req:?}"),
                                }],
                                "_meta": {
                                    "spotuify_daemon_request": format!("{req:?}"),
                                }
                            }),
                        )
                    }
                }
                Ok(crate::bridge::TranslatedCall::LocalJson(value)) => ok_response(
                    id,
                    json!({
                        "content": [{
                            "type": "text",
                            "text": serde_json::to_string_pretty(&value).unwrap_or_else(|err| {
                                format!("{{\"error\":\"serialization failed: {err}\"}}")
                            }),
                        }],
                        "_meta": {
                            "spotuify_response_kind": "local_json",
                        }
                    }),
                ),
                Ok(crate::bridge::TranslatedCall::PlaylistResolveTracks { plan, .. }) => {
                    ok_response(
                        id,
                        json!({
                            "content": [{
                                "type": "text",
                                "text": format!(
                                    "Translated to daemon search workflow for {} candidate(s).",
                                    plan.candidate_searches.len()
                                ),
                            }],
                            "_meta": {
                                "spotuify_daemon_workflow": "playlist_resolve_tracks",
                            }
                        }),
                    )
                }
                Ok(crate::bridge::TranslatedCall::RelatedArtists { .. }) => ok_response(
                    id,
                    json!({
                        "content": [{
                            "type": "text",
                            "text": "Translated to daemon target-resolution workflow.",
                        }],
                        "_meta": {
                            "spotuify_daemon_workflow": "related_artists",
                        }
                    }),
                ),
                Err(err) => error_response(id, RpcError::invalid_params(err.to_string())),
            }
        }
    }
}

fn translated_preview_response(
    id: Value,
    name: &str,
    args: &Value,
    request: &spotuify_protocol::Request,
) -> RpcResponse {
    let preview = destructive_preview(name, args);
    ok_response(
        id,
        json!({
            "content": [{
                "type": "text",
                "text": format!(
                    "Translated `{name}` to a read-only daemon preview request. The synchronous dispatcher did not execute it; re-invoke with `confirm: true` and `dry_run: false` after the user approves."
                ),
            }],
            "_meta": {
                "spotuify_preview_only": true,
                "spotuify_preview": preview,
                "spotuify_daemon_request": format!("{request:?}"),
            },
        }),
    )
}

pub(crate) fn is_playlist_preview_request(request: &spotuify_protocol::Request) -> bool {
    matches!(
        request,
        spotuify_protocol::Request::PlaylistCreatePreview { .. }
            | spotuify_protocol::Request::PlaylistItemsPreview { .. }
    )
}

pub(crate) fn preview_response(id: Value, name: &str, args: &Value) -> RpcResponse {
    let preview = destructive_preview(name, args);
    ok_response(
        id,
        json!({
            "content": [{
                "type": "text",
                "text": format!(
                    "Preview for destructive tool `{name}`. Not executed; re-invoke with `confirm: true` after the user approves."
                ),
            }],
            "_meta": {
                "spotuify_preview_only": true,
                "spotuify_preview": preview,
            },
        }),
    )
}

pub(crate) fn destructive_preview(name: &str, args: &Value) -> Value {
    let clean_args = args_without_confirm(args);
    let action = match name {
        "playlist_create" => "playlist-create",
        "playlist_add" => "playlist-add",
        "playlist_remove" => "playlist-remove",
        "library_save" => "library-save",
        "library_unsave" => "library-unsave",
        "queue_add" => "queue-add",
        "transfer_device" => "transfer-device",
        other => other,
    };
    let mut preview = json!({
        "tool": name,
        "action": action,
        "confirm_required": true,
        "would_execute": clean_args,
    });
    if let Some(obj) = preview.as_object_mut() {
        for key in ["name", "description", "playlist", "uri", "device", "uris"] {
            if let Some(value) = clean_args.get(key) {
                obj.insert(key.to_string(), value.clone());
            }
        }
    }
    preview
}

fn args_without_confirm(args: &Value) -> Value {
    let mut clean = args.clone();
    if let Some(obj) = clean.as_object_mut() {
        obj.remove("confirm");
    }
    clean
}

fn resources_list(id: Value) -> RpcResponse {
    let resources: Vec<Value> = ResourceCatalogue::all()
        .iter()
        .map(|r| {
            json!({
                "uri": r.uri,
                "name": r.name,
                "description": r.description,
                "mimeType": r.mime_type,
            })
        })
        .collect();
    ok_response(id, json!({ "resources": resources }))
}

fn resources_read(id: Value, params: Value) -> RpcResponse {
    let uri = match params.get("uri").and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => {
            return error_response(
                id,
                RpcError::invalid_params("resources/read: missing `uri`"),
            );
        }
    };
    match ResourceCatalogue::by_uri(&uri) {
        Some(r) => ok_response(
            id,
            json!({
                "contents": [{
                    "uri": r.uri,
                    "mimeType": r.mime_type,
                    "text": format!(
                        "{} -- live data is fetched from the daemon over IPC; transport wiring lands as a follow-up.",
                        r.description
                    ),
                }]
            }),
        ),
        None => error_response(
            id,
            RpcError::invalid_params(format!("unknown resource `{uri}`")),
        ),
    }
}

/// Minimal JSON-Schema for each tool's `arguments` object.
///
/// Returning a permissive schema (additionalProperties: true) lets the
/// daemon-side bridge do the typed validation; the schema's purpose
/// here is to tell the MCP client which inputs are expected.
fn tool_input_schema(tool: &str, catalog: Option<&ProviderCatalog>) -> Value {
    let required_props = required_props_for(tool);
    let mut properties = serde_json::Map::new();
    for prop in &required_props {
        let schema = match (tool, *prop) {
            // `playlist_create`'s `uris` is optional (handled below); only the
            // add/remove batches are required non-empty arrays.
            ("playlist_add" | "playlist_remove", "uris") => {
                json!({
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1
                })
            }
            _ => json!({ "type": "string" }),
        };
        properties.insert((*prop).to_string(), schema);
    }
    // Confirm is universal for destructive tools.
    if ToolCatalogue::by_name(tool).is_some_and(|t| t.destructive) {
        properties.insert(
            "confirm".into(),
            json!({ "type": "boolean", "default": false }),
        );
    }
    if matches!(
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
    ) {
        properties.insert(
            "provider".into(),
            json!({
                "type": "string",
                "description": "Optional provider id; omitted selects the daemon default."
            }),
        );
    }
    if tool == "search" {
        let mut source = json!({
            "type": "string",
            "description": "local, hybrid, or remote. Omitted uses local when the selected/default provider has no remote search; otherwise hybrid. Mixed provider catalogs therefore have no single static default annotation."
        });
        if let Some(default) = crate::tools::search_default_source(catalog) {
            source["default"] = json!(default);
        }
        properties.insert("source".into(), source);
    }
    if matches!(
        tool,
        "radio_start" | "playlist_create" | "playlist_add" | "playlist_remove"
    ) {
        properties.insert(
            "dry_run".into(),
            json!({ "type": "boolean", "default": false }),
        );
    }
    if tool == "playlist_create" {
        properties.insert("description".into(), json!({ "type": "string" }));
        // `uris` is optional for create: absent/empty makes an empty playlist,
        // and Track+Episode seeds are accepted (mirrors `playlist_add`).
        properties.insert(
            "uris".into(),
            json!({
                "type": "array",
                "items": { "type": "string" },
                "description": "Optional track or episode URIs to seed the playlist; omitted or empty creates an empty playlist."
            }),
        );
    }
    if tool_accepts_live_mutation_id(tool) {
        properties.insert(
            "mutation_id".into(),
            json!({
                "type": "string",
                "format": "uuid",
                "description": "Optional caller-owned UUIDv7 retry key for live execution. Retain and reuse it after a timeout. Not accepted by read-only previews."
            }),
        );
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required_props,
        "additionalProperties": true,
    })
}

fn tool_accepts_live_mutation_id(tool: &str) -> bool {
    matches!(
        tool,
        "queue_add"
            | "playlist_create"
            | "playlist_add"
            | "playlist_remove"
            | "playlist_unfollow"
            | "playlist_set_image"
            | "library_save"
            | "library_unsave"
            | "radio_start"
            | "undo_last"
    )
}

fn required_props_for(tool: &str) -> Vec<&'static str> {
    match tool {
        "search" => vec!["query"],
        "playlist_plan" => vec!["brief"],
        "playlist_resolve_tracks" => vec!["plan"],
        "play" | "play_uri" => vec!["uri"],
        "playlist_tracks" => vec!["playlist"],
        "playlist_create" => vec!["name"],
        "playlist_add" | "playlist_remove" => vec!["playlist", "uris"],
        "playlist_unfollow" => vec!["playlist"],
        "playlist_set_image" => vec!["playlist", "image_base64"],
        "related_artists" => vec!["artist"],
        "radio_start" => vec!["seed_uri"],
        "library_save" | "library_unsave" => vec!["uri"],
        "queue_add" => vec!["uri"],
        "transfer_device" => vec!["device"],
        "seek" => vec!["position_ms"],
        "volume" => vec!["percent"],
        "shuffle" => vec!["on"],
        "repeat" => vec!["mode"],
        _ => vec![],
    }
}
