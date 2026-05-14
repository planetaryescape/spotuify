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

use crate::confirm::{decide, Authorized};
use crate::resources::ResourceCatalogue;
use crate::tools::{ToolCatalogue, ToolManifest};

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
    let id = request.id.unwrap_or(Value::Null);

    if request.jsonrpc != "2.0" {
        return error_response(
            id,
            RpcError::invalid_request("only JSON-RPC 2.0 is supported"),
        );
    }

    match request.method.as_str() {
        "initialize" => initialize(id, request.params),
        "tools/list" => tools_list(id),
        "tools/call" => tools_call(id, request.params),
        "resources/list" => resources_list(id),
        "resources/read" => resources_read(id, request.params),
        "ping" => ok_response(id, json!({})),
        other => error_response(
            id,
            RpcError::method_not_found(format!("unknown method `{other}`")),
        ),
    }
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

fn tools_list(id: Value) -> RpcResponse {
    let tools: Vec<Value> = ToolCatalogue::all()
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": tool_input_schema(t.name),
            })
        })
        .collect();
    ok_response(id, json!({ "tools": tools }))
}

fn tools_call(id: Value, params: Value) -> RpcResponse {
    let name = match params.get("name").and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => {
            return error_response(id, RpcError::invalid_params("tools/call: missing `name`"));
        }
    };
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let confirm = args.get("confirm").and_then(Value::as_bool);

    match decide(&name, confirm) {
        Err(err) => error_response(id, RpcError::invalid_request(err.to_string())),
        Ok(Authorized::PreviewOnly) => ok_response(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": format!(
                        "Tool `{name}` is destructive; re-invoke with `confirm: true` after the user approves."
                    ),
                }],
                "isError": true,
                "_meta": { "spotuify_preview_only": true },
            }),
        ),
        Ok(Authorized::Execute) => {
            // The bridge translates to a daemon Request; the wire
            // layer (Phase 8 follow-up) actually dispatches it. For
            // now we report the translated form so MCP clients see a
            // consistent envelope.
            match crate::bridge::translate(&name, &args) {
                Ok(crate::bridge::TranslatedCall::Request(req)) => ok_response(
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
                ),
                Ok(crate::bridge::TranslatedCall::LocalDeferred(label)) => ok_response(
                    id,
                    json!({
                        "content": [{
                            "type": "text",
                            "text": format!("Deferred: {label} (gated on a later phase)."),
                        }],
                        "isError": true,
                    }),
                ),
                Err(err) => error_response(id, RpcError::invalid_params(err.to_string())),
            }
        }
    }
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
fn tool_input_schema(tool: &str) -> Value {
    let required_props = required_props_for(tool);
    let mut properties = serde_json::Map::new();
    for prop in &required_props {
        properties.insert((*prop).to_string(), json!({ "type": "string" }));
    }
    // Confirm is universal for destructive tools.
    if ToolCatalogue::by_name(tool)
        .map(|t| t.destructive)
        .unwrap_or(false)
    {
        properties.insert(
            "confirm".into(),
            json!({ "type": "boolean", "default": false }),
        );
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required_props,
        "additionalProperties": true,
    })
}

fn required_props_for(tool: &str) -> Vec<&'static str> {
    match tool {
        "search" => vec!["query"],
        "play" | "play_uri" => vec!["uri"],
        "playlist_tracks" => vec!["playlist"],
        "playlist_create" => vec!["name"],
        "playlist_add" | "playlist_remove" => vec!["playlist", "uris"],
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
