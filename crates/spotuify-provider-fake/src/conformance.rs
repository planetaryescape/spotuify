//! Reusable, capability-driven checks for provider adapters.

use spotuify_core::{
    AccessOutcome, CollectionRequest, LibraryRequest, MediaItem, MediaKind, MusicProvider,
    Mutation, MutationCompletion, MutationOutcome, PageContinuation, PageRequest, PlayRequest,
    PlaySource, PlaylistInsertion, PlaylistItemRef, ProviderCaps, ProviderError, ProviderResult,
    RemoteTransport, RepeatMode, RequestContext, ResourceUri, SearchRequest, TransportCaps,
    TransportCommand, TransportDevice,
};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct SearchFixture {
    pub kind: MediaKind,
    pub query: String,
    pub expected_uri: ResourceUri,
}

#[derive(Clone, Debug)]
pub struct LibraryFixture {
    pub kind: MediaKind,
    pub initially_saved: ResourceUri,
    pub writable_unsaved: Option<ResourceUri>,
}

#[derive(Clone, Debug)]
pub struct PlaylistFixture {
    pub uri: ResourceUri,
    pub initial_items: Vec<ResourceUri>,
}

#[derive(Clone, Debug)]
pub struct TransportFixture {
    pub primary: ResourceUri,
    pub secondary: ResourceUri,
    pub transfer_device_id: Option<String>,
    pub previous_progress_ms: u64,
}

#[derive(Clone, Debug, Default)]
pub struct ConformanceFixtures {
    pub search: Vec<SearchFixture>,
    pub catalog_items: Vec<ResourceUri>,
    pub recently_played: Option<ResourceUri>,
    pub library: Vec<LibraryFixture>,
    pub album: Option<ResourceUri>,
    pub artist: Option<ResourceUri>,
    pub show: Option<ResourceUri>,
    pub playlist: Option<PlaylistFixture>,
    pub transport: Option<TransportFixture>,
}

impl ConformanceFixtures {
    pub fn fake(namespace: &str) -> ProviderResult<Self> {
        let uri = |kind, id| {
            ResourceUri::parse(&format!("{namespace}:{kind}:{id}")).map_err(|err| {
                ProviderError::InvalidInput {
                    field: "conformance_fixture".to_string(),
                    message: err.to_string(),
                }
            })
        };
        let track_one = uri("track", "track-1")?;
        let track_two = uri("track", "track-2")?;
        let episode = uri("episode", "episode-1")?;
        let show = uri("show", "show-1")?;
        let album = uri("album", "album-1")?;
        let artist = uri("artist", "artist-1")?;
        let artist_two = uri("artist", "artist-2")?;
        let playlist = uri("playlist", "playlist-1")?;
        let catalog_items = vec![
            track_one.clone(),
            episode.clone(),
            show.clone(),
            album.clone(),
            artist.clone(),
            playlist.clone(),
        ];
        Ok(Self {
            search: catalog_items
                .iter()
                .cloned()
                .map(|expected_uri| SearchFixture {
                    kind: expected_uri.kind(),
                    query: "fake".to_string(),
                    expected_uri,
                })
                .collect(),
            catalog_items,
            recently_played: Some(track_two.clone()),
            library: vec![
                LibraryFixture {
                    kind: MediaKind::Track,
                    initially_saved: track_one.clone(),
                    writable_unsaved: Some(track_two.clone()),
                },
                LibraryFixture {
                    kind: MediaKind::Artist,
                    initially_saved: artist.clone(),
                    writable_unsaved: Some(artist_two),
                },
            ],
            album: Some(album),
            artist: Some(artist),
            show: Some(show),
            playlist: Some(PlaylistFixture {
                uri: playlist,
                initial_items: vec![track_one.clone(), track_two.clone()],
            }),
            transport: Some(TransportFixture {
                primary: track_one,
                secondary: track_two,
                transfer_device_id: Some(format!("{namespace}-device-2")),
                previous_progress_ms: 0,
            }),
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ConformanceOptions {
    pub page_size: u32,
    pub exercise_mutations: bool,
}

impl Default for ConformanceOptions {
    fn default() -> Self {
        Self {
            page_size: 1,
            exercise_mutations: true,
        }
    }
}

pub async fn run_provider_conformance<P>(
    provider: &P,
    fixtures: &ConformanceFixtures,
    options: ConformanceOptions,
) -> ProviderResult<()>
where
    P: MusicProvider + ?Sized,
{
    let caps = provider.capabilities();
    validate_provider_fixtures(provider, &caps, fixtures, options)?;
    search_conformance(provider, &caps, fixtures, options).await?;
    catalog_conformance(provider, &caps, fixtures, options).await?;
    library_conformance(provider, &caps, fixtures, options).await?;
    playlist_conformance(provider, &caps, fixtures, options).await
}

fn validate_provider_fixtures<P>(
    provider: &P,
    caps: &ProviderCaps,
    fixtures: &ConformanceFixtures,
    options: ConformanceOptions,
) -> ProviderResult<()>
where
    P: MusicProvider + ?Sized,
{
    invariant(!provider.id().as_str().is_empty(), "provider id is empty")?;
    invariant(
        !provider.uri_scheme().label().is_empty(),
        "provider URI scheme is empty",
    )?;
    for uri in fixture_uris(fixtures) {
        invariant(
            uri.scheme() == provider.uri_scheme(),
            "fixture belongs to another provider namespace",
        )?;
    }
    if caps.search.remote {
        invariant(!caps.search.kinds.is_empty(), "remote search has no kinds")?;
        for kind in &caps.search.kinds {
            let fixture = search_fixture(fixtures, kind)?;
            invariant(!fixture.query.is_empty(), "search fixture query is empty")?;
            invariant(
                fixture.expected_uri.kind() == *kind,
                "search fixture has the wrong media kind",
            )?;
        }
    }
    for kind in &caps.catalog.lookup_kinds {
        catalog_fixture(fixtures, kind)?;
    }
    require(
        caps.catalog.recently_played,
        fixtures.recently_played.as_ref(),
        "recently played",
    )?;
    require(
        caps.catalog.album_tracks,
        fixtures.album.as_ref(),
        "album relationship",
    )?;
    require(
        caps.catalog.artist_albums,
        fixtures.artist.as_ref(),
        "artist relationship",
    )?;
    require(
        caps.catalog.show_episodes,
        fixtures.show.as_ref(),
        "show relationship",
    )?;
    for kind in &caps.library.read_kinds {
        library_fixture(fixtures, kind)?;
    }
    for kind in &caps.library.save_kinds {
        let fixture = library_fixture(fixtures, kind)?;
        invariant(
            fixture.writable_unsaved.is_some(),
            "writable library kind lacks an unsaved fixture",
        )?;
    }
    for kind in &caps.library.follow_kinds {
        let fixture = library_fixture(fixtures, kind)?;
        invariant(
            fixture.writable_unsaved.is_some(),
            "followable library kind lacks an unfollowed fixture",
        )?;
    }
    if caps.library.freshness_probe {
        invariant(
            !caps.library.read_kinds.is_empty(),
            "freshness probe lacks a library fixture kind",
        )?;
    }
    let playlist_caps = &caps.playlists;
    let any_playlist = playlist_caps.list
        || playlist_caps.item_read
        || playlist_caps.create
        || playlist_caps.add
        || playlist_caps.remove
        || playlist_caps.reorder
        || playlist_caps.image
        || playlist_caps.unfollow;
    if any_playlist {
        let fixture = fixtures
            .playlist
            .as_ref()
            .ok_or_else(|| conformance_error("advertised playlist capability lacks fixture"))?;
        invariant(
            fixture.uri.kind() == MediaKind::Playlist,
            "playlist fixture has wrong kind",
        )?;
    }
    if playlist_caps.add || playlist_caps.remove || playlist_caps.reorder {
        invariant(
            playlist_caps.item_read,
            "playlist mutation lacks item_read, so it cannot be observed",
        )?;
        invariant(
            fixtures
                .playlist
                .as_ref()
                .is_some_and(|fixture| fixture.initial_items.len() >= 2),
            "playlist mutation requires two item fixtures",
        )?;
    }
    let any_mutation = !caps.library.save_kinds.is_empty()
        || !caps.library.follow_kinds.is_empty()
        || playlist_caps.create
        || playlist_caps.add
        || playlist_caps.remove
        || playlist_caps.reorder
        || playlist_caps.image
        || playlist_caps.unfollow;
    invariant(
        options.exercise_mutations || !any_mutation,
        "mutation capabilities cannot be skipped by conformance options",
    )?;
    if caps.transport.is_some() {
        invariant(
            fixtures.transport.is_some(),
            "advertised transport lacks fixtures",
        )?;
    }
    Ok(())
}

async fn search_conformance<P>(
    provider: &P,
    caps: &ProviderCaps,
    fixtures: &ConformanceFixtures,
    options: ConformanceOptions,
) -> ProviderResult<()>
where
    P: MusicProvider + ?Sized,
{
    if !caps.search.remote {
        return Ok(());
    }
    for kind in &caps.search.kinds {
        let fixture = search_fixture(fixtures, kind)?;
        let page = provider
            .search(
                RequestContext::FOREGROUND,
                SearchRequest {
                    query: fixture.query.clone(),
                    kind: kind.clone(),
                    page: PageRequest::new(options.page_size, 0),
                },
            )
            .await?;
        validate_page(
            provider,
            &page.items,
            page.requested_offset,
            0,
            options.page_size,
        )?;
        invariant(
            page.items
                .iter()
                .any(|item| item.uri == fixture.expected_uri.as_uri()),
            format!("search omitted expected {kind} fixture"),
        )?;
        validate_continuation(&page, options.page_size)?;
    }
    if let Some(kind) = caps.search.kinds.first() {
        let fixture = search_fixture(fixtures, kind)?;
        assert_invalid_limit(
            provider
                .search(
                    RequestContext::FOREGROUND,
                    SearchRequest {
                        query: fixture.query.clone(),
                        kind: kind.clone(),
                        page: PageRequest::new(0, 0),
                    },
                )
                .await,
        )?;
        if let Some(over_limit) = caps.search.max_page_size.and_then(over_limit) {
            assert_invalid_limit(
                provider
                    .search(
                        RequestContext::FOREGROUND,
                        SearchRequest {
                            query: fixture.query.clone(),
                            kind: kind.clone(),
                            page: PageRequest::new(over_limit, 0),
                        },
                    )
                    .await,
            )?;
        }
    }
    Ok(())
}

async fn catalog_conformance<P>(
    provider: &P,
    caps: &ProviderCaps,
    fixtures: &ConformanceFixtures,
    options: ConformanceOptions,
) -> ProviderResult<()>
where
    P: MusicProvider + ?Sized,
{
    for kind in &caps.catalog.lookup_kinds {
        let uri = catalog_fixture(fixtures, kind)?;
        let item = provider
            .media_item(RequestContext::FOREGROUND, uri)
            .await?
            .ok_or_else(|| conformance_error(format!("lookup lost {kind} fixture")))?;
        canonical_item_uri(provider, &item)?;
        invariant(item.uri == uri.as_uri(), "lookup returned a different URI")?;
    }
    if caps.catalog.recently_played {
        let expected = fixtures
            .recently_played
            .as_ref()
            .expect("validated fixture");
        let page = provider
            .recently_played(
                RequestContext::BACKGROUND_SYNC,
                PageRequest::new(options.page_size, 0),
            )
            .await?;
        validate_page(
            provider,
            &page.items,
            page.requested_offset,
            0,
            options.page_size,
        )?;
        invariant(
            page.items.iter().any(|item| item.uri == expected.as_uri()),
            "recently played omitted expected fixture",
        )?;
        probe_page_bounds(caps.catalog.recently_played_max_page_size, |page| {
            provider.recently_played(RequestContext::BACKGROUND_SYNC, page)
        })
        .await?;
    }
    relation_conformance(provider, caps, fixtures, options).await
}

async fn relation_conformance<P>(
    provider: &P,
    caps: &ProviderCaps,
    fixtures: &ConformanceFixtures,
    options: ConformanceOptions,
) -> ProviderResult<()>
where
    P: MusicProvider + ?Sized,
{
    if caps.catalog.album_tracks {
        let uri = fixtures.album.as_ref().expect("validated fixture");
        let page = provider
            .album_tracks(
                RequestContext::BACKGROUND_SYNC,
                collection(uri, options.page_size),
            )
            .await?;
        validate_nonempty_page(provider, &page, options.page_size)?;
        probe_collection_bounds(caps.catalog.album_tracks_max_page_size, uri, |request| {
            provider.album_tracks(RequestContext::BACKGROUND_SYNC, request)
        })
        .await?;
    }
    if caps.catalog.artist_albums {
        let uri = fixtures.artist.as_ref().expect("validated fixture");
        let page = provider
            .artist_albums(
                RequestContext::BACKGROUND_SYNC,
                collection(uri, options.page_size),
            )
            .await?;
        validate_nonempty_page(provider, &page, options.page_size)?;
        probe_collection_bounds(caps.catalog.artist_albums_max_page_size, uri, |request| {
            provider.artist_albums(RequestContext::BACKGROUND_SYNC, request)
        })
        .await?;
    }
    if caps.catalog.show_episodes {
        let uri = fixtures.show.as_ref().expect("validated fixture");
        let page = provider
            .show_episodes(
                RequestContext::BACKGROUND_SYNC,
                collection(uri, options.page_size),
            )
            .await?;
        validate_nonempty_page(provider, &page, options.page_size)?;
        probe_collection_bounds(caps.catalog.show_episodes_max_page_size, uri, |request| {
            provider.show_episodes(RequestContext::BACKGROUND_SYNC, request)
        })
        .await?;
    }
    Ok(())
}

async fn library_conformance<P>(
    provider: &P,
    caps: &ProviderCaps,
    fixtures: &ConformanceFixtures,
    options: ConformanceOptions,
) -> ProviderResult<()>
where
    P: MusicProvider + ?Sized,
{
    let limit = options
        .page_size
        .max(1)
        .min(caps.library.max_page_size.unwrap_or(100) as u32);
    for kind in &caps.library.read_kinds {
        let fixture = library_fixture(fixtures, kind)?;
        let items = library_items(provider, kind, limit).await?;
        invariant(
            items
                .iter()
                .any(|item| item.uri == fixture.initially_saved.as_uri()),
            format!("library omitted initially saved {kind} fixture"),
        )?;
    }
    if let Some(kind) = caps.library.read_kinds.first() {
        assert_invalid_limit(
            provider
                .library_items(
                    RequestContext::BACKGROUND_SYNC,
                    LibraryRequest {
                        kind: kind.clone(),
                        page: PageRequest::new(0, 0),
                    },
                )
                .await,
        )?;
    }
    if let (Some(over_limit), Some(kind)) = (
        caps.library.max_page_size.and_then(over_limit),
        caps.library.read_kinds.first(),
    ) {
        assert_invalid_limit(
            provider
                .library_items(
                    RequestContext::BACKGROUND_SYNC,
                    LibraryRequest {
                        kind: kind.clone(),
                        page: PageRequest::new(over_limit, 0),
                    },
                )
                .await,
        )?;
    }
    for kind in &caps.library.save_kinds {
        let fixture = library_fixture(fixtures, kind)?;
        let candidate = fixture
            .writable_unsaved
            .as_ref()
            .expect("validated fixture");
        if let Some(over_limit) = caps.library.mutation_max_batch.and_then(over_limit) {
            assert_invalid_batch(
                provider
                    .apply_mutation(
                        RequestContext::FOREGROUND,
                        Uuid::now_v7(),
                        &Mutation::LibrarySave {
                            uris: vec![candidate.clone(); over_limit as usize],
                        },
                    )
                    .await,
            )?;
        }
        let observable = caps.library.can_read(kind);
        if observable {
            let before = library_items(provider, kind, limit).await?;
            invariant(
                !before.iter().any(|item| item.uri == candidate.as_uri()),
                "writable fixture must initially be unsaved",
            )?;
        }
        let freshness = if observable && caps.library.freshness_probe {
            Some(
                provider
                    .library_freshness_probe(RequestContext::BACKGROUND_SYNC, kind.clone())
                    .await?,
            )
        } else {
            None
        };
        let mutation = Mutation::LibrarySave {
            uris: vec![candidate.clone()],
        };
        let id = Uuid::now_v7();
        let receipt = provider
            .apply_mutation(RequestContext::FOREGROUND, id, &mutation)
            .await?;
        assert_receipt(provider, &receipt, id)?;
        invariant(
            receipt.outcome
                == MutationOutcome::LibraryChanged {
                    uris: vec![candidate.clone()],
                    saved: true,
                },
            "library save returned wrong outcome",
        )?;
        if observable {
            let after = library_items(provider, kind, limit).await?;
            invariant(
                after.iter().any(|item| item.uri == candidate.as_uri()),
                format!("library save for {kind} was not observable"),
            )?;
        }
        if let Some(previous) = freshness {
            let current = provider
                .library_freshness_probe(RequestContext::BACKGROUND_SYNC, kind.clone())
                .await?;
            invariant(
                provider.library_freshness_changed(&previous, &current),
                format!("library save for {kind} did not change freshness"),
            )?;
        }
        let id = Uuid::now_v7();
        let receipt = provider
            .apply_mutation(
                RequestContext::FOREGROUND,
                id,
                &Mutation::LibraryUnsave {
                    uris: vec![candidate.clone()],
                },
            )
            .await?;
        assert_receipt(provider, &receipt, id)?;
        if observable {
            let final_items = library_items(provider, kind, limit).await?;
            invariant(
                !final_items
                    .iter()
                    .any(|item| item.uri == candidate.as_uri()),
                format!("library unsave for {kind} was not observable"),
            )?;
        }
    }
    for kind in &caps.library.follow_kinds {
        let fixture = library_fixture(fixtures, kind)?;
        let candidate = fixture
            .writable_unsaved
            .as_ref()
            .expect("validated fixture");
        if let Some(over_limit) = caps.library.mutation_max_batch.and_then(over_limit) {
            assert_invalid_batch(
                provider
                    .apply_mutation(
                        RequestContext::FOREGROUND,
                        Uuid::now_v7(),
                        &Mutation::Follow {
                            uris: vec![candidate.clone(); over_limit as usize],
                        },
                    )
                    .await,
            )?;
        }
        let observable = caps.library.can_read(kind);
        if observable {
            let before = library_items(provider, kind, limit).await?;
            invariant(
                !before.iter().any(|item| item.uri == candidate.as_uri()),
                "followable fixture must initially be unfollowed",
            )?;
        }
        let freshness = if observable && caps.library.freshness_probe {
            Some(
                provider
                    .library_freshness_probe(RequestContext::BACKGROUND_SYNC, kind.clone())
                    .await?,
            )
        } else {
            None
        };
        let id = Uuid::now_v7();
        let receipt = provider
            .apply_mutation(
                RequestContext::FOREGROUND,
                id,
                &Mutation::Follow {
                    uris: vec![candidate.clone()],
                },
            )
            .await?;
        assert_receipt(provider, &receipt, id)?;
        invariant(
            receipt.outcome
                == MutationOutcome::FollowChanged {
                    uris: vec![candidate.clone()],
                    following: true,
                },
            "follow returned wrong outcome",
        )?;
        if observable {
            let after = library_items(provider, kind, limit).await?;
            invariant(
                after.iter().any(|item| item.uri == candidate.as_uri()),
                format!("follow for {kind} was not observable"),
            )?;
        }
        if let Some(previous) = freshness {
            let current = provider
                .library_freshness_probe(RequestContext::BACKGROUND_SYNC, kind.clone())
                .await?;
            invariant(
                provider.library_freshness_changed(&previous, &current),
                format!("follow for {kind} did not change freshness"),
            )?;
        }
        let id = Uuid::now_v7();
        let receipt = provider
            .apply_mutation(
                RequestContext::FOREGROUND,
                id,
                &Mutation::Unfollow {
                    uris: vec![candidate.clone()],
                },
            )
            .await?;
        assert_receipt(provider, &receipt, id)?;
        if observable {
            let final_items = library_items(provider, kind, limit).await?;
            invariant(
                !final_items
                    .iter()
                    .any(|item| item.uri == candidate.as_uri()),
                format!("unfollow for {kind} was not observable"),
            )?;
        }
    }
    Ok(())
}

async fn playlist_conformance<P>(
    provider: &P,
    caps: &ProviderCaps,
    fixtures: &ConformanceFixtures,
    options: ConformanceOptions,
) -> ProviderResult<()>
where
    P: MusicProvider + ?Sized,
{
    let pc = &caps.playlists;
    if !(pc.list
        || pc.item_read
        || pc.create
        || pc.add
        || pc.remove
        || pc.reorder
        || pc.image
        || pc.unfollow)
    {
        return Ok(());
    }
    let Some(fixture) = &fixtures.playlist else {
        return Ok(());
    };
    if pc.list {
        let page = provider
            .playlists(
                RequestContext::BACKGROUND_SYNC,
                PageRequest::new(options.page_size, 0),
            )
            .await?;
        invariant(
            page.items
                .iter()
                .any(|playlist| playlist.id == fixture.uri.as_uri()),
            "playlist list omitted fixture",
        )?;
        probe_page_bounds(pc.list_max_page_size, |page| {
            provider.playlists(RequestContext::BACKGROUND_SYNC, page)
        })
        .await?;
    }
    let item_page_size = options
        .page_size
        .max(1)
        .min(pc.items_max_page_size.unwrap_or(100) as u32);
    if pc.item_read {
        let items = playlist_items(provider, &fixture.uri, item_page_size).await?;
        for expected in &fixture.initial_items {
            invariant(
                items.iter().any(|item| item.uri == expected.as_uri()),
                "playlist item read omitted fixture",
            )?;
        }
        assert_invalid_limit(
            provider
                .playlist_items(
                    RequestContext::BACKGROUND_SYNC,
                    CollectionRequest {
                        uri: fixture.uri.clone(),
                        page: PageRequest::new(0, 0),
                    },
                )
                .await,
        )?;
        if let Some(over_limit) = pc.items_max_page_size.and_then(over_limit) {
            assert_invalid_limit(
                provider
                    .playlist_items(
                        RequestContext::BACKGROUND_SYNC,
                        CollectionRequest {
                            uri: fixture.uri.clone(),
                            page: PageRequest::new(over_limit, 0),
                        },
                    )
                    .await,
            )?;
        }
    }
    let mut target = fixture.uri.clone();
    let mut version = provider
        .playlist(RequestContext::FOREGROUND, &target)
        .await?
        .and_then(|playlist| playlist.version_token);
    if pc.create {
        let id = Uuid::now_v7();
        let receipt = provider
            .apply_mutation(
                RequestContext::FOREGROUND,
                id,
                &Mutation::PlaylistCreate {
                    name: "Provider Conformance".to_string(),
                    public: Some(false),
                    description: None,
                },
            )
            .await?;
        assert_receipt(provider, &receipt, id)?;
        let MutationOutcome::PlaylistCreated { playlist } = receipt.outcome else {
            return Err(conformance_error("playlist create returned wrong outcome"));
        };
        target = ResourceUri::parse(&playlist.id)
            .map_err(|err| conformance_error(format!("created playlist URI: {err}")))?;
        invariant(
            target.scheme() == provider.uri_scheme(),
            "created playlist belongs to another provider namespace",
        )?;
        invariant(
            target.kind() == MediaKind::Playlist,
            "playlist create returned a non-playlist resource",
        )?;
        invariant(
            receipt.version_token == playlist.version_token,
            "playlist create receipt and payload disagree on item version",
        )?;
        invariant(
            provider
                .playlist(RequestContext::FOREGROUND, &target)
                .await?
                .is_some(),
            "playlist create was not observable",
        )?;
        version = receipt.version_token;
    }
    if pc.add {
        let before = playlist_items(provider, &target, item_page_size).await?;
        let additions = fixture
            .initial_items
            .iter()
            .take(2)
            .cloned()
            .collect::<Vec<_>>();
        if let Some(over_limit) = pc.add_max_batch.and_then(over_limit) {
            assert_invalid_batch(
                provider
                    .apply_mutation(
                        RequestContext::FOREGROUND,
                        Uuid::now_v7(),
                        &Mutation::PlaylistAdd {
                            playlist_uri: target.clone(),
                            items: vec![
                                PlaylistInsertion {
                                    uri: additions[0].clone(),
                                    position: None,
                                };
                                over_limit as usize
                            ],
                            expected_version: version.clone(),
                        },
                    )
                    .await,
            )?;
        }
        let id = Uuid::now_v7();
        let receipt = provider
            .apply_mutation(
                RequestContext::FOREGROUND,
                id,
                &Mutation::PlaylistAdd {
                    playlist_uri: target.clone(),
                    items: additions
                        .iter()
                        .cloned()
                        .map(|uri| PlaylistInsertion {
                            uri,
                            position: None,
                        })
                        .collect(),
                    expected_version: version.clone(),
                },
            )
            .await?;
        assert_receipt(provider, &receipt, id)?;
        let after = playlist_items(provider, &target, item_page_size).await?;
        invariant(
            after.len() == before.len() + additions.len(),
            "playlist add was not observable",
        )?;
        if pc.version_tokens {
            invariant(
                provider
                    .playlist_version_changed(version.as_deref(), receipt.version_token.as_deref()),
                "playlist add did not advance item version",
            )?;
        }
        version = receipt.version_token;
        let no_op = provider
            .apply_mutation(
                RequestContext::FOREGROUND,
                Uuid::now_v7(),
                &Mutation::PlaylistAdd {
                    playlist_uri: target.clone(),
                    items: Vec::new(),
                    expected_version: version.clone(),
                },
            )
            .await?;
        invariant(
            no_op.version_token == version,
            "empty playlist add advanced item version",
        )?;
    }
    if pc.reorder {
        let before = playlist_items(provider, &target, item_page_size).await?;
        invariant(before.len() >= 2, "playlist reorder fixture is too short")?;
        let id = Uuid::now_v7();
        let receipt = provider
            .apply_mutation(
                RequestContext::FOREGROUND,
                id,
                &Mutation::PlaylistReorder {
                    playlist_uri: target.clone(),
                    range_start: 0,
                    insert_before: 2,
                    range_length: 1,
                    expected_version: version.clone(),
                },
            )
            .await?;
        assert_receipt(provider, &receipt, id)?;
        let after = playlist_items(provider, &target, item_page_size).await?;
        invariant(before != after, "playlist reorder was not observable")?;
        version = receipt.version_token;
    }
    if pc.remove {
        let before = playlist_items(provider, &target, item_page_size).await?;
        let removed = ResourceUri::parse(
            &before
                .first()
                .ok_or_else(|| conformance_error("playlist remove fixture is empty"))?
                .uri,
        )
        .map_err(|err| conformance_error(format!("playlist item URI: {err}")))?;
        if let Some(over_limit) = pc.remove_max_batch.and_then(over_limit) {
            assert_invalid_batch(
                provider
                    .apply_mutation(
                        RequestContext::FOREGROUND,
                        Uuid::now_v7(),
                        &Mutation::PlaylistRemove {
                            playlist_uri: target.clone(),
                            items: vec![
                                PlaylistItemRef {
                                    uri: removed.clone(),
                                    positions: vec![],
                                };
                                over_limit as usize
                            ],
                            expected_version: version.clone(),
                        },
                    )
                    .await,
            )?;
        }
        let id = Uuid::now_v7();
        let receipt = provider
            .apply_mutation(
                RequestContext::FOREGROUND,
                id,
                &Mutation::PlaylistRemove {
                    playlist_uri: target.clone(),
                    items: vec![PlaylistItemRef {
                        uri: removed,
                        positions: vec![0],
                    }],
                    expected_version: version.clone(),
                },
            )
            .await?;
        assert_receipt(provider, &receipt, id)?;
        let after = playlist_items(provider, &target, item_page_size).await?;
        invariant(
            after.len() + 1 == before.len(),
            "playlist remove was not observable",
        )?;
        version = receipt.version_token;
    }
    if pc.image {
        let before = provider
            .playlist(RequestContext::FOREGROUND, &target)
            .await?
            .ok_or_else(|| conformance_error("playlist image target disappeared"))?;
        let id = Uuid::now_v7();
        let receipt = provider
            .apply_mutation(
                RequestContext::FOREGROUND,
                id,
                &Mutation::PlaylistSetImage {
                    playlist_uri: target.clone(),
                    jpeg: vec![0xff, 0xd8, 0xff],
                },
            )
            .await?;
        assert_receipt(provider, &receipt, id)?;
        let after = provider
            .playlist(RequestContext::FOREGROUND, &target)
            .await?
            .ok_or_else(|| conformance_error("playlist image target disappeared"))?;
        invariant(
            after.image_url.is_some() && after.image_url != before.image_url,
            "playlist image mutation was not observable",
        )?;
        invariant(
            receipt.version_token == version,
            "playlist image advanced item version",
        )?;
    }
    if pc.unfollow {
        let id = Uuid::now_v7();
        let receipt = provider
            .apply_mutation(
                RequestContext::FOREGROUND,
                id,
                &Mutation::PlaylistUnfollow {
                    playlist_uri: target.clone(),
                },
            )
            .await?;
        assert_receipt(provider, &receipt, id)?;
        // MOCK-ONLY INVARIANT: the fake drops an unfollowed playlist so
        // `playlist()` returns None. Real Spotify keeps returning owned
        // (deleted) playlists after an unfollow, so a live conformance run may
        // legitimately fail this check; it holds only for the in-memory fake.
        invariant(
            provider
                .playlist(RequestContext::FOREGROUND, &target)
                .await?
                .is_none(),
            "playlist unfollow was not observable",
        )?;
    }
    Ok(())
}

pub async fn run_transport_conformance<T>(
    transport: &T,
    caps: &TransportCaps,
    fixtures: &ConformanceFixtures,
) -> ProviderResult<()>
where
    T: RemoteTransport + ?Sized,
{
    let fixture = fixtures
        .transport
        .as_ref()
        .ok_or_else(|| conformance_error("advertised transport lacks fixtures"))?;
    validate_transport_fixtures(transport, caps, fixture)?;
    let devices = if caps.devices || caps.volume || caps.transfer {
        transport.devices(RequestContext::FOREGROUND).await?
    } else {
        Vec::new()
    };
    if caps.devices {
        invariant(!devices.is_empty(), "declared device listing is empty")?;
    }
    if caps.play {
        play_and_assert(transport, fixture, 1_234).await?;
    }
    if caps.queue_add {
        let before = transport.queue(RequestContext::FOREGROUND).await?;
        transport
            .execute(
                RequestContext::FOREGROUND,
                TransportCommand::QueueAdd(spotuify_core::QueueAddRequest {
                    uri: fixture.secondary.clone(),
                    device: TransportDevice::Active,
                }),
            )
            .await?;
        let after = transport.queue(RequestContext::FOREGROUND).await?;
        invariant(
            after.items.len() == before.items.len() + 1
                && after
                    .items
                    .last()
                    .is_some_and(|item| item.uri == fixture.secondary.as_uri()),
            "queue add was not observable",
        )?;
    }
    if caps.seek {
        let before = transport.playback(RequestContext::FOREGROUND).await?;
        let target = if before.progress_ms == 42_000 {
            43_000
        } else {
            42_000
        };
        transport
            .execute(
                RequestContext::FOREGROUND,
                TransportCommand::Seek {
                    position_ms: target,
                },
            )
            .await?;
        let after = transport.playback(RequestContext::FOREGROUND).await?;
        invariant(
            after.progress_ms == target && after != before,
            "seek was not observable",
        )?;
    }
    if caps.volume {
        let before = transport.devices(RequestContext::FOREGROUND).await?;
        let current = active_device(&before)?.volume_percent.unwrap_or(50);
        let target = if current == 37 { 38 } else { 37 };
        transport
            .execute(
                RequestContext::FOREGROUND,
                TransportCommand::Volume { percent: target },
            )
            .await?;
        let after = transport.devices(RequestContext::FOREGROUND).await?;
        invariant(
            active_device(&after)?.volume_percent == Some(target) && after != before,
            "volume was not observable",
        )?;
    }
    if caps.shuffle {
        let before = transport.playback(RequestContext::FOREGROUND).await?;
        transport
            .execute(
                RequestContext::FOREGROUND,
                TransportCommand::Shuffle {
                    enabled: !before.shuffle,
                },
            )
            .await?;
        let after = transport.playback(RequestContext::FOREGROUND).await?;
        invariant(
            after.shuffle != before.shuffle,
            "shuffle was not observable",
        )?;
    }
    if caps.repeat {
        let before = transport.playback(RequestContext::FOREGROUND).await?;
        let target = if before.repeat == RepeatMode::Track {
            RepeatMode::Context
        } else {
            RepeatMode::Track
        };
        transport
            .execute(
                RequestContext::FOREGROUND,
                TransportCommand::Repeat { mode: target },
            )
            .await?;
        let after = transport.playback(RequestContext::FOREGROUND).await?;
        invariant(
            after.repeat == target && after != before,
            "repeat was not observable",
        )?;
    }
    if caps.next {
        play_and_assert(transport, fixture, 2_000).await?;
        let before = transport.playback(RequestContext::FOREGROUND).await?;
        transport
            .execute(RequestContext::FOREGROUND, TransportCommand::Next)
            .await?;
        let after = transport.playback(RequestContext::FOREGROUND).await?;
        invariant(
            playback_uri(&after) == Some(fixture.secondary.as_uri()) && after != before,
            "next was not observable",
        )?;
    }
    if caps.previous {
        play_and_assert(transport, fixture, 42_000).await?;
        let before = transport.playback(RequestContext::FOREGROUND).await?;
        transport
            .execute(RequestContext::FOREGROUND, TransportCommand::Previous)
            .await?;
        let after = transport.playback(RequestContext::FOREGROUND).await?;
        invariant(
            after.progress_ms == fixture.previous_progress_ms && after != before,
            "previous was not observable",
        )?;
    }
    if caps.pause {
        play_and_assert(transport, fixture, 3_000).await?;
        let before = transport.playback(RequestContext::FOREGROUND).await?;
        transport
            .execute(RequestContext::FOREGROUND, TransportCommand::Pause)
            .await?;
        let after = transport.playback(RequestContext::FOREGROUND).await?;
        invariant(
            !after.is_playing && after != before,
            "pause was not observable",
        )?;
    }
    if caps.resume {
        if !caps.pause {
            return Err(conformance_error(
                "resume requires pause for deterministic setup",
            ));
        }
        transport
            .execute(RequestContext::FOREGROUND, TransportCommand::Pause)
            .await?;
        let before = transport.playback(RequestContext::FOREGROUND).await?;
        transport
            .execute(RequestContext::FOREGROUND, TransportCommand::Resume)
            .await?;
        let after = transport.playback(RequestContext::FOREGROUND).await?;
        invariant(
            after.is_playing && after != before,
            "resume was not observable",
        )?;
    }
    if caps.transfer {
        let target = fixture
            .transfer_device_id
            .as_ref()
            .expect("validated fixture");
        let before = transport.devices(RequestContext::FOREGROUND).await?;
        invariant(
            active_device(&before)?.id.as_deref() != Some(target),
            "transfer fixture is already active",
        )?;
        transport
            .execute(
                RequestContext::FOREGROUND,
                TransportCommand::Transfer {
                    device_id: target.clone(),
                    play: true,
                },
            )
            .await?;
        let after = transport.devices(RequestContext::FOREGROUND).await?;
        invariant(
            active_device(&after)?.id.as_deref() == Some(target) && after != before,
            "transfer was not observable",
        )?;
    }
    if caps.queue_read {
        transport.queue(RequestContext::FOREGROUND).await?;
    }
    if caps.playback_state {
        transport.playback(RequestContext::FOREGROUND).await?;
    }
    Ok(())
}

fn validate_transport_fixtures<T>(
    transport: &T,
    caps: &TransportCaps,
    fixture: &TransportFixture,
) -> ProviderResult<()>
where
    T: RemoteTransport + ?Sized,
{
    for uri in [&fixture.primary, &fixture.secondary] {
        invariant(
            uri.scheme() == transport.uri_scheme(),
            "transport fixture belongs to another namespace",
        )?;
        invariant(
            uri.kind() == MediaKind::Track,
            "transport fixture is not a track",
        )?;
    }
    let playback_mutation = caps.play
        || caps.pause
        || caps.resume
        || caps.next
        || caps.previous
        || caps.seek
        || caps.shuffle
        || caps.repeat;
    invariant(
        !playback_mutation || caps.playback_state,
        "playback mutation lacks playback_state readback",
    )?;
    invariant(
        !(caps.pause || caps.next || caps.previous || caps.seek || caps.shuffle || caps.repeat)
            || caps.play,
        "stateful playback mutation lacks play setup capability",
    )?;
    invariant(
        !caps.resume || (caps.play && caps.pause),
        "resume lacks play/pause setup capabilities",
    )?;
    invariant(
        !caps.queue_add || caps.queue_read,
        "queue_add lacks queue_read readback",
    )?;
    invariant(
        !(caps.volume || caps.transfer) || caps.devices,
        "device mutation lacks devices readback",
    )?;
    invariant(
        !caps.transfer || fixture.transfer_device_id.is_some(),
        "transfer lacks target device fixture",
    )
}

async fn play_and_assert<T>(
    transport: &T,
    fixture: &TransportFixture,
    position_ms: u64,
) -> ProviderResult<()>
where
    T: RemoteTransport + ?Sized,
{
    let before = transport.playback(RequestContext::FOREGROUND).await?;
    transport
        .execute(
            RequestContext::FOREGROUND,
            TransportCommand::Play(PlayRequest {
                start_uri: fixture.primary.clone(),
                source: PlaySource::Ordered(vec![
                    fixture.primary.clone(),
                    fixture.secondary.clone(),
                ]),
                device: TransportDevice::Active,
                position_ms,
            }),
        )
        .await?;
    let after = transport.playback(RequestContext::FOREGROUND).await?;
    invariant(
        playback_uri(&after) == Some(fixture.primary.as_uri())
            && after.is_playing
            && after.progress_ms == position_ms
            && after != before,
        "play was not observable",
    )
}

async fn playlist_items<P>(
    provider: &P,
    uri: &ResourceUri,
    limit: u32,
) -> ProviderResult<Vec<MediaItem>>
where
    P: MusicProvider + ?Sized,
{
    let mut request = PageRequest::new(limit, 0);
    let mut items = Vec::new();
    for _ in 0..1_000 {
        let expected_offset = request.offset;
        let outcome = provider
            .playlist_items(
                RequestContext::BACKGROUND_SYNC,
                CollectionRequest {
                    uri: uri.clone(),
                    page: request,
                },
            )
            .await?;
        let AccessOutcome::Available(page) = outcome else {
            return Err(conformance_error("playlist is unavailable"));
        };
        validate_page(
            provider,
            &page.items,
            page.requested_offset,
            expected_offset,
            limit,
        )?;
        items.extend(page.items);
        request = match page.next {
            Some(PageContinuation::Offset(offset)) => {
                invariant(
                    offset > expected_offset,
                    "playlist continuation did not advance",
                )?;
                PageRequest::new(limit, offset)
            }
            Some(PageContinuation::Cursor(cursor)) => {
                PageRequest::with_cursor(limit, items.len() as u64, cursor)
            }
            None => return Ok(items),
        };
    }
    Err(conformance_error("playlist pagination exceeded 1000 pages"))
}

async fn library_items<P>(
    provider: &P,
    kind: &MediaKind,
    limit: u32,
) -> ProviderResult<Vec<MediaItem>>
where
    P: MusicProvider + ?Sized,
{
    let mut request = PageRequest::new(limit, 0);
    let mut items = Vec::new();
    for _ in 0..1_000 {
        let expected_offset = request.offset;
        let page = provider
            .library_items(
                RequestContext::BACKGROUND_SYNC,
                LibraryRequest {
                    kind: kind.clone(),
                    page: request,
                },
            )
            .await?;
        validate_page(
            provider,
            &page.items,
            page.requested_offset,
            expected_offset,
            limit,
        )?;
        items.extend(page.items);
        request = match page.next {
            Some(PageContinuation::Offset(offset)) => {
                invariant(
                    offset > expected_offset,
                    "library continuation did not advance",
                )?;
                PageRequest::new(limit, offset)
            }
            Some(PageContinuation::Cursor(cursor)) => {
                PageRequest::with_cursor(limit, items.len() as u64, cursor)
            }
            None => return Ok(items),
        };
    }
    Err(conformance_error("library pagination exceeded 1000 pages"))
}

fn search_fixture<'a>(
    fixtures: &'a ConformanceFixtures,
    kind: &MediaKind,
) -> ProviderResult<&'a SearchFixture> {
    fixtures
        .search
        .iter()
        .find(|fixture| &fixture.kind == kind)
        .ok_or_else(|| conformance_error(format!("advertised search kind {kind} lacks fixture")))
}

fn catalog_fixture<'a>(
    fixtures: &'a ConformanceFixtures,
    kind: &MediaKind,
) -> ProviderResult<&'a ResourceUri> {
    fixtures
        .catalog_items
        .iter()
        .find(|uri| &uri.kind() == kind)
        .ok_or_else(|| conformance_error(format!("advertised lookup kind {kind} lacks fixture")))
}

fn library_fixture<'a>(
    fixtures: &'a ConformanceFixtures,
    kind: &MediaKind,
) -> ProviderResult<&'a LibraryFixture> {
    fixtures
        .library
        .iter()
        .find(|fixture| &fixture.kind == kind)
        .ok_or_else(|| conformance_error(format!("advertised library kind {kind} lacks fixture")))
}

fn require<T>(enabled: bool, fixture: Option<&T>, label: &str) -> ProviderResult<()> {
    invariant(
        !enabled || fixture.is_some(),
        format!("advertised {label} capability lacks fixture"),
    )
}

fn fixture_uris(fixtures: &ConformanceFixtures) -> Vec<&ResourceUri> {
    let mut uris = fixtures.catalog_items.iter().collect::<Vec<_>>();
    uris.extend(fixtures.search.iter().map(|fixture| &fixture.expected_uri));
    uris.extend(fixtures.recently_played.iter());
    for fixture in &fixtures.library {
        uris.push(&fixture.initially_saved);
        uris.extend(fixture.writable_unsaved.iter());
    }
    uris.extend(fixtures.album.iter());
    uris.extend(fixtures.artist.iter());
    uris.extend(fixtures.show.iter());
    if let Some(fixture) = &fixtures.playlist {
        uris.push(&fixture.uri);
        uris.extend(&fixture.initial_items);
    }
    if let Some(fixture) = &fixtures.transport {
        uris.push(&fixture.primary);
        uris.push(&fixture.secondary);
    }
    uris
}

fn collection(uri: &ResourceUri, limit: u32) -> CollectionRequest {
    CollectionRequest {
        uri: uri.clone(),
        page: PageRequest::new(limit, 0),
    }
}

fn validate_nonempty_page<P>(
    provider: &P,
    page: &spotuify_core::ProviderPage<MediaItem>,
    requested_limit: u32,
) -> ProviderResult<()>
where
    P: MusicProvider + ?Sized,
{
    validate_page(
        provider,
        &page.items,
        page.requested_offset,
        0,
        requested_limit,
    )?;
    invariant(
        !page.items.is_empty(),
        "relationship returned no fixture items",
    )
}

fn validate_page<P>(
    provider: &P,
    items: &[MediaItem],
    requested_offset: u64,
    expected_offset: u64,
    requested_limit: u32,
) -> ProviderResult<()>
where
    P: MusicProvider + ?Sized,
{
    for item in items {
        canonical_item_uri(provider, item)?;
    }
    invariant(
        requested_offset == expected_offset,
        "provider page did not echo requested offset",
    )?;
    invariant(
        items.len() <= requested_limit as usize,
        "provider page exceeded requested limit",
    )
}

fn validate_continuation<T>(
    page: &spotuify_core::ProviderPage<T>,
    requested_limit: u32,
) -> ProviderResult<()> {
    if page.items.len() as u32 == requested_limit
        && page
            .total
            .is_some_and(|total| total > page.items.len() as u64)
    {
        invariant(
            matches!(
                page.next,
                Some(PageContinuation::Offset(_)) | Some(PageContinuation::Cursor(_))
            ),
            "paged response omitted its continuation",
        )?;
    }
    Ok(())
}

async fn probe_page_bounds<T, F, Fut>(max: Option<usize>, mut call: F) -> ProviderResult<()>
where
    F: FnMut(PageRequest) -> Fut,
    Fut: std::future::Future<Output = ProviderResult<spotuify_core::ProviderPage<T>>>,
{
    assert_invalid_limit(call(PageRequest::new(0, 0)).await)?;
    if let Some(over_limit) = max.and_then(over_limit) {
        assert_invalid_limit(call(PageRequest::new(over_limit, 0)).await)?;
    }
    Ok(())
}

async fn probe_collection_bounds<T, F, Fut>(
    max: Option<usize>,
    uri: &ResourceUri,
    mut call: F,
) -> ProviderResult<()>
where
    F: FnMut(CollectionRequest) -> Fut,
    Fut: std::future::Future<Output = ProviderResult<spotuify_core::ProviderPage<T>>>,
{
    assert_invalid_limit(
        call(CollectionRequest {
            uri: uri.clone(),
            page: PageRequest::new(0, 0),
        })
        .await,
    )?;
    if let Some(over_limit) = max.and_then(over_limit) {
        assert_invalid_limit(
            call(CollectionRequest {
                uri: uri.clone(),
                page: PageRequest::new(over_limit, 0),
            })
            .await,
        )?;
    }
    Ok(())
}

fn over_limit(maximum: usize) -> Option<u32> {
    u32::try_from(maximum).ok()?.checked_add(1)
}

fn assert_receipt<P>(
    provider: &P,
    receipt: &spotuify_core::MutationReceipt,
    id: Uuid,
) -> ProviderResult<()>
where
    P: MusicProvider + ?Sized,
{
    invariant(
        receipt.mutation_id == id,
        "receipt changed mutation identity",
    )?;
    invariant(
        &receipt.provider == provider.id(),
        "receipt changed provider identity",
    )?;
    invariant(
        receipt.completion == MutationCompletion::Applied && receipt.failures.is_empty(),
        "atomic conformance mutation returned partial completion",
    )
}

fn assert_invalid_limit<T>(result: ProviderResult<T>) -> ProviderResult<()> {
    invariant(
        matches!(result, Err(ProviderError::InvalidInput { ref field, .. }) if field == "limit"),
        "provider accepted a page larger than its advertised maximum",
    )
}

fn assert_invalid_batch<T>(result: ProviderResult<T>) -> ProviderResult<()> {
    invariant(
        matches!(
            result,
            Err(ProviderError::InvalidInput { ref field, .. })
                if field == "items" || field == "uris"
        ),
        "provider accepted a mutation batch larger than its advertised maximum",
    )
}

fn canonical_item_uri<P>(provider: &P, item: &MediaItem) -> ProviderResult<ResourceUri>
where
    P: MusicProvider + ?Sized,
{
    let uri = ResourceUri::parse(&item.uri)
        .map_err(|err| conformance_error(format!("item URI is not canonical: {err}")))?;
    invariant(
        uri.scheme() == provider.uri_scheme(),
        "item URI belongs to another provider namespace",
    )?;
    invariant(
        uri.kind() == item.kind,
        "item URI kind differs from item kind",
    )?;
    Ok(uri)
}

fn active_device(devices: &[spotuify_core::Device]) -> ProviderResult<&spotuify_core::Device> {
    devices
        .iter()
        .find(|device| device.is_active)
        .ok_or_else(|| conformance_error("device mutation has no active device"))
}

fn playback_uri(playback: &spotuify_core::Playback) -> Option<String> {
    playback.item.as_ref().map(|item| item.uri.clone())
}

fn invariant(condition: bool, message: impl Into<String>) -> ProviderResult<()> {
    if condition {
        Ok(())
    } else {
        Err(conformance_error(message))
    }
}

fn conformance_error(message: impl Into<String>) -> ProviderError {
    ProviderError::Provider(format!("provider conformance failed: {}", message.into()))
}
