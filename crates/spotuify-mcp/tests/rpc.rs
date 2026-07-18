#![allow(clippy::panic, clippy::unwrap_used)]

//! Phase 8.6/8.7 — MCP JSON-RPC dispatch tests.

use serde_json::{json, Value};
use spotuify_core::{
    LibraryCaps, MediaKind, PlaylistCaps, ProviderCaps, ProviderCatalog, ProviderDescriptor,
    ProviderExtrasCaps, ProviderId, SearchCaps, TransportCaps, UriScheme,
};
use spotuify_mcp::{dispatch, rpc::dispatch_with_catalog, RpcRequest};

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

fn catalog(capabilities: ProviderCaps) -> ProviderCatalog {
    let provider = ProviderId::new("music").unwrap();
    ProviderCatalog {
        default_provider: Some(provider.clone()),
        providers: vec![ProviderDescriptor {
            id: provider,
            uri_scheme: UriScheme::new("music").unwrap(),
            display_name: "Music".to_string(),
            capabilities,
            is_default: true,
        }],
    }
}

fn listed_names(catalog: Option<&ProviderCatalog>) -> Vec<String> {
    let response = dispatch_with_catalog(request("tools/list", json!({}), 90), catalog);
    response.result.unwrap()["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect()
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
    for name in [
        "search",
        "playlists_list",
        "library_list",
        "playlist_resolve_tracks",
        "playlist_create",
        "playlist_tracks",
        "playlist_add",
        "playlist_remove",
        "playlist_unfollow",
        "playlist_set_image",
        "related_artists",
    ] {
        let tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
        assert!(
            tool["inputSchema"]["properties"].get("provider").is_some(),
            "{name} must advertise its provider scope"
        );
    }
    let search = tools.iter().find(|tool| tool["name"] == "search").unwrap();
    assert_eq!(
        search["inputSchema"]["properties"]["source"]["default"],
        "hybrid"
    );
    let related = tools
        .iter()
        .find(|tool| tool["name"] == "related_artists")
        .unwrap();
    assert!(related["inputSchema"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .any(|field| field == "artist"));

    for name in ["playlist_add", "playlist_remove"] {
        let tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
        assert_eq!(
            tool["inputSchema"]["properties"]["uris"]["type"], "array",
            "{name} must advertise uris as an array"
        );
        assert_eq!(
            tool["inputSchema"]["properties"]["uris"]["items"]["type"],
            "string"
        );
        assert_eq!(
            tool["inputSchema"]["properties"]["uris"]["minItems"], 1,
            "{name} must reject empty mutation batches"
        );
        assert!(tool["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field == "uris"));
        assert_eq!(
            tool["inputSchema"]["properties"]["dry_run"]["type"],
            "boolean"
        );
        assert_eq!(
            tool["inputSchema"]["properties"]["dry_run"]["default"],
            false
        );
    }
    let create = tools
        .iter()
        .find(|tool| tool["name"] == "playlist_create")
        .unwrap();
    assert_eq!(
        create["inputSchema"]["properties"]["description"]["type"],
        "string"
    );
    // playlist_create's uris seed is optional (empty makes an empty playlist),
    // so it is advertised as an array but not required and not minItems-gated.
    assert_eq!(
        create["inputSchema"]["properties"]["uris"]["type"], "array",
        "playlist_create must advertise uris as an array"
    );
    assert!(
        create["inputSchema"]["properties"]["uris"]
            .get("minItems")
            .is_none(),
        "playlist_create must not force a non-empty uris batch"
    );
    assert!(
        !create["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field == "uris"),
        "playlist_create must not require uris"
    );
    assert_eq!(
        create["inputSchema"]["properties"]["dry_run"]["type"],
        "boolean"
    );
    for name in [
        "queue_add",
        "playlist_create",
        "playlist_add",
        "playlist_remove",
        "playlist_unfollow",
        "playlist_set_image",
        "library_save",
        "library_unsave",
        "radio_start",
        "undo_last",
    ] {
        let tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
        assert_eq!(
            tool["inputSchema"]["properties"]["mutation_id"]["format"], "uuid",
            "{name} must advertise its caller-owned live retry key"
        );
    }
    let search = tools.iter().find(|tool| tool["name"] == "search").unwrap();
    assert!(search["inputSchema"]["properties"]
        .get("mutation_id")
        .is_none());

    for (name, required) in [
        ("playlist_unfollow", &["playlist"][..]),
        ("playlist_set_image", &["playlist", "image_base64"][..]),
    ] {
        let tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
        let schema = &tool["inputSchema"];
        for field in required {
            assert!(
                schema["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|value| value == field),
                "{name} must require {field}"
            );
            assert!(
                schema["properties"].get(*field).is_some(),
                "{name} must declare {field}"
            );
        }
    }
}

#[test]
fn playlist_mutation_tools_reject_non_boolean_dry_run_before_dispatch() {
    let catalog = catalog(ProviderCaps {
        playlists: PlaylistCaps {
            add: true,
            remove: true,
            ..Default::default()
        },
        ..Default::default()
    });
    for (id, name, arguments) in [
        (
            960,
            "playlist_create",
            json!({"name": "Focus", "uris": ["music:track:one"]}),
        ),
        (
            961,
            "playlist_add",
            json!({
                "playlist": "music:playlist:focus",
                "uris": ["music:track:one"]
            }),
        ),
        (
            962,
            "playlist_remove",
            json!({
                "playlist": "music:playlist:focus",
                "uris": ["music:track:one"]
            }),
        ),
    ] {
        let mut arguments = arguments;
        arguments["dry_run"] = json!("true");
        arguments["confirm"] = json!(true);
        let response = dispatch_with_catalog(
            request(
                "tools/call",
                json!({
                    "name": name,
                    "arguments": arguments
                }),
                id,
            ),
            Some(&catalog),
        );
        let error = response.error.expect("malformed dry_run must be rejected");
        assert_eq!(error.code, -32602);
        assert!(error.message.contains("`dry_run` must be boolean"));
    }
}

#[test]
fn explicit_empty_catalog_hides_and_rejects_provider_tools() {
    let catalog = ProviderCatalog::default();
    let names = listed_names(Some(&catalog));
    assert!(names.contains(&"search".to_string()));
    assert!(!names.contains(&"play".to_string()));
    // Lyrics stays visible even with an empty catalog: the LRCLIB fallback
    // needs no provider.
    assert!(names.contains(&"lyrics".to_string()));
    assert!(names.contains(&"playlist_plan".to_string()));
    assert!(names.contains(&"analytics_top".to_string()));
    assert!(names.contains(&"ops_log".to_string()));

    let listed = dispatch_with_catalog(request("tools/list", json!({}), 910), Some(&catalog));
    let search = listed.result.unwrap()["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "search")
        .unwrap()
        .clone();
    assert_eq!(
        search["inputSchema"]["properties"]["source"]["default"],
        "local"
    );

    let minimal = dispatch_with_catalog(
        request(
            "tools/call",
            json!({"name": "search", "arguments": {"query": "x"}}),
            9101,
        ),
        Some(&catalog),
    );
    assert!(minimal.error.is_none());
    assert!(minimal.result.unwrap()["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("source: Local"));

    for source in ["hybrid", "remote"] {
        let response = dispatch_with_catalog(
            request(
                "tools/call",
                json!({"name": "search", "arguments": {"query": "x", "source": source}}),
                91,
            ),
            Some(&catalog),
        );
        assert_eq!(response.error.unwrap().code, -32600);
    }

    let local = dispatch_with_catalog(
        request(
            "tools/call",
            json!({"name": "search", "arguments": {"query": "x", "source": "local"}}),
            911,
        ),
        Some(&catalog),
    );
    assert!(local.error.is_none());

    // Capability denial must happen before the destructive preview path.
    let response = dispatch_with_catalog(
        request(
            "tools/call",
            json!({"name": "playlist_create", "arguments": {"name": "x"}}),
            92,
        ),
        Some(&catalog),
    );
    assert_eq!(response.error.unwrap().code, -32600);
}

#[test]
fn mixed_search_catalog_omits_static_schema_default_and_routes_by_provider() {
    let local = ProviderId::new("localmusic").unwrap();
    let remote = ProviderId::new("remotemusic").unwrap();
    let mut remote_caps = ProviderCaps::default();
    remote_caps.search.remote = true;
    remote_caps.search.kinds = vec![MediaKind::Track];
    let catalog = ProviderCatalog {
        default_provider: Some(local.clone()),
        providers: vec![
            ProviderDescriptor {
                id: local,
                uri_scheme: UriScheme::new("localmusic").unwrap(),
                display_name: "Local".to_string(),
                capabilities: ProviderCaps::default(),
                is_default: true,
            },
            ProviderDescriptor {
                id: remote.clone(),
                uri_scheme: UriScheme::new("remotemusic").unwrap(),
                display_name: "Remote".to_string(),
                capabilities: remote_caps,
                is_default: false,
            },
        ],
    };

    let listed = dispatch_with_catalog(request("tools/list", json!({}), 912), Some(&catalog));
    let tools = listed.result.unwrap()["tools"].as_array().unwrap().clone();
    let search = tools.iter().find(|tool| tool["name"] == "search").unwrap();
    assert!(search["inputSchema"]["properties"]["source"]
        .get("default")
        .is_none());

    let local_call = dispatch_with_catalog(
        request(
            "tools/call",
            json!({"name": "search", "arguments": {"query": "x"}}),
            913,
        ),
        Some(&catalog),
    );
    assert!(local_call.result.unwrap()["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("source: Local"));

    let remote_call = dispatch_with_catalog(
        request(
            "tools/call",
            json!({"name": "search", "arguments": {"query": "x", "provider": remote.as_str()}}),
            914,
        ),
        Some(&catalog),
    );
    assert!(remote_call.result.unwrap()["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("source: Hybrid"));
}

#[test]
fn provider_scoped_tool_visibility_and_calls_consider_nondefault_providers() {
    let mut catalog = catalog(ProviderCaps::default());
    catalog.providers.push(ProviderDescriptor {
        id: ProviderId::new("searchable").unwrap(),
        uri_scheme: UriScheme::new("searchable").unwrap(),
        display_name: "Searchable".to_string(),
        capabilities: ProviderCaps {
            search: SearchCaps {
                remote: true,
                kinds: vec![MediaKind::Track],
                ..Default::default()
            },
            playlists: PlaylistCaps {
                list: true,
                ..Default::default()
            },
            transport: None,
            ..Default::default()
        },
        is_default: false,
    });

    let names = listed_names(Some(&catalog));
    assert!(names.contains(&"search".to_string()));
    assert!(names.contains(&"playlists_list".to_string()));
    assert!(!names.contains(&"play".to_string()));

    for (name, arguments) in [
        (
            "search",
            json!({"query": "x", "source": "remote", "provider": "searchable"}),
        ),
        ("playlists_list", json!({"provider": "searchable"})),
    ] {
        let response = dispatch_with_catalog(
            request(
                "tools/call",
                json!({"name": name, "arguments": arguments}),
                912,
            ),
            Some(&catalog),
        );
        assert!(
            response.error.is_none(),
            "{name} should route: {:?}",
            response.error
        );
    }
}

#[test]
fn active_transport_tools_consider_nondefault_providers_and_defer_routing() {
    let mut catalog = catalog(ProviderCaps::default());
    catalog.providers.push(ProviderDescriptor {
        id: ProviderId::new("secondary").unwrap(),
        uri_scheme: UriScheme::new("secondary").unwrap(),
        display_name: "Secondary".to_string(),
        capabilities: ProviderCaps {
            transport: Some(TransportCaps {
                pause: true,
                ..Default::default()
            }),
            ..Default::default()
        },
        is_default: false,
    });

    let names = listed_names(Some(&catalog));
    assert!(names.contains(&"pause".to_string()));
    assert!(!names.contains(&"next".to_string()));

    let pause = dispatch_with_catalog(
        request("tools/call", json!({"name": "pause", "arguments": {}}), 913),
        Some(&catalog),
    );
    assert!(
        pause.error.is_none(),
        "the daemon must decide whether the supporting provider is active: {:?}",
        pause.error
    );
}

#[test]
fn transportless_catalog_keeps_metadata_tools_but_hides_and_rejects_transport() {
    let catalog = catalog(ProviderCaps {
        search: SearchCaps {
            remote: true,
            kinds: vec![MediaKind::Track],
            ..Default::default()
        },
        library: LibraryCaps {
            read_kinds: vec![MediaKind::Track],
            ..Default::default()
        },
        playlists: PlaylistCaps {
            list: true,
            item_read: true,
            ..Default::default()
        },
        transport: None,
        ..Default::default()
    });
    let names = listed_names(Some(&catalog));
    for name in [
        "search",
        "playlists_list",
        "playlist_tracks",
        "library_list",
    ] {
        assert!(names.contains(&name.to_string()), "missing {name}");
    }
    for name in ["now_playing", "devices_list", "queue_show", "play", "pause"] {
        assert!(!names.contains(&name.to_string()), "unexpected {name}");
    }

    let denied = dispatch_with_catalog(
        request(
            "tools/call",
            json!({"name": "play", "arguments": {"uri": "music:track:one"}}),
            93,
        ),
        Some(&catalog),
    );
    assert_eq!(denied.error.unwrap().code, -32600);

    let allowed = dispatch_with_catalog(
        request(
            "tools/call",
            json!({"name": "search", "arguments": {"query": "x"}}),
            94,
        ),
        Some(&catalog),
    );
    assert!(allowed.error.is_none());
}

#[test]
fn artist_library_save_is_gated_by_follow_capability() {
    // A follow-only provider (Artist in follow_kinds, save_kinds empty) must
    // still expose library_save/unsave and accept an artist URI: artist likes
    // route to follow.
    let follow_only = catalog(ProviderCaps {
        library: LibraryCaps {
            follow_kinds: vec![MediaKind::Artist],
            ..Default::default()
        },
        ..Default::default()
    });
    let names = listed_names(Some(&follow_only));
    assert!(names.contains(&"library_save".to_string()));
    assert!(names.contains(&"library_unsave".to_string()));

    let allowed = dispatch_with_catalog(
        request(
            "tools/call",
            json!({
                "name": "library_save",
                "arguments": {"uri": "music:artist:one", "confirm": true}
            }),
            970,
        ),
        Some(&follow_only),
    );
    assert!(
        allowed.error.is_none(),
        "artist save must be allowed via the follow capability: {:?}",
        allowed.error
    );

    // A provider with neither save nor follow hides the tools and rejects the
    // call.
    let neither = catalog(ProviderCaps::default());
    assert!(!listed_names(Some(&neither)).contains(&"library_save".to_string()));
    let denied = dispatch_with_catalog(
        request(
            "tools/call",
            json!({
                "name": "library_save",
                "arguments": {"uri": "music:artist:one", "confirm": true}
            }),
            971,
        ),
        Some(&neither),
    );
    assert_eq!(denied.error.unwrap().code, -32600);
}

#[test]
fn known_catalog_rejects_unknown_provider_on_listed_tool_call() {
    let catalog = catalog(ProviderCaps {
        search: SearchCaps {
            remote: true,
            kinds: vec![MediaKind::Track],
            ..Default::default()
        },
        transport: Some(TransportCaps::default()),
        ..Default::default()
    });
    let response = dispatch_with_catalog(
        request(
            "tools/call",
            json!({
                "name": "search",
                "arguments": {"query": "x", "provider": "missing"}
            }),
            95,
        ),
        Some(&catalog),
    );
    let error = response.error.unwrap();
    assert_eq!(error.code, -32600);
    assert!(error.message.contains("unknown provider `missing`"));
}

#[test]
fn unknown_catalog_rejects_every_explicit_provider_scope() {
    for (id, name, arguments) in [
        (951, "library_list", json!({"provider": "music"})),
        (
            952,
            "search",
            json!({"query": "x", "source": "local", "provider": "music"}),
        ),
        (
            953,
            "playlist_remove",
            json!({
                "playlist": "music:playlist:one",
                "uris": ["music:track:one"],
                "provider": "music"
            }),
        ),
    ] {
        let response = dispatch_with_catalog(
            request(
                "tools/call",
                json!({"name": name, "arguments": arguments}),
                id,
            ),
            None,
        );
        let error = response.error.expect("explicit provider must fail closed");
        assert_eq!(error.code, -32600);
        assert!(error.message.contains("requires a daemon provider catalog"));
    }

    let providerless = dispatch_with_catalog(
        request(
            "tools/call",
            json!({"name": "library_list", "arguments": {}}),
            954,
        ),
        None,
    );
    assert!(providerless.error.is_none());
}

#[test]
fn provider_extra_tools_follow_their_semantic_capabilities() {
    let related_catalog = catalog(ProviderCaps {
        extras: ProviderExtrasCaps {
            related_artists: true,
            ..Default::default()
        },
        ..Default::default()
    });
    let names = listed_names(Some(&related_catalog));
    assert!(names.contains(&"related_artists".to_string()));
    assert!(!names.contains(&"radio_start".to_string()));

    let radio_catalog = catalog(ProviderCaps {
        extras: ProviderExtrasCaps {
            radio: true,
            ..Default::default()
        },
        ..Default::default()
    });
    let names = listed_names(Some(&radio_catalog));
    assert!(names.contains(&"radio_start".to_string()));
    assert!(!names.contains(&"related_artists".to_string()));
}

#[test]
fn transportless_radio_is_previewable_but_live_dispatch_requires_queue_add() {
    let transportless = catalog(ProviderCaps {
        extras: ProviderExtrasCaps {
            radio: true,
            ..Default::default()
        },
        ..Default::default()
    });
    let dry_run = dispatch_with_catalog(
        request(
            "tools/call",
            json!({
                "name": "radio_start",
                "arguments": {"seed_uri": "music:track:one", "dry_run": true}
            }),
            951,
        ),
        Some(&transportless),
    );
    assert!(dry_run.error.is_none());

    let live = dispatch_with_catalog(
        request(
            "tools/call",
            json!({
                "name": "radio_start",
                "arguments": {"seed_uri": "music:track:one"}
            }),
            952,
        ),
        Some(&transportless),
    );
    let error = live.error.unwrap();
    assert_eq!(error.code, -32600);
    assert!(error
        .message
        .contains("does not support radio queue additions"));

    let queue_only = catalog(ProviderCaps {
        transport: Some(TransportCaps {
            queue_read: true,
            queue_add: true,
            ..Default::default()
        }),
        extras: ProviderExtrasCaps {
            radio: true,
            ..Default::default()
        },
        ..Default::default()
    });
    let live = dispatch_with_catalog(
        request(
            "tools/call",
            json!({
                "name": "radio_start",
                "arguments": {"seed_uri": "music:track:one"}
            }),
            953,
        ),
        Some(&queue_only),
    );
    assert!(live.error.is_none());

    let playable = catalog(ProviderCaps {
        transport: Some(TransportCaps {
            play: true,
            queue_read: true,
            queue_add: true,
            ..Default::default()
        }),
        extras: ProviderExtrasCaps {
            radio: true,
            ..Default::default()
        },
        ..Default::default()
    });
    let live = dispatch_with_catalog(
        request(
            "tools/call",
            json!({
                "name": "radio_start",
                "arguments": {"seed_uri": "music:track:one"}
            }),
            954,
        ),
        Some(&playable),
    );
    assert!(live.error.is_none());
}

#[test]
fn radio_schema_and_call_validation_keep_dry_run_type_safe() {
    let catalog = catalog(ProviderCaps {
        transport: Some(TransportCaps {
            queue_read: true,
            queue_add: true,
            ..Default::default()
        }),
        extras: ProviderExtrasCaps {
            radio: true,
            ..Default::default()
        },
        ..Default::default()
    });
    let listed = dispatch_with_catalog(request("tools/list", json!({}), 955), Some(&catalog));
    let tools = listed.result.unwrap()["tools"].as_array().unwrap().clone();
    let radio = tools
        .iter()
        .find(|tool| tool["name"] == "radio_start")
        .unwrap();
    assert_eq!(radio["inputSchema"]["required"], json!(["seed_uri"]));
    assert_eq!(
        radio["inputSchema"]["properties"]["dry_run"]["type"],
        "boolean"
    );

    for (id, invalid) in [
        (956, json!("true")),
        (957, Value::Null),
        (958, json!({ "unexpected": true })),
    ] {
        let response = dispatch_with_catalog(
            request(
                "tools/call",
                json!({
                    "name": "radio_start",
                    "arguments": {
                        "seed_uri": "music:track:one",
                        "dry_run": invalid
                    }
                }),
                id,
            ),
            Some(&catalog),
        );
        let error = response.error.unwrap();
        assert_eq!(error.code, -32602);
        assert!(error.message.contains("`dry_run` must be boolean"));
    }
}

#[test]
fn uri_authority_selects_nondefault_provider_and_unknown_schemes_fail_closed() {
    let mut catalog = catalog(ProviderCaps::default());
    catalog.providers.push(ProviderDescriptor {
        id: ProviderId::new("playable").unwrap(),
        uri_scheme: UriScheme::new("playable").unwrap(),
        display_name: "Playable".to_string(),
        capabilities: ProviderCaps {
            transport: Some(TransportCaps {
                play: true,
                ..Default::default()
            }),
            ..Default::default()
        },
        is_default: false,
    });

    let names = listed_names(Some(&catalog));
    assert!(names.contains(&"play".to_string()));

    let allowed = dispatch_with_catalog(
        request(
            "tools/call",
            json!({"name": "play", "arguments": {"uri": "playable:track:one"}}),
            96,
        ),
        Some(&catalog),
    );
    assert!(allowed.error.is_none());

    let denied = dispatch_with_catalog(
        request(
            "tools/call",
            json!({"name": "play", "arguments": {"uri": "missing:track:one"}}),
            97,
        ),
        Some(&catalog),
    );
    let error = denied.error.unwrap();
    assert_eq!(error.code, -32600);
    assert!(error
        .message
        .contains("unknown provider URI scheme `missing`"));
}

#[test]
fn playlist_scope_rejects_provider_uri_ownership_conflicts() {
    let playlist_caps = PlaylistCaps {
        list: true,
        item_read: true,
        ..Default::default()
    };
    let mut catalog = catalog(ProviderCaps {
        playlists: playlist_caps.clone(),
        ..Default::default()
    });
    catalog.providers.push(ProviderDescriptor {
        id: ProviderId::new("other").unwrap(),
        uri_scheme: UriScheme::new("other").unwrap(),
        display_name: "Other".to_string(),
        capabilities: ProviderCaps {
            playlists: playlist_caps,
            ..Default::default()
        },
        is_default: false,
    });

    let denied = dispatch_with_catalog(
        request(
            "tools/call",
            json!({
                "name": "playlist_tracks",
                "arguments": {
                    "playlist": "other:playlist:one",
                    "provider": "music"
                }
            }),
            971,
        ),
        Some(&catalog),
    );
    let error = denied.error.unwrap();
    assert_eq!(error.code, -32600);
    assert!(error.message.contains("conflicts with URI scheme `other`"));
}

#[test]
fn search_call_checks_requested_media_kind_not_only_remote_boolean() {
    let catalog = catalog(ProviderCaps {
        search: SearchCaps {
            remote: true,
            kinds: vec![MediaKind::Track],
            ..Default::default()
        },
        ..Default::default()
    });
    let response = dispatch_with_catalog(
        request(
            "tools/call",
            json!({"name": "search", "arguments": {"query": "x", "kind": "episode"}}),
            98,
        ),
        Some(&catalog),
    );
    assert_eq!(response.error.unwrap().code, -32600);
}

#[test]
fn search_all_accepts_a_nonempty_provider_intersection() {
    let catalog = catalog(ProviderCaps {
        search: SearchCaps {
            remote: true,
            kinds: vec![MediaKind::Track],
            ..Default::default()
        },
        ..Default::default()
    });
    let all = dispatch_with_catalog(
        request(
            "tools/call",
            json!({"name": "search", "arguments": {"query": "x", "kind": "all"}}),
            981,
        ),
        Some(&catalog),
    );
    assert!(all.error.is_none());

    let exact = dispatch_with_catalog(
        request(
            "tools/call",
            json!({"name": "search", "arguments": {"query": "x", "kind": "episode"}}),
            982,
        ),
        Some(&catalog),
    );
    assert_eq!(exact.error.unwrap().code, -32600);
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
            "arguments": {
                "name": "Focus",
                "description": "deep work",
                "uris": ["spotify:track:one"]
            }
        }),
        7,
    ));
    let preview_meta = result["_meta"]["spotuify_preview_only"].as_bool();
    assert_eq!(preview_meta, Some(true));
    let preview = &result["_meta"]["spotuify_preview"];
    assert_eq!(preview["action"], "playlist-create");
    assert_eq!(preview["name"], "Focus");
    assert_eq!(preview["uris"][0], "spotify:track:one");
    assert_eq!(preview["confirm_required"], true);
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("confirm: true"), "text should guide LLM");
}

#[test]
fn tools_call_destructive_with_confirm_executes() {
    let result = ok_value(request(
        "tools/call",
        json!({
            "name": "playlist_create",
            "arguments": {
                "name": "Focus",
                "uris": ["spotify:track:one"],
                "confirm": true
            }
        }),
        8,
    ));
    // Got past confirm; text should reflect translated Request.
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("PlaylistCreate"), "got text: {text}");
}

#[test]
fn playlist_previews_translate_and_validate_before_sync_preview_response() {
    for (id, name, arguments, expected_request) in [
        (
            801,
            "playlist_create",
            json!({"name": "Focus", "uris": ["spotify:track:one"]}),
            "PlaylistCreatePreview",
        ),
        (
            802,
            "playlist_add",
            json!({
                "playlist": "spotify:playlist:focus",
                "uris": ["spotify:track:one"]
            }),
            "PlaylistItemsPreview",
        ),
        (
            803,
            "playlist_remove",
            json!({
                "playlist": "spotify:playlist:focus",
                "uris": ["spotify:track:one"],
                "confirm": true,
                "dry_run": true
            }),
            "PlaylistItemsPreview",
        ),
    ] {
        let result = ok_value(request(
            "tools/call",
            json!({"name": name, "arguments": arguments}),
            id,
        ));
        assert_eq!(result["_meta"]["spotuify_preview_only"], true);
        assert!(result["_meta"]["spotuify_daemon_request"]
            .as_str()
            .is_some_and(|request| request.contains(expected_request)));
    }

    // Name-only and empty-uris now create an empty playlist and still preview
    // as a read-only PlaylistCreatePreview.
    for (id, arguments) in [
        (804, json!({"name": "Focus"})),
        (805, json!({"name": "Focus", "uris": []})),
    ] {
        let result = ok_value(request(
            "tools/call",
            json!({"name": "playlist_create", "arguments": arguments}),
            id,
        ));
        assert_eq!(result["_meta"]["spotuify_preview_only"], true);
        assert!(result["_meta"]["spotuify_daemon_request"]
            .as_str()
            .is_some_and(|request| request.contains("PlaylistCreatePreview")));
    }

    // A malformed uris item is still rejected before dispatch.
    let response = dispatch(request(
        "tools/call",
        json!({
            "name": "playlist_create",
            "arguments": {"name": "Focus", "uris": ["spotify:track:one", null]}
        }),
        806,
    ));
    assert_eq!(response.error.unwrap().code, -32602);
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
fn mercury_tools_are_advertised_as_callable() {
    // Reversed from the old deferral: related_artists + radio_start are
    // live tools now. They appear in the catalogue, and calling them with
    // a missing required arg is an arg error — not "tool not found".
    let result = ok_value(request("tools/list", json!({}), 2));
    let names: Vec<&str> = result
        .get("tools")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .collect();
    assert!(names.contains(&"related_artists"));
    assert!(names.contains(&"radio_start"));

    let resp = dispatch(request(
        "tools/call",
        json!({"name": "radio_start", "arguments": {}}),
        14,
    ));
    let err = resp.error.expect("missing required arg should error");
    assert!(
        !err.message.contains("not found"),
        "radio_start is a known tool now, got: {}",
        err.message
    );
}

#[test]
fn resources_subscribe_accepts_known_uri_and_rejects_unknown() {
    // The capability advertises resources.subscribe:true; the handlers
    // must exist (no method-not-found) and validate the URI.
    let ok = dispatch(request(
        "resources/subscribe",
        json!({"uri": "spotuify://playback"}),
        20,
    ));
    assert!(ok.error.is_none(), "subscribing a known resource succeeds");

    let bad = dispatch(request(
        "resources/subscribe",
        json!({"uri": "spotuify://nope"}),
        21,
    ));
    assert!(bad.error.is_some(), "unknown resource uri is rejected");

    let unsub = dispatch(request(
        "resources/unsubscribe",
        json!({"uri": "spotuify://playback"}),
        22,
    ));
    assert!(unsub.error.is_none(), "unsubscribe succeeds");
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
