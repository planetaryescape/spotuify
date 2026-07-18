#![allow(clippy::panic, clippy::unwrap_used)]

use async_trait::async_trait;
use spotuify_core::{
    AccessOutcome, CollectionRequest, LibraryCaps, LibraryRequest, MediaItem, MediaKind,
    MusicProvider, Mutation, MutationCompletion, MutationOutcome, MutationReceipt,
    PageContinuation, PageRequest, PlayRequest, PlaySource, PlaylistInsertion, ProviderCaps,
    ProviderError, ProviderId, ProviderPage, QueueAddRequest, RemoteTransport, RequestContext,
    RequestPriority, ResourceUri, SearchRequest, TransportCommand, TransportDevice, UriScheme,
};
use spotuify_provider_fake::{
    run_provider_conformance, run_transport_conformance, ConformanceFixtures, ConformanceOptions,
    FakeDataset, FakeProvider,
};
use uuid::Uuid;

#[tokio::test]
async fn fake_passes_provider_and_transport_conformance() {
    let provider = FakeProvider::new();
    let fixtures = ConformanceFixtures::fake("fake").unwrap();
    run_provider_conformance(&provider, &fixtures, ConformanceOptions::default())
        .await
        .unwrap();
    let caps = provider.capabilities().transport.unwrap();
    run_transport_conformance(&provider, &caps, &fixtures)
        .await
        .unwrap();
}

#[tokio::test]
async fn single_collection_play_resolves_only_playable_items() {
    let provider = FakeProvider::isolated("fake").unwrap();
    for target in [
        "fake:album:album-1",
        "fake:playlist:playlist-1",
        "fake:artist:artist-1",
    ] {
        provider
            .execute(
                RequestContext::PLAYBACK_CONTROL,
                TransportCommand::Play(PlayRequest {
                    start_uri: ResourceUri::parse(target).unwrap(),
                    source: PlaySource::Single,
                    device: TransportDevice::Active,
                    position_ms: 0,
                }),
            )
            .await
            .unwrap();
        let playback = provider.playback(RequestContext::FOREGROUND).await.unwrap();
        assert_eq!(
            playback.item.as_ref().map(|item| item.uri.as_str()),
            Some("fake:track:track-1"),
            "{target} must resolve to a playable first item"
        );
        let queue = provider.queue(RequestContext::FOREGROUND).await.unwrap();
        assert!(queue
            .items
            .iter()
            .all(|item| { matches!(item.kind, MediaKind::Track | MediaKind::Episode) }));
    }

    let error = provider
        .execute(
            RequestContext::PLAYBACK_CONTROL,
            TransportCommand::Play(PlayRequest {
                start_uri: ResourceUri::parse("fake:show:show-1").unwrap(),
                source: PlaySource::Single,
                device: TransportDevice::Active,
                position_ms: 0,
            }),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ProviderError::InvalidInput { .. }));
}

#[tokio::test]
async fn dual_fake_namespaces_route_and_mutate_independently() {
    let first = FakeProvider::isolated("fake-a").unwrap();
    let second = FakeProvider::isolated("fake-b").unwrap();
    assert_ne!(first.id(), second.id());
    assert_ne!(
        MusicProvider::uri_scheme(&first),
        MusicProvider::uri_scheme(&second)
    );

    let first_result = first
        .search(
            RequestContext::FOREGROUND,
            SearchRequest {
                query: "track two".to_string(),
                kind: MediaKind::Track,
                page: PageRequest::default(),
            },
        )
        .await
        .unwrap();
    let second_result = second
        .search(
            RequestContext::FOREGROUND,
            SearchRequest {
                query: "track two".to_string(),
                kind: MediaKind::Track,
                page: PageRequest::default(),
            },
        )
        .await
        .unwrap();
    assert!(first_result.items[0].uri.starts_with("fake-a:"));
    assert!(second_result.items[0].uri.starts_with("fake-b:"));

    let first_uri = ResourceUri::parse(&first_result.items[0].uri).unwrap();
    first
        .apply_mutation(
            RequestContext::FOREGROUND,
            Uuid::now_v7(),
            &Mutation::LibrarySave {
                uris: vec![first_uri],
            },
        )
        .await
        .unwrap();
    let first_library = first
        .library_items(
            RequestContext::BACKGROUND_SYNC,
            LibraryRequest {
                kind: MediaKind::Track,
                page: PageRequest::new(100, 0),
            },
        )
        .await
        .unwrap();
    let second_library = second
        .library_items(
            RequestContext::BACKGROUND_SYNC,
            LibraryRequest {
                kind: MediaKind::Track,
                page: PageRequest::new(100, 0),
            },
        )
        .await
        .unwrap();
    assert_eq!(first_library.items.len(), 2);
    assert_eq!(second_library.items.len(), 1);
}

#[tokio::test]
async fn pagination_echoes_offset_continuation_and_enforces_maximum() {
    let provider = FakeProvider::new();
    let first = provider
        .search(
            RequestContext::FOREGROUND,
            SearchRequest {
                query: String::new(),
                kind: MediaKind::Track,
                page: PageRequest::new(1, 0),
            },
        )
        .await
        .unwrap();
    assert_eq!(first.requested_offset, 0);
    assert_eq!(first.total, Some(2));
    assert_eq!(first.next, Some(PageContinuation::Offset(1)));

    let second = provider
        .search(
            RequestContext::FOREGROUND,
            SearchRequest {
                query: String::new(),
                kind: MediaKind::Track,
                page: PageRequest::new(1, 1),
            },
        )
        .await
        .unwrap();
    assert_eq!(second.requested_offset, 1);
    assert_eq!(second.next, None);

    let error = provider
        .search(
            RequestContext::FOREGROUND,
            SearchRequest {
                query: String::new(),
                kind: MediaKind::Track,
                page: PageRequest::new(101, 0),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ProviderError::InvalidInput { ref field, .. } if field == "limit"
    ));

    let zero_error = provider
        .search(
            RequestContext::FOREGROUND,
            SearchRequest {
                query: String::new(),
                kind: MediaKind::Track,
                page: PageRequest::new(0, 0),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        zero_error,
        ProviderError::InvalidInput { ref field, .. } if field == "limit"
    ));
}

#[tokio::test]
async fn mutation_batches_enforce_declared_maximum_before_writes() {
    let provider = FakeProvider::new();
    let track = ResourceUri::parse("fake:track:track-1").unwrap();
    for mutation in [
        Mutation::LibrarySave {
            uris: vec![track.clone(); 101],
        },
        Mutation::LibraryUnsave {
            uris: vec![track.clone(); 101],
        },
        Mutation::Follow {
            uris: vec![track.clone(); 101],
        },
        Mutation::Unfollow {
            uris: vec![track.clone(); 101],
        },
    ] {
        let error = provider
            .apply_mutation(RequestContext::FOREGROUND, Uuid::now_v7(), &mutation)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ProviderError::InvalidInput { ref field, .. } if field == "uris"
        ));
    }

    let playlist_uri = ResourceUri::parse("fake:playlist:playlist-1").unwrap();
    let additions = vec![
        PlaylistInsertion {
            uri: track.clone(),
            position: None,
        };
        101
    ];
    let error = provider
        .apply_mutation(
            RequestContext::FOREGROUND,
            Uuid::now_v7(),
            &Mutation::PlaylistAdd {
                playlist_uri,
                items: additions,
                expected_version: None,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ProviderError::InvalidInput { ref field, .. } if field == "items"
    ));
}

#[tokio::test]
async fn undeclared_library_kinds_are_unsupported_without_state_changes() {
    let provider = FakeProvider::new();
    let read_error = provider
        .library_items(
            RequestContext::BACKGROUND_SYNC,
            LibraryRequest {
                kind: MediaKind::Album,
                page: PageRequest::new(10, 0),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(read_error, ProviderError::Unsupported { .. }));

    let artist_two = ResourceUri::parse("fake:artist:artist-2").unwrap();
    let save_error = provider
        .apply_mutation(
            RequestContext::FOREGROUND,
            Uuid::now_v7(),
            &Mutation::LibrarySave {
                uris: vec![artist_two.clone()],
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(save_error, ProviderError::Unsupported { .. }));
    let followed = provider
        .library_items(
            RequestContext::BACKGROUND_SYNC,
            LibraryRequest {
                kind: MediaKind::Artist,
                page: PageRequest::new(10, 0),
            },
        )
        .await
        .unwrap();
    assert!(!followed
        .items
        .iter()
        .any(|item| item.uri == artist_two.as_uri()));

    let track_two = ResourceUri::parse("fake:track:track-2").unwrap();
    let follow_error = provider
        .apply_mutation(
            RequestContext::FOREGROUND,
            Uuid::now_v7(),
            &Mutation::Follow {
                uris: vec![track_two.clone()],
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(follow_error, ProviderError::Unsupported { .. }));
    let saved = provider
        .library_items(
            RequestContext::BACKGROUND_SYNC,
            LibraryRequest {
                kind: MediaKind::Track,
                page: PageRequest::new(10, 0),
            },
        )
        .await
        .unwrap();
    assert!(!saved
        .items
        .iter()
        .any(|item| item.uri == track_two.as_uri()));
}

#[tokio::test]
async fn spotify_compatibility_snapshot_is_seeded_under_a_non_spotify_scheme() {
    // The dataset seeds playback/queue from track keys; those must be derived
    // from the instance scheme, not hardcoded `spotify:`, or the snapshot is
    // silently empty under `fake` (the scheme `FakeProvider::from_env()` uses).
    let provider = FakeProvider::with_identity(
        ProviderId::new("fake").unwrap(),
        UriScheme::Fake,
        FakeDataset::SpotifyCompatibility,
    );

    let playback = provider.playback(RequestContext::FOREGROUND).await.unwrap();
    assert_eq!(
        playback.item.as_ref().map(|item| item.uri.as_str()),
        Some("fake:track:never-too-much"),
        "compatibility playback item must be present under the fake scheme"
    );

    let queue = provider.queue(RequestContext::FOREGROUND).await.unwrap();
    assert_eq!(
        queue
            .currently_playing
            .as_ref()
            .map(|item| item.uri.as_str()),
        Some("fake:track:never-too-much")
    );
    assert_eq!(
        queue
            .items
            .iter()
            .map(|item| item.uri.as_str())
            .collect::<Vec<_>>(),
        vec!["fake:track:sweet-thing"],
        "compatibility queue must be seeded under the fake scheme"
    );
}

#[tokio::test]
async fn custom_provider_id_is_preserved_independently_of_uri_scheme() {
    let provider = FakeProvider::with_identity(
        ProviderId::new("custom-cloud").unwrap(),
        UriScheme::Spotify,
        FakeDataset::Standard,
    );
    let result = provider
        .search(
            RequestContext::FOREGROUND,
            SearchRequest {
                query: "track one".to_string(),
                kind: MediaKind::Track,
                page: PageRequest::new(10, 0),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        result.items[0]
            .source
            .as_ref()
            .map(spotuify_core::ItemSource::as_str),
        Some("custom-cloud")
    );
    assert_eq!(
        ResourceUri::parse(&result.items[0].uri).unwrap().scheme(),
        &UriScheme::Spotify
    );
}

#[tokio::test]
async fn queue_add_activates_the_fake_queue_session() {
    let provider = FakeProvider::new();
    provider
        .execute(
            RequestContext::PLAYBACK_CONTROL,
            TransportCommand::QueueAdd(QueueAddRequest {
                uri: ResourceUri::parse("fake:track:track-1").unwrap(),
                device: TransportDevice::Active,
            }),
        )
        .await
        .unwrap();
    assert!(
        provider
            .queue(RequestContext::FOREGROUND)
            .await
            .unwrap()
            .session_active
    );
}

#[tokio::test]
async fn positioned_inserts_are_stable_and_metadata_writes_keep_item_version() {
    let provider = FakeProvider::new();
    let created = provider
        .apply_mutation(
            RequestContext::FOREGROUND,
            Uuid::now_v7(),
            &Mutation::PlaylistCreate {
                name: "Insertion semantics".to_string(),
                public: Some(false),
                description: None,
            },
        )
        .await
        .unwrap();
    let MutationOutcome::PlaylistCreated { playlist } = created.outcome else {
        panic!("expected playlist creation");
    };
    let playlist_uri = ResourceUri::parse(&playlist.id).unwrap();
    let first = ResourceUri::parse("fake:track:track-1").unwrap();
    let second = ResourceUri::parse("fake:track:track-2").unwrap();
    let added = provider
        .apply_mutation(
            RequestContext::FOREGROUND,
            Uuid::now_v7(),
            &Mutation::PlaylistAdd {
                playlist_uri: playlist_uri.clone(),
                items: vec![
                    PlaylistInsertion {
                        uri: first.clone(),
                        position: Some(0),
                    },
                    PlaylistInsertion {
                        uri: second.clone(),
                        position: Some(0),
                    },
                ],
                expected_version: created.version_token,
            },
        )
        .await
        .unwrap();
    let version = added.version_token.clone();
    let AccessOutcome::Available(items) = provider
        .playlist_items(
            RequestContext::FOREGROUND,
            CollectionRequest {
                uri: playlist_uri.clone(),
                page: PageRequest::new(100, 0),
            },
        )
        .await
        .unwrap()
    else {
        panic!("created playlist should be readable");
    };
    assert_eq!(
        items
            .items
            .iter()
            .map(|item| item.uri.clone())
            .collect::<Vec<_>>(),
        vec![first.as_uri(), second.as_uri()]
    );

    for mutation in [
        Mutation::PlaylistAdd {
            playlist_uri: playlist_uri.clone(),
            items: Vec::new(),
            expected_version: version.clone(),
        },
        Mutation::PlaylistSetImage {
            playlist_uri: playlist_uri.clone(),
            jpeg: vec![0xff, 0xd8, 0xff],
        },
        Mutation::PlaylistReorder {
            playlist_uri,
            range_start: 0,
            insert_before: 0,
            range_length: 0,
            expected_version: version.clone(),
        },
    ] {
        let receipt = provider
            .apply_mutation(RequestContext::FOREGROUND, Uuid::now_v7(), &mutation)
            .await
            .unwrap();
        assert_eq!(receipt.version_token, version);
    }
}

struct NoOpLibraryProvider(FakeProvider);

#[async_trait]
impl MusicProvider for NoOpLibraryProvider {
    fn id(&self) -> &ProviderId {
        MusicProvider::id(&self.0)
    }

    fn uri_scheme(&self) -> &UriScheme {
        MusicProvider::uri_scheme(&self.0)
    }

    fn display_name(&self) -> &str {
        "No-op Library"
    }

    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            library: LibraryCaps {
                read_kinds: vec![MediaKind::Track],
                save_kinds: vec![MediaKind::Track],
                follow_kinds: Vec::new(),
                mutation_max_batch: Some(100),
                max_page_size: Some(100),
                freshness_probe: false,
            },
            ..ProviderCaps::default()
        }
    }

    async fn library_items(
        &self,
        context: RequestContext,
        request: LibraryRequest,
    ) -> Result<ProviderPage<MediaItem>, ProviderError> {
        self.0.library_items(context, request).await
    }

    async fn apply_mutation(
        &self,
        _context: RequestContext,
        mutation_id: Uuid,
        mutation: &Mutation,
    ) -> Result<MutationReceipt, ProviderError> {
        let (uris, saved) = match mutation {
            Mutation::LibrarySave { uris } => (uris.clone(), true),
            Mutation::LibraryUnsave { uris } => (uris.clone(), false),
            _ => return Err(ProviderError::unsupported("apply_mutation")),
        };
        if uris.len() > 100 {
            return Err(ProviderError::InvalidInput {
                field: "uris".to_string(),
                message: "batch exceeds provider maximum 100".to_string(),
            });
        }
        Ok(MutationReceipt {
            mutation_id,
            provider: self.id().clone(),
            completion: MutationCompletion::Applied,
            outcome: MutationOutcome::LibraryChanged { uris, saved },
            version_token: None,
            failures: Vec::new(),
        })
    }
}

struct WriteOnlyLibraryProvider(FakeProvider);

#[async_trait]
impl MusicProvider for WriteOnlyLibraryProvider {
    fn id(&self) -> &ProviderId {
        MusicProvider::id(&self.0)
    }

    fn uri_scheme(&self) -> &UriScheme {
        MusicProvider::uri_scheme(&self.0)
    }

    fn display_name(&self) -> &str {
        "Write-only Library"
    }

    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            library: LibraryCaps {
                read_kinds: Vec::new(),
                save_kinds: vec![MediaKind::Track],
                follow_kinds: Vec::new(),
                mutation_max_batch: Some(100),
                max_page_size: Some(100),
                freshness_probe: false,
            },
            ..ProviderCaps::default()
        }
    }

    async fn apply_mutation(
        &self,
        context: RequestContext,
        mutation_id: Uuid,
        mutation: &Mutation,
    ) -> Result<MutationReceipt, ProviderError> {
        self.0.apply_mutation(context, mutation_id, mutation).await
    }
}

#[tokio::test]
async fn conformance_rejects_a_provider_that_reports_success_without_mutating() {
    let provider = NoOpLibraryProvider(FakeProvider::new());
    let fixtures = ConformanceFixtures::fake("fake").unwrap();
    let error = run_provider_conformance(&provider, &fixtures, ConformanceOptions::default())
        .await
        .unwrap_err();
    assert!(
        matches!(error, ProviderError::Provider(ref message) if message.contains("not observable"))
    );
}

#[tokio::test]
async fn conformance_does_not_imply_read_from_save_capability() {
    let provider = WriteOnlyLibraryProvider(FakeProvider::new());
    let fixtures = ConformanceFixtures::fake("fake").unwrap();
    run_provider_conformance(&provider, &fixtures, ConformanceOptions::default())
        .await
        .unwrap();
}

#[tokio::test]
async fn advertised_capability_without_fixture_fails_before_provider_call() {
    let provider = FakeProvider::new();
    let error = run_provider_conformance(
        &provider,
        &ConformanceFixtures::default(),
        ConformanceOptions::default(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error, ProviderError::Provider(ref message) if message.contains("lacks fixture"))
    );
    assert!(provider.observed_requests().await.is_empty());
}

#[tokio::test]
async fn fake_replays_matching_mutation_uuid_and_rejects_mismatch() {
    let provider = FakeProvider::new();
    let id = Uuid::now_v7();
    let mutation = Mutation::LibrarySave {
        uris: vec![ResourceUri::parse("fake:track:track-1").unwrap()],
    };
    let first = provider
        .apply_mutation(RequestContext::FOREGROUND, id, &mutation)
        .await
        .unwrap();
    let replay = provider
        .apply_mutation(RequestContext::FOREGROUND, id, &mutation)
        .await
        .unwrap();
    assert_eq!(replay, first);
    let error = provider
        .apply_mutation(
            RequestContext::FOREGROUND,
            id,
            &Mutation::LibraryUnsave {
                uris: vec![ResourceUri::parse("fake:track:track-1").unwrap()],
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ProviderError::InvalidInput { ref field, .. } if field == "mutation_id"
    ));
}

#[tokio::test]
async fn transport_writes_force_playback_control_priority() {
    let provider = FakeProvider::new();
    provider
        .execute(RequestContext::BACKGROUND_SYNC, TransportCommand::Pause)
        .await
        .unwrap();
    let observed = provider.observed_requests().await;
    let call = observed.last().unwrap();
    assert_eq!(call.operation, "transport.execute");
    assert_eq!(call.priority, RequestPriority::PlaybackControl);
}

struct UnsupportedProvider {
    id: ProviderId,
    scheme: UriScheme,
}

#[async_trait]
impl MusicProvider for UnsupportedProvider {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn uri_scheme(&self) -> &UriScheme {
        &self.scheme
    }

    fn display_name(&self) -> &str {
        "Unsupported"
    }

    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps::default()
    }
}

#[tokio::test]
async fn default_methods_return_typed_unsupported_errors() {
    let provider = UnsupportedProvider {
        id: ProviderId::new("unsupported").unwrap(),
        scheme: UriScheme::new("unsupported").unwrap(),
    };
    let error = provider
        .search(RequestContext::FOREGROUND, SearchRequest::default())
        .await
        .unwrap_err();
    assert_eq!(
        error,
        ProviderError::Unsupported {
            operation: "search".to_string()
        }
    );
}
