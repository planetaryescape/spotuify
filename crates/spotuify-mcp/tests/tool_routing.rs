#![allow(clippy::panic, clippy::unwrap_used)]

//! MCP tool catalogue + bridge + confirmation tests.

use serde_json::json;
use spotuify_mcp::bridge::{
    translate, translate_playlist_preview_with_catalog, translate_with_catalog,
    translate_with_default_provider, BridgeError, TranslatedCall,
};
use spotuify_mcp::confirm::{decide, Authorized, ConfirmDecision};
use spotuify_mcp::tools::{ToolCatalogue, ToolKind};

// --- Catalogue invariants ---

#[test]
fn catalogue_has_no_duplicate_tool_names() {
    let mut names: Vec<&str> = ToolCatalogue::all().iter().map(|t| t.name).collect();
    names.sort();
    let mut dedup = names.clone();
    dedup.dedup();
    assert_eq!(names, dedup, "duplicate tool name detected");
}

#[test]
fn destructive_tools_iterator_matches_destructive_flag() {
    let from_flag: Vec<&str> = ToolCatalogue::all()
        .iter()
        .filter(|t| t.destructive)
        .map(|t| t.name)
        .collect();
    let from_iter: Vec<&str> = ToolCatalogue::destructive().map(|t| t.name).collect();
    assert_eq!(from_flag, from_iter);
}

#[test]
fn every_destructive_tool_has_kind_destructive() {
    for t in ToolCatalogue::destructive() {
        assert_eq!(
            t.kind,
            ToolKind::Destructive,
            "tool {} is destructive=true but kind!=Destructive",
            t.name
        );
    }
}

#[test]
fn by_name_lookup_matches_iteration() {
    for t in ToolCatalogue::all() {
        assert_eq!(ToolCatalogue::by_name(t.name), Some(t));
    }
    assert!(ToolCatalogue::by_name("not_a_tool").is_none());
}

// --- Confirmation gating ---

#[test]
fn read_tools_authorize_to_execute_without_confirm() {
    let result = decide("search", None).unwrap();
    assert_eq!(result, Authorized::Execute);
}

#[test]
fn transport_tools_authorize_to_execute_without_confirm() {
    let result = decide("play", None).unwrap();
    assert_eq!(result, Authorized::Execute);
}

#[test]
fn destructive_tool_without_confirm_yields_preview_only() {
    let result = decide("playlist_create", None).unwrap();
    assert_eq!(result, Authorized::PreviewOnly);

    let result = decide("playlist_create", Some(false)).unwrap();
    assert_eq!(result, Authorized::PreviewOnly);
}

#[test]
fn destructive_tool_with_confirm_true_authorizes_execute() {
    let result = decide("playlist_create", Some(true)).unwrap();
    assert_eq!(result, Authorized::Execute);
}

#[test]
fn unknown_tool_returns_unknown_tool_error() {
    match decide("not_a_real_tool", None) {
        Err(ConfirmDecision::UnknownTool(name)) => assert_eq!(name, "not_a_real_tool"),
        other => panic!("expected UnknownTool, got {other:?}"),
    }
}

#[test]
fn undo_last_is_not_destructive_so_no_confirm_required() {
    let result = decide("undo_last", None).unwrap();
    assert_eq!(
        result,
        Authorized::Execute,
        "undo_last should execute without confirm -- it is the safety net"
    );
}

// --- Bridge translation ---

#[test]
fn search_tool_translates_to_request_search() {
    let call = translate("search", &json!({"query": "luther vandross"})).unwrap();
    match call {
        TranslatedCall::Request(spotuify_protocol::Request::Search { query, limit, .. }) => {
            assert_eq!(query, "luther vandross");
            assert_eq!(limit, 20, "default limit should be 20");
        }
        other => panic!("expected Search request, got {other:?}"),
    }
}

#[test]
fn playlist_plan_builds_agent_plan_without_daemon() {
    let call = translate("playlist_plan", &json!({"brief": "rainy night focus"})).unwrap();
    match call {
        TranslatedCall::LocalJson(value) => {
            assert_eq!(value["title"], "Rainy Night Focus");
            assert_eq!(value["target_length"], 12);
            assert!(value["candidate_searches"]
                .as_array()
                .expect("candidate_searches should be an array")
                .iter()
                .any(|query| query == "rainy night focus"));
        }
        other => panic!("expected local playlist plan JSON, got {other:?}"),
    }
}

#[test]
fn playlist_resolve_tracks_accepts_plan_for_daemon_search_resolution() {
    let call = translate(
        "playlist_resolve_tracks",
        &json!({
            "plan": {
                "title": "Focus",
                "description": "desc",
                "target_length": 2,
                "mood": "quiet",
                "theme_notes": [],
                "candidate_searches": ["one", "two"],
                "sequencing_notes": [],
                "exclusions": []
            }
        }),
    )
    .unwrap();

    match call {
        TranslatedCall::PlaylistResolveTracks { plan, provider } => {
            assert_eq!(plan.candidate_searches, vec!["one", "two"]);
            assert_eq!(provider, None);
        }
        other => panic!("expected PlaylistResolveTracks, got {other:?}"),
    }
}

#[test]
fn search_tool_clamps_excessive_limit_to_50() {
    let call = translate("search", &json!({"query": "x", "limit": 1000})).unwrap();
    match call {
        TranslatedCall::Request(spotuify_protocol::Request::Search { limit, .. }) => {
            assert_eq!(limit, 50);
        }
        other => panic!("expected Search request, got {other:?}"),
    }
}

#[test]
fn now_playing_translates_to_playback_get() {
    let call = translate("now_playing", &json!({})).unwrap();
    assert!(matches!(
        call,
        TranslatedCall::Request(spotuify_protocol::Request::PlaybackGet)
    ));
}

#[test]
fn play_uri_requires_uri_arg() {
    let err = translate("play_uri", &json!({})).unwrap_err();
    match err {
        BridgeError::MissingArg { tool, arg } => {
            assert_eq!(tool, "play_uri");
            assert_eq!(arg, "uri");
        }
        other => panic!("expected MissingArg, got {other:?}"),
    }
}

#[test]
fn play_uri_with_wrong_type_yields_bad_arg_type() {
    let err = translate("play_uri", &json!({"uri": 42})).unwrap_err();
    match err {
        BridgeError::BadArgType { tool, arg } => {
            assert_eq!(tool, "play_uri");
            assert_eq!(arg, "uri");
        }
        other => panic!("expected BadArgType, got {other:?}"),
    }
}

#[test]
fn playlist_create_translates_with_uris_array() {
    let call = translate(
        "playlist_create",
        &json!({
            "name": "Focus",
            "description": "deep work",
            "uris": ["spotify:track:1", "spotify:track:2"]
        }),
    )
    .unwrap();
    match call {
        TranslatedCall::Request(spotuify_protocol::Request::PlaylistCreate {
            name,
            description,
            uris,
            provider,
        }) => {
            assert_eq!(name, "Focus");
            assert_eq!(description.as_deref(), Some("deep work"));
            assert_eq!(uris.len(), 2);
            assert_eq!(provider, None);
        }
        other => panic!("expected PlaylistCreate, got {other:?}"),
    }
}

#[test]
fn playlist_create_with_missing_name_errors() {
    let err = translate("playlist_create", &json!({"description": "x"})).unwrap_err();
    match err {
        BridgeError::MissingArg { arg, .. } => assert_eq!(arg, "name"),
        other => panic!("expected MissingArg name, got {other:?}"),
    }
}

#[test]
fn playlist_create_allows_empty_or_episode_seeds() {
    // Absent or empty `uris` creates an empty playlist; episodes are accepted
    // (aligned with playlist_add's Track+Episode kinds).
    for args in [
        json!({"name": "Focus"}),
        json!({"name": "Focus", "uris": []}),
        json!({"name": "Focus", "uris": ["spotify:episode:one"]}),
        json!({"name": "Focus", "uris": ["spotify:track:one", "spotify:episode:one"]}),
    ] {
        let call = translate("playlist_create", &args)
            .expect("valid create seeds must translate to a daemon request");
        assert!(matches!(
            call,
            TranslatedCall::Request(spotuify_protocol::Request::PlaylistCreate { .. })
        ));
    }
}

#[test]
fn playlist_create_rejects_malformed_uri_seeds() {
    for args in [
        json!({"name": "Focus", "uris": "spotify:track:one"}),
        json!({"name": "Focus", "uris": ["spotify:track:one", 7]}),
        json!({"name": "Focus", "uris": ["not-a-resource-uri"]}),
        json!({"name": "Focus", "uris": ["spotify:album:one"]}),
    ] {
        let error = translate("playlist_create", &args)
            .expect_err("malformed create URI arrays must not produce a daemon request");
        assert!(
            matches!(
                &error,
                BridgeError::BadArgType { arg, .. }
                    | BridgeError::InvalidArg { arg, .. }
                    if arg == "uris"
            ),
            "unexpected playlist_create error: {error:?}"
        );
    }
}

#[test]
fn playlist_create_rejects_a_non_string_description() {
    let error = translate(
        "playlist_create",
        &json!({
            "name": "Focus",
            "description": 7,
            "uris": ["spotify:track:one"]
        }),
    )
    .expect_err("malformed description must not produce a daemon request");
    assert!(matches!(
        error,
        BridgeError::BadArgType { ref arg, .. } if arg == "description"
    ));
}

#[test]
fn playlist_create_dry_run_translates_only_to_wire_safe_preview_request() {
    let call = translate(
        "playlist_create",
        &json!({
            "name": "Focus",
            "description": "deep work",
            "uris": ["spotify:track:one"],
            "dry_run": true,
        }),
    )
    .unwrap();
    let TranslatedCall::Request(request) = call else {
        panic!("expected daemon preview request")
    };
    assert!(matches!(
        &request,
        spotuify_protocol::Request::PlaylistCreatePreview {
            name,
            description,
            uris,
            provider: None,
        } if name == "Focus"
            && description.as_deref() == Some("deep work")
            && uris.len() == 1
            && uris[0] == "spotify:track:one"
    ));
    assert!(!request.requires_mutation_id());
    assert_eq!(request.kind_label(), "playlist-create-preview");

    let error = translate(
        "playlist_create",
        &json!({
            "name": "Focus",
            "uris": ["spotify:track:one"],
            "dry_run": "true",
        }),
    )
    .expect_err("malformed dry_run must not produce a daemon request");
    assert!(matches!(
        error,
        BridgeError::BadArgType { ref arg, .. } if arg == "dry_run"
    ));
}

#[test]
fn unconfirmed_playlist_tools_force_read_only_preview_requests() {
    for (tool, args, expected_kind) in [
        (
            "playlist_create",
            json!({"name": "Focus", "uris": ["spotify:track:one"]}),
            "playlist-create-preview",
        ),
        (
            "playlist_add",
            json!({
                "playlist": "spotify:playlist:focus",
                "uris": ["spotify:track:one"]
            }),
            "playlist-items-preview",
        ),
        (
            "playlist_remove",
            json!({
                "playlist": "spotify:playlist:focus",
                "uris": ["spotify:track:one"],
                "dry_run": false
            }),
            "playlist-items-preview",
        ),
    ] {
        let request = translate_playlist_preview_with_catalog(tool, &args, None)
            .unwrap()
            .expect("playlist mutation must have a daemon preview command");
        assert_eq!(request.kind_label(), expected_kind);
        assert!(!request.requires_mutation_id());
    }

    assert!(translate_playlist_preview_with_catalog(
        "playlist_unfollow",
        &json!({"playlist": "spotify:playlist:focus"}),
        None,
    )
    .unwrap()
    .is_none());
}

#[test]
fn playlist_remove_routes_to_typed_daemon_request() {
    let call = translate(
        "playlist_remove",
        &json!({
            "playlist": "Focus",
            "uris": ["spotify:track:one", "spotify:track:two"]
        }),
    )
    .unwrap();

    match call {
        TranslatedCall::Request(spotuify_protocol::Request::PlaylistRemoveItems {
            playlist,
            uris,
            provider,
        }) => {
            assert_eq!(playlist, "Focus");
            assert_eq!(uris, vec!["spotify:track:one", "spotify:track:two"]);
            assert_eq!(provider, None);
        }
        other => panic!("expected PlaylistRemoveItems, got {other:?}"),
    }
}

#[test]
fn playlist_tools_preserve_explicit_provider_scope() {
    let call = translate(
        "playlist_remove",
        &json!({
            "playlist": "music:playlist:focus",
            "uris": ["music:track:one"],
            "provider": "music"
        }),
    )
    .unwrap();
    assert!(matches!(
        call,
        TranslatedCall::Request(spotuify_protocol::Request::PlaylistRemoveItems {
            provider: Some(provider),
            ..
        }) if provider.as_str() == "music"
    ));
}

#[test]
fn playlist_item_tools_reject_invalid_uri_arrays_before_translation() {
    for tool in ["playlist_add", "playlist_remove"] {
        for uris in [
            json!(["spotify:track:one", 7]),
            json!(["spotify:track:one", null]),
            json!(["not-a-resource-uri"]),
            json!(["spotify:album:one"]),
            json!([]),
        ] {
            let error = translate(
                tool,
                &json!({
                    "playlist": "spotify:playlist:focus",
                    "uris": uris,
                }),
            )
            .expect_err("invalid URI arrays must not produce a daemon request");
            assert!(
                matches!(
                    &error,
                    BridgeError::BadArgType { arg, .. }
                        | BridgeError::InvalidArg { arg, .. }
                        if arg == "uris"
                ),
                "unexpected {tool} error: {error:?}"
            );
        }
    }
}

#[test]
fn playlist_item_dry_runs_translate_only_to_wire_safe_preview_requests() {
    for (tool, action) in [
        (
            "playlist_add",
            spotuify_protocol::PlaylistItemMutationAction::Add,
        ),
        (
            "playlist_remove",
            spotuify_protocol::PlaylistItemMutationAction::Remove,
        ),
    ] {
        let call = translate(
            tool,
            &json!({
                "playlist": "spotify:playlist:focus",
                "uris": ["spotify:track:one"],
                "dry_run": true,
            }),
        )
        .unwrap();
        let TranslatedCall::Request(request) = call else {
            panic!("expected daemon preview request")
        };
        assert!(matches!(
            &request,
            spotuify_protocol::Request::PlaylistItemsPreview {
                playlist,
                uris,
                action: actual,
                provider: None,
            } if playlist == "spotify:playlist:focus"
                && uris.len() == 1
                && uris[0] == "spotify:track:one"
                && *actual == action
        ));
        assert!(!request.requires_mutation_id());
        assert_eq!(request.kind_label(), "playlist-items-preview");

        let live = translate(
            tool,
            &json!({
                "playlist": "spotify:playlist:focus",
                "uris": ["spotify:track:one"],
                "dry_run": false,
            }),
        )
        .unwrap();
        assert!(matches!(
            (action, live),
            (
                spotuify_protocol::PlaylistItemMutationAction::Add,
                TranslatedCall::Request(spotuify_protocol::Request::PlaylistAddItems { .. })
            ) | (
                spotuify_protocol::PlaylistItemMutationAction::Remove,
                TranslatedCall::Request(spotuify_protocol::Request::PlaylistRemoveItems { .. })
            )
        ));

        let error = translate(
            tool,
            &json!({
                "playlist": "spotify:playlist:focus",
                "uris": ["spotify:track:one"],
                "dry_run": "true",
            }),
        )
        .expect_err("malformed dry_run must not produce a daemon request");
        assert!(matches!(
            error,
            BridgeError::BadArgType { ref arg, .. } if arg == "dry_run"
        ));
    }
}

#[test]
fn library_unsave_routes_to_unsave_request_not_save() {
    let call = translate("library_unsave", &json!({"uri": "spotify:track:remove-me"})).unwrap();

    match call {
        TranslatedCall::Request(spotuify_protocol::Request::LibraryUnsave { uri }) => {
            assert_eq!(uri, "spotify:track:remove-me");
        }
        other => panic!("expected LibraryUnsave, got {other:?}"),
    }
}

#[test]
fn related_artists_defers_free_form_resolution_and_rejects_wrong_canonical_kind() {
    for artist in ["artist-1", "music:artist:artist-1"] {
        let call = translate(
            "related_artists",
            &json!({"artist": artist, "provider": "music"}),
        )
        .unwrap();
        assert!(matches!(
            call,
            TranslatedCall::RelatedArtists {
                artist: routed,
                provider: Some(provider),
            } if routed == artist && provider.as_str() == "music"
        ));
    }

    assert!(matches!(
        translate(
            "related_artists",
            &json!({"artist": "music:track:track-1"}),
        ),
        Err(BridgeError::InvalidArg { arg, .. }) if arg == "artist"
    ));
}

#[test]
fn search_routes_explicit_provider_without_provider_named_source_variant() {
    let call = translate(
        "search",
        &json!({"query": "x", "source": "remote", "provider": "music"}),
    )
    .unwrap();
    assert!(matches!(
        call,
        TranslatedCall::Request(spotuify_protocol::Request::Search {
            source: spotuify_protocol::SearchSourceData::Remote(source),
            provider: Some(provider),
            ..
        }) if source.as_str() == "music" && provider.as_str() == "music"
    ));

    assert!(matches!(
        translate(
            "search",
            &json!({"query": "x", "source": "music", "provider": "music"}),
        ),
        Err(BridgeError::InvalidArg { arg, .. }) if arg == "source"
    ));
}

#[test]
fn search_accepts_legacy_spotify_source_as_remote_alias() {
    // `"spotify"` is the documented pre-abstraction source value; it must keep
    // working (existing agent configs) as an alias for `"remote"`.
    let call = translate("search", &json!({"query": "x", "source": "spotify"})).unwrap();
    assert!(matches!(
        call,
        TranslatedCall::Request(spotuify_protocol::Request::Search {
            source: spotuify_protocol::SearchSourceData::Remote(source),
            ..
        }) if source.as_str() == "spotify"
    ));

    // Truly unknown source values still hard-error.
    assert!(matches!(
        translate("search", &json!({"query": "x", "source": "bogus"})),
        Err(BridgeError::InvalidArg { arg, .. }) if arg == "source"
    ));
}

#[test]
fn library_save_of_artist_routes_to_follow() {
    let call = translate("library_save", &json!({"uri": "spotify:artist:abc"})).unwrap();
    assert!(matches!(
        call,
        TranslatedCall::Request(spotuify_protocol::Request::ArtistFollow { artist })
            if artist == "spotify:artist:abc"
    ));

    // Non-artist saves stay on the library-save path.
    let call = translate("library_save", &json!({"uri": "spotify:track:abc"})).unwrap();
    assert!(matches!(
        call,
        TranslatedCall::Request(spotuify_protocol::Request::LibrarySave {
            uri: Some(uri),
            current: false,
        }) if uri == "spotify:track:abc"
    ));
}

#[test]
fn library_unsave_of_artist_routes_to_unfollow() {
    let call = translate("library_unsave", &json!({"uri": "spotify:artist:abc"})).unwrap();
    assert!(matches!(
        call,
        TranslatedCall::Request(spotuify_protocol::Request::ArtistUnfollow { artist })
            if artist == "spotify:artist:abc"
    ));
}

#[test]
fn remote_search_uses_discovered_default_without_serializing_an_explicit_scope() {
    let default = spotuify_core::ProviderId::new("music").unwrap();
    let call = translate_with_default_provider(
        "search",
        &json!({"query": "x", "source": "remote"}),
        Some(&default),
    )
    .unwrap();
    assert!(matches!(
        call,
        TranslatedCall::Request(spotuify_protocol::Request::Search {
            source: spotuify_protocol::SearchSourceData::Remote(source),
            provider: None,
            ..
        }) if source == default
    ));
}

#[test]
fn omitted_search_source_uses_local_for_explicit_empty_catalog() {
    let catalog = spotuify_core::ProviderCatalog::default();
    let call = translate_with_catalog("search", &json!({"query": "x"}), Some(&catalog)).unwrap();
    assert!(matches!(
        call,
        TranslatedCall::Request(spotuify_protocol::Request::Search {
            source: spotuify_protocol::SearchSourceData::Local,
            provider: None,
            ..
        })
    ));
}

#[test]
fn pause_translates_to_playback_command_pause() {
    let call = translate("pause", &json!({})).unwrap();
    match call {
        TranslatedCall::Request(spotuify_protocol::Request::PlaybackCommand { command }) => {
            assert!(matches!(command, spotuify_protocol::PlaybackCommand::Pause));
        }
        other => panic!("expected PlaybackCommand::Pause, got {other:?}"),
    }
}

#[test]
fn lyrics_routes_to_typed_request() {
    let call = translate("lyrics", &json!({"track_uri": "spotify:track:abc"})).unwrap();
    match call {
        TranslatedCall::Request(spotuify_protocol::Request::LyricsGet { track_uri, .. }) => {
            assert_eq!(track_uri.as_deref(), Some("spotify:track:abc"));
        }
        other => panic!("expected LyricsGet, got {other:?}"),
    }
}

#[test]
fn phase_12_ops_tools_route_to_typed_requests() {
    use spotuify_protocol::Request;
    // ops_log -> Request::OpsLog
    let call = translate("ops_log", &json!({"limit": 5, "source": "mcp"})).unwrap();
    let TranslatedCall::Request(r) = call else {
        panic!("ops_log must route to a typed Request, not LocalDeferred")
    };
    match r {
        Request::OpsLog { limit, source, .. } => {
            assert_eq!(limit, 5);
            assert_eq!(source, Some(spotuify_protocol::OperationSource::Mcp));
        }
        other => panic!("expected OpsLog, got {other:?}"),
    }

    // undo_last -> Request::OpsUndo
    let call = translate("undo_last", &json!({})).unwrap();
    let TranslatedCall::Request(r) = call else {
        panic!("undo_last must route to a typed Request")
    };
    match r {
        Request::OpsUndo {
            operation_id,
            dry_run,
            force,
            bulk_since_ms,
        } => {
            assert!(
                operation_id.is_none(),
                "undo_last targets the last reversible op"
            );
            assert!(!dry_run);
            assert!(!force);
            assert!(bulk_since_ms.is_none());
        }
        other => panic!("expected OpsUndo, got {other:?}"),
    }
}

#[test]
fn phase_10_analytics_tools_route_to_typed_requests() {
    use spotuify_protocol::Request;

    let call = translate(
        "analytics_top",
        &json!({"kind": "artists", "since": "7d", "limit": 5}),
    )
    .unwrap();
    let TranslatedCall::Request(r) = call else {
        panic!("analytics_top must route to a typed Request")
    };
    match r {
        Request::AnalyticsTop {
            kind,
            since_window,
            limit,
        } => {
            assert_eq!(kind, spotuify_protocol::TopKind::Artists);
            assert_eq!(since_window, spotuify_protocol::SinceWindow::Days(7));
            assert_eq!(limit, 5);
        }
        other => panic!("expected AnalyticsTop, got {other:?}"),
    }

    let call = translate("analytics_habits", &json!({"window": "month"})).unwrap();
    let TranslatedCall::Request(r) = call else {
        panic!("analytics_habits must route to a typed Request")
    };
    assert!(matches!(
        r,
        Request::AnalyticsHabits {
            window: spotuify_protocol::HabitWindow::Month,
            ..
        }
    ));
}

#[test]
fn unknown_tool_returns_bridge_unknown_tool() {
    let err = translate("not_a_tool", &json!({})).unwrap_err();
    match err {
        BridgeError::UnknownTool(name) => assert_eq!(name, "not_a_tool"),
        other => panic!("expected UnknownTool, got {other:?}"),
    }
}

#[test]
fn analytics_search_routes_with_mode_and_limit() {
    use spotuify_protocol::Request;
    let call = translate(
        "analytics_search",
        &json!({"mode": "normalized", "limit": 75}),
    )
    .unwrap();
    let TranslatedCall::Request(r) = call else {
        panic!("analytics_search must route to a typed Request")
    };
    match r {
        Request::AnalyticsSearch { mode, limit } => {
            assert_eq!(mode, spotuify_protocol::SearchMode::Normalized);
            assert_eq!(limit, 75);
        }
        other => panic!("expected AnalyticsSearch, got {other:?}"),
    }
}

#[test]
fn analytics_search_defaults_mode_to_raw_when_unset() {
    use spotuify_protocol::Request;
    let call = translate("analytics_search", &json!({})).unwrap();
    let TranslatedCall::Request(Request::AnalyticsSearch { mode, limit }) = call else {
        panic!("expected AnalyticsSearch")
    };
    assert_eq!(mode, spotuify_protocol::SearchMode::Raw);
    assert_eq!(limit, 50, "default limit is 50");
}

#[test]
fn analytics_search_caps_limit_at_200() {
    use spotuify_protocol::Request;
    let call = translate("analytics_search", &json!({"limit": 100_000})).unwrap();
    let TranslatedCall::Request(Request::AnalyticsSearch { limit, .. }) = call else {
        panic!("expected AnalyticsSearch")
    };
    assert_eq!(limit, 200, "limit must clamp to 200 to bound result size");
}

#[test]
fn analytics_rediscovery_parses_gap_value() {
    use spotuify_protocol::Request;
    let call = translate("analytics_rediscovery", &json!({"gap": "365d"})).unwrap();
    let TranslatedCall::Request(Request::AnalyticsRediscovery { gap_days }) = call else {
        panic!("expected AnalyticsRediscovery")
    };
    assert_eq!(gap_days, 365);
}

#[test]
fn analytics_rediscovery_defaults_gap_to_90() {
    use spotuify_protocol::Request;
    let call = translate("analytics_rediscovery", &json!({})).unwrap();
    let TranslatedCall::Request(Request::AnalyticsRediscovery { gap_days }) = call else {
        panic!("expected AnalyticsRediscovery")
    };
    assert_eq!(gap_days, 90, "default gap is 90 days");
}

#[test]
fn radio_dry_run_requires_a_boolean_at_the_bridge_boundary() {
    for invalid in [json!("true"), json!(null), json!({ "value": true })] {
        assert!(matches!(
            translate(
                "radio_start",
                &json!({ "seed_uri": "music:track:one", "dry_run": invalid }),
            ),
            Err(BridgeError::BadArgType { arg, .. }) if arg == "dry_run"
        ));
    }

    assert!(matches!(
        translate(
            "radio_start",
            &json!({ "seed_uri": "music:track:one", "dry_run": true }),
        ),
        Ok(TranslatedCall::Request(
            spotuify_protocol::Request::RadioStart { dry_run: true, .. }
        ))
    ));
}

#[test]
fn analytics_top_clamps_limit() {
    use spotuify_protocol::Request;
    let call = translate(
        "analytics_top",
        &json!({"kind": "tracks", "limit": 100_000}),
    )
    .unwrap();
    let TranslatedCall::Request(Request::AnalyticsTop { limit, .. }) = call else {
        panic!("expected AnalyticsTop")
    };
    assert_eq!(limit, 100, "limit must clamp at 100");
}

#[test]
fn analytics_top_defaults_to_tracks_30d() {
    use spotuify_protocol::Request;
    let call = translate("analytics_top", &json!({})).unwrap();
    let TranslatedCall::Request(Request::AnalyticsTop {
        kind,
        since_window,
        limit,
    }) = call
    else {
        panic!("expected AnalyticsTop")
    };
    assert_eq!(kind, spotuify_protocol::TopKind::Tracks);
    assert_eq!(since_window, spotuify_protocol::SinceWindow::Days(30));
    assert_eq!(limit, 25);
}

#[test]
fn analytics_top_since_all_maps_to_unbounded_window() {
    use spotuify_protocol::Request;
    let call = translate("analytics_top", &json!({"since": "all"})).unwrap();
    let TranslatedCall::Request(Request::AnalyticsTop { since_window, .. }) = call else {
        panic!("expected AnalyticsTop")
    };
    assert_eq!(since_window, spotuify_protocol::SinceWindow::All);
}

#[test]
fn ops_log_propagates_unknown_source_as_none() {
    use spotuify_protocol::Request;
    // An unknown `source` label must NOT abort — it just drops the
    // filter so the user sees the unfiltered log.
    let call = translate("ops_log", &json!({"source": "robot"})).unwrap();
    let TranslatedCall::Request(Request::OpsLog { source, .. }) = call else {
        panic!("expected OpsLog")
    };
    assert!(
        source.is_none(),
        "unknown source label must degrade to no filter"
    );
}

#[test]
fn undo_last_honours_force_flag() {
    use spotuify_protocol::Request;
    let call = translate("undo_last", &json!({"force": true})).unwrap();
    let TranslatedCall::Request(Request::OpsUndo { force, .. }) = call else {
        panic!("expected OpsUndo")
    };
    assert!(
        force,
        "force=true on the MCP call must round-trip into Request::OpsUndo"
    );
}
