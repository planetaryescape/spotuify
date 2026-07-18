//! Provider-trait adapter for Spotify's Web API and Connect transport.

use async_trait::async_trait;
use base64::Engine as _;
use spotuify_core::{
    AccessOutcome, AccessUnavailable, CatalogCaps, CollectionRequest, FreshnessProbe, LibraryCaps,
    LibraryRequest, MediaItem, MediaKind, MusicProvider, Mutation, MutationCompletion,
    MutationFailure, MutationOutcome, MutationReceipt, PageContinuation, PageRequest, PlayRequest,
    PlaySource, Playlist, PlaylistCaps, ProviderCaps, ProviderError, ProviderExtrasCaps,
    ProviderId, ProviderPage, ProviderResult, RemoteTransport, RequestContext, RequestPriority,
    ResourceUri, SearchCaps, SearchRequest, TargetClaim, TransportCaps, TransportCommand,
    TransportDevice, TransportOutcome, UriScheme,
};
use uuid::Uuid;

use crate::client::SpotifyClient;
use crate::error::SpotifyError;
use crate::rate_limit::Priority;

const SEARCH_PAGE_MAX: u32 = 10;
const SEARCH_QUERY_MAX_CHARS: usize = 144;
const PAGE_MAX: u32 = 50;
const ARTIST_ALBUM_PAGE_MAX: u32 = 10;
const RECENT_PAGE_MAX: u32 = 20;
const PLAYLIST_MUTATION_MAX: usize = 100;
const LIBRARY_MUTATION_MAX: usize = 50;
const PLAY_URIS_MAX: usize = 100;

fn spotify_priority(priority: RequestPriority) -> Priority {
    match priority {
        RequestPriority::Foreground => Priority::Foreground,
        RequestPriority::BackgroundSync => Priority::BackgroundSync,
        RequestPriority::PlaybackControl => Priority::PlaybackControl,
    }
}

fn prioritized(client: &SpotifyClient, context: RequestContext) -> SpotifyClient {
    client
        .clone()
        .with_default_priority(spotify_priority(context.priority))
}

fn playback_client(client: &SpotifyClient) -> SpotifyClient {
    client
        .clone()
        .with_default_priority(Priority::PlaybackControl)
}

/// Lossless provider-neutral classification for Spotify client errors.
pub fn provider_error(error: SpotifyError, operation: &str) -> ProviderError {
    match error {
        SpotifyError::AuthRequired => ProviderError::AuthRequired,
        SpotifyError::RateLimited { retry_after, scope } => ProviderError::RateLimited {
            scope: Some(scope),
            retry_after: Some(retry_after),
        },
        SpotifyError::AuthExpired => ProviderError::AuthExpired,
        SpotifyError::AuthRevoked => ProviderError::AuthRevoked,
        SpotifyError::Forbidden { scope } => ProviderError::Forbidden { operation: scope },
        SpotifyError::NotFound => ProviderError::NotFound {
            resource: operation.to_string(),
        },
        SpotifyError::Deprecated { endpoint } => ProviderError::Upstream {
            status: 410,
            message: format!("Spotify deprecated endpoint {endpoint}"),
        },
        SpotifyError::Network { endpoint, message } => {
            ProviderError::Network(format!("{endpoint}: {message}"))
        }
        SpotifyError::Decode { endpoint, message } => {
            ProviderError::Decode(format!("{endpoint}: {message}"))
        }
        SpotifyError::Api {
            status,
            endpoint,
            message,
            body,
        } => {
            let detail = if message.is_empty() {
                format!("Spotify {endpoint} returned HTTP {status}")
            } else {
                format!("{endpoint}: {message}")
            };
            let searchable = format!("{message} {body}").to_ascii_lowercase();
            if searchable.contains("no active device") {
                ProviderError::NoActiveDevice
            } else {
                ProviderError::Upstream {
                    status,
                    message: detail,
                }
            }
        }
        SpotifyError::InvalidInput { message } => ProviderError::InvalidInput {
            field: operation.to_string(),
            message,
        },
        SpotifyError::Client { message } => ProviderError::Provider(message),
    }
}

fn ensure_spotify_uri(uri: &ResourceUri, field: &str) -> ProviderResult<()> {
    if uri.scheme() != &UriScheme::Spotify {
        return Err(ProviderError::InvalidInput {
            field: field.to_string(),
            message: format!("URI `{uri}` is outside the Spotify namespace"),
        });
    }
    Ok(())
}

/// Spotify-namespace classification for input `normalize_spotify_target`
/// rejected: still ours (so a rejection is `Invalid`) or foreign (`NotMine`).
enum SpotifyNamespace {
    Local,
    Other,
}

fn spotify_target_namespace(input: &str) -> Option<SpotifyNamespace> {
    if input
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("spotify:"))
    {
        let is_local = input[8..]
            .split(':')
            .next()
            .is_some_and(|kind| kind.eq_ignore_ascii_case("local"));
        return Some(if is_local {
            SpotifyNamespace::Local
        } else {
            SpotifyNamespace::Other
        });
    }
    let is_share_host = url::Url::parse(input)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .as_deref()
        == Some("open.spotify.com");
    is_share_host.then_some(SpotifyNamespace::Other)
}

fn ensure_kind(uri: &ResourceUri, allowed: &[MediaKind], field: &str) -> ProviderResult<()> {
    ensure_spotify_uri(uri, field)?;
    if !allowed.contains(&uri.kind()) {
        return Err(ProviderError::InvalidInput {
            field: field.to_string(),
            message: format!(
                "Spotify does not support `{}` for this operation",
                uri.kind()
            ),
        });
    }
    Ok(())
}

fn require_offset_page(request: &PageRequest, max: u32) -> ProviderResult<u32> {
    if request.cursor.is_some() {
        return Err(ProviderError::InvalidInput {
            field: "cursor".to_string(),
            message: "Spotify returned an offset continuation for this operation".to_string(),
        });
    }
    require_page_limit(request, max)
}

fn require_page_limit(request: &PageRequest, max: u32) -> ProviderResult<u32> {
    if request.limit == 0 {
        return Err(ProviderError::InvalidInput {
            field: "limit".to_string(),
            message: "page limit must be greater than zero".to_string(),
        });
    }
    if request.limit > max {
        return Err(ProviderError::InvalidInput {
            field: "limit".to_string(),
            message: format!("Spotify supports at most {max} items per page"),
        });
    }
    Ok(request.limit)
}

fn provider_page_from_upstream<T>(
    items: Vec<T>,
    total: u64,
    request: &PageRequest,
    max: u32,
) -> ProviderResult<ProviderPage<T>> {
    let limit = u64::from(require_offset_page(request, max)?);
    let next_offset = request.offset.saturating_add(limit);
    Ok(ProviderPage {
        items,
        requested_offset: request.offset,
        total: Some(total),
        next: (next_offset < total).then_some(PageContinuation::Offset(next_offset)),
    })
}

/// Window a locally-materialized, cursor-shaped result set. The caller must
/// validate page bounds before any I/O; `total` stays `None` because the
/// upstream endpoints these back do not report an authoritative count.
fn windowed_page<T>(items: Vec<T>, offset: u64, limit: u32) -> ProviderResult<ProviderPage<T>> {
    let fetched = items.len() as u64;
    let start = offset.min(fetched);
    let end = start.saturating_add(u64::from(limit)).min(fetched);
    let start_index = usize::try_from(start).map_err(|_| ProviderError::InvalidInput {
        field: "offset".to_string(),
        message: "page offset exceeds this platform's addressable range".to_string(),
    })?;
    let take = usize::try_from(end - start).expect("bounded page length fits usize");
    let items = items.into_iter().skip(start_index).take(take).collect();
    Ok(ProviderPage {
        items,
        requested_offset: offset,
        total: None,
        next: (end < fetched).then_some(PageContinuation::Offset(end)),
    })
}

fn canonical_playlist(mut playlist: Playlist) -> ProviderResult<Playlist> {
    let uri = match ResourceUri::parse(&playlist.id) {
        Ok(uri) => uri,
        Err(_) => ResourceUri::spotify(MediaKind::Playlist, &playlist.id).map_err(|error| {
            ProviderError::InvalidInput {
                field: "playlist.id".to_string(),
                message: error.to_string(),
            }
        })?,
    };
    ensure_kind(&uri, &[MediaKind::Playlist], "playlist.id")?;
    playlist.id = uri.as_uri();
    Ok(playlist)
}

fn spotify_caps() -> ProviderCaps {
    ProviderCaps {
        search: SearchCaps {
            remote: true,
            kinds: vec![
                MediaKind::Track,
                MediaKind::Episode,
                MediaKind::Show,
                MediaKind::Album,
                MediaKind::Artist,
                MediaKind::Playlist,
            ],
            max_page_size: Some(SEARCH_PAGE_MAX as usize),
            max_query_chars: Some(SEARCH_QUERY_MAX_CHARS),
        },
        catalog: CatalogCaps {
            lookup_kinds: vec![MediaKind::Track],
            recently_played: true,
            recently_played_max_page_size: Some(RECENT_PAGE_MAX as usize),
            album_tracks: true,
            album_tracks_max_page_size: Some(PAGE_MAX as usize),
            artist_albums: true,
            artist_albums_max_page_size: Some(ARTIST_ALBUM_PAGE_MAX as usize),
            show_episodes: true,
            show_episodes_max_page_size: Some(PAGE_MAX as usize),
        },
        library: LibraryCaps {
            read_kinds: vec![
                MediaKind::Track,
                MediaKind::Episode,
                MediaKind::Album,
                MediaKind::Show,
                MediaKind::Artist,
            ],
            save_kinds: vec![
                MediaKind::Track,
                MediaKind::Episode,
                MediaKind::Show,
                MediaKind::Album,
            ],
            follow_kinds: vec![MediaKind::Artist],
            mutation_max_batch: Some(LIBRARY_MUTATION_MAX),
            max_page_size: Some(PAGE_MAX as usize),
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
            list_max_page_size: Some(PAGE_MAX as usize),
            items_max_page_size: Some(PAGE_MAX as usize),
            add_max_batch: Some(PLAYLIST_MUTATION_MAX),
            remove_max_batch: Some(PLAYLIST_MUTATION_MAX),
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
            queue_snapshots_complete: false,
            queue_add: true,
            devices: true,
            transfer: true,
        }),
        extras: ProviderExtrasCaps::default(),
    }
}

#[async_trait]
impl MusicProvider for SpotifyClient {
    fn id(&self) -> &ProviderId {
        self.provider_id()
    }

    fn uri_scheme(&self) -> &UriScheme {
        &UriScheme::Spotify
    }

    fn display_name(&self) -> &str {
        "Spotify"
    }

    fn capabilities(&self) -> ProviderCaps {
        spotify_caps()
    }

    /// Claim Spotify share URLs and legacy URI forms on top of the strict
    /// canonical URIs the trait default already resolves.
    /// [`crate::selection::normalize_spotify_target`] owns every messy input
    /// shape: `open.spotify.com` links (with `?si=` junk, locale/embed
    /// prefixes, legacy `/user/<u>/playlist/<id>`), uppercase schemes, and
    /// legacy `spotify:user:<u>:playlist:<id>` URIs. Anything it canonicalizes
    /// is `Resolved`. Input that still names the Spotify namespace but cannot be
    /// canonicalized — a bad `open.spotify.com` path, an unsupported URI kind,
    /// or a `spotify:local:` file reference — is `Invalid`, never silently
    /// reinterpreted as free-text search. `spotify:local:` URIs are `Invalid`
    /// by design: local files have no Web API resource and are not addressable.
    fn claim_target(&self, input: &str) -> TargetClaim {
        let trimmed = input.trim();
        if let Some(resource) = crate::selection::normalize_spotify_target(trimmed) {
            return TargetClaim::Resolved(resource);
        }
        match spotify_target_namespace(trimmed) {
            Some(SpotifyNamespace::Local) => TargetClaim::Invalid {
                message: format!(
                    "`{trimmed}` is a Spotify local-file URI; local files are not addressable through the Web API"
                ),
            },
            Some(SpotifyNamespace::Other) => TargetClaim::Invalid {
                message: format!(
                    "`{trimmed}` names the Spotify namespace but is not a valid track, episode, show, album, artist, or playlist target"
                ),
            },
            None => TargetClaim::NotMine,
        }
    }

    async fn search(
        &self,
        context: RequestContext,
        request: SearchRequest,
    ) -> ProviderResult<ProviderPage<MediaItem>> {
        let limit = require_offset_page(&request.page, SEARCH_PAGE_MAX)?;
        if request.query.trim().is_empty() {
            return Ok(ProviderPage {
                items: Vec::new(),
                requested_offset: request.page.offset,
                total: Some(0),
                next: None,
            });
        }
        if request.query.chars().count() > SEARCH_QUERY_MAX_CHARS {
            return Err(ProviderError::InvalidInput {
                field: "query".to_string(),
                message: format!(
                    "Spotify search queries must be at most {SEARCH_QUERY_MAX_CHARS} characters"
                ),
            });
        }
        let offset =
            u32::try_from(request.page.offset).map_err(|_| ProviderError::InvalidInput {
                field: "offset".to_string(),
                message: "Spotify search offsets must fit in 32 bits".to_string(),
            })?;
        let client = prioritized(self, context);
        let page = client
            .search_single_type(&request.query, request.kind, limit as u8, offset)
            .await
            .map_err(|error| provider_error(error, "search"))?;
        let consumed = page.consumed;
        let next_offset = request.page.offset.saturating_add(consumed);
        let has_more = consumed > 0
            && next_offset < 1000
            && page
                .total
                .map_or(consumed == u64::from(limit), |total| next_offset < total);
        Ok(ProviderPage {
            items: page.items,
            requested_offset: request.page.offset,
            total: page.total,
            next: has_more.then_some(PageContinuation::Offset(next_offset)),
        })
    }

    async fn media_item(
        &self,
        context: RequestContext,
        uri: &ResourceUri,
    ) -> ProviderResult<Option<MediaItem>> {
        ensure_spotify_uri(uri, "uri")?;
        if uri.kind() != MediaKind::Track {
            return Err(ProviderError::unsupported(format!(
                "media_item.{}",
                uri.kind()
            )));
        }
        match prioritized(self, context)
            .media_item_by_uri(&uri.as_uri())
            .await
        {
            Ok(item) => Ok(item),
            Err(SpotifyError::NotFound) | Err(SpotifyError::Api { status: 404, .. }) => Ok(None),
            Err(error) => Err(provider_error(error, "media_item")),
        }
    }

    async fn recently_played(
        &self,
        context: RequestContext,
        page: PageRequest,
    ) -> ProviderResult<ProviderPage<MediaItem>> {
        let limit = require_offset_page(&page, RECENT_PAGE_MAX)?;
        let mut client = prioritized(self, context);
        let items = SpotifyClient::recently_played(&mut client)
            .await
            .map_err(|error| provider_error(error, "recently_played"))?;
        windowed_page(items, page.offset, limit)
    }

    async fn library_items(
        &self,
        context: RequestContext,
        request: LibraryRequest,
    ) -> ProviderResult<ProviderPage<MediaItem>> {
        let limit = if request.kind == MediaKind::Artist {
            require_page_limit(&request.page, PAGE_MAX)?
        } else {
            require_offset_page(&request.page, PAGE_MAX)?
        };
        let mut client = prioritized(self, context);
        if request.kind == MediaKind::Track {
            let page = client
                .saved_tracks_page(limit as u8, request.page.offset)
                .await
                .map_err(|error| provider_error(error, "library_items.track"))?;
            let consumed = page.items.len() as u64;
            let next_offset = request.page.offset.saturating_add(consumed);
            return Ok(ProviderPage {
                items: page.items,
                requested_offset: request.page.offset,
                total: Some(page.total),
                next: (consumed > 0 && next_offset < page.total)
                    .then_some(PageContinuation::Offset(next_offset)),
            });
        }
        if request.kind == MediaKind::Artist {
            if request.page.cursor.is_none() && request.page.offset != 0 {
                return Err(ProviderError::InvalidInput {
                    field: "offset".to_string(),
                    message: "Spotify followed artists require a cursor after the first page"
                        .to_string(),
                });
            }
            let (items, next) = client
                .followed_artists_page(limit as u8, request.page.cursor.as_deref())
                .await
                .map_err(|error| provider_error(error, "library_items.artist"))?;
            return Ok(ProviderPage {
                items,
                requested_offset: request.page.offset,
                total: None,
                next: next.map(PageContinuation::Cursor),
            });
        }
        let page = match request.kind {
            MediaKind::Episode => {
                client
                    .saved_episodes_page(limit as u8, request.page.offset)
                    .await
            }
            MediaKind::Album => {
                client
                    .saved_albums_page(limit as u8, request.page.offset)
                    .await
            }
            MediaKind::Show => {
                client
                    .saved_shows_page(limit as u8, request.page.offset)
                    .await
            }
            other => return Err(ProviderError::unsupported(format!("library_items.{other}"))),
        }
        .map_err(|error| provider_error(error, "library_items"))?;
        provider_page_from_upstream(page.items, page.total, &request.page, PAGE_MAX)
    }

    async fn library_freshness_probe(
        &self,
        context: RequestContext,
        kind: MediaKind,
    ) -> ProviderResult<FreshnessProbe> {
        let page = self
            .library_items(
                context,
                LibraryRequest {
                    kind,
                    page: PageRequest::new(PAGE_MAX, 0),
                },
            )
            .await?;
        let uris = page
            .items
            .iter()
            .map(|item| item.uri.as_str())
            .collect::<Vec<_>>();
        serde_json::to_vec(&(page.total, uris))
            .map(FreshnessProbe)
            .map_err(|error| ProviderError::Provider(error.to_string()))
    }

    async fn playlists(
        &self,
        context: RequestContext,
        page: PageRequest,
    ) -> ProviderResult<ProviderPage<Playlist>> {
        let limit = require_offset_page(&page, PAGE_MAX)?;
        let mut client = prioritized(self, context);
        let upstream = client
            .playlists_page(limit as u8, page.offset)
            .await
            .map_err(|error| provider_error(error, "playlists"))?;
        let playlists = upstream
            .items
            .into_iter()
            .map(canonical_playlist)
            .collect::<ProviderResult<Vec<_>>>()?;
        provider_page_from_upstream(playlists, upstream.total, &page, PAGE_MAX)
    }

    async fn playlist(
        &self,
        context: RequestContext,
        uri: &ResourceUri,
    ) -> ProviderResult<Option<Playlist>> {
        ensure_kind(uri, &[MediaKind::Playlist], "playlist_uri")?;
        match prioritized(self, context)
            .playlist_metadata(uri.bare_id())
            .await
        {
            Ok(playlist) => canonical_playlist(playlist).map(Some),
            Err(SpotifyError::NotFound) | Err(SpotifyError::Api { status: 404, .. }) => Ok(None),
            Err(error) => Err(provider_error(error, "playlist")),
        }
    }

    async fn playlist_items(
        &self,
        context: RequestContext,
        request: CollectionRequest,
    ) -> ProviderResult<AccessOutcome<ProviderPage<MediaItem>>> {
        ensure_kind(&request.uri, &[MediaKind::Playlist], "playlist_uri")?;
        let limit = require_offset_page(&request.page, PAGE_MAX)?;
        let result = prioritized(self, context)
            .playlist_tracks_page(request.uri.bare_id(), limit as u8, request.page.offset)
            .await;
        match result {
            Ok(page) => {
                provider_page_from_upstream(page.items, page.total, &request.page, PAGE_MAX)
                    .map(AccessOutcome::Available)
            }
            Err(SpotifyError::Api {
                status: 403,
                message,
                body,
                ..
            }) => {
                let detail = format!("{message} {body}").to_ascii_lowercase();
                let unavailable = if detail.contains("region") || detail.contains("market") {
                    AccessUnavailable::RegionRestricted
                } else if detail.contains("premium") || detail.contains("subscription") {
                    AccessUnavailable::SubscriptionRequired
                } else if detail.contains("temporar") {
                    AccessUnavailable::TemporarilyUnavailable
                } else {
                    AccessUnavailable::Private
                };
                Ok(AccessOutcome::Unavailable(unavailable))
            }
            Err(error) => Err(provider_error(error, "playlist_items")),
        }
    }

    async fn album_tracks(
        &self,
        context: RequestContext,
        request: CollectionRequest,
    ) -> ProviderResult<ProviderPage<MediaItem>> {
        ensure_kind(&request.uri, &[MediaKind::Album], "album_uri")?;
        let limit = require_offset_page(&request.page, PAGE_MAX)?;
        let mut client = prioritized(self, context);
        let page = client
            .album_tracks_page(request.uri.bare_id(), limit as u8, request.page.offset)
            .await
            .map_err(|error| provider_error(error, "album_tracks"))?;
        provider_page_from_upstream(page.items, page.total, &request.page, PAGE_MAX)
    }

    async fn artist_albums(
        &self,
        context: RequestContext,
        request: CollectionRequest,
    ) -> ProviderResult<ProviderPage<MediaItem>> {
        ensure_kind(&request.uri, &[MediaKind::Artist], "artist_uri")?;
        let limit = require_offset_page(&request.page, ARTIST_ALBUM_PAGE_MAX)?;
        let mut client = prioritized(self, context);
        let page = client
            .artist_albums_page(request.uri.bare_id(), limit as u8, request.page.offset)
            .await
            .map_err(|error| provider_error(error, "artist_albums"))?;
        provider_page_from_upstream(page.items, page.total, &request.page, ARTIST_ALBUM_PAGE_MAX)
    }

    async fn show_episodes(
        &self,
        context: RequestContext,
        request: CollectionRequest,
    ) -> ProviderResult<ProviderPage<MediaItem>> {
        ensure_kind(&request.uri, &[MediaKind::Show], "show_uri")?;
        let limit = require_offset_page(&request.page, PAGE_MAX)?;
        let mut client = prioritized(self, context);
        let items = SpotifyClient::show_episodes(
            &mut client,
            request.uri.bare_id(),
            limit as u8,
            request.page.offset,
        )
        .await
        .map_err(|error| provider_error(error, "show_episodes"))?;
        let consumed = items.len() as u64;
        let next_offset = request.page.offset.saturating_add(consumed);
        Ok(ProviderPage {
            items,
            requested_offset: request.page.offset,
            total: None,
            next: (consumed == u64::from(limit)).then_some(PageContinuation::Offset(next_offset)),
        })
    }

    async fn apply_mutation(
        &self,
        context: RequestContext,
        mutation_id: Uuid,
        mutation: &Mutation,
    ) -> ProviderResult<MutationReceipt> {
        apply_mutation(prioritized(self, context), mutation_id, mutation).await
    }
}

async fn check_version(
    client: &mut SpotifyClient,
    playlist_uri: &ResourceUri,
    expected: Option<&str>,
) -> ProviderResult<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let playlist = client
        .playlist_metadata(playlist_uri.bare_id())
        .await
        .map_err(|error| provider_error(error, "playlist_version"))?;
    if playlist.version_token.as_deref() != Some(expected) {
        return Err(ProviderError::VersionConflict {
            expected: Some(expected.to_string()),
            actual: playlist.version_token,
        });
    }
    Ok(())
}

fn playlist_mutation_error(
    error: SpotifyError,
    operation: &str,
    expected: Option<&str>,
) -> ProviderError {
    let conflict = match &error {
        SpotifyError::Api {
            status: 409 | 412, ..
        } => true,
        SpotifyError::Api {
            status: 400,
            message,
            body,
            ..
        } => {
            let detail = format!("{message} {body}").to_ascii_lowercase();
            detail.contains("snapshot")
                && [
                    "mismatch",
                    "conflict",
                    "current snapshot",
                    "latest snapshot",
                    "stale",
                    "out of date",
                ]
                .iter()
                .any(|marker| detail.contains(marker))
        }
        _ => false,
    };
    if conflict {
        ProviderError::VersionConflict {
            expected: expected.map(str::to_string),
            actual: None,
        }
    } else {
        provider_error(error, operation)
    }
}

fn completion(failures: &[MutationFailure]) -> MutationCompletion {
    if failures.is_empty() {
        MutationCompletion::Applied
    } else {
        MutationCompletion::PartiallyApplied
    }
}

fn receipt(
    provider: &ProviderId,
    mutation_id: Uuid,
    outcome: MutationOutcome,
    version_token: Option<String>,
    failures: Vec<MutationFailure>,
) -> MutationReceipt {
    MutationReceipt {
        mutation_id,
        provider: provider.clone(),
        completion: completion(&failures),
        outcome,
        version_token,
        failures,
    }
}

fn validate_batch_size(actual: usize, max: usize, field: &str) -> ProviderResult<()> {
    if actual > max {
        return Err(ProviderError::InvalidInput {
            field: field.to_string(),
            message: format!("batch contains {actual} items; Spotify supports at most {max}"),
        });
    }
    Ok(())
}

fn is_unavailable_playlist_placeholder(uri: &ResourceUri) -> bool {
    uri.kind() == MediaKind::Track
        && (uri.bare_id().starts_with("local~") || uri.bare_id().starts_with("unavailable~"))
}

/// Per-URI receipt partition from applying library/follow mutations.
#[derive(Default)]
struct LibraryBatchResult {
    successful: Vec<ResourceUri>,
    failures: Vec<MutationFailure>,
    first_error: Option<ProviderError>,
}

/// Apply a library save/unsave or follow/unfollow as batched first-party
/// requests: one call per media kind (each kind targets a distinct `/me`
/// collection), sending all of that kind's IDs in a single `?ids=a,b,c`.
///
/// A batch is atomic at the HTTP layer, so the per-URI receipt partition is
/// honest but coarse: on success every URI in the group is recorded as
/// successful; on failure the group's single typed error is attributed to each
/// URI. First-encounter kind order is preserved for a deterministic receipt.
async fn apply_library_batches(
    client: &mut SpotifyClient,
    uris: &[ResourceUri],
    add: bool,
    operation: &str,
) -> LibraryBatchResult {
    let mut order: Vec<MediaKind> = Vec::new();
    let mut groups: std::collections::HashMap<MediaKind, Vec<ResourceUri>> =
        std::collections::HashMap::new();
    for uri in uris {
        let kind = uri.kind();
        if !groups.contains_key(&kind) {
            order.push(kind.clone());
        }
        groups.entry(kind).or_default().push(uri.clone());
    }

    let mut result = LibraryBatchResult::default();
    for kind in order {
        let group = groups.remove(&kind).expect("kind was grouped above");
        let group_uris = group.iter().map(ResourceUri::as_uri).collect::<Vec<_>>();
        let call = if add {
            client.library_save_by_uris(&group_uris).await
        } else {
            client.library_unsave_by_uris(&group_uris).await
        };
        match call {
            Ok(()) => result.successful.extend(group),
            Err(error) => {
                let error = provider_error(error, operation);
                result.first_error.get_or_insert_with(|| error.clone());
                result
                    .failures
                    .extend(group.into_iter().map(|uri| MutationFailure {
                        uri: Some(uri),
                        message: error.to_string(),
                    }));
            }
        }
    }
    result
}

async fn apply_mutation(
    mut client: SpotifyClient,
    mutation_id: Uuid,
    mutation: &Mutation,
) -> ProviderResult<MutationReceipt> {
    let provider_id = client.provider_id().clone();
    match mutation {
        Mutation::PlaylistCreate {
            name,
            public,
            description,
        } => {
            let playlist = client
                .create_playlist(name, description.as_deref(), public.unwrap_or(false))
                .await
                .map_err(|error| provider_error(error, "playlist_create"))?;
            let playlist = canonical_playlist(playlist)?;
            let version = playlist.version_token.clone();
            Ok(receipt(
                &provider_id,
                mutation_id,
                MutationOutcome::PlaylistCreated { playlist },
                version,
                Vec::new(),
            ))
        }
        Mutation::PlaylistAdd {
            playlist_uri,
            items,
            expected_version,
        } => {
            ensure_kind(playlist_uri, &[MediaKind::Playlist], "playlist_uri")?;
            validate_batch_size(items.len(), PLAYLIST_MUTATION_MAX, "items")?;
            for item in items {
                ensure_kind(
                    &item.uri,
                    &[MediaKind::Track, MediaKind::Episode],
                    "items.uri",
                )?;
                if is_unavailable_playlist_placeholder(&item.uri) {
                    return Err(ProviderError::InvalidInput {
                        field: "items.uri".to_string(),
                        message: "unavailable and local playlist items cannot be added on Spotify"
                            .to_string(),
                    });
                }
            }
            check_version(&mut client, playlist_uri, expected_version.as_deref()).await?;
            let mut positioned = std::collections::BTreeMap::<u32, Vec<&_>>::new();
            let mut appended = Vec::new();
            for item in items {
                match item.position {
                    Some(position) => positioned.entry(position).or_default().push(item),
                    None => appended.push(item),
                }
            }
            let mut groups = positioned
                .into_iter()
                .map(|(position, items)| (Some(position), items))
                .collect::<Vec<_>>();
            if !appended.is_empty() {
                groups.push((None, appended));
            }
            let mut failures = Vec::new();
            let mut first_error = None;
            let mut applied = 0usize;
            let mut version = expected_version.clone();
            for (position, group) in groups {
                let uris = group
                    .iter()
                    .map(|item| item.uri.as_uri())
                    .collect::<Vec<_>>();
                match client
                    .add_playlist_items_with_snapshot(playlist_uri.bare_id(), &uris, position)
                    .await
                {
                    Ok(snapshot) => {
                        applied += group.len();
                        version = (!snapshot.is_empty()).then_some(snapshot);
                    }
                    Err(error) => {
                        let error = playlist_mutation_error(
                            error,
                            "playlist_add",
                            expected_version.as_deref(),
                        );
                        first_error.get_or_insert_with(|| error.clone());
                        failures.extend(group.into_iter().map(|item| MutationFailure {
                            uri: Some(item.uri.clone()),
                            message: error.to_string(),
                        }));
                    }
                }
            }
            if applied == 0 {
                if let Some(error) = first_error {
                    return Err(error);
                }
            }
            Ok(receipt(
                &provider_id,
                mutation_id,
                MutationOutcome::PlaylistChanged {
                    playlist_uri: playlist_uri.clone(),
                },
                version,
                failures,
            ))
        }
        Mutation::PlaylistRemove {
            playlist_uri,
            items,
            expected_version,
        } => {
            ensure_kind(playlist_uri, &[MediaKind::Playlist], "playlist_uri")?;
            validate_batch_size(items.len(), PLAYLIST_MUTATION_MAX, "items")?;
            for item in items {
                ensure_kind(
                    &item.uri,
                    &[MediaKind::Track, MediaKind::Episode],
                    "items.uri",
                )?;
                if is_unavailable_playlist_placeholder(&item.uri) {
                    return Err(ProviderError::InvalidInput {
                        field: "items.uri".to_string(),
                        message: "unavailable and local playlist items cannot be removed safely; Spotify cannot restore them for undo"
                            .to_string(),
                    });
                }
            }
            check_version(&mut client, playlist_uri, expected_version.as_deref()).await?;
            let refs = items
                .iter()
                .map(|item| (item.uri.as_uri(), item.positions.clone()))
                .collect::<Vec<_>>();
            let version = client
                .remove_playlist_item_refs(
                    playlist_uri.bare_id(),
                    &refs,
                    expected_version.as_deref(),
                )
                .await
                .map_err(|error| {
                    playlist_mutation_error(error, "playlist_remove", expected_version.as_deref())
                })?;
            Ok(receipt(
                &provider_id,
                mutation_id,
                MutationOutcome::PlaylistChanged {
                    playlist_uri: playlist_uri.clone(),
                },
                (!version.is_empty()).then_some(version),
                Vec::new(),
            ))
        }
        Mutation::PlaylistReorder {
            playlist_uri,
            range_start,
            insert_before,
            range_length,
            expected_version,
        } => {
            ensure_kind(playlist_uri, &[MediaKind::Playlist], "playlist_uri")?;
            check_version(&mut client, playlist_uri, expected_version.as_deref()).await?;
            let version = client
                .reorder_playlist_items(
                    playlist_uri.bare_id(),
                    *range_start,
                    *insert_before,
                    *range_length,
                    expected_version.as_deref(),
                )
                .await
                .map_err(|error| {
                    playlist_mutation_error(error, "playlist_reorder", expected_version.as_deref())
                })?;
            Ok(receipt(
                &provider_id,
                mutation_id,
                MutationOutcome::PlaylistChanged {
                    playlist_uri: playlist_uri.clone(),
                },
                Some(version),
                Vec::new(),
            ))
        }
        Mutation::PlaylistSetImage { playlist_uri, jpeg } => {
            ensure_kind(playlist_uri, &[MediaKind::Playlist], "playlist_uri")?;
            if jpeg.is_empty() {
                return Err(ProviderError::InvalidInput {
                    field: "jpeg".to_string(),
                    message: "playlist image cannot be empty".to_string(),
                });
            }
            let encoded = base64::engine::general_purpose::STANDARD.encode(jpeg);
            if encoded.len() > 256 * 1024 {
                return Err(ProviderError::InvalidInput {
                    field: "jpeg".to_string(),
                    message: "Spotify playlist images must be at most 256 KB after base64 encoding"
                        .to_string(),
                });
            }
            let version = client
                .playlist_metadata(playlist_uri.bare_id())
                .await
                .map_err(|error| provider_error(error, "playlist_set_image.version"))?
                .version_token;
            client
                .set_playlist_image(playlist_uri.bare_id(), &encoded)
                .await
                .map_err(|error| provider_error(error, "playlist_set_image"))?;
            Ok(receipt(
                &provider_id,
                mutation_id,
                MutationOutcome::PlaylistImageSet {
                    playlist_uri: playlist_uri.clone(),
                },
                version,
                Vec::new(),
            ))
        }
        Mutation::PlaylistUnfollow { playlist_uri } => {
            ensure_kind(playlist_uri, &[MediaKind::Playlist], "playlist_uri")?;
            client
                .unfollow_playlist(playlist_uri.bare_id())
                .await
                .map_err(|error| provider_error(error, "playlist_unfollow"))?;
            Ok(receipt(
                &provider_id,
                mutation_id,
                MutationOutcome::PlaylistUnfollowed {
                    playlist_uri: playlist_uri.clone(),
                },
                None,
                Vec::new(),
            ))
        }
        Mutation::LibrarySave { uris } | Mutation::LibraryUnsave { uris } => {
            validate_batch_size(uris.len(), LIBRARY_MUTATION_MAX, "uris")?;
            for uri in uris {
                ensure_kind(
                    uri,
                    &[
                        MediaKind::Track,
                        MediaKind::Episode,
                        MediaKind::Show,
                        MediaKind::Album,
                    ],
                    "uris",
                )?;
            }
            let saved = matches!(mutation, Mutation::LibrarySave { .. });
            let batches = apply_library_batches(&mut client, uris, saved, "library_mutation").await;
            if batches.successful.is_empty() {
                if let Some(error) = batches.first_error {
                    return Err(error);
                }
            }
            Ok(receipt(
                &provider_id,
                mutation_id,
                MutationOutcome::LibraryChanged {
                    uris: batches.successful,
                    saved,
                },
                None,
                batches.failures,
            ))
        }
        Mutation::Follow { uris } | Mutation::Unfollow { uris } => {
            validate_batch_size(uris.len(), LIBRARY_MUTATION_MAX, "uris")?;
            for uri in uris {
                ensure_kind(uri, &[MediaKind::Artist], "uris")?;
            }
            let following = matches!(mutation, Mutation::Follow { .. });
            // Artist follow/unfollow routes through the same batched library
            // endpoints (`/me/following`), so reuse the batch applier.
            let batches =
                apply_library_batches(&mut client, uris, following, "follow_mutation").await;
            if batches.successful.is_empty() {
                if let Some(error) = batches.first_error {
                    return Err(error);
                }
            }
            Ok(receipt(
                &provider_id,
                mutation_id,
                MutationOutcome::FollowChanged {
                    uris: batches.successful,
                    following,
                },
                None,
                batches.failures,
            ))
        }
    }
}

#[async_trait]
impl RemoteTransport for SpotifyClient {
    fn provider_id(&self) -> &ProviderId {
        self.provider_id()
    }

    fn uri_scheme(&self) -> &UriScheme {
        &UriScheme::Spotify
    }

    async fn playback(&self, context: RequestContext) -> ProviderResult<spotuify_core::Playback> {
        let mut client = prioritized(self, context);
        SpotifyClient::playback(&mut client)
            .await
            .map_err(|error| provider_error(error, "transport.playback"))
    }

    async fn devices(&self, context: RequestContext) -> ProviderResult<Vec<spotuify_core::Device>> {
        let mut client = prioritized(self, context);
        SpotifyClient::devices(&mut client)
            .await
            .map_err(|error| provider_error(error, "transport.devices"))
    }

    async fn queue(&self, context: RequestContext) -> ProviderResult<spotuify_core::Queue> {
        let mut client = prioritized(self, context);
        SpotifyClient::queue(&mut client)
            .await
            .map_err(|error| provider_error(error, "transport.queue"))
    }

    async fn execute(
        &self,
        _context: RequestContext,
        command: TransportCommand,
    ) -> ProviderResult<TransportOutcome> {
        let mut client = playback_client(self);
        let refresh = match &command {
            TransportCommand::QueueAdd(_) => RefreshAfter::Queue,
            TransportCommand::Transfer { .. } => RefreshAfter::Transfer,
            _ => RefreshAfter::Playback,
        };
        execute_transport(&mut client, command).await?;
        let mut outcome = TransportOutcome::default();
        match refresh {
            RefreshAfter::Playback => {
                outcome.playback = SpotifyClient::playback(&mut client).await.ok()
            }
            RefreshAfter::Queue => outcome.queue = SpotifyClient::queue(&mut client).await.ok(),
            RefreshAfter::Transfer => {
                outcome.playback = SpotifyClient::playback(&mut client).await.ok();
                outcome.devices = SpotifyClient::devices(&mut client).await.ok();
            }
        }
        Ok(outcome)
    }
}

enum RefreshAfter {
    Playback,
    Queue,
    Transfer,
}

async fn execute_transport(
    client: &mut SpotifyClient,
    command: TransportCommand,
) -> ProviderResult<()> {
    match command {
        TransportCommand::Play(request) => execute_play(client, request).await,
        TransportCommand::Pause => client
            .play_pause(true)
            .await
            .map_err(|error| provider_error(error, "transport.pause")),
        TransportCommand::Resume => client
            .play_pause(false)
            .await
            .map_err(|error| provider_error(error, "transport.resume")),
        TransportCommand::Next => client
            .next()
            .await
            .map_err(|error| provider_error(error, "transport.next")),
        TransportCommand::Previous => client
            .previous()
            .await
            .map_err(|error| provider_error(error, "transport.previous")),
        TransportCommand::Seek { position_ms } => client
            .seek(position_ms)
            .await
            .map_err(|error| provider_error(error, "transport.seek")),
        TransportCommand::Volume { percent } => {
            if percent > 100 {
                return Err(ProviderError::InvalidInput {
                    field: "percent".to_string(),
                    message: "volume must be between 0 and 100".to_string(),
                });
            }
            client
                .volume(percent)
                .await
                .map_err(|error| provider_error(error, "transport.volume"))
        }
        TransportCommand::Shuffle { enabled } => client
            .shuffle(enabled)
            .await
            .map_err(|error| provider_error(error, "transport.shuffle")),
        TransportCommand::Repeat { mode } => client
            .repeat(mode)
            .await
            .map_err(|error| provider_error(error, "transport.repeat")),
        TransportCommand::QueueAdd(request) => {
            ensure_kind(
                &request.uri,
                &[MediaKind::Track, MediaKind::Episode],
                "queue.uri",
            )?;
            match request.device {
                TransportDevice::Active => client.add_to_queue(&request.uri.as_uri()).await,
                TransportDevice::Id(device_id) => {
                    client
                        .add_to_queue_on_device(&request.uri.as_uri(), &device_id)
                        .await
                }
            }
            .map_err(|error| provider_error(error, "transport.queue_add"))
        }
        TransportCommand::Transfer { device_id, play } => client
            .transfer(&device_id, play)
            .await
            .map_err(|error| provider_error(error, "transport.transfer")),
    }
}

async fn execute_play(client: &mut SpotifyClient, request: PlayRequest) -> ProviderResult<()> {
    request.validate()?;
    let (context, ordered) = match &request.source {
        PlaySource::Single => {
            ensure_kind(
                &request.start_uri,
                &[
                    MediaKind::Track,
                    MediaKind::Episode,
                    MediaKind::Album,
                    MediaKind::Artist,
                    MediaKind::Playlist,
                    MediaKind::Show,
                ],
                "play.start_uri",
            )?;
            let context = matches!(
                request.start_uri.kind(),
                MediaKind::Album | MediaKind::Artist | MediaKind::Playlist | MediaKind::Show
            )
            .then(|| request.start_uri.as_uri());
            (context, None)
        }
        PlaySource::Context(uri) => {
            ensure_kind(
                &request.start_uri,
                &[MediaKind::Track, MediaKind::Episode],
                "play.start_uri",
            )?;
            ensure_kind(
                uri,
                &[MediaKind::Album, MediaKind::Playlist],
                "play.context_uri",
            )?;
            (Some(uri.as_uri()), None)
        }
        PlaySource::Ordered(uris) => {
            ensure_kind(
                &request.start_uri,
                &[MediaKind::Track, MediaKind::Episode],
                "play.start_uri",
            )?;
            if uris.len() > PLAY_URIS_MAX {
                return Err(ProviderError::InvalidInput {
                    field: "play.source".to_string(),
                    message: format!(
                        "Spotify accepts at most {PLAY_URIS_MAX} explicitly ordered URIs"
                    ),
                });
            }
            for uri in uris {
                ensure_kind(
                    uri,
                    &[MediaKind::Track, MediaKind::Episode],
                    "play.ordered_uri",
                )?;
            }
            (
                None,
                Some(uris.iter().map(ResourceUri::as_uri).collect::<Vec<_>>()),
            )
        }
    };
    let start = request.start_uri.as_uri();
    let result = match request.device {
        TransportDevice::Active => {
            client
                .play_context(
                    &start,
                    context.as_deref(),
                    ordered.as_deref(),
                    request.position_ms,
                )
                .await
        }
        TransportDevice::Id(device_id) => {
            client
                .play_context_on_device(
                    &device_id,
                    &start,
                    context.as_deref(),
                    ordered.as_deref(),
                    request.position_ms,
                )
                .await
        }
    };
    result.map_err(|error| provider_error(error, "transport.play"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::path::PathBuf;
    use std::sync::Arc;

    use spotuify_core::{MusicProvider as _, RemoteTransport as _};
    use tokio::sync::Mutex;
    use wiremock::matchers::{body_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::auth::StoredToken;
    use crate::config::{
        AnalyticsConfig, CacheConfig, Config, DiscordConfig, NotificationsConfig, PlayerConfig,
        VizConfig,
    };

    fn test_config() -> Config {
        Config {
            client_id: "test-client-id".to_string(),
            client_secret: Some("test-client-secret".to_string()),
            redirect_uri: "http://127.0.0.1:8888/callback".to_string(),
            config_path: PathBuf::from("test-spotuify.toml"),
            player: PlayerConfig::default(),
            cache: CacheConfig::default(),
            analytics: AnalyticsConfig::default(),
            notifications: NotificationsConfig::default(),
            discord: DiscordConfig::default(),
            viz: VizConfig::default(),
        }
    }

    async fn test_client(server: &MockServer) -> SpotifyClient {
        let token = StoredToken {
            access_token: "test-access".to_string(),
            refresh_token: "test-refresh".to_string(),
            expires_at: 4_000_000_000,
            scope: "user-read-private playlist-read-private user-modify-playback-state".to_string(),
            token_type: "Bearer".to_string(),
        };
        SpotifyClient::new(test_config())
            .expect("test client")
            .with_api_base_for_tests(format!("{}/v1", server.uri()))
            .with_token_cache(Arc::new(Mutex::new(Some(token))))
    }

    #[test]
    fn priorities_map_without_collapsing_background_or_playback_lanes() {
        assert_eq!(
            spotify_priority(RequestPriority::Foreground),
            Priority::Foreground
        );
        assert_eq!(
            spotify_priority(RequestPriority::BackgroundSync),
            Priority::BackgroundSync
        );
        assert_eq!(
            spotify_priority(RequestPriority::PlaybackControl),
            Priority::PlaybackControl
        );
    }

    #[test]
    fn upstream_errors_preserve_status_and_retryability() {
        let error = provider_error(
            SpotifyError::Api {
                status: 503,
                endpoint: "GET /me".to_string(),
                message: "unavailable".to_string(),
                body: String::new(),
            },
            "profile",
        );
        assert!(matches!(error, ProviderError::Upstream { status: 503, .. }));
        assert!(error.is_retryable());
    }

    #[tokio::test]
    async fn foreign_uri_is_rejected_before_spotify_dispatch() {
        let server = MockServer::start().await;
        let client = test_client(&server).await;
        let uri = ResourceUri::parse("fake:track:track-1").expect("canonical URI");
        let error = client
            .media_item(RequestContext::FOREGROUND, &uri)
            .await
            .expect_err("foreign URI must fail");
        assert!(matches!(error, ProviderError::InvalidInput { .. }));
        assert!(
            server
                .received_requests()
                .await
                .expect("request journal")
                .is_empty(),
            "foreign URI validation must happen before HTTP dispatch"
        );
    }

    #[tokio::test]
    async fn structured_404s_are_none_at_optional_lookup_boundaries() {
        let server = MockServer::start().await;
        for endpoint in ["/v1/tracks/missing", "/v1/playlists/missing"] {
            Mock::given(method("GET"))
                .and(path(endpoint))
                .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                    "error": { "status": 404, "message": "Resource is unavailable" }
                })))
                .expect(1)
                .mount(&server)
                .await;
        }
        let client = test_client(&server).await;

        assert_eq!(
            client
                .media_item(
                    RequestContext::FOREGROUND,
                    &ResourceUri::parse("spotify:track:missing").unwrap(),
                )
                .await
                .expect("missing media is optional"),
            None
        );
        assert_eq!(
            client
                .playlist(
                    RequestContext::FOREGROUND,
                    &ResourceUri::parse("spotify:playlist:missing").unwrap(),
                )
                .await
                .expect("missing playlist is optional"),
            None
        );
    }

    #[tokio::test]
    async fn positioned_remove_is_one_snapshot_relative_request_and_maps_late_conflict() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/playlists/playlist-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "playlist-1",
                "uri": "spotify:playlist:playlist-1",
                "name": "Playlist",
                "owner": { "display_name": "Owner" },
                "tracks": { "total": 12 },
                "images": [],
                "snapshot_id": "snap-A"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/v1/playlists/playlist-1/items"))
            .and(body_json(serde_json::json!({
                "items": [
                    { "uri": "spotify:track:second", "positions": [9, 2] },
                    { "uri": "spotify:track:first", "positions": [7] }
                ],
                "snapshot_id": "snap-A"
            })))
            .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
                "error": { "status": 409, "message": "Snapshot conflict" }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = test_client(&server).await;
        let playlist_uri = ResourceUri::parse("spotify:playlist:playlist-1").unwrap();
        let error = client
            .apply_mutation(
                RequestContext::FOREGROUND,
                Uuid::now_v7(),
                &Mutation::PlaylistRemove {
                    playlist_uri,
                    items: vec![
                        spotuify_core::PlaylistItemRef {
                            uri: ResourceUri::parse("spotify:track:second").unwrap(),
                            positions: vec![9, 2],
                        },
                        spotuify_core::PlaylistItemRef {
                            uri: ResourceUri::parse("spotify:track:first").unwrap(),
                            positions: vec![7],
                        },
                    ],
                    expected_version: Some("snap-A".to_string()),
                },
            )
            .await
            .expect_err("post-preflight conflict must remain typed");

        assert_eq!(
            error,
            ProviderError::VersionConflict {
                expected: Some("snap-A".to_string()),
                actual: None,
            }
        );
    }

    #[test]
    fn snapshot_mismatch_400_maps_to_version_conflict_only_when_identified() {
        let conflict = playlist_mutation_error(
            SpotifyError::Api {
                status: 400,
                endpoint: "DELETE /playlists/id/items".to_string(),
                message: "Snapshot ID does not match the current snapshot".to_string(),
                body: String::new(),
            },
            "playlist_remove",
            Some("snap-A"),
        );
        assert!(matches!(
            conflict,
            ProviderError::VersionConflict {
                expected: Some(ref expected),
                actual: None,
            } if expected == "snap-A"
        ));

        let unrelated = playlist_mutation_error(
            SpotifyError::Api {
                status: 400,
                endpoint: "DELETE /playlists/id/items".to_string(),
                message: "Invalid position".to_string(),
                body: String::new(),
            },
            "playlist_remove",
            Some("snap-A"),
        );
        assert!(matches!(
            unrelated,
            ProviderError::Upstream { status: 400, .. }
        ));
    }

    #[tokio::test]
    async fn playlist_403_becomes_typed_unavailable_outcome() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/playlists/private-playlist/items"))
            .and(query_param("limit", "50"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": { "status": 403, "message": "Insufficient client access" }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = test_client(&server).await;
        let result = client
            .playlist_items(
                RequestContext::BACKGROUND_SYNC,
                CollectionRequest {
                    uri: ResourceUri::parse("spotify:playlist:private-playlist").unwrap(),
                    page: PageRequest::default(),
                },
            )
            .await
            .expect("access outcome");
        assert_eq!(
            result,
            AccessOutcome::Unavailable(AccessUnavailable::Private)
        );
    }

    #[tokio::test]
    async fn local_playlist_placeholders_preserve_remove_and_reorder_positions() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/playlists/mixed-playlist/items"))
            .and(query_param("limit", "50"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total": 3,
                "items": [
                    {
                        "item": {
                            "type": "track",
                            "id": null,
                            "uri": "spotify:local:Artist:Album:Local Song:123",
                            "name": "Local Song",
                            "duration_ms": 123000,
                            "is_local": true,
                            "artists": [{"name": "Artist"}],
                            "album": {
                                "id": null,
                                "uri": null,
                                "name": "Album",
                                "images": []
                            }
                        }
                    },
                    {
                        "item": {
                            "type": "track",
                            "id": "remote-track",
                            "uri": "spotify:track:remote-track",
                            "name": "Remote Track",
                            "duration_ms": 180000,
                            "is_local": false,
                            "artists": [{"name": "Artist"}],
                            "album": {
                                "id": "remote-album",
                                "uri": "spotify:album:remote-album",
                                "name": "Remote Album",
                                "images": []
                            }
                        }
                    },
                    {"item": null}
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/v1/playlists/mixed-playlist/items"))
            .and(body_json(serde_json::json!({
                "items": [{"uri": "spotify:track:remote-track", "positions": [1]}]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "snapshot_id": "after-remove"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/v1/playlists/mixed-playlist/items"))
            .and(body_json(serde_json::json!({
                "range_start": 1,
                "range_length": 1,
                "insert_before": 0
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "snapshot_id": "after-reorder"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = test_client(&server).await;

        let result = client
            .playlist_items(
                RequestContext::BACKGROUND_SYNC,
                CollectionRequest {
                    uri: ResourceUri::parse("spotify:playlist:mixed-playlist").unwrap(),
                    page: PageRequest::default(),
                },
            )
            .await
            .expect("mixed playlist remains readable");
        let page = match result {
            AccessOutcome::Available(page) => Ok(page),
            AccessOutcome::Unavailable(reason) => Err(reason),
        }
        .expect("mixed playlist should be available");

        assert_eq!(page.items.len(), 3);
        assert!(ResourceUri::parse(&page.items[0].uri)
            .expect("local surrogate should be canonical")
            .bare_id()
            .starts_with("local~"));
        assert_eq!(page.items[0].is_playable, Some(false));
        assert!(ResourceUri::parse(&page.items[0].uri).is_ok());
        assert_eq!(page.items[1].uri, "spotify:track:remote-track");
        assert!(ResourceUri::parse(&page.items[2].uri)
            .expect("unavailable surrogate should be canonical")
            .bare_id()
            .starts_with("unavailable~"));

        for index in [0_usize, 2] {
            let error = client
                .apply_mutation(
                    RequestContext::FOREGROUND,
                    Uuid::now_v7(),
                    &Mutation::PlaylistRemove {
                        playlist_uri: ResourceUri::parse("spotify:playlist:mixed-playlist")
                            .unwrap(),
                        items: vec![spotuify_core::PlaylistItemRef {
                            uri: ResourceUri::parse(&page.items[index].uri).unwrap(),
                            positions: vec![index as u32],
                        }],
                        expected_version: None,
                    },
                )
                .await
                .expect_err("non-reversible placeholder removal must fail before Spotify");
            assert!(matches!(error, ProviderError::InvalidInput { .. }));
        }

        client
            .apply_mutation(
                RequestContext::FOREGROUND,
                Uuid::now_v7(),
                &Mutation::PlaylistRemove {
                    playlist_uri: ResourceUri::parse("spotify:playlist:mixed-playlist").unwrap(),
                    items: vec![spotuify_core::PlaylistItemRef {
                        uri: ResourceUri::parse(&page.items[1].uri).unwrap(),
                        positions: vec![1],
                    }],
                    expected_version: None,
                },
            )
            .await
            .expect("remote row removal keeps its raw Spotify position");

        client
            .apply_mutation(
                RequestContext::FOREGROUND,
                Uuid::now_v7(),
                &Mutation::PlaylistReorder {
                    playlist_uri: ResourceUri::parse("spotify:playlist:mixed-playlist").unwrap(),
                    range_start: 1,
                    insert_before: 0,
                    range_length: 1,
                    expected_version: None,
                },
            )
            .await
            .expect("reorder uses raw Spotify positions including local rows");
        assert_eq!(
            server
                .received_requests()
                .await
                .expect("request journal")
                .len(),
            3,
            "placeholder rejection must not dispatch any Spotify write"
        );
    }

    #[tokio::test]
    async fn playlists_fetch_only_the_requested_upstream_page() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/me/playlists"))
            .and(query_param("limit", "2"))
            .and(query_param("offset", "50"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total": 100,
                "items": [
                    {"id": "p50", "name": "Fifty", "tracks": {"total": 1}},
                    {"id": "p51", "name": "Fifty One", "tracks": {"total": 1}}
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = test_client(&server).await;

        let page = client
            .playlists(RequestContext::BACKGROUND_SYNC, PageRequest::new(2, 50))
            .await
            .expect("requested playlist page");

        assert_eq!(page.requested_offset, 50);
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.next, Some(PageContinuation::Offset(52)));
        assert_eq!(
            server
                .received_requests()
                .await
                .expect("request journal")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn collection_relationships_fetch_only_the_requested_upstream_page() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/playlists/playlist-1/items"))
            .and(query_param("limit", "2"))
            .and(query_param("offset", "50"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total": 100,
                "items": [{"item": {
                    "type": "track", "id": "track-50",
                    "uri": "spotify:track:track-50", "name": "Track Fifty",
                    "duration_ms": 1000, "artists": [],
                    "album": {"id": "album-1", "uri": "spotify:album:album-1", "name": "Album"}
                }}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/albums/album-1/tracks"))
            .and(query_param("limit", "2"))
            .and(query_param("offset", "50"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total": 100,
                "items": [{
                    "id": "track-50", "uri": "spotify:track:track-50",
                    "name": "Track Fifty", "duration_ms": 1000, "artists": []
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/artists/artist-1/albums"))
            .and(query_param("limit", "2"))
            .and(query_param("offset", "50"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total": 100,
                "items": [{
                    "id": "album-50", "uri": "spotify:album:album-50",
                    "name": "Album Fifty", "artists": []
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = test_client(&server).await;
        let playlist_page = match client
            .playlist_items(
                RequestContext::BACKGROUND_SYNC,
                CollectionRequest {
                    uri: ResourceUri::parse("spotify:playlist:playlist-1").unwrap(),
                    page: PageRequest::new(2, 50),
                },
            )
            .await
            .expect("playlist page")
        {
            AccessOutcome::Available(page) => Ok(page),
            AccessOutcome::Unavailable(reason) => Err(reason),
        }
        .expect("playlist relationship must be available");
        let album_page = client
            .album_tracks(
                RequestContext::BACKGROUND_SYNC,
                CollectionRequest {
                    uri: ResourceUri::parse("spotify:album:album-1").unwrap(),
                    page: PageRequest::new(2, 50),
                },
            )
            .await
            .expect("album tracks page");
        let artist_page = client
            .artist_albums(
                RequestContext::BACKGROUND_SYNC,
                CollectionRequest {
                    uri: ResourceUri::parse("spotify:artist:artist-1").unwrap(),
                    page: PageRequest::new(2, 50),
                },
            )
            .await
            .expect("artist albums page");

        for page in [playlist_page, album_page, artist_page] {
            assert_eq!(page.requested_offset, 50);
            assert_eq!(page.items.len(), 1);
            assert_eq!(page.next, Some(PageContinuation::Offset(52)));
        }
        assert_eq!(
            server
                .received_requests()
                .await
                .expect("request journal")
                .len(),
            3
        );
    }

    #[tokio::test]
    async fn non_track_offset_libraries_fetch_only_the_requested_upstream_page() {
        let server = MockServer::start().await;
        let fixtures = [
            (
                MediaKind::Album,
                "/v1/me/albums",
                serde_json::json!({
                    "total": 100,
                    "items": [{"album": {
                        "id": "album-50", "uri": "spotify:album:album-50",
                        "name": "Album Fifty"
                    }}]
                }),
            ),
            (
                MediaKind::Episode,
                "/v1/me/episodes",
                serde_json::json!({
                    "total": 100,
                    "items": [{"episode": {
                        "id": "episode-50", "uri": "spotify:episode:episode-50",
                        "name": "Episode Fifty", "duration_ms": 1000
                    }}]
                }),
            ),
            (
                MediaKind::Show,
                "/v1/me/shows",
                serde_json::json!({
                    "total": 100,
                    "items": [{"show": {
                        "id": "show-50", "uri": "spotify:show:show-50",
                        "name": "Show Fifty", "publisher": "Publisher"
                    }}]
                }),
            ),
        ];
        for (_, endpoint, body) in &fixtures {
            Mock::given(method("GET"))
                .and(path(*endpoint))
                .and(query_param("limit", "2"))
                .and(query_param("offset", "50"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .expect(1)
                .mount(&server)
                .await;
        }
        let client = test_client(&server).await;

        for (kind, _, _) in fixtures {
            let page = client
                .library_items(
                    RequestContext::BACKGROUND_SYNC,
                    LibraryRequest {
                        kind,
                        page: PageRequest::new(2, 50),
                    },
                )
                .await
                .expect("requested library page");
            assert_eq!(page.requested_offset, 50);
            assert_eq!(page.items.len(), 1);
            assert_eq!(page.next, Some(PageContinuation::Offset(52)));
        }
        assert_eq!(
            server
                .received_requests()
                .await
                .expect("request journal")
                .len(),
            3
        );
    }

    #[tokio::test]
    async fn followed_artists_use_the_supplied_cursor_for_one_upstream_page() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/me/following"))
            .and(query_param("type", "artist"))
            .and(query_param("limit", "2"))
            .and(query_param("after", "artist-49"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "artists": {
                    "items": [{
                        "id": "artist-50", "uri": "spotify:artist:artist-50",
                        "name": "Artist Fifty"
                    }],
                    "next": "https://api.spotify.com/v1/me/following?after=artist-50",
                    "cursors": {"after": "artist-50"}
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = test_client(&server).await;

        let page = client
            .library_items(
                RequestContext::BACKGROUND_SYNC,
                LibraryRequest {
                    kind: MediaKind::Artist,
                    page: PageRequest::with_cursor(2, 50, "artist-49"),
                },
            )
            .await
            .expect("requested followed-artists page");

        assert_eq!(page.requested_offset, 50);
        assert_eq!(page.items.len(), 1);
        assert_eq!(
            page.next,
            Some(PageContinuation::Cursor("artist-50".to_string()))
        );
        assert_eq!(
            server
                .received_requests()
                .await
                .expect("request journal")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn ordered_play_on_selected_device_preserves_uri_window() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/v1/me/player/play"))
            .and(query_param("device_id", "device-1"))
            .and(body_json(serde_json::json!({
                "uris": ["spotify:track:first", "spotify:track:second"],
                "position_ms": 1234
            })))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let client = test_client(&server).await;
        client
            .execute(
                RequestContext::BACKGROUND_SYNC,
                TransportCommand::Play(PlayRequest {
                    start_uri: ResourceUri::parse("spotify:track:first").unwrap(),
                    source: PlaySource::Ordered(vec![
                        ResourceUri::parse("spotify:track:first").unwrap(),
                        ResourceUri::parse("spotify:track:second").unwrap(),
                    ]),
                    device: TransportDevice::Id("device-1".to_string()),
                    position_ms: 1234,
                }),
            )
            .await
            .expect("ordered play");
    }

    #[tokio::test]
    async fn single_show_play_targets_the_show_as_a_context() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/v1/me/player/play"))
            .and(body_json(serde_json::json!({
                "context_uri": "spotify:show:show-1",
                "position_ms": 0
            })))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let client = test_client(&server).await;
        client
            .execute(
                RequestContext::PLAYBACK_CONTROL,
                TransportCommand::Play(PlayRequest {
                    start_uri: ResourceUri::parse("spotify:show:show-1").unwrap(),
                    source: PlaySource::Single,
                    device: TransportDevice::Active,
                    position_ms: 0,
                }),
            )
            .await
            .expect("show context play");
    }

    #[test]
    fn capabilities_match_spotify_lookup_and_library_limits() {
        let caps = spotify_caps();
        assert_eq!(caps.catalog.lookup_kinds, vec![MediaKind::Track]);
        assert!(caps.library.can_read(&MediaKind::Artist));
        assert!(caps.library.can_read(&MediaKind::Episode));
        assert_eq!(caps.search.max_page_size, Some(10));
        assert!(caps.transport.is_some());
    }

    #[test]
    fn claim_target_owns_share_links_legacy_uris_and_rejects_malformed() {
        let client = SpotifyClient::new(test_config()).expect("client");

        assert!(matches!(
            client.claim_target("https://open.spotify.com/track/abc123?si=deadbeef"),
            TargetClaim::Resolved(uri) if uri.as_uri() == "spotify:track:abc123"
        ));
        assert!(matches!(
            client.claim_target("SPOTIFY:track:abc123"),
            TargetClaim::Resolved(uri) if uri.as_uri() == "spotify:track:abc123"
        ));
        assert!(matches!(
            client.claim_target("spotify:user:alice:playlist:p1"),
            TargetClaim::Resolved(uri) if uri.as_uri() == "spotify:playlist:p1"
        ));
        assert!(matches!(
            client.claim_target("https://open.spotify.com/notakind/abc123"),
            TargetClaim::Invalid { .. }
        ));
        assert!(matches!(
            client.claim_target("spotify:local:Artist:Album:Song:123"),
            TargetClaim::Invalid { message } if message.contains("local")
        ));
        assert_eq!(
            client.claim_target("just a song title"),
            TargetClaim::NotMine
        );
        assert_eq!(client.claim_target("fake:track:one"), TargetClaim::NotMine);
    }

    #[tokio::test]
    async fn library_save_batches_multiple_tracks_into_one_request() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/v1/me/tracks"))
            .and(query_param("ids", "track-1,track-2,track-3"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let client = test_client(&server).await;

        let receipt = client
            .apply_mutation(
                RequestContext::FOREGROUND,
                Uuid::now_v7(),
                &Mutation::LibrarySave {
                    uris: vec![
                        ResourceUri::parse("spotify:track:track-1").unwrap(),
                        ResourceUri::parse("spotify:track:track-2").unwrap(),
                        ResourceUri::parse("spotify:track:track-3").unwrap(),
                    ],
                },
            )
            .await
            .expect("batched save");

        assert_eq!(receipt.completion, MutationCompletion::Applied);
        assert_eq!(
            server
                .received_requests()
                .await
                .expect("request journal")
                .len(),
            1,
            "a multi-track save must be a single batched request"
        );
    }

    #[tokio::test]
    async fn follow_batches_multiple_artists_into_one_request() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/v1/me/following"))
            .and(query_param("type", "artist"))
            .and(query_param("ids", "artist-1,artist-2"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let client = test_client(&server).await;

        client
            .apply_mutation(
                RequestContext::FOREGROUND,
                Uuid::now_v7(),
                &Mutation::Follow {
                    uris: vec![
                        ResourceUri::parse("spotify:artist:artist-1").unwrap(),
                        ResourceUri::parse("spotify:artist:artist-2").unwrap(),
                    ],
                },
            )
            .await
            .expect("batched follow");

        assert_eq!(
            server
                .received_requests()
                .await
                .expect("request journal")
                .len(),
            1,
            "a multi-artist follow must be a single batched request"
        );
    }

    #[test]
    fn custom_provider_id_is_preserved_in_mutation_receipts() {
        let provider = ProviderId::new("spotify-work").expect("valid provider id");
        let result = receipt(
            &provider,
            Uuid::now_v7(),
            MutationOutcome::FollowChanged {
                uris: Vec::new(),
                following: true,
            },
            None,
            Vec::new(),
        );

        assert_eq!(result.provider, provider);
    }
}
