//! Deterministic, stateful provider adapter and reusable conformance harness.

pub mod conformance;
mod fixtures;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::env;
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use spotuify_core::{
    AccessOutcome, CatalogCaps, CollectionRequest, Device, FreshnessProbe, LibraryCaps,
    LibraryRequest, MediaItem, MediaKind, MusicProvider, Mutation, MutationCompletion,
    MutationOutcome, MutationReceipt, PageContinuation, PageRequest, PlaySource, Playback,
    Playlist, PlaylistCaps, ProviderCaps, ProviderError, ProviderExtrasCaps, ProviderId,
    ProviderPage, ProviderResult, Queue, RemoteTransport, RequestContext, RequestPriority,
    ResourceUri, SearchCaps, SearchRequest, TransportCaps, TransportCommand, TransportDevice,
    TransportOutcome, UriScheme,
};
use tokio::sync::Mutex;
use uuid::Uuid;

pub use conformance::{
    run_provider_conformance, run_transport_conformance, ConformanceFixtures, ConformanceOptions,
    LibraryFixture, PlaylistFixture, SearchFixture, TransportFixture,
};

pub const FAKE_DATASET_ENV: &str = "SPOTUIFY_FAKE_DATASET";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FakeDataset {
    #[default]
    Standard,
    /// Legacy `SPOTUIFY_FAKE_SPOTIFY` catalog and live transport snapshot.
    /// Kept here rather than in the Spotify adapter so production adapters do
    /// not carry test-only execution branches.
    SpotifyCompatibility,
    Empty,
}

impl FromStr for FakeDataset {
    type Err = ProviderError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "standard" => Ok(Self::Standard),
            "spotify-compatibility" => Ok(Self::SpotifyCompatibility),
            "empty" => Ok(Self::Empty),
            other => Err(ProviderError::InvalidInput {
                field: FAKE_DATASET_ENV.to_string(),
                message: format!("unknown fake dataset `{other}`"),
            }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedRequest {
    pub operation: &'static str,
    pub priority: RequestPriority,
}

#[derive(Clone)]
pub struct FakeProvider {
    id: ProviderId,
    scheme: UriScheme,
    state: Arc<Mutex<FakeState>>,
    observed: Arc<Mutex<Vec<ObservedRequest>>>,
}

#[derive(Clone)]
struct AppliedMutation {
    mutation: Mutation,
    receipt: MutationReceipt,
}

struct FakePlaylist {
    metadata: Playlist,
    items: Vec<String>,
    version: u64,
}

struct FakeState {
    media: BTreeMap<String, MediaItem>,
    relations: BTreeMap<String, Vec<String>>,
    playlists: BTreeMap<String, FakePlaylist>,
    library: BTreeSet<String>,
    followed: BTreeSet<String>,
    recent: Vec<String>,
    applied_mutations: HashMap<Uuid, AppliedMutation>,
    next_playlist: u64,
    playback: Playback,
    queue: Queue,
    devices: Vec<Device>,
}

impl Default for FakeProvider {
    fn default() -> Self {
        Self::with_identity(
            ProviderId::new("fake").expect("built-in fake provider id is valid"),
            UriScheme::Fake,
            FakeDataset::Standard,
        )
    }
}

impl FakeProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_env() -> ProviderResult<Self> {
        let dataset = env::var(FAKE_DATASET_ENV)
            .ok()
            .map(|value| value.parse())
            .transpose()?
            .unwrap_or_default();
        Ok(Self::with_identity(
            ProviderId::new("fake").expect("built-in fake provider id is valid"),
            UriScheme::Fake,
            dataset,
        ))
    }

    /// Construct an isolated fake instance whose provider ID and URI scheme
    /// share `namespace` (for example `fake-a` and `fake-b`).
    pub fn isolated(namespace: &str) -> ProviderResult<Self> {
        let id = ProviderId::new(namespace).map_err(|err| ProviderError::InvalidInput {
            field: "provider_id".to_string(),
            message: err.to_string(),
        })?;
        let scheme = UriScheme::new(namespace).map_err(|err| ProviderError::InvalidInput {
            field: "uri_scheme".to_string(),
            message: err.to_string(),
        })?;
        Ok(Self::with_identity(id, scheme, FakeDataset::Standard))
    }

    pub fn with_identity(id: ProviderId, scheme: UriScheme, dataset: FakeDataset) -> Self {
        let fixture = match dataset {
            FakeDataset::Standard => fixtures::standard(&id, &scheme),
            FakeDataset::SpotifyCompatibility => fixtures::spotify_compatibility(&id, &scheme),
            FakeDataset::Empty => fixtures::Fixtures {
                media: BTreeMap::new(),
                relations: BTreeMap::new(),
                playlists: Vec::new(),
                library: Vec::new(),
                followed: Vec::new(),
                recent: Vec::new(),
            },
        };
        let playlists = fixture
            .playlists
            .into_iter()
            .map(|(metadata, items)| {
                let key = metadata.id.clone();
                (
                    key,
                    FakePlaylist {
                        metadata,
                        items,
                        version: 1,
                    },
                )
            })
            .collect();
        let compatibility = dataset == FakeDataset::SpotifyCompatibility;
        let device = if compatibility {
            Device {
                id: Some("fake-device".to_string()),
                name: "spotuify-fake".to_string(),
                kind: "Computer".to_string(),
                is_active: true,
                is_restricted: false,
                volume_percent: Some(70),
                supports_volume: true,
            }
        } else {
            Device {
                id: Some(format!("{}-device", id.as_str())),
                name: format!("{} player", id.as_str()),
                kind: "computer".to_string(),
                is_active: true,
                is_restricted: false,
                volume_percent: Some(50),
                supports_volume: true,
            }
        };
        let inactive_device = Device {
            id: Some(format!("{}-device-2", id.as_str())),
            name: format!("{} secondary player", id.as_str()),
            kind: "computer".to_string(),
            is_active: false,
            is_restricted: false,
            volume_percent: Some(50),
            supports_volume: true,
        };
        // Seed keys must be derived from this instance's scheme exactly as the
        // fixtures build them; hardcoding `spotify:track:…` left the snapshot
        // silently empty under any non-`spotify` scheme (e.g. `fake`).
        let never_too_much = fixtures::uri(&scheme, MediaKind::Track, "never-too-much");
        let sweet_thing = fixtures::uri(&scheme, MediaKind::Track, "sweet-thing");
        let playback = if compatibility {
            Playback {
                item: fixture.media.get(&never_too_much).cloned(),
                device: Some(device.clone()),
                is_playing: true,
                progress_ms: 42_000,
                source: Some(spotuify_core::PlaybackStateSource::RemotePoll),
                ..Default::default()
            }
        } else {
            Playback::default()
        };
        let queue = if compatibility {
            Queue {
                currently_playing: fixture.media.get(&never_too_much).cloned(),
                items: fixture
                    .media
                    .get(&sweet_thing)
                    .cloned()
                    .into_iter()
                    .collect(),
                session_active: true,
                as_of_ms: 0,
            }
        } else {
            Queue::default()
        };
        Self {
            id,
            scheme,
            state: Arc::new(Mutex::new(FakeState {
                media: fixture.media,
                relations: fixture.relations,
                playlists,
                library: fixture.library.into_iter().collect(),
                followed: fixture.followed.into_iter().collect(),
                recent: fixture.recent,
                applied_mutations: HashMap::new(),
                next_playlist: 2,
                playback,
                queue,
                devices: vec![device, inactive_device],
            })),
            observed: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn observed_requests(&self) -> Vec<ObservedRequest> {
        self.observed.lock().await.clone()
    }

    async fn observe(&self, operation: &'static str, context: RequestContext) {
        self.observed.lock().await.push(ObservedRequest {
            operation,
            priority: context.priority,
        });
    }

    fn ensure_own_uri(&self, uri: &ResourceUri) -> ProviderResult<()> {
        if uri.scheme() != &self.scheme {
            return Err(ProviderError::InvalidInput {
                field: "uri".to_string(),
                message: format!(
                    "URI scheme `{}` does not belong to provider namespace `{}`",
                    uri.scheme(),
                    self.scheme
                ),
            });
        }
        Ok(())
    }
}

#[async_trait]
impl MusicProvider for FakeProvider {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn uri_scheme(&self) -> &UriScheme {
        &self.scheme
    }

    fn display_name(&self) -> &str {
        "Fake Provider"
    }

    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            search: SearchCaps {
                remote: true,
                kinds: all_media_kinds(),
                max_page_size: Some(100),
                max_query_chars: None,
            },
            catalog: CatalogCaps {
                lookup_kinds: all_media_kinds(),
                recently_played: true,
                recently_played_max_page_size: Some(100),
                album_tracks: true,
                album_tracks_max_page_size: Some(100),
                artist_albums: true,
                artist_albums_max_page_size: Some(100),
                show_episodes: true,
                show_episodes_max_page_size: Some(100),
            },
            library: LibraryCaps {
                read_kinds: vec![MediaKind::Track, MediaKind::Artist],
                save_kinds: vec![MediaKind::Track],
                follow_kinds: vec![MediaKind::Artist],
                mutation_max_batch: Some(100),
                max_page_size: Some(100),
                freshness_probe: true,
            },
            playlists: PlaylistCaps {
                list: true,
                item_read: true,
                create: true,
                add: true,
                remove: true,
                reorder: true,
                image: true,
                unfollow: true,
                version_tokens: true,
                list_max_page_size: Some(100),
                items_max_page_size: Some(100),
                add_max_batch: Some(100),
                remove_max_batch: Some(100),
            },
            transport: Some(TransportCaps {
                playback_state: true,
                play: true,
                pause: true,
                resume: true,
                next: true,
                previous: true,
                seek: true,
                volume: true,
                shuffle: true,
                repeat: true,
                queue_read: true,
                queue_snapshots_complete: true,
                queue_add: true,
                devices: true,
                transfer: true,
            }),
            extras: ProviderExtrasCaps::default(),
        }
    }

    async fn search(
        &self,
        context: RequestContext,
        request: SearchRequest,
    ) -> ProviderResult<ProviderPage<MediaItem>> {
        self.observe("search", context).await;
        let needle = request.query.to_ascii_lowercase();
        let state = self.state.lock().await;
        let values = state
            .media
            .values()
            .filter(|item| item.kind == request.kind)
            .filter(|item| {
                needle.is_empty()
                    || format!("{} {} {}", item.name, item.subtitle, item.context)
                        .to_ascii_lowercase()
                        .contains(&needle)
            })
            .cloned()
            .collect();
        page(values, request.page, 100)
    }

    async fn media_item(
        &self,
        context: RequestContext,
        uri: &ResourceUri,
    ) -> ProviderResult<Option<MediaItem>> {
        self.observe("media_item", context).await;
        self.ensure_own_uri(uri)?;
        Ok(self.state.lock().await.media.get(&uri.as_uri()).cloned())
    }

    async fn recently_played(
        &self,
        context: RequestContext,
        page_request: PageRequest,
    ) -> ProviderResult<ProviderPage<MediaItem>> {
        self.observe("recently_played", context).await;
        let state = self.state.lock().await;
        page(media_for_uris(&state, &state.recent), page_request, 100)
    }

    async fn library_items(
        &self,
        context: RequestContext,
        request: LibraryRequest,
    ) -> ProviderResult<ProviderPage<MediaItem>> {
        self.observe("library_items", context).await;
        if !self.capabilities().library.can_read(&request.kind) {
            return Err(ProviderError::unsupported(format!(
                "library_items.{}",
                request.kind
            )));
        }
        let state = self.state.lock().await;
        let values = state
            .library
            .iter()
            .chain(state.followed.iter())
            .filter_map(|uri| state.media.get(uri))
            .filter(|item| item.kind == request.kind)
            .cloned()
            .collect();
        page(values, request.page, 100)
    }

    async fn library_freshness_probe(
        &self,
        context: RequestContext,
        kind: MediaKind,
    ) -> ProviderResult<FreshnessProbe> {
        self.observe("library_freshness_probe", context).await;
        let library_caps = self.capabilities().library;
        if !library_caps.freshness_probe || !library_caps.can_read(&kind) {
            return Err(ProviderError::unsupported(format!(
                "library_freshness_probe.{kind}"
            )));
        }
        let state = self.state.lock().await;
        let token = state
            .library
            .iter()
            .chain(state.followed.iter())
            .filter(|uri| state.media.get(*uri).is_some_and(|item| item.kind == kind))
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
            .into_bytes();
        Ok(FreshnessProbe(token))
    }

    async fn playlists(
        &self,
        context: RequestContext,
        page_request: PageRequest,
    ) -> ProviderResult<ProviderPage<Playlist>> {
        self.observe("playlists", context).await;
        let state = self.state.lock().await;
        page(
            state
                .playlists
                .values()
                .map(|playlist| playlist.metadata.clone())
                .collect(),
            page_request,
            100,
        )
    }

    async fn playlist(
        &self,
        context: RequestContext,
        uri: &ResourceUri,
    ) -> ProviderResult<Option<Playlist>> {
        self.observe("playlist", context).await;
        self.ensure_own_uri(uri)?;
        Ok(self
            .state
            .lock()
            .await
            .playlists
            .get(&uri.as_uri())
            .map(|playlist| playlist.metadata.clone()))
    }

    async fn playlist_items(
        &self,
        context: RequestContext,
        request: CollectionRequest,
    ) -> ProviderResult<AccessOutcome<ProviderPage<MediaItem>>> {
        self.observe("playlist_items", context).await;
        self.ensure_own_uri(&request.uri)?;
        let state = self.state.lock().await;
        let playlist =
            state
                .playlists
                .get(&request.uri.as_uri())
                .ok_or_else(|| ProviderError::NotFound {
                    resource: request.uri.as_uri(),
                })?;
        Ok(AccessOutcome::Available(page(
            media_for_uris(&state, &playlist.items),
            request.page,
            100,
        )?))
    }

    async fn album_tracks(
        &self,
        context: RequestContext,
        request: CollectionRequest,
    ) -> ProviderResult<ProviderPage<MediaItem>> {
        self.observe("album_tracks", context).await;
        relation_page(self, request, MediaKind::Album, 100).await
    }

    async fn artist_albums(
        &self,
        context: RequestContext,
        request: CollectionRequest,
    ) -> ProviderResult<ProviderPage<MediaItem>> {
        self.observe("artist_albums", context).await;
        relation_page(self, request, MediaKind::Artist, 100).await
    }

    async fn show_episodes(
        &self,
        context: RequestContext,
        request: CollectionRequest,
    ) -> ProviderResult<ProviderPage<MediaItem>> {
        self.observe("show_episodes", context).await;
        relation_page(self, request, MediaKind::Show, 100).await
    }

    async fn apply_mutation(
        &self,
        context: RequestContext,
        mutation_id: Uuid,
        mutation: &Mutation,
    ) -> ProviderResult<MutationReceipt> {
        self.observe("apply_mutation", context).await;
        let mut state = self.state.lock().await;
        // DIVERGENCE FROM REAL ADAPTERS: the fake replays a mutation by its
        // `mutation_id`, returning the cached receipt without re-applying. The
        // Spotify adapter has no such memory and re-applies on retry — only the
        // daemon's durable operation claim provides exactly-once semantics
        // (plan / D027: adapters offer at most best-effort replay suppression).
        if let Some(applied) = state.applied_mutations.get(&mutation_id) {
            if &applied.mutation != mutation {
                return Err(ProviderError::InvalidInput {
                    field: "mutation_id".to_string(),
                    message: "mutation UUID was already used for a different mutation".to_string(),
                });
            }
            return Ok(applied.receipt.clone());
        }

        let receipt = apply_mutation(self, &mut state, mutation_id, mutation)?;
        state.applied_mutations.insert(
            mutation_id,
            AppliedMutation {
                mutation: mutation.clone(),
                receipt: receipt.clone(),
            },
        );
        Ok(receipt)
    }
}

#[async_trait]
impl RemoteTransport for FakeProvider {
    fn provider_id(&self) -> &ProviderId {
        &self.id
    }

    fn uri_scheme(&self) -> &UriScheme {
        &self.scheme
    }

    async fn playback(&self, context: RequestContext) -> ProviderResult<Playback> {
        self.observe("transport.playback", context).await;
        Ok(self.state.lock().await.playback.clone())
    }

    async fn devices(&self, context: RequestContext) -> ProviderResult<Vec<Device>> {
        self.observe("transport.devices", context).await;
        Ok(self.state.lock().await.devices.clone())
    }

    async fn queue(&self, context: RequestContext) -> ProviderResult<Queue> {
        self.observe("transport.queue", context).await;
        Ok(self.state.lock().await.queue.clone())
    }

    async fn execute(
        &self,
        _context: RequestContext,
        command: TransportCommand,
    ) -> ProviderResult<TransportOutcome> {
        // Transport writes always use the latency-sensitive lane.
        self.observe("transport.execute", RequestContext::PLAYBACK_CONTROL)
            .await;
        let mut state = self.state.lock().await;
        apply_transport(self, &mut state, command)?;
        Ok(TransportOutcome {
            playback: Some(state.playback.clone()),
            queue: Some(state.queue.clone()),
            devices: Some(state.devices.clone()),
        })
    }
}

async fn relation_page(
    provider: &FakeProvider,
    request: CollectionRequest,
    expected_kind: MediaKind,
    max_page_size: usize,
) -> ProviderResult<ProviderPage<MediaItem>> {
    provider.ensure_own_uri(&request.uri)?;
    if request.uri.kind() != expected_kind {
        return Err(ProviderError::InvalidInput {
            field: "uri".to_string(),
            message: format!("expected {expected_kind} URI, got {}", request.uri.kind()),
        });
    }
    let state = provider.state.lock().await;
    let uris =
        state
            .relations
            .get(&request.uri.as_uri())
            .ok_or_else(|| ProviderError::NotFound {
                resource: request.uri.as_uri(),
            })?;
    page(media_for_uris(&state, uris), request.page, max_page_size)
}

fn apply_mutation(
    provider: &FakeProvider,
    state: &mut FakeState,
    mutation_id: Uuid,
    mutation: &Mutation,
) -> ProviderResult<MutationReceipt> {
    let (field, count, maximum) = match mutation {
        Mutation::PlaylistAdd { items, .. } => ("items", items.len(), 100),
        Mutation::PlaylistRemove { items, .. } => ("items", items.len(), 100),
        Mutation::LibrarySave { uris }
        | Mutation::LibraryUnsave { uris }
        | Mutation::Follow { uris }
        | Mutation::Unfollow { uris } => ("uris", uris.len(), 100),
        _ => ("", 0, usize::MAX),
    };
    if count > maximum {
        return Err(ProviderError::InvalidInput {
            field: field.to_string(),
            message: format!("batch size {count} exceeds provider maximum {maximum}"),
        });
    }
    let (outcome, version_token) = match mutation {
        Mutation::PlaylistCreate { name, .. } => {
            if name.trim().is_empty() {
                return Err(ProviderError::InvalidInput {
                    field: "name".to_string(),
                    message: "playlist name cannot be empty".to_string(),
                });
            }
            let uri = ResourceUri::new(
                provider.scheme.clone(),
                MediaKind::Playlist,
                format!("playlist-{}", state.next_playlist),
            )
            .map_err(|err| ProviderError::InvalidInput {
                field: "playlist_uri".to_string(),
                message: err.to_string(),
            })?;
            state.next_playlist += 1;
            let playlist = Playlist {
                id: uri.as_uri(),
                name: name.clone(),
                owner: provider.id.to_string(),
                tracks_total: 0,
                image_url: None,
                version_token: Some("v1".to_string()),
            };
            state.playlists.insert(
                uri.as_uri(),
                FakePlaylist {
                    metadata: playlist.clone(),
                    items: Vec::new(),
                    version: 1,
                },
            );
            (
                MutationOutcome::PlaylistCreated { playlist },
                Some("v1".to_string()),
            )
        }
        Mutation::PlaylistAdd {
            playlist_uri,
            items,
            expected_version,
        } => {
            provider.ensure_own_uri(playlist_uri)?;
            for item in items {
                provider.ensure_own_uri(&item.uri)?;
                if !state.media.contains_key(&item.uri.as_uri()) {
                    return Err(ProviderError::NotFound {
                        resource: item.uri.as_uri(),
                    });
                }
            }
            let playlist = playlist_mut(state, playlist_uri, expected_version)?;
            let original_items = playlist.items.clone();
            let mut updated_items = original_items.clone();
            let mut positioned = items
                .iter()
                .enumerate()
                .filter_map(|(order, insertion)| {
                    insertion
                        .position
                        .map(|position| (position as usize, order, insertion.uri.as_uri()))
                })
                .collect::<Vec<_>>();
            positioned.sort_by_key(|(position, order, _)| (*position, *order));
            let mut index = 0;
            while index < positioned.len() {
                let position = positioned[index].0;
                if position > updated_items.len() {
                    return Err(invalid_position(position, updated_items.len()));
                }
                let mut group_offset = 0;
                while index < positioned.len() && positioned[index].0 == position {
                    updated_items.insert(position + group_offset, positioned[index].2.clone());
                    group_offset += 1;
                    index += 1;
                }
            }
            for insertion in items.iter().filter(|item| item.position.is_none()) {
                updated_items.push(insertion.uri.as_uri());
            }
            playlist.items = updated_items;
            let token = if playlist.items == original_items {
                playlist.metadata.version_token.clone()
            } else {
                Some(advance_playlist(playlist))
            };
            (
                MutationOutcome::PlaylistChanged {
                    playlist_uri: playlist_uri.clone(),
                },
                token,
            )
        }
        Mutation::PlaylistRemove {
            playlist_uri,
            items,
            expected_version,
        } => {
            provider.ensure_own_uri(playlist_uri)?;
            for item in items {
                provider.ensure_own_uri(&item.uri)?;
            }
            let playlist = playlist_mut(state, playlist_uri, expected_version)?;
            let original_items = playlist.items.clone();
            let mut updated_items = original_items.clone();
            let mut positioned = Vec::new();
            let remove_all = items
                .iter()
                .filter(|item| item.positions.is_empty())
                .map(|item| item.uri.as_uri())
                .collect::<BTreeSet<_>>();
            for item in items.iter().filter(|item| !item.positions.is_empty()) {
                for position in &item.positions {
                    positioned.push((*position as usize, item.uri.as_uri()));
                }
            }
            positioned.sort_by_key(|entry| std::cmp::Reverse(entry.0));
            for (position, uri) in positioned {
                if updated_items.get(position) != Some(&uri) {
                    return Err(invalid_position(position, updated_items.len()));
                }
                updated_items.remove(position);
            }
            updated_items.retain(|uri| !remove_all.contains(uri));
            playlist.items = updated_items;
            let token = if playlist.items == original_items {
                playlist.metadata.version_token.clone()
            } else {
                Some(advance_playlist(playlist))
            };
            (
                MutationOutcome::PlaylistChanged {
                    playlist_uri: playlist_uri.clone(),
                },
                token,
            )
        }
        Mutation::PlaylistReorder {
            playlist_uri,
            range_start,
            insert_before,
            range_length,
            expected_version,
        } => {
            provider.ensure_own_uri(playlist_uri)?;
            let playlist = playlist_mut(state, playlist_uri, expected_version)?;
            let original_items = playlist.items.clone();
            let mut updated_items = original_items.clone();
            let start = *range_start as usize;
            let length = *range_length as usize;
            let end = start.saturating_add(length);
            let before = *insert_before as usize;
            if length == 0 {
                return Ok(MutationReceipt {
                    mutation_id,
                    provider: provider.id.clone(),
                    completion: MutationCompletion::Applied,
                    outcome: MutationOutcome::PlaylistChanged {
                        playlist_uri: playlist_uri.clone(),
                    },
                    version_token: playlist.metadata.version_token.clone(),
                    failures: Vec::new(),
                });
            }
            if end > updated_items.len() || before > updated_items.len() {
                return Err(invalid_position(end.max(before), updated_items.len()));
            }
            let moved = updated_items.drain(start..end).collect::<Vec<_>>();
            let destination = if before > start {
                before.saturating_sub(length)
            } else {
                before
            };
            if destination > updated_items.len() {
                return Err(invalid_position(destination, updated_items.len()));
            }
            updated_items.splice(destination..destination, moved);
            playlist.items = updated_items;
            let token = if playlist.items == original_items {
                playlist.metadata.version_token.clone()
            } else {
                Some(advance_playlist(playlist))
            };
            (
                MutationOutcome::PlaylistChanged {
                    playlist_uri: playlist_uri.clone(),
                },
                token,
            )
        }
        Mutation::PlaylistSetImage { playlist_uri, jpeg } => {
            provider.ensure_own_uri(playlist_uri)?;
            // DIVERGENCE FROM REAL ADAPTER: the fake only rejects an empty
            // image. The Spotify adapter also rejects any image larger than
            // 256 KB after base64 encoding, so a payload the fake accepts may
            // still fail against real Spotify.
            if jpeg.is_empty() {
                return Err(ProviderError::InvalidInput {
                    field: "jpeg".to_string(),
                    message: "playlist image cannot be empty".to_string(),
                });
            }
            let playlist = playlist_mut(state, playlist_uri, &None)?;
            playlist.metadata.image_url = Some(format!("fake://image/{}", playlist_uri.bare_id()));
            let token = playlist.metadata.version_token.clone();
            (
                MutationOutcome::PlaylistImageSet {
                    playlist_uri: playlist_uri.clone(),
                },
                token,
            )
        }
        Mutation::PlaylistUnfollow { playlist_uri } => {
            provider.ensure_own_uri(playlist_uri)?;
            if state.playlists.remove(&playlist_uri.as_uri()).is_none() {
                return Err(ProviderError::NotFound {
                    resource: playlist_uri.as_uri(),
                });
            }
            (
                MutationOutcome::PlaylistUnfollowed {
                    playlist_uri: playlist_uri.clone(),
                },
                None,
            )
        }
        Mutation::LibrarySave { uris } | Mutation::LibraryUnsave { uris } => {
            let saved = matches!(mutation, Mutation::LibrarySave { .. });
            let caps = provider.capabilities().library;
            for uri in uris {
                provider.ensure_own_uri(uri)?;
                if !caps.can_save(&uri.kind()) {
                    return Err(ProviderError::unsupported(format!(
                        "library_save.{}",
                        uri.kind()
                    )));
                }
                if !state.media.contains_key(&uri.as_uri()) {
                    return Err(ProviderError::NotFound {
                        resource: uri.as_uri(),
                    });
                }
            }
            for uri in uris {
                if saved {
                    state.library.insert(uri.as_uri());
                } else {
                    state.library.remove(&uri.as_uri());
                }
            }
            (
                MutationOutcome::LibraryChanged {
                    uris: uris.clone(),
                    saved,
                },
                None,
            )
        }
        Mutation::Follow { uris } | Mutation::Unfollow { uris } => {
            let following = matches!(mutation, Mutation::Follow { .. });
            let caps = provider.capabilities().library;
            for uri in uris {
                provider.ensure_own_uri(uri)?;
                if !caps.can_follow(&uri.kind()) {
                    return Err(ProviderError::unsupported(format!("follow.{}", uri.kind())));
                }
                if !state.media.contains_key(&uri.as_uri()) {
                    return Err(ProviderError::NotFound {
                        resource: uri.as_uri(),
                    });
                }
            }
            for uri in uris {
                if following {
                    state.followed.insert(uri.as_uri());
                } else {
                    state.followed.remove(&uri.as_uri());
                }
            }
            (
                MutationOutcome::FollowChanged {
                    uris: uris.clone(),
                    following,
                },
                None,
            )
        }
    };

    Ok(MutationReceipt {
        mutation_id,
        provider: provider.id.clone(),
        completion: MutationCompletion::Applied,
        outcome,
        version_token,
        failures: Vec::new(),
    })
}

fn apply_transport(
    provider: &FakeProvider,
    state: &mut FakeState,
    command: TransportCommand,
) -> ProviderResult<()> {
    match command {
        TransportCommand::Play(request) => {
            request.validate()?;
            provider.ensure_own_uri(&request.start_uri)?;
            let ordered_source = match &request.source {
                // A collection URI used as a single play target means "play
                // this collection from its first item". This matches the
                // Spotify and embedded-player adapters; only playable track /
                // episode targets are truly single-item sources.
                PlaySource::Single => single_play_source(state, &request.start_uri)?,
                PlaySource::Context(uri) => {
                    provider.ensure_own_uri(uri)?;
                    // Aligned with the Spotify adapter (provider.rs): a play
                    // context must be an album or playlist. Rejecting the same
                    // kinds keeps fake-driven daemon development honest.
                    if !matches!(uri.kind(), MediaKind::Album | MediaKind::Playlist) {
                        return Err(ProviderError::InvalidInput {
                            field: "source".to_string(),
                            message: format!(
                                "context playback requires an album or playlist URI, got {}",
                                uri.kind()
                            ),
                        });
                    }
                    let uris = if uri.kind() == MediaKind::Playlist {
                        state
                            .playlists
                            .get(&uri.as_uri())
                            .map(|playlist| playlist.items.clone())
                    } else {
                        state.relations.get(&uri.as_uri()).cloned()
                    }
                    .ok_or_else(|| ProviderError::NotFound {
                        resource: uri.as_uri(),
                    })?;
                    Some(uris)
                }
                PlaySource::Ordered(uris) => {
                    for uri in uris {
                        provider.ensure_own_uri(uri)?;
                        if !state.media.contains_key(&uri.as_uri()) {
                            return Err(ProviderError::NotFound {
                                resource: uri.as_uri(),
                            });
                        }
                    }
                    Some(uris.iter().map(ResourceUri::as_uri).collect())
                }
            };
            let start_uri = if matches!(
                request.start_uri.kind(),
                MediaKind::Track | MediaKind::Episode
            ) {
                request.start_uri.as_uri()
            } else {
                ordered_source
                    .as_ref()
                    .and_then(|uris| uris.first())
                    .cloned()
                    .ok_or_else(|| ProviderError::NotFound {
                        resource: request.start_uri.as_uri(),
                    })?
            };
            if ordered_source
                .as_ref()
                .is_some_and(|uris| !uris.contains(&start_uri))
            {
                return Err(ProviderError::InvalidInput {
                    field: "source".to_string(),
                    message: "playback source must contain start_uri".to_string(),
                });
            }
            let item =
                state
                    .media
                    .get(&start_uri)
                    .cloned()
                    .ok_or_else(|| ProviderError::NotFound {
                        resource: start_uri.clone(),
                    })?;
            activate_device(&mut state.devices, &request.device)?;
            state.playback.item = Some(item.clone());
            state.playback.device = state
                .devices
                .iter()
                .find(|device| device.is_active)
                .cloned();
            state.playback.is_playing = true;
            state.playback.progress_ms = request.position_ms;
            state.queue.currently_playing = Some(item);
            state.queue.session_active = true;
            if let Some(uris) = ordered_source {
                let index = uris
                    .iter()
                    .position(|uri| uri == &start_uri)
                    .expect("validated playback source contains start URI");
                state.queue.items = uris[index + 1..]
                    .iter()
                    .filter_map(|uri| state.media.get(uri).cloned())
                    .collect();
            } else {
                state.queue.items.clear();
            }
        }
        TransportCommand::Pause => state.playback.is_playing = false,
        TransportCommand::Resume => {
            if state.playback.item.is_none() {
                return Err(ProviderError::NoActiveDevice);
            }
            state.playback.is_playing = true;
        }
        TransportCommand::Next => {
            if !state.queue.items.is_empty() {
                let next = state.queue.items.remove(0);
                state.playback.item = Some(next.clone());
                state.playback.progress_ms = 0;
                state.queue.currently_playing = Some(next);
            }
        }
        TransportCommand::Previous => state.playback.progress_ms = 0,
        TransportCommand::Seek { position_ms } => state.playback.progress_ms = position_ms,
        TransportCommand::Volume { percent } => {
            if percent > 100 {
                return Err(ProviderError::InvalidInput {
                    field: "percent".to_string(),
                    message: "volume must be between 0 and 100".to_string(),
                });
            }
            let active = state
                .devices
                .iter_mut()
                .find(|device| device.is_active)
                .ok_or(ProviderError::NoActiveDevice)?;
            active.volume_percent = Some(percent);
            state.playback.device = Some(active.clone());
        }
        TransportCommand::Shuffle { enabled } => state.playback.shuffle = enabled,
        TransportCommand::Repeat { mode } => state.playback.repeat = mode,
        TransportCommand::QueueAdd(request) => {
            provider.ensure_own_uri(&request.uri)?;
            activate_device(&mut state.devices, &request.device)?;
            let item = state
                .media
                .get(&request.uri.as_uri())
                .cloned()
                .ok_or_else(|| ProviderError::NotFound {
                    resource: request.uri.as_uri(),
                })?;
            state.queue.items.push(item);
            state.queue.session_active = true;
        }
        TransportCommand::Transfer { device_id, play } => {
            activate_device(&mut state.devices, &TransportDevice::Id(device_id))?;
            state.playback.device = state
                .devices
                .iter()
                .find(|device| device.is_active)
                .cloned();
            state.playback.is_playing = play;
        }
    }
    Ok(())
}

fn single_play_source(state: &FakeState, uri: &ResourceUri) -> ProviderResult<Option<Vec<String>>> {
    let uris = match uri.kind() {
        MediaKind::Track | MediaKind::Episode => return Ok(None),
        MediaKind::Album | MediaKind::Show => state.relations.get(&uri.as_uri()).cloned(),
        MediaKind::Playlist => state
            .playlists
            .get(&uri.as_uri())
            .map(|playlist| playlist.items.clone()),
        MediaKind::Artist => {
            let related =
                state
                    .relations
                    .get(&uri.as_uri())
                    .ok_or_else(|| ProviderError::NotFound {
                        resource: uri.as_uri(),
                    })?;
            let mut playable = Vec::new();
            for related_uri in related {
                match state.media.get(related_uri).map(|item| item.kind.clone()) {
                    Some(MediaKind::Track | MediaKind::Episode) => {
                        playable.push(related_uri.clone());
                    }
                    Some(MediaKind::Album) => {
                        let album_items = state.relations.get(related_uri).ok_or_else(|| {
                            ProviderError::NotFound {
                                resource: related_uri.clone(),
                            }
                        })?;
                        playable.extend(album_items.iter().cloned());
                    }
                    _ => {
                        return Err(ProviderError::InvalidInput {
                            field: "play.start_uri".to_string(),
                            message: format!(
                                "artist playback relation `{related_uri}` is not playable"
                            ),
                        });
                    }
                }
            }
            Some(playable)
        }
    }
    .ok_or_else(|| ProviderError::NotFound {
        resource: uri.as_uri(),
    })?;

    if uris.is_empty()
        || uris.iter().any(|item_uri| {
            !state
                .media
                .get(item_uri)
                .is_some_and(|item| matches!(item.kind, MediaKind::Track | MediaKind::Episode))
        })
    {
        return Err(ProviderError::InvalidInput {
            field: "play.start_uri".to_string(),
            message: format!("collection `{uri}` does not resolve to playable items"),
        });
    }
    Ok(Some(uris))
}

fn activate_device(devices: &mut [Device], target: &TransportDevice) -> ProviderResult<()> {
    match target {
        TransportDevice::Active => {
            if devices.iter().any(|device| device.is_active) {
                Ok(())
            } else {
                Err(ProviderError::NoActiveDevice)
            }
        }
        TransportDevice::Id(id) => {
            if !devices
                .iter()
                .any(|device| device.id.as_deref() == Some(id.as_str()))
            {
                return Err(ProviderError::NotFound {
                    resource: format!("device:{id}"),
                });
            }
            for device in devices {
                device.is_active = device.id.as_deref() == Some(id.as_str());
            }
            Ok(())
        }
    }
}

fn playlist_mut<'a>(
    state: &'a mut FakeState,
    uri: &ResourceUri,
    expected_version: &Option<String>,
) -> ProviderResult<&'a mut FakePlaylist> {
    let playlist =
        state
            .playlists
            .get_mut(&uri.as_uri())
            .ok_or_else(|| ProviderError::NotFound {
                resource: uri.as_uri(),
            })?;
    let actual = playlist.metadata.version_token.clone();
    if expected_version.is_some() && expected_version != &actual {
        return Err(ProviderError::VersionConflict {
            expected: expected_version.clone(),
            actual,
        });
    }
    Ok(playlist)
}

fn advance_playlist(playlist: &mut FakePlaylist) -> String {
    playlist.version += 1;
    playlist.metadata.tracks_total = playlist.items.len() as u64;
    let token = format!("v{}", playlist.version);
    playlist.metadata.version_token = Some(token.clone());
    token
}

fn invalid_position(position: usize, length: usize) -> ProviderError {
    ProviderError::InvalidInput {
        field: "position".to_string(),
        message: format!("position {position} is outside playlist length {length}"),
    }
}

fn media_for_uris(state: &FakeState, uris: &[String]) -> Vec<MediaItem> {
    uris.iter()
        .filter_map(|uri| state.media.get(uri).cloned())
        .collect()
}

fn page<T>(
    items: Vec<T>,
    request: PageRequest,
    max_page_size: usize,
) -> ProviderResult<ProviderPage<T>> {
    if request.limit == 0 {
        return Err(ProviderError::InvalidInput {
            field: "limit".to_string(),
            message: "page size must be greater than zero".to_string(),
        });
    }
    if request.limit as usize > max_page_size {
        return Err(ProviderError::InvalidInput {
            field: "limit".to_string(),
            message: format!(
                "requested page size {} exceeds provider maximum {max_page_size}",
                request.limit
            ),
        });
    }
    if request.cursor.is_some() {
        return Err(ProviderError::InvalidInput {
            field: "cursor".to_string(),
            message: "fake provider uses offset continuations".to_string(),
        });
    }
    let total = items.len() as u64;
    let requested_offset = request.offset;
    let items = items
        .into_iter()
        .skip(requested_offset as usize)
        .take(request.limit as usize)
        .collect::<Vec<_>>();
    let consumed = requested_offset.saturating_add(items.len() as u64);
    let next =
        (!items.is_empty() && consumed < total).then_some(PageContinuation::Offset(consumed));
    Ok(ProviderPage {
        items,
        requested_offset,
        total: Some(total),
        next,
    })
}

fn all_media_kinds() -> Vec<MediaKind> {
    vec![
        MediaKind::Track,
        MediaKind::Episode,
        MediaKind::Show,
        MediaKind::Album,
        MediaKind::Artist,
        MediaKind::Playlist,
    ]
}
