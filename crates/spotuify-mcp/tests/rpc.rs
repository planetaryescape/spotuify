//! Phase 8.6/8.7 — MCP JSON-RPC dispatch tests.

use serde_json::{json, Value};
use spotuify_mcp::{dispatch, RpcRequest};

fn request(method: &str, params: Value, id: i64) -> RpcRequest {
    RpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(id)),
        method: method.to_string(),
        params,
    }
}

fn ok_value(req: RpcRequest) -> Value {
    let resp = dispatch(req);
    assert!(resp.error.is_none(), "expected ok, got {:?}", resp.error);
    resp.result.expect("ok response has result")
}

fn err_code(req: RpcRequest) -> i32 {
    let resp = dispatch(req);
    resp.error.expect("expected error").code
}

#[test]
fn initialize_returns_protocol_version_and_capabilities() {
    let result = ok_value(request("initialize", json!({}), 1));
    assert_eq!(
        result.get("protocolVersion").and_then(Value::as_str),
        Some("2024-11-05")
    );
    let caps = result.get("capabilities").unwrap();
    assert!(caps.get("tools").is_some());
    assert!(caps.get("resources").is_some());
    assert_eq!(
        caps["resources"]["subscribe"].as_bool(),
        Some(true),
        "Phase 6.9 event stream → MCP resource subscription"
    );
}

#[test]
fn tools_list_returns_full_catalogue() {
    let result = ok_value(request("tools/list", json!({}), 2));
    let tools = result.get("tools").and_then(Value::as_array).unwrap();
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();

    // Spot-check tools from each ToolKind bucket.
    assert!(names.contains(&"search"));
    assert!(names.contains(&"play"));
    assert!(names.contains(&"playlist_create"));
    assert!(names.contains(&"undo_last"));
    assert!(names.contains(&"lyrics"));
    assert!(names.contains(&"analytics_top"));
    assert!(names.contains(&"ops_log"));

    // Destructive tools advertise `confirm` in their schema.
    let create = tools
        .iter()
        .find(|t| t["name"] == "playlist_create")
        .unwrap();
    let props = &create["inputSchema"]["properties"];
    assert!(
        props.get("confirm").is_some(),
        "confirm should be in schema"
    );
}

#[test]
fn resources_list_returns_full_catalogue() {
    let result = ok_value(request("resources/list", json!({}), 3));
    let resources = result.get("resources").and_then(Value::as_array).unwrap();
    let uris: Vec<&str> = resources
        .iter()
        .filter_map(|r| r.get("uri").and_then(Value::as_str))
        .collect();
    assert!(uris.contains(&"spotuify://playback"));
    assert!(uris.contains(&"spotuify://devices"));
    assert!(uris.contains(&"spotuify://playlists"));
}

#[test]
fn resources_read_known_uri_returns_contents() {
    let result = ok_value(request(
        "resources/read",
        json!({"uri": "spotuify://playback"}),
        4,
    ));
    let contents = result.get("contents").and_then(Value::as_array).unwrap();
    assert!(!contents.is_empty());
    assert_eq!(contents[0]["uri"], "spotuify://playback");
    assert_eq!(contents[0]["mimeType"], "application/json");
}

#[test]
fn resources_read_unknown_uri_returns_invalid_params() {
    let code = err_code(request(
        "resources/read",
        json!({"uri": "spotuify://does-not-exist"}),
        5,
    ));
    assert_eq!(code, -32602);
}

#[test]
fn tools_call_read_only_executes_without_confirm() {
    let result = ok_value(request(
        "tools/call",
        json!({"name": "search", "arguments": {"query": "luther"}}),
        6,
    ));
    // Translated to a Request::Search; the response text mentions it.
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Search"), "got text: {text}");
}

#[test]
fn tools_call_destructive_without_confirm_returns_preview() {
    let result = ok_value(request(
        "tools/call",
        json!({
            "name": "playlist_create",
            "arguments": { "name": "Focus" }
        }),
        7,
    ));
    let is_error = result.get("isError").and_then(Value::as_bool);
    assert_eq!(is_error, Some(true), "preview path sets isError");
    let preview_meta = result["_meta"]["spotuify_preview_only"].as_bool();
    assert_eq!(preview_meta, Some(true));
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("confirm: true"), "text should guide LLM");
}

#[test]
fn tools_call_destructive_with_confirm_executes() {
    let result = ok_value(request(
        "tools/call",
        json!({
            "name": "playlist_create",
            "arguments": { "name": "Focus", "confirm": true }
        }),
        8,
    ));
    // Got past confirm; text should reflect translated Request.
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("PlaylistCreate"), "got text: {text}");
}

#[test]
fn tools_call_unknown_tool_returns_invalid_request() {
    let code = err_code(request("tools/call", json!({"name": "not_a_tool"}), 9));
    assert_eq!(code, -32600);
}

#[test]
fn tools_call_missing_required_arg_returns_invalid_params() {
    let code = err_code(request("tools/call", json!({"name": "play_uri"}), 10));
    assert_eq!(code, -32602);
}

#[test]
fn unknown_method_returns_method_not_found() {
    let code = err_code(request("unknown/method", json!({}), 11));
    assert_eq!(code, -32601);
}

#[test]
fn wrong_jsonrpc_version_returns_invalid_request() {
    let req = RpcRequest {
        jsonrpc: "1.0".to_string(),
        id: Some(json!(12)),
        method: "initialize".to_string(),
        params: json!({}),
    };
    assert_eq!(err_code(req), -32600);
}

#[test]
fn ping_returns_empty_ok() {
    let result = ok_value(request("ping", json!({}), 13));
    assert_eq!(result, json!({}));
}

#[test]
fn deferred_tool_returns_ok_with_local_deferred_marker() {
    let result = ok_value(request(
        "tools/call",
        json!({"name": "lyrics", "arguments": {}}),
        14,
    ));
    let is_error = result.get("isError").and_then(Value::as_bool);
    assert_eq!(is_error, Some(true), "deferred tools mark isError");
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.to_lowercase().contains("deferred"), "got text: {text}");
}

#[test]
fn null_id_round_trips_in_response() {
    let req = RpcRequest {
        jsonrpc: "2.0".to_string(),
        id: None,
        method: "ping".to_string(),
        params: json!({}),
    };
    let resp = dispatch(req);
    assert_eq!(resp.id, Value::Null);
}
