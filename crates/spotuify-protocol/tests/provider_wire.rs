#![allow(clippy::panic, clippy::unwrap_used)]

use serde::{Deserialize, Serialize};
use serde_json::json;
use spotuify_core::{
    ClientPreferences, MediaItem, MediaKind, Playback, ProviderCaps, ProviderCatalog,
    ProviderDescriptor, ProviderId, Queue, ResolvedTarget, ResourceUri, UriScheme,
};
use spotuify_protocol::{
    DaemonEvent, EpisodeSort, IpcErrorKind, PlaylistItemMutationAction, Request, Response,
    ResponseData, SearchScopeData, SearchSortData, SearchSourceData, SyncTargetData,
    VizDiagnostics,
};

fn provider_id(value: &str) -> ProviderId {
    ProviderId::new(value).unwrap()
}

fn descriptor(value: &str, is_default: bool) -> ProviderDescriptor {
    ProviderDescriptor {
        id: provider_id(value),
        uri_scheme: UriScheme::new(value).unwrap(),
        display_name: value.to_string(),
        capabilities: ProviderCaps::default(),
        is_default,
    }
}

#[test]
fn search_source_preserves_legacy_spotify_and_encodes_extensible_remote_ids() {
    let spotify = SearchSourceData::Remote(provider_id("spotify"));
    let remote = SearchSourceData::Remote(provider_id("apple"));
    assert_eq!(
        serde_json::to_value(SearchSourceData::Local).unwrap(),
        json!("local")
    );
    assert_eq!(
        serde_json::to_value(SearchSourceData::Hybrid).unwrap(),
        json!("hybrid")
    );
    assert_eq!(serde_json::to_value(&spotify).unwrap(), json!("spotify"));
    assert_eq!(SearchSourceData::legacy_default_remote(), spotify);
    assert_eq!(
        serde_json::to_value(&remote).unwrap(),
        json!({"remote": "apple"})
    );

    for value in [
        json!("local"),
        json!("hybrid"),
        json!("spotify"),
        json!({"remote": "apple"}),
        json!({"remote": "spotify"}),
    ] {
        let decoded: SearchSourceData = serde_json::from_value(value.clone()).unwrap();
        let encoded = serde_json::to_value(decoded).unwrap();
        if value == json!({"remote": "spotify"}) {
            assert_eq!(encoded, json!("spotify"));
        } else {
            assert_eq!(encoded, value);
        }
    }
}

#[test]
fn search_source_rejects_unknown_or_malformed_forms() {
    for value in [
        json!("remote"),
        json!("apple"),
        json!({"remote": "Apple"}),
        json!({"remote": "apple", "extra": true}),
        json!({"remote": null}),
    ] {
        assert!(
            serde_json::from_value::<SearchSourceData>(value.clone()).is_err(),
            "unexpectedly accepted {value}"
        );
    }
}

#[test]
fn search_provider_and_remote_source_conflict_remains_detectable_for_daemon_rejection() {
    let request: Request = serde_json::from_value(json!({
        "cmd": "search",
        "query": "conflict",
        "scope": "all",
        "source": {"remote": "apple"},
        "limit": 10,
        "provider": "spotify"
    }))
    .unwrap();
    let Request::Search {
        source: SearchSourceData::Remote(remote),
        provider: Some(provider),
        ..
    } = request
    else {
        panic!("expected scoped remote search")
    };
    assert_ne!(provider, remote, "daemon must reject this dual authority");
}

#[test]
fn optional_provider_scopes_default_to_none_and_are_omitted() {
    let requests = [
        Request::Search {
            query: "x".to_string(),
            scope: SearchScopeData::All,
            source: SearchSourceData::Remote(provider_id("spotify")),
            limit: 10,
            provider: None,
            kinds: None,
            sort: None,
        },
        Request::LibraryList {
            limit: 10,
            provider: None,
        },
        Request::SavedTracks {
            limit: 10,
            offset: 0,
            provider: None,
        },
        Request::PlaylistsList { provider: None },
        Request::PlaylistTracks {
            playlist: "spotify:playlist:one".to_string(),
            wait: true,
            provider: None,
        },
        Request::PlaylistAddItems {
            playlist: "spotify:playlist:one".to_string(),
            uris: vec!["spotify:track:one".to_string()],
            provider: None,
        },
        Request::PlaylistRemoveItems {
            playlist: "spotify:playlist:one".to_string(),
            uris: vec!["spotify:track:one".to_string()],
            provider: None,
        },
        Request::PlaylistUnfollow {
            playlist: "spotify:playlist:one".to_string(),
            provider: None,
        },
        Request::PlaylistSetImage {
            playlist: "spotify:playlist:one".to_string(),
            image_base64: "jpeg".to_string(),
            provider: None,
        },
        Request::Sync {
            target: SyncTargetData::Library,
            provider: None,
        },
        Request::RecentlyPlayed { provider: None },
        Request::PlaylistCreate {
            name: "new".to_string(),
            description: None,
            uris: Vec::new(),
            provider: None,
        },
        Request::EpisodeFeed {
            limit: 10,
            sort: EpisodeSort::Newest,
            refresh: false,
            provider: None,
        },
    ];

    for request in requests {
        let encoded = serde_json::to_value(&request).unwrap();
        assert!(
            encoded.get("provider").is_none(),
            "unexpected scope: {encoded}"
        );
        assert_eq!(serde_json::from_value::<Request>(encoded).unwrap(), request);
    }

    let legacy: Request = serde_json::from_value(json!({
        "cmd": "search",
        "query": "legacy",
        "scope": "all",
        "source": "spotify",
        "limit": 10
    }))
    .unwrap();
    assert!(matches!(
        legacy,
        Request::Search {
            source: SearchSourceData::Remote(provider),
            provider: None,
            ..
        } if provider.as_str() == "spotify"
    ));
}

#[test]
fn unscoped_canonical_uri_requests_have_no_provider_authority_to_override() {
    let request: Request = serde_json::from_value(json!({
        "cmd": "album-tracks",
        "album": "apple:album:one",
        "provider": "spotify"
    }))
    .unwrap();
    let encoded = serde_json::to_value(request).unwrap();
    assert!(encoded.get("provider").is_none());
}

#[test]
fn playlist_requests_preserve_explicit_provider_for_daemon_conflict_validation() {
    for value in [
        json!({
            "cmd": "playlist-tracks",
            "playlist": "apple:playlist:one",
            "wait": true,
            "provider": "spotify"
        }),
        json!({
            "cmd": "playlist-add-items",
            "playlist": "apple:playlist:one",
            "uris": ["apple:track:one"],
            "provider": "spotify"
        }),
        json!({
            "cmd": "playlist-remove-items",
            "playlist": "apple:playlist:one",
            "uris": ["apple:track:one"],
            "provider": "spotify"
        }),
        json!({
            "cmd": "playlist-unfollow",
            "playlist": "apple:playlist:one",
            "provider": "spotify"
        }),
        json!({
            "cmd": "playlist-set-image",
            "playlist": "apple:playlist:one",
            "image_base64": "jpeg",
            "provider": "spotify"
        }),
    ] {
        let request: Request = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), value);
    }
}

#[test]
fn legacy_playlist_requests_decode_with_no_provider_scope() {
    for value in [
        json!({"cmd":"playlist-tracks","playlist":"mix","wait":false}),
        json!({"cmd":"playlist-add-items","playlist":"mix","uris":[]}),
        json!({"cmd":"playlist-remove-items","playlist":"mix","uris":[]}),
        json!({"cmd":"playlist-unfollow","playlist":"mix"}),
        json!({"cmd":"playlist-set-image","playlist":"mix","image_base64":"jpeg"}),
    ] {
        let request: Request = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), value);

        let mut nullable = value.clone();
        nullable["provider"] = serde_json::Value::Null;
        let request: Request = serde_json::from_value(nullable).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), value);
    }
}

#[test]
fn playlist_item_preview_is_read_only_and_preserves_provider_scope() {
    let request = Request::PlaylistItemsPreview {
        playlist: "apple:playlist:one".to_string(),
        uris: vec!["apple:track:one".to_string()],
        action: PlaylistItemMutationAction::Remove,
        provider: Some(provider_id("apple")),
    };

    assert!(!request.requires_mutation_id());
    assert_eq!(request.kind_label(), "playlist-items-preview");
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        json!({
            "cmd": "playlist-items-preview",
            "playlist": "apple:playlist:one",
            "uris": ["apple:track:one"],
            "action": "remove",
            "provider": "apple"
        })
    );
}

#[test]
fn playlist_create_preview_is_a_distinct_read_only_wire_command() {
    let request = Request::PlaylistCreatePreview {
        name: "Focus".to_string(),
        description: Some("Deep work".to_string()),
        uris: vec!["apple:track:one".to_string()],
        provider: Some(provider_id("apple")),
    };

    assert!(!request.requires_mutation_id());
    assert_eq!(request.kind_label(), "playlist-create-preview");
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        json!({
            "cmd": "playlist-create-preview",
            "name": "Focus",
            "description": "Deep work",
            "uris": ["apple:track:one"],
            "provider": "apple"
        })
    );

    let legacy: Request = serde_json::from_value(json!({
        "cmd": "playlist-create-preview",
        "name": "Focus",
        "uris": ["apple:track:one"]
    }))
    .unwrap();
    assert!(matches!(
        &legacy,
        Request::PlaylistCreatePreview {
            description: None,
            ..
        }
    ));
    assert!(serde_json::to_value(legacy)
        .unwrap()
        .get("description")
        .is_none());
}

#[test]
fn provider_responses_have_frozen_top_level_shapes() {
    let provider = descriptor("spotify", true);
    let provider_json = serde_json::to_value(&provider).unwrap();
    assert_eq!(
        serde_json::to_value(ResponseData::ProviderList {
            default_provider: Some(provider_id("spotify")),
            providers: vec![provider],
        })
        .unwrap(),
        json!({
            "kind": "provider-list",
            "default_provider": "spotify",
            "providers": [provider_json]
        })
    );

    assert_eq!(
        serde_json::to_value(ResponseData::TargetResolved { target: None }).unwrap(),
        json!({"kind": "target-resolved", "target": null})
    );
    assert_eq!(
        serde_json::to_value(ResponseData::TargetResolved {
            target: Some(ResolvedTarget {
                provider: provider_id("apple"),
                uri: ResourceUri::parse("apple:track:one").unwrap(),
            }),
        })
        .unwrap(),
        json!({
            "kind": "target-resolved",
            "target": {"provider": "apple", "uri": "apple:track:one"}
        })
    );
    assert_eq!(
        serde_json::to_value(ResponseData::AudioOutputs {
            outputs: vec!["Built-in Output".to_string()],
            selected: Some("Built-in Output".to_string()),
        })
        .unwrap(),
        json!({
            "kind": "audio-outputs",
            "outputs": ["Built-in Output"],
            "selected": "Built-in Output"
        })
    );
}

fn client_seed(
    provider_catalog: Option<ProviderCatalog>,
    preferences: Option<ClientPreferences>,
) -> ResponseData {
    ResponseData::ClientSeed {
        playback: Playback::default(),
        queue: Queue::default(),
        devices: Vec::new(),
        recent: Vec::new(),
        viz: VizDiagnostics::default(),
        provider_catalog,
        preferences,
        provider_policies: Vec::new(),
    }
}

#[test]
fn client_seed_distinguishes_unknown_catalog_from_explicit_empty() {
    let absent = serde_json::to_value(client_seed(None, None)).unwrap();
    assert!(absent.get("provider_catalog").is_none());
    assert!(absent.get("preferences").is_none());

    let explicit = serde_json::to_value(client_seed(
        Some(ProviderCatalog::default()),
        Some(ClientPreferences::default()),
    ))
    .unwrap();
    assert_eq!(
        explicit.get("provider_catalog"),
        Some(&json!({"providers": []}))
    );
    assert_eq!(explicit.get("preferences"), Some(&json!({})));
}

#[test]
fn legacy_error_context_defaults_and_new_context_is_additive() {
    let legacy: Response = serde_json::from_value(json!({
        "Error": {
            "message": "failed",
            "kind": "provider",
            "code": "provider",
            "retryable": false
        }
    }))
    .unwrap();
    assert!(matches!(
        legacy,
        Response::Error {
            provider: None,
            detail: None,
            ..
        }
    ));

    let contextual = Response::Error {
        message: "failed".to_string(),
        kind: IpcErrorKind::Provider,
        code: "provider".to_string(),
        retryable: false,
        provider: Some(provider_id("apple")),
        detail: Some("upstream code 12".to_string()),
    };
    let encoded = serde_json::to_value(contextual).unwrap();
    assert_eq!(encoded["Error"]["provider"], "apple");
    assert_eq!(encoded["Error"]["detail"], "upstream code 12");
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum FrozenV7SearchSource {
    Local,
    Spotify,
    Hybrid,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
enum FrozenV7Request {
    Search {
        query: String,
        scope: SearchScopeData,
        source: FrozenV7SearchSource,
        limit: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kinds: Option<Vec<MediaKind>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sort: Option<SearchSortData>,
    },
    PlaylistsList,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
enum FrozenV7Event {
    SearchPage {
        query: String,
        kind: MediaKind,
        offset: u32,
        version: u64,
        items: Vec<MediaItem>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum FrozenV7ResponseData {
    ClientSeed {
        playback: Playback,
        queue: Queue,
        devices: Vec<spotuify_core::Device>,
        recent: Vec<MediaItem>,
        viz: VizDiagnostics,
    },
}

#[test]
fn frozen_v7_default_provider_requests_work_in_both_directions() {
    let old = FrozenV7Request::Search {
        query: "old".to_string(),
        scope: SearchScopeData::Track,
        source: FrozenV7SearchSource::Spotify,
        limit: 10,
        kinds: None,
        sort: None,
    };
    let decoded_new: Request = serde_json::from_value(serde_json::to_value(&old).unwrap()).unwrap();
    assert!(matches!(
        decoded_new,
        Request::Search {
            provider: None,
            source: SearchSourceData::Remote(provider),
            ..
        } if provider.as_str() == "spotify"
    ));

    let new = Request::Search {
        query: "new".to_string(),
        scope: SearchScopeData::Track,
        source: SearchSourceData::Remote(provider_id("spotify")),
        limit: 10,
        provider: Some(provider_id("spotify")),
        kinds: None,
        sort: None,
    };
    let decoded_old: FrozenV7Request =
        serde_json::from_value(serde_json::to_value(new).unwrap()).unwrap();
    assert!(matches!(
        decoded_old,
        FrozenV7Request::Search {
            source: FrozenV7SearchSource::Spotify,
            ..
        }
    ));

    let old_unit: FrozenV7Request = serde_json::from_value(
        serde_json::to_value(Request::PlaylistsList { provider: None }).unwrap(),
    )
    .unwrap();
    assert_eq!(old_unit, FrozenV7Request::PlaylistsList);
}

#[test]
fn frozen_v7_events_and_client_seed_ignore_or_default_new_fields() {
    let new_event = DaemonEvent::SearchPage {
        query: "x".to_string(),
        kind: MediaKind::Track,
        offset: 0,
        version: 1,
        items: Vec::new(),
        provider: Some(provider_id("spotify")),
    };
    let old_event: FrozenV7Event =
        serde_json::from_value(serde_json::to_value(new_event).unwrap()).unwrap();
    assert!(matches!(old_event, FrozenV7Event::SearchPage { .. }));

    let old_event = FrozenV7Event::SearchPage {
        query: "x".to_string(),
        kind: MediaKind::Track,
        offset: 0,
        version: 1,
        items: Vec::new(),
    };
    let new_event: DaemonEvent =
        serde_json::from_value(serde_json::to_value(old_event).unwrap()).unwrap();
    assert!(matches!(
        new_event,
        DaemonEvent::SearchPage { provider: None, .. }
    ));

    let new_seed = client_seed(
        Some(ProviderCatalog {
            default_provider: Some(provider_id("spotify")),
            providers: vec![descriptor("spotify", true)],
        }),
        Some(ClientPreferences {
            viz_color_scheme: Some("rainbow".to_string()),
        }),
    );
    let old_seed: FrozenV7ResponseData =
        serde_json::from_value(serde_json::to_value(new_seed).unwrap()).unwrap();
    assert!(matches!(old_seed, FrozenV7ResponseData::ClientSeed { .. }));

    let old_seed = FrozenV7ResponseData::ClientSeed {
        playback: Playback::default(),
        queue: Queue::default(),
        devices: Vec::new(),
        recent: Vec::new(),
        viz: VizDiagnostics::default(),
    };
    let new_seed: ResponseData =
        serde_json::from_value(serde_json::to_value(old_seed).unwrap()).unwrap();
    assert!(matches!(
        new_seed,
        ResponseData::ClientSeed {
            provider_catalog: None,
            preferences: None,
            provider_policies,
            ..
        } if provider_policies.is_empty()
    ));
}

#[test]
fn typed_auth_and_sync_provider_ids_keep_string_json() {
    let auth = Request::AuthStatus {
        provider: Some(provider_id("spotify")),
    };
    assert_eq!(serde_json::to_value(auth).unwrap()["provider"], "spotify");

    let sync = DaemonEvent::SyncStarted {
        target: SyncTargetData::Library,
        provider: Some(provider_id("apple")),
    };
    assert_eq!(serde_json::to_value(sync).unwrap()["provider"], "apple");
}
