use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use spotuify_core::{
    active_lyric_line_index, LyricLine, MediaItem, MediaKind, Playback, Playlist, ProviderCaps,
    ProviderCatalog, ProviderDescriptor, ProviderId, ResourceUri, SyncedLyrics,
};
use spotuify_protocol::{
    AuthLogoutData, AuthSessionData, AuthSessionState, AuthStatusData, DaemonEvent, IpcClient,
    OperationSource, PlaybackCommand, PlaylistItemMutationAction, Request, Response, ResponseData,
    SearchScopeData, SearchSortData, SearchSourceData, SyncTargetData,
};

use crate::output::{self, OutputFormat};
use crate::selection;

/// Capabilities whose authority follows a canonical resource URI. Keeping the
/// classification closed prevents resource commands from accidentally gating
/// the selected/default provider before target resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResourceCapability {
    Playback,
    QueueAdd,
    PlaylistItems,
    PlaylistAdd,
    PlaylistRemove,
    PlaylistImage,
    PlaylistUnfollow,
    ShowEpisodes,
    AlbumTracks,
    ArtistAlbums,
    ArtistFollow,
    LibrarySave,
    LibraryUnsave,
    RelatedArtists,
}

impl ResourceCapability {
    const fn operation(self) -> &'static str {
        match self {
            Self::Playback => "playback",
            Self::QueueAdd => "queue additions",
            Self::PlaylistItems => "playlist item listing",
            Self::PlaylistAdd => "adding playlist items",
            Self::PlaylistRemove => "removing playlist items",
            Self::PlaylistImage => "playlist image upload",
            Self::PlaylistUnfollow => "playlist unfollow",
            Self::ShowEpisodes => "show episode listing",
            Self::AlbumTracks => "album track listing",
            Self::ArtistAlbums => "artist album listing",
            Self::ArtistFollow => "artist follow mutations",
            Self::LibrarySave => "saving this media type",
            Self::LibraryUnsave => "removing this media type from the library",
            Self::RelatedArtists => "related artists",
        }
    }

    fn supported(self, caps: &ProviderCaps, resource: &ResourceUri) -> bool {
        match self {
            Self::Playback => caps
                .transport
                .as_ref()
                .is_some_and(|transport| transport.play),
            Self::QueueAdd => caps
                .transport
                .as_ref()
                .is_some_and(|transport| transport.queue_add),
            Self::PlaylistItems => caps.playlists.item_read,
            Self::PlaylistAdd => caps.playlists.add,
            Self::PlaylistRemove => caps.playlists.remove,
            Self::PlaylistImage => caps.playlists.image,
            Self::PlaylistUnfollow => caps.playlists.unfollow,
            Self::ShowEpisodes => caps.catalog.show_episodes,
            Self::AlbumTracks => caps.catalog.album_tracks,
            Self::ArtistAlbums => caps.catalog.artist_albums,
            Self::ArtistFollow => caps.library.can_follow(&resource.kind()),
            Self::LibrarySave | Self::LibraryUnsave => caps.library.can_save(&resource.kind()),
            Self::RelatedArtists => caps.extras.related_artists,
        }
    }
}

#[derive(Clone, Debug)]
struct ProviderRouter {
    catalog: Option<ProviderCatalog>,
    selected: Option<ProviderId>,
}

impl ProviderRouter {
    async fn load(provider: Option<String>) -> Result<Self> {
        let selected = provider.map(ProviderId::new).transpose().map_err(|error| {
            provider_error(spotuify_protocol::IpcErrorKind::InvalidRequest, error)
        })?;
        let catalog = match daemon_request(Request::ProvidersList).await {
            Ok(ResponseData::ProviderList {
                default_provider,
                providers,
            }) => {
                let catalog = ProviderCatalog {
                    default_provider,
                    providers,
                };
                catalog.validate().map_err(|message| {
                    provider_error(spotuify_protocol::IpcErrorKind::Internal, message)
                })?;
                Some(catalog)
            }
            Ok(other) => anyhow::bail!("unexpected providers-list response: {other:?}"),
            // An older daemon may close the connection while decoding this
            // newer request instead of returning a typed Unsupported error.
            // The real command still runs next and reports any genuine IPC
            // failure, so catalog discovery remains additive for legacy use.
            Err(_) if selected.is_none() => None,
            Err(error) if is_catalog_compat_error(&error) => {
                return Err(provider_error(
                    spotuify_protocol::IpcErrorKind::Unsupported,
                    "the running daemon cannot validate --provider; upgrade or omit the flag",
                ));
            }
            Err(error) => return Err(error),
        };
        if let (Some(provider), Some(catalog)) = (&selected, &catalog) {
            if !catalog
                .providers
                .iter()
                .any(|descriptor| &descriptor.id == provider)
            {
                return Err(provider_error(
                    spotuify_protocol::IpcErrorKind::InvalidRequest,
                    format!("unknown provider `{provider}`"),
                ));
            }
        }
        Ok(Self { catalog, selected })
    }

    fn request_provider(&self) -> Option<ProviderId> {
        self.selected.clone()
    }

    fn effective_provider(&self) -> Option<ProviderId> {
        self.selected.clone().or_else(|| {
            self.catalog
                .as_ref()
                .and_then(|catalog| catalog.default_provider.clone())
        })
    }

    fn remote_source(&self) -> Result<SearchSourceData> {
        self.require("remote search", |caps| caps.search.remote)?;
        Ok(self.effective_provider().map_or_else(
            SearchSourceData::legacy_default_remote,
            SearchSourceData::Remote,
        ))
    }

    fn search_source(&self, source: crate::SearchSourceArg) -> Result<SearchSourceData> {
        match source {
            crate::SearchSourceArg::Local => Ok(SearchSourceData::Local),
            crate::SearchSourceArg::Remote => self.remote_source(),
            crate::SearchSourceArg::Hybrid => {
                self.require("hybrid search", |caps| caps.search.remote)?;
                Ok(SearchSourceData::Hybrid)
            }
        }
    }

    fn require(
        &self,
        operation: &str,
        supported: impl FnOnce(&ProviderCaps) -> bool,
    ) -> Result<()> {
        let Some(catalog) = &self.catalog else {
            // Capability discovery is additive for released daemons that do
            // not expose a provider catalog yet.
            return Ok(());
        };
        let provider = self
            .selected
            .as_ref()
            .or(catalog.default_provider.as_ref())
            .ok_or_else(|| {
                provider_error(
                    spotuify_protocol::IpcErrorKind::Unsupported,
                    format!("no default provider is configured for {operation}"),
                )
            })?;
        let descriptor = catalog
            .providers
            .iter()
            .find(|descriptor| &descriptor.id == provider)
            .ok_or_else(|| {
                provider_error(
                    spotuify_protocol::IpcErrorKind::Internal,
                    format!("provider catalog does not contain selected provider `{provider}`"),
                )
            })?;
        if supported(&descriptor.capabilities) {
            return Ok(());
        }
        Err(provider_error(
            spotuify_protocol::IpcErrorKind::Unsupported,
            format!("provider `{}` does not support {operation}", descriptor.id),
        ))
    }

    fn require_search_kinds(&self, kinds: &[MediaKind]) -> Result<()> {
        self.require("searching the requested media types", |caps| {
            caps.search.remote && kinds.iter().all(|kind| caps.search.kinds.contains(kind))
        })
    }

    fn require_search_scope(&self, scope: &SearchScopeData) -> Result<()> {
        if matches!(scope, SearchScopeData::All) {
            return self.require("searching any media type", |caps| {
                caps.search.remote && !caps.search.kinds.is_empty()
            });
        }
        self.require_search_kinds(&scope_kinds(scope))
    }

    fn descriptor_for_resource(&self, value: &str) -> Result<Option<&ProviderDescriptor>> {
        let Some(catalog) = &self.catalog else {
            return Ok(None);
        };
        let resource = ResourceUri::parse(value).map_err(|error| {
            provider_error(spotuify_protocol::IpcErrorKind::InvalidRequest, error)
        })?;
        let descriptor = catalog
            .providers
            .iter()
            .find(|descriptor| &descriptor.uri_scheme == resource.scheme())
            .ok_or_else(|| {
                provider_error(
                    spotuify_protocol::IpcErrorKind::InvalidRequest,
                    format!("unknown provider URI scheme `{}`", resource.scheme()),
                )
            })?;
        if let Some(selected) = &self.selected {
            if selected != &descriptor.id {
                return Err(provider_error(
                    spotuify_protocol::IpcErrorKind::InvalidRequest,
                    format!(
                        "provider `{selected}` conflicts with URI scheme `{}`",
                        resource.scheme()
                    ),
                ));
            }
        }
        Ok(Some(descriptor))
    }

    fn provider_for_resource(&self, value: &str) -> Result<Option<ProviderId>> {
        if ResourceUri::parse(value).is_err() {
            return Ok(self.request_provider());
        }
        Ok(self
            .descriptor_for_resource(value)?
            .map(|descriptor| descriptor.id.clone())
            .or_else(|| self.request_provider()))
    }

    fn require_resource(
        &self,
        value: &str,
        operation: &str,
        supported: impl FnOnce(&ProviderCaps) -> bool,
    ) -> Result<()> {
        let Some(descriptor) = self.descriptor_for_resource(value)? else {
            return Ok(());
        };
        if supported(&descriptor.capabilities) {
            return Ok(());
        }
        Err(provider_error(
            spotuify_protocol::IpcErrorKind::Unsupported,
            format!("provider `{}` does not support {operation}", descriptor.id),
        ))
    }

    fn require_resolved_capability(&self, uri: &str, capability: ResourceCapability) -> Result<()> {
        // Capability discovery is additive. Older daemons do not expose a
        // provider catalog or target resolution, so keep accepting their
        // legacy bare IDs instead of trying to parse them as canonical URIs.
        if self.catalog.is_none() {
            return Ok(());
        }
        let resource = ResourceUri::parse(uri).map_err(|error| {
            provider_error(spotuify_protocol::IpcErrorKind::InvalidRequest, error)
        })?;
        self.require_resource(uri, capability.operation(), |caps| {
            capability.supported(caps, &resource)
        })
    }

    fn require_resolved_capabilities<'a>(
        &self,
        uris: impl IntoIterator<Item = &'a str>,
        capability: ResourceCapability,
    ) -> Result<()> {
        for uri in uris {
            self.require_resolved_capability(uri, capability)?;
        }
        Ok(())
    }

    fn require_radio_start(&self, seed_uri: &str, dry_run: bool) -> Result<()> {
        self.require_resource(seed_uri, "radio", |caps| caps.extras.radio)?;
        if !dry_run {
            self.require_resource(seed_uri, "radio queue additions", |caps| {
                caps.transport
                    .as_ref()
                    .is_some_and(|transport| transport.queue_add)
            })?;
        }
        Ok(())
    }

    async fn resolve_optional(
        &self,
        input: &str,
        expected_kinds: Vec<MediaKind>,
    ) -> Result<Option<String>> {
        if let Some(uri) = self.canonical_resource(input, &expected_kinds)? {
            return Ok(Some(uri));
        }
        match daemon_request(self.resolve_request(input, expected_kinds)).await {
            Ok(ResponseData::TargetResolved { target }) => {
                Ok(target.map(|target| target.uri.as_uri()))
            }
            Ok(other) => anyhow::bail!("unexpected resolve-target response: {other:?}"),
            Err(error) if is_catalog_compat_error(&error) => {
                Ok(ResourceUri::parse(input).ok().map(|uri| uri.as_uri()))
            }
            Err(error) => Err(error),
        }
    }

    async fn resolve_required(
        &self,
        input: &str,
        expected_kinds: Vec<MediaKind>,
    ) -> Result<String> {
        if let Some(uri) = self.canonical_resource(input, &expected_kinds)? {
            return Ok(uri);
        }
        match daemon_request(self.resolve_request(input, expected_kinds)).await {
            Ok(ResponseData::TargetResolved {
                target: Some(target),
            }) => Ok(target.uri.as_uri()),
            Ok(ResponseData::TargetResolved { target: None }) => Err(provider_error(
                spotuify_protocol::IpcErrorKind::InvalidRequest,
                format!("unrecognized resource reference `{input}`"),
            )),
            Ok(other) => anyhow::bail!("unexpected resolve-target response: {other:?}"),
            Err(error) if is_catalog_compat_error(&error) => Ok(input.to_string()),
            Err(error) => Err(error),
        }
    }

    /// Canonical URIs are already normalized. Validate their kind and catalog
    /// ownership locally so an explicit foreign provider cannot turn a clear
    /// scope conflict into the selected adapter's `NotMine` result.
    fn canonical_resource(
        &self,
        input: &str,
        expected_kinds: &[MediaKind],
    ) -> Result<Option<String>> {
        let Ok(resource) = ResourceUri::parse(input) else {
            return Ok(None);
        };
        let actual = resource.kind();
        if !expected_kinds.contains(&actual) {
            return Err(provider_error(
                spotuify_protocol::IpcErrorKind::InvalidRequest,
                format!("resolved target has kind `{actual}`, which is not allowed"),
            ));
        }
        if self.catalog.is_some() {
            self.descriptor_for_resource(input)?;
        }
        Ok(Some(resource.as_uri()))
    }

    /// Resolve user input to its canonical provider URI before evaluating the
    /// operation capability. This preserves explicit-provider conflict errors
    /// and makes the URI owner, not the catalog default, authoritative.
    async fn resolve_and_require(
        &self,
        input: &str,
        expected_kinds: Vec<MediaKind>,
        capability: ResourceCapability,
    ) -> Result<String> {
        let uri = self.resolve_required(input, expected_kinds).await?;
        self.require_resolved_capability(&uri, capability)?;
        Ok(uri)
    }

    async fn resolve_optional_and_require(
        &self,
        input: &str,
        expected_kinds: Vec<MediaKind>,
        capability: ResourceCapability,
    ) -> Result<Option<String>> {
        let Some(uri) = self.resolve_optional(input, expected_kinds).await? else {
            return Ok(None);
        };
        self.require_resolved_capability(&uri, capability)?;
        Ok(Some(uri))
    }

    async fn resolve_many(
        &self,
        inputs: Vec<String>,
        expected_kinds: Vec<MediaKind>,
    ) -> Result<Vec<String>> {
        let mut resolved = Vec::with_capacity(inputs.len());
        for input in inputs {
            resolved.push(
                self.resolve_required(&input, expected_kinds.clone())
                    .await?,
            );
        }
        Ok(resolved)
    }

    fn resolve_request(&self, input: &str, expected_kinds: Vec<MediaKind>) -> Request {
        Request::ResolveTarget {
            input: input.to_string(),
            provider: self.request_provider(),
            expected_kinds: Some(expected_kinds),
        }
    }
}

fn provider_error(kind: spotuify_protocol::IpcErrorKind, message: impl ToString) -> anyhow::Error {
    anyhow::Error::new(DaemonRequestError::new(kind, message.to_string()))
}

#[allow(clippy::too_many_arguments)]
fn structured_daemon_error(
    kind: spotuify_protocol::IpcErrorKind,
    message: String,
    retryable: bool,
    provider: Option<ProviderId>,
    detail: Option<String>,
    retry_after_secs: Option<u64>,
    context: Option<&str>,
    terminal_mutation: bool,
) -> anyhow::Error {
    let message = match context {
        Some(context) => format!("{context}: {message}"),
        None => message,
    };
    let message = if kind == spotuify_protocol::IpcErrorKind::AuthRevoked {
        if terminal_mutation {
            terminal_auth_revoked_message(&message, provider.as_ref())
        } else {
            format!(
                "{message}. Run `{}` to recover",
                auth_recovery_command(provider.as_ref())
            )
        }
    } else {
        message
    };
    anyhow::Error::new(DaemonRequestError {
        kind,
        message,
        retryable,
        provider,
        detail,
        retry_after_secs,
    })
}

fn is_catalog_compat_error(error: &anyhow::Error) -> bool {
    if error
        .downcast_ref::<DaemonRequestError>()
        .is_some_and(|error| {
            matches!(
                error.kind,
                spotuify_protocol::IpcErrorKind::InvalidRequest
                    | spotuify_protocol::IpcErrorKind::Unsupported
            )
        })
    {
        return true;
    }
    let message = format!("{error:#}").to_ascii_lowercase();
    [
        "unknown variant",
        "failed to decode",
        "connection closed",
        "unexpected end of file",
        "early eof",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

fn scope_kinds(scope: &SearchScopeData) -> Vec<MediaKind> {
    match scope {
        SearchScopeData::All => vec![
            MediaKind::Track,
            MediaKind::Episode,
            MediaKind::Show,
            MediaKind::Album,
            MediaKind::Artist,
            MediaKind::Playlist,
        ],
        SearchScopeData::Track => vec![MediaKind::Track],
        SearchScopeData::Episode => vec![MediaKind::Episode],
        SearchScopeData::Show => vec![MediaKind::Show],
        SearchScopeData::Album => vec![MediaKind::Album],
        SearchScopeData::Artist => vec![MediaKind::Artist],
        SearchScopeData::Playlist => vec![MediaKind::Playlist],
    }
}

pub async fn ipc_status(format: OutputFormat) -> Result<()> {
    match daemon_request(Request::PlaybackGet).await? {
        ResponseData::Playback { playback } => output::print_playback(&playback, format),
        _ => unexpected_response(),
    }
}

pub async fn ipc_devices(format: OutputFormat) -> Result<()> {
    match daemon_request(Request::DevicesList).await? {
        ResponseData::Devices { devices } => output::print_devices(&devices, format),
        _ => unexpected_response(),
    }
}

pub async fn ipc_providers_list(format: OutputFormat) -> Result<()> {
    let catalog = ipc_provider_catalog().await?;
    output::print_provider_catalog(catalog.default_provider, catalog.providers, format)
}

pub async fn ipc_provider_catalog() -> Result<ProviderCatalog> {
    match daemon_request(Request::ProvidersList).await? {
        ResponseData::ProviderList {
            default_provider,
            providers,
        } => {
            let catalog = ProviderCatalog {
                default_provider,
                providers,
            };
            catalog.validate().map_err(|message| {
                provider_error(spotuify_protocol::IpcErrorKind::Internal, message)
            })?;
            Ok(catalog)
        }
        _ => unexpected_response(),
    }
}

pub async fn ipc_audio_outputs(format: OutputFormat) -> Result<()> {
    match daemon_request(Request::ListAudioOutputs).await? {
        ResponseData::AudioOutputs { outputs, selected } => {
            output::print_audio_outputs(&outputs, selected.as_deref(), format)
        }
        _ => unexpected_response(),
    }
}

pub async fn ipc_set_audio_output(name: &str) -> Result<()> {
    let device = match name.trim() {
        "" | "default" => None,
        name => Some(name.to_string()),
    };
    print_ack(Request::SetAudioOutput { device }).await
}

#[allow(clippy::too_many_arguments)]
pub async fn ipc_search(
    query: &str,
    scope: SearchScopeData,
    source: crate::SearchSourceArg,
    limit: u32,
    pages: u8,
    play: bool,
    index: usize,
    sort: Option<SearchSortData>,
    provider: Option<String>,
    format: OutputFormat,
) -> Result<()> {
    let router = ProviderRouter::load(provider).await?;
    if !matches!(source, crate::SearchSourceArg::Local) {
        router.require_search_scope(&scope)?;
    }
    let source = router.search_source(source)?;
    let request_provider = router.request_provider();
    // pages > 1 uses the same streaming path as the TUI (Request::SearchStream
    // → 18 parallel daemon-spawned tasks → DaemonEvent::SearchPage events →
    // SearchComplete). Aggregate events synchronously before printing so the
    // CLI experience stays one-shot.
    let items = if pages > 1 {
        stream_search_aggregate(query, scope, source, request_provider).await?
    } else {
        match daemon_request(Request::Search {
            query: query.to_string(),
            scope,
            source,
            limit,
            provider: request_provider,
            kinds: None,
            sort,
        })
        .await?
        {
            ResponseData::SearchResults { items } => items,
            _ => return unexpected_response(),
        }
    };

    if play {
        let item = selection::media_item_at_index(items, query, index)?;
        router.require_resolved_capability(&item.uri, ResourceCapability::Playback)?;
        daemon_request(Request::PlaybackCommand {
            command: PlaybackCommand::PlayUri {
                uri: item.uri.clone(),
                context_uri: None,
            },
        })
        .await?;
        return output::print_item_receipt("play", &item, format);
    }

    output::print_media_items(&items, format)
}

/// CLI equivalent of TUI scroll-load-more: fetch a single page of
/// results for one media kind at a given offset.
pub async fn ipc_search_page(
    query: &str,
    kind: MediaKind,
    offset: u32,
    provider: Option<String>,
    format: OutputFormat,
) -> Result<()> {
    let router = ProviderRouter::load(provider).await?;
    router.require_search_kinds(std::slice::from_ref(&kind))?;
    let version = 1u64;
    let mut client = IpcClient::connect_with_source(OperationSource::Cli).await?;
    // Subscribe FIRST: a fast local page can broadcast before a late
    // subscribe and is never replayed.
    client.subscribe_events().await?;
    let ack = client
        .request(Request::SearchPage {
            query: query.to_string(),
            kind: kind.clone(),
            offset,
            version,
            provider: router.request_provider(),
        })
        .await?;
    match ack {
        Response::Ok {
            data: ResponseData::SearchStarted { .. },
        } => {}
        Response::Error {
            message,
            kind,
            retryable,
            provider,
            detail,
            ..
        } => {
            return Err(structured_daemon_error(
                kind,
                message,
                retryable,
                provider,
                detail,
                None,
                Some("search-page request failed"),
                false,
            ));
        }
        other => anyhow::bail!("unexpected ack: {other:?}"),
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    loop {
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for SearchPage event");
        }
        let ev = tokio::time::timeout(Duration::from_millis(500), client.next_event()).await;
        match ev {
            Ok(Ok(DaemonEvent::SearchPage {
                kind: ev_kind,
                offset: ev_offset,
                version: ev_version,
                items,
                ..
            })) if ev_kind == kind && ev_offset == offset && ev_version == version => {
                return output::print_media_items(&items, format);
            }
            Ok(Ok(DaemonEvent::SearchFailed {
                kind: Some(ev_kind),
                offset: Some(ev_offset),
                version: ev_version,
                message,
                ..
            })) if ev_kind == kind && ev_offset == offset && ev_version == version => {
                anyhow::bail!("{message}");
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => continue,
        }
    }
}

/// Connect, subscribe to events, fire `Request::SearchStream`, drain
/// pages until `SearchComplete`. Used by `spotuify search --pages 3`
/// to give CLI users the same 180-result capability as the TUI.
async fn stream_search_aggregate(
    query: &str,
    scope: SearchScopeData,
    source: SearchSourceData,
    provider: Option<ProviderId>,
) -> Result<Vec<MediaItem>> {
    let version = 1u64;
    let mut client = IpcClient::connect_with_source(OperationSource::Cli).await?;
    // Subscribe FIRST — see ipc_search_page.
    client.subscribe_events().await?;
    let ack = client
        .request(Request::SearchStream {
            query: query.to_string(),
            scope,
            source,
            version,
            provider,
        })
        .await?;
    match ack {
        Response::Ok {
            data: ResponseData::SearchStarted { .. },
        } => {}
        Response::Error {
            message,
            kind,
            retryable,
            provider,
            detail,
            ..
        } => {
            return Err(structured_daemon_error(
                kind,
                message,
                retryable,
                provider,
                detail,
                None,
                Some("search-stream request failed"),
                false,
            ));
        }
        other => anyhow::bail!("unexpected ack: {other:?}"),
    }

    let mut items: Vec<MediaItem> = Vec::new();
    let mut seen_uris: std::collections::HashSet<String> = std::collections::HashSet::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if std::time::Instant::now() >= deadline {
            // Partial results are better than a hard error; just return
            // what we collected so far. Mirrors TUI behavior when an
            // event leg lags.
            break;
        }
        let ev = tokio::time::timeout(Duration::from_millis(500), client.next_event()).await;
        match ev {
            Ok(Ok(DaemonEvent::SearchPage {
                version: ev_version,
                items: page_items,
                ..
            })) if ev_version == version => {
                for item in page_items {
                    if seen_uris.insert(item.uri.clone()) {
                        items.push(item);
                    }
                }
            }
            Ok(Ok(DaemonEvent::SearchComplete {
                version: ev_version,
                ..
            })) if ev_version == version => break,
            Ok(Ok(DaemonEvent::SearchFailed {
                version: ev_version,
                message,
                ..
            })) if ev_version == version => anyhow::bail!("{message}"),
            Ok(Ok(_)) => continue,
            Ok(Err(e)) => return Err(e),
            Err(_) => continue,
        }
    }
    Ok(items)
}

pub async fn ipc_queue(command: Option<crate::QueueCommand>, format: OutputFormat) -> Result<()> {
    match command {
        Some(crate::QueueCommand::Add {
            uris,
            ids,
            search,
            many,
            wait,
            provider,
            format,
        }) => ipc_queue_add(uris, ids, search, many, wait, provider, format).await,
        None => match daemon_request(Request::QueueGet).await? {
            ResponseData::Queue { queue } => output::print_queue(&queue, format),
            _ => unexpected_response(),
        },
    }
}

pub async fn ipc_playlists(provider: Option<String>, format: OutputFormat) -> Result<()> {
    let router = ProviderRouter::load(provider).await?;
    router.require("playlist listing", |caps| caps.playlists.list)?;
    match daemon_request(Request::PlaylistsList {
        provider: router.request_provider(),
    })
    .await?
    {
        ResponseData::Playlists { playlists } => output::print_playlists(&playlists, format),
        _ => unexpected_response(),
    }
}

pub async fn ipc_resolve_tracks(
    from: &Path,
    provider: Option<String>,
    format: OutputFormat,
) -> Result<()> {
    let router = ProviderRouter::load(provider).await?;
    router.require_search_kinds(&[MediaKind::Track])?;
    let raw = read_input(from)?;
    let plan = crate::agent_playlists::parse_plan(&raw)?;
    let mut results = Vec::with_capacity(plan.candidate_searches.len());
    for query in &plan.candidate_searches {
        let items = match daemon_request(Request::Search {
            query: query.clone(),
            scope: SearchScopeData::Track,
            // Plan resolution = catalog discovery, not library lookup.
            source: router.remote_source()?,
            limit: 50,
            provider: router.request_provider(),
            kinds: None,
            sort: None,
        })
        .await?
        {
            ResponseData::SearchResults { items } => items,
            _ => return unexpected_response(),
        };
        results.push(items);
    }
    let candidates = crate::agent_playlists::resolve_plan_candidates(&plan, results);
    output::print_resolved_track_candidates(&candidates, format)
}

pub async fn ipc_play_query(
    query: &str,
    scope: SearchScopeData,
    provider: Option<String>,
    format: OutputFormat,
) -> Result<()> {
    let router = ProviderRouter::load(provider).await?;
    if let Some(uri) = router
        .resolve_optional_and_require(query, scope_kinds(&scope), ResourceCapability::Playback)
        .await?
    {
        return ipc_play_resolved_uri(&uri, format).await;
    }
    router.require_search_scope(&scope)?;
    let items = match daemon_request(Request::Search {
        query: query.to_string(),
        scope,
        source: router.remote_source()?,
        limit: 10,
        provider: router.request_provider(),
        kinds: None,
        sort: None,
    })
    .await?
    {
        ResponseData::SearchResults { items } => items,
        _ => return unexpected_response(),
    };
    let item = selection::media_item_at_index(items, query, 1)?;
    router.require_resolved_capability(&item.uri, ResourceCapability::Playback)?;
    daemon_request(Request::PlaybackCommand {
        command: PlaybackCommand::PlayUri {
            uri: item.uri.clone(),
            context_uri: None,
        },
    })
    .await?;
    output::print_item_receipt("play", &item, format)
}

pub async fn ipc_reindex(format: OutputFormat) -> Result<()> {
    match daemon_request(Request::Reindex).await? {
        ResponseData::Reindex { stats } => output::print_reindex_stats(&stats, format),
        _ => unexpected_response(),
    }
}

pub async fn ipc_cache_status(format: OutputFormat) -> Result<()> {
    match daemon_request(Request::CacheStatus).await? {
        ResponseData::CacheStatus { status } => output::print_cache_status(&status, format),
        _ => unexpected_response(),
    }
}

pub async fn ipc_lyrics(command: crate::LyricsCommand) -> Result<()> {
    match command {
        crate::LyricsCommand::Show {
            track,
            provider,
            format,
        } => {
            let router = ProviderRouter::load(provider).await?;
            let track = match track {
                Some(track) => Some(
                    router
                        .resolve_required(&track, vec![MediaKind::Track])
                        .await?,
                ),
                None => None,
            };
            let data = daemon_request(Request::LyricsGet {
                track_uri: track,
                force_refresh: false,
            })
            .await?;
            output::print_response_data(&data, format)
        }
        crate::LyricsCommand::Follow {
            lines,
            lead,
            format,
        } => ipc_lyrics_follow(lines, lead.as_deref(), format.into()).await,
        crate::LyricsCommand::Fetch {
            track_uri,
            provider,
            format,
        } => {
            let router = ProviderRouter::load(provider).await?;
            let track_uri = router
                .resolve_required(&track_uri, vec![MediaKind::Track])
                .await?;
            let data = daemon_request(Request::LyricsGet {
                track_uri: Some(track_uri),
                force_refresh: true,
            })
            .await?;
            output::print_response_data(&data, format)
        }
        crate::LyricsCommand::Export {
            track_uri,
            provider,
            output,
        } => {
            let router = ProviderRouter::load(provider).await?;
            let track_uri = router
                .resolve_required(&track_uri, vec![MediaKind::Track])
                .await?;
            let data = daemon_request(Request::LyricsGet {
                track_uri: Some(track_uri),
                force_refresh: false,
            })
            .await?;
            output::export_lyrics_lrc(&data, output.as_deref())
        }
        crate::LyricsCommand::Offset {
            track_uri,
            offset,
            provider,
            format,
        } => {
            let router = ProviderRouter::load(provider).await?;
            let track_uri = router
                .resolve_required(&track_uri, vec![MediaKind::Track])
                .await?;
            let offset_ms = parse_lyrics_offset(&offset)?;
            let data = daemon_request(Request::LyricsOffsetSet {
                track_uri,
                offset_ms,
            })
            .await?;
            output::print_response_data(&data, format)
        }
    }
}

pub async fn ipc_lyrics_follow(
    lines: usize,
    lead: Option<&str>,
    format: OutputFormat,
) -> Result<()> {
    if lines == 0 {
        anyhow::bail!("lyrics follow: --lines must be at least 1");
    }
    if !matches!(format, OutputFormat::Table | OutputFormat::Jsonl) {
        anyhow::bail!("lyrics follow supports only --format table or --format jsonl");
    }
    let lead_ms = lead.map_or(Ok(0), parse_lyrics_offset)?;

    spotuify_launcher::ensure_daemon_running().await?;
    let mut client = IpcClient::connect_with_source(OperationSource::Cli).await?;
    client_request(
        &mut client,
        Request::SubscribeEvents {
            provider_policy: true,
        },
    )
    .await?;

    let initial = client_playback_get(&mut client).await?;
    if initial.item.is_none() {
        anyhow::bail!("nothing is playing; run `spotuify play \"...\"` first");
    }

    let mut follower = LyricsFollower::new(initial, lead_ms);
    follower.refresh_lyrics(&mut client).await?;

    let mut stdout = std::io::stdout();
    let clear_screen = format == OutputFormat::Table && stdout.is_terminal();
    let mut last_render: Option<FollowRenderKey> = None;
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            _ = ticker.tick() => {
                follower.render_if_changed(&mut stdout, lines, format, clear_screen, &mut last_render)?;
            }
            event = client.next_event() => {
                match event? {
                    DaemonEvent::PlaybackChanged { playback: Some(playback), .. } => {
                        let track_changed = follower.update_playback(playback);
                        if track_changed {
                            follower.refresh_lyrics(&mut client).await?;
                            last_render = None;
                        }
                    }
                    DaemonEvent::ShutdownRequested => return Ok(()),
                    _ => {}
                }
            }
        }
    }
}

async fn client_request(client: &mut IpcClient, request: Request) -> Result<ResponseData> {
    match client.request(request).await? {
        Response::Ok { data } => Ok(data),
        Response::Error {
            message,
            kind,
            retryable,
            provider,
            detail,
            ..
        } => Err(structured_daemon_error(
            kind, message, retryable, provider, detail, None, None, false,
        )),
    }
}

async fn client_playback_get(client: &mut IpcClient) -> Result<Playback> {
    match client_request(client, Request::PlaybackGet).await? {
        ResponseData::Playback { playback } => Ok(playback),
        _ => unexpected_response(),
    }
}

async fn client_lyrics_get(client: &mut IpcClient, track_uri: &str) -> Result<FollowLyrics> {
    match client_request(
        client,
        Request::LyricsGet {
            track_uri: Some(track_uri.to_string()),
            force_refresh: false,
        },
    )
    .await?
    {
        ResponseData::Lyrics { lyrics, offset_ms } => Ok(FollowLyrics { lyrics, offset_ms }),
        _ => unexpected_response(),
    }
}

#[derive(Debug)]
struct LyricsFollower {
    playback: Playback,
    anchored_at: Instant,
    lyrics: Option<SyncedLyrics>,
    lyrics_offset_ms: i64,
    lead_ms: i64,
    status: Option<String>,
}

impl LyricsFollower {
    fn new(playback: Playback, lead_ms: i64) -> Self {
        Self {
            playback,
            anchored_at: Instant::now(),
            lyrics: None,
            lyrics_offset_ms: 0,
            lead_ms,
            status: None,
        }
    }

    fn update_playback(&mut self, playback: Playback) -> bool {
        let old_uri = self.playback.item.as_ref().map(|item| item.uri.as_str());
        let new_uri = playback.item.as_ref().map(|item| item.uri.as_str());
        let changed = old_uri != new_uri;
        self.playback = playback;
        self.anchored_at = Instant::now();
        if changed {
            self.lyrics = None;
            self.lyrics_offset_ms = 0;
            self.status = None;
        }
        changed
    }

    async fn refresh_lyrics(&mut self, client: &mut IpcClient) -> Result<()> {
        let Some(item) = self.playback.item.as_ref() else {
            self.lyrics = None;
            self.status = Some("No active track. Waiting for playback.".to_string());
            return Ok(());
        };
        let data = client_lyrics_get(client, &item.uri).await?;
        self.lyrics_offset_ms = data.offset_ms;
        match data.lyrics {
            Some(lyrics) if lyrics.synced => {
                self.lyrics = Some(lyrics);
                self.status = None;
            }
            Some(_) => {
                self.lyrics = None;
                self.status =
                    Some("synced lyrics unavailable; use `spotuify lyrics show`".to_string());
            }
            None => {
                self.lyrics = None;
                self.status = Some("No lyrics available for this track".to_string());
            }
        }
        Ok(())
    }

    fn render_if_changed<W: Write>(
        &self,
        writer: &mut W,
        lines: usize,
        format: OutputFormat,
        clear_screen: bool,
        last_render: &mut Option<FollowRenderKey>,
    ) -> Result<()> {
        let view = self.view_at(Instant::now());
        let key = FollowRenderKey::from(&view);
        if last_render.as_ref() == Some(&key) {
            return Ok(());
        }
        match format {
            OutputFormat::Table => write_follow_table(writer, &view, lines, clear_screen)?,
            OutputFormat::Jsonl => write_follow_jsonl(writer, &view)?,
            _ => unreachable!("validated before follow loop"),
        }
        *last_render = Some(key);
        Ok(())
    }

    fn view_at(&self, now: Instant) -> FollowView<'_> {
        let progress_ms = playback_progress_at(&self.playback, self.anchored_at, now);
        let active_line = self.lyrics.as_ref().and_then(|lyrics| {
            active_lyric_line_index(
                &lyrics.lines,
                progress_ms,
                self.lyrics_offset_ms.saturating_add(self.lead_ms),
            )
        });
        FollowView {
            playback: &self.playback,
            lyrics: self.lyrics.as_ref(),
            progress_ms,
            active_line,
            status: self.status.as_deref(),
        }
    }
}

#[derive(Debug)]
struct FollowLyrics {
    lyrics: Option<SyncedLyrics>,
    offset_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FollowRenderKey {
    track_uri: Option<String>,
    active_line: Option<usize>,
    is_playing: bool,
    status: Option<String>,
}

impl From<&FollowView<'_>> for FollowRenderKey {
    fn from(view: &FollowView<'_>) -> Self {
        Self {
            track_uri: view.playback.item.as_ref().map(|item| item.uri.clone()),
            active_line: view.active_line,
            is_playing: view.playback.is_playing,
            status: view.status.map(str::to_string),
        }
    }
}

#[derive(Debug)]
struct FollowView<'a> {
    playback: &'a Playback,
    lyrics: Option<&'a SyncedLyrics>,
    progress_ms: u64,
    active_line: Option<usize>,
    status: Option<&'a str>,
}

fn playback_progress_at(playback: &Playback, anchored_at: Instant, now: Instant) -> u64 {
    let elapsed_ms = if playback.is_playing {
        now.saturating_duration_since(anchored_at).as_millis() as u64
    } else {
        0
    };
    let progress = playback.progress_ms.saturating_add(elapsed_ms);
    playback
        .item
        .as_ref()
        .filter(|item| item.duration_ms > 0)
        .map_or(progress, |item| progress.min(item.duration_ms))
}

fn lyric_window(lines: &[LyricLine], active: usize, desired: usize) -> std::ops::Range<usize> {
    if lines.is_empty() || desired == 0 {
        return 0..0;
    }
    let desired = desired.min(lines.len());
    let before = desired / 2;
    let mut start = active.saturating_sub(before);
    if start + desired > lines.len() {
        start = lines.len().saturating_sub(desired);
    }
    start..(start + desired)
}

fn write_follow_table<W: Write>(
    writer: &mut W,
    view: &FollowView<'_>,
    lines: usize,
    clear_screen: bool,
) -> Result<()> {
    if clear_screen {
        write!(writer, "\x1B[2J\x1B[H")?;
    }
    let Some(item) = view.playback.item.as_ref() else {
        writeln!(writer, "No active track. Waiting for playback.")?;
        writer.flush()?;
        return Ok(());
    };
    writeln!(writer, "{} - {}", item.name, item.subtitle)?;
    writeln!(
        writer,
        "{}  {}",
        if view.playback.is_playing {
            "playing"
        } else {
            "paused"
        },
        format_duration(view.progress_ms)
    )?;
    if let Some(status) = view.status {
        writeln!(writer, "\n{status}")?;
        writer.flush()?;
        return Ok(());
    }
    let Some(lyrics) = view.lyrics else {
        writeln!(writer, "\nNo lyrics loaded yet.")?;
        writer.flush()?;
        return Ok(());
    };
    let active = view
        .active_line
        .unwrap_or(0)
        .min(lyrics.lines.len().saturating_sub(1));
    writeln!(writer)?;
    for index in lyric_window(&lyrics.lines, active, lines) {
        let marker = if index == active { ">" } else { " " };
        writeln!(writer, "{marker} {}", lyrics.lines[index].text)?;
    }
    writer.flush()?;
    Ok(())
}

fn write_follow_jsonl<W: Write>(writer: &mut W, view: &FollowView<'_>) -> Result<()> {
    let item = view.playback.item.as_ref();
    if let Some(status) = view.status {
        writeln!(
            writer,
            "{}",
            serde_json::json!({
                "event": "status",
                "track_uri": item.map(|item| item.uri.as_str()),
                "track_name": item.map(|item| item.name.as_str()),
                "artist": item.map(|item| item.subtitle.as_str()),
                "is_playing": view.playback.is_playing,
                "progress_ms": view.progress_ms,
                "message": status,
            })
        )?;
        writer.flush()?;
        return Ok(());
    }
    let Some((lyrics, active)) = view.lyrics.zip(view.active_line) else {
        return Ok(());
    };
    let Some(line) = lyrics.lines.get(active) else {
        return Ok(());
    };
    writeln!(
        writer,
        "{}",
        serde_json::json!({
            "event": "line",
            "track_uri": item.map(|item| item.uri.as_str()),
            "track_name": item.map(|item| item.name.as_str()),
            "artist": item.map(|item| item.subtitle.as_str()),
            "is_playing": view.playback.is_playing,
            "progress_ms": view.progress_ms,
            "line_index": active,
            "line_start_ms": line.start_ms,
            "text": line.text.as_str(),
            "is_rtl": line.is_rtl,
        })
    )?;
    writer.flush()?;
    Ok(())
}

fn format_duration(ms: u64) -> String {
    let total_seconds = ms / 1_000;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{minutes:02}:{seconds:02}")
}

pub async fn ipc_refresh_media(format: OutputFormat) -> Result<()> {
    let playback = daemon_current_playback().await?.unwrap_or_default();
    let item = playback
        .item
        .context("no active track; start playback before refreshing media")?;

    let cover_art = match item.image_url.clone() {
        Some(url) => match daemon_request(Request::CoverArt { url }).await? {
            ResponseData::CoverArt {
                path,
                cache_hit,
                bytes,
                ..
            } => Some(output::MediaRefreshCover {
                path,
                cache_hit,
                bytes,
            }),
            _ => return unexpected_response(),
        },
        None => None,
    };

    let lyrics_data = daemon_request(Request::LyricsGet {
        track_uri: Some(item.uri.clone()),
        force_refresh: true,
    })
    .await?;
    let lyrics = match lyrics_data {
        ResponseData::Lyrics { lyrics, offset_ms } => output::MediaRefreshLyrics {
            found: lyrics.is_some(),
            lines: lyrics.as_ref().map_or(0, |lyrics| lyrics.lines.len()),
            offset_ms,
        },
        _ => return unexpected_response(),
    };

    output::print_media_refresh(
        &output::MediaRefreshOutput {
            track_uri: item.uri,
            track_name: item.name,
            cover_art,
            lyrics,
        },
        format,
    )
}

pub async fn ipc_sync(
    target: SyncTargetData,
    provider: Option<String>,
    format: OutputFormat,
) -> Result<()> {
    let router = ProviderRouter::load(provider).await?;
    match target {
        SyncTargetData::All => router.require("synchronization", |caps| {
            caps.search.remote
                || !caps.library.read_kinds.is_empty()
                || caps.playlists.list
                || caps.catalog.recently_played
                || caps.transport.is_some()
        })?,
        SyncTargetData::Playback => router.require("playback synchronization", |caps| {
            caps.transport
                .as_ref()
                .is_some_and(|transport| transport.playback_state)
        })?,
        SyncTargetData::Queue => router.require("queue synchronization", |caps| {
            caps.transport
                .as_ref()
                .is_some_and(|transport| transport.queue_read)
        })?,
        SyncTargetData::Devices => router.require("device synchronization", |caps| {
            caps.transport
                .as_ref()
                .is_some_and(|transport| transport.devices)
        })?,
        SyncTargetData::Playlists => {
            router.require("playlist synchronization", |caps| caps.playlists.list)?
        }
        SyncTargetData::Recent => router.require("recent-play synchronization", |caps| {
            caps.catalog.recently_played
        })?,
        SyncTargetData::Library => router.require("library synchronization", |caps| {
            !caps.library.read_kinds.is_empty()
        })?,
    }
    match daemon_request(Request::Sync {
        target,
        provider: router.request_provider(),
    })
    .await?
    {
        ResponseData::Sync { summary } => output::print_sync_summary(&summary, format),
        _ => unexpected_response(),
    }
}

pub async fn ipc_viz(command: crate::VizCommand) -> Result<()> {
    match command {
        crate::VizCommand::Enable => print_ack(Request::SetVizEnabled { enabled: true }).await,
        crate::VizCommand::Disable => print_ack(Request::SetVizEnabled { enabled: false }).await,
        crate::VizCommand::Source { kind } => {
            print_ack(Request::SetVizSource { kind: kind.into() }).await
        }
        crate::VizCommand::Status { format } => {
            match daemon_request(Request::GetVizStatus).await? {
                data @ ResponseData::VizStatus { .. } => output::print_response_data(&data, format),
                _ => unexpected_response(),
            }
        }
    }
}

pub async fn ipc_mpris(command: crate::MprisCommand) -> Result<()> {
    match command {
        crate::MprisCommand::Status { format } => {
            match daemon_request(Request::GetDoctorReport).await? {
                ResponseData::DoctorReport { report } => {
                    let diagnostics = report
                        .system
                        .context("daemon did not return media-control diagnostics")?;
                    output::print_system_diagnostics(&diagnostics, format)
                }
                _ => unexpected_response(),
            }
        }
    }
}

pub async fn ipc_play_uri(uri: &str, provider: Option<String>, format: OutputFormat) -> Result<()> {
    let router = ProviderRouter::load(provider).await?;
    let uri = router
        .resolve_and_require(
            uri,
            vec![
                MediaKind::Track,
                MediaKind::Episode,
                MediaKind::Album,
                MediaKind::Artist,
                MediaKind::Playlist,
            ],
            ResourceCapability::Playback,
        )
        .await?;
    ipc_play_resolved_uri(&uri, format).await
}

async fn ipc_play_resolved_uri(uri: &str, format: OutputFormat) -> Result<()> {
    match daemon_request(Request::PlaybackCommand {
        command: PlaybackCommand::PlayUri {
            uri: uri.to_string(),
            context_uri: None,
        },
    })
    .await?
    {
        ResponseData::Mutation { receipt } => {
            output::print_uri_receipt(&receipt.action, uri, &receipt.message, format)
        }
        _ => unexpected_response(),
    }
}

/// Issue a mutation and BLOCK until the daemon finalizes it, failing
/// with the daemon's error when the optimistic mutation later fails
/// upstream. Optimistic receipts otherwise report ok/exit-0 even when
/// the body 404s — fine interactively, wrong for scripts. Subscribes
/// BEFORE sending on one connection so a fast finalize can't be missed.
async fn daemon_request_finalized(request: Request) -> Result<ResponseData> {
    spotuify_launcher::ensure_daemon_running().await?;
    let mut client = IpcClient::connect_with_source(OperationSource::Cli).await?;
    client.subscribe_events().await?;
    let mutation_id = mutation_id_for_request(&request);
    let response = client
        .request_with_mutation_id(request.clone(), mutation_id)
        .await?;
    let data = match response {
        Response::Ok { data } => data,
        Response::Error {
            kind: spotuify_protocol::IpcErrorKind::AuthRevoked,
            message,
            retryable,
            provider,
            detail,
            ..
        } if request.requires_mutation_id() => {
            return Err(anyhow::Error::new(DaemonRequestError::from_response(
                spotuify_protocol::IpcErrorKind::AuthRevoked,
                terminal_auth_revoked_message(&message, provider.as_ref()),
                retryable,
                provider,
                detail,
            )))
        }
        Response::Error {
            kind,
            message,
            retryable,
            provider,
            detail,
            ..
        } => {
            return Err(anyhow::Error::new(DaemonRequestError::from_response(
                kind, message, retryable, provider, detail,
            )))
        }
    };
    if let ResponseData::Mutation { receipt } = &data {
        match receipt.status {
            Some(spotuify_protocol::ReceiptStatus::Confirmed)
            | Some(spotuify_protocol::ReceiptStatus::Failed) => {
                return terminal_mutation_result(data)
            }
            Some(spotuify_protocol::ReceiptStatus::Pending) | None => {}
        }
    }
    let receipt_id = match &data {
        ResponseData::Mutation {
            receipt:
                spotuify_protocol::CommandReceipt {
                    receipt_id: Some(id),
                    ..
                },
        } => *id,
        // No receipt id (old daemon) or not a receipt-carrying
        // response; nothing to wait on.
        _ => return Ok(data),
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut last_replay = std::time::Instant::now();
    loop {
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for the mutation to finalize");
        }
        match tokio::time::timeout(Duration::from_millis(100), client.next_event()).await {
            Ok(Ok(DaemonEvent::MutationFinalized {
                receipt_id: id,
                status,
                message,
            })) if id == receipt_id => {
                if let Some(id) = mutation_id {
                    let response = client
                        .request_with_mutation_id(request.clone(), Some(id))
                        .await?;
                    let terminal = match response {
                        Response::Ok { data } => data,
                        Response::Error {
                            kind,
                            message,
                            retryable,
                            provider,
                            detail,
                            ..
                        } => {
                            let message = if kind == spotuify_protocol::IpcErrorKind::AuthRevoked
                                && request.requires_mutation_id()
                            {
                                terminal_auth_revoked_message(&message, provider.as_ref())
                            } else {
                                message
                            };
                            return Err(anyhow::Error::new(DaemonRequestError::from_response(
                                kind, message, retryable, provider, detail,
                            )));
                        }
                    };
                    return terminal_mutation_result(terminal);
                }
                let mut terminal = data;
                if let ResponseData::Mutation { receipt } = &mut terminal {
                    receipt.status = Some(status);
                    receipt.message = message;
                }
                return terminal_mutation_result(terminal);
            }
            Ok(Ok(_)) => {}
            Ok(Err(err)) => return Err(err),
            Err(_) => {}
        }
        if last_replay.elapsed() >= Duration::from_millis(500) {
            last_replay = std::time::Instant::now();
            if let Some(id) = mutation_id {
                if let Some(terminal) =
                    replay_mutation_if_terminal(&mut client, &request, id).await?
                {
                    return terminal_mutation_result(terminal);
                }
            }
        }
    }
}

async fn replay_mutation_if_terminal(
    client: &mut IpcClient,
    request: &Request,
    mutation_id: spotuify_protocol::MutationId,
) -> Result<Option<ResponseData>> {
    let response = client
        .request_with_mutation_id_and_timeout(
            request.clone(),
            Some(mutation_id),
            Duration::from_secs(2),
        )
        .await?;
    let data = match response {
        Response::Ok { data } => data,
        Response::Error {
            kind,
            message,
            retryable,
            provider,
            detail,
            ..
        } => {
            let message = if kind == spotuify_protocol::IpcErrorKind::AuthRevoked
                && request.requires_mutation_id()
            {
                terminal_auth_revoked_message(&message, provider.as_ref())
            } else {
                message
            };
            return Err(anyhow::Error::new(DaemonRequestError::from_response(
                kind, message, retryable, provider, detail,
            )));
        }
    };
    let terminal = matches!(
        &data,
        ResponseData::Mutation {
            receipt: spotuify_protocol::CommandReceipt {
                status: Some(
                    spotuify_protocol::ReceiptStatus::Confirmed
                        | spotuify_protocol::ReceiptStatus::Failed
                ),
                ..
            }
        }
    );
    Ok(terminal.then_some(data))
}

fn terminal_mutation_result(data: ResponseData) -> Result<ResponseData> {
    if let ResponseData::Mutation { receipt } = &data {
        if receipt.status == Some(spotuify_protocol::ReceiptStatus::Failed) {
            let summary = receipt.error.as_ref();
            let kind = summary.map_or(spotuify_protocol::IpcErrorKind::Provider, |error| {
                error.kind
            });
            let provider = summary.and_then(|error| error.provider.clone());
            return Err(structured_daemon_error(
                kind,
                receipt.message.clone(),
                kind.is_retryable(),
                provider,
                summary.and_then(|error| error.detail.clone()),
                summary.and_then(|error| error.retry_after_secs),
                None,
                true,
            ));
        }
    }
    Ok(data)
}

/// Remove a track/album/etc. from the library. CLI parity for the
/// TUI's UnsaveSelection — there was no way to un-like from the CLI.
pub async fn ipc_unsave_target(
    target: &str,
    wait: bool,
    provider: Option<String>,
    format: OutputFormat,
) -> Result<()> {
    let router = ProviderRouter::load(provider).await?;
    let uri = router
        .resolve_and_require(
            target,
            vec![
                MediaKind::Track,
                MediaKind::Episode,
                MediaKind::Album,
                MediaKind::Show,
                MediaKind::Artist,
            ],
            ResourceCapability::LibraryUnsave,
        )
        .await?;
    let request = Request::LibraryUnsave { uri };
    let data = if wait {
        daemon_request_finalized(request).await?
    } else {
        daemon_request(request).await?
    };
    match data {
        ResponseData::Mutation { mut receipt } => {
            receipt.action = "unlike".to_string();
            output::print_basic_receipt(&receipt.action, &receipt.message, format)
        }
        _ => unexpected_response(),
    }
}

async fn print_ack(request: Request) -> Result<()> {
    print_ack_formatted(request, OutputFormat::Table).await
}

/// Format-aware ack: `--format json` must emit JSON on stdout — bare
/// prose under json/jsonl broke the stable-output contract for agents
/// parsing these commands.
async fn print_ack_formatted(request: Request, format: OutputFormat) -> Result<()> {
    match daemon_request(request).await? {
        ResponseData::Ack { message } => {
            match format {
                OutputFormat::Json => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(
                            &serde_json::json!({ "ok": true, "message": message })
                        )?
                    );
                }
                OutputFormat::Jsonl => {
                    println!(
                        "{}",
                        serde_json::to_string(
                            &serde_json::json!({ "ok": true, "message": message })
                        )?
                    );
                }
                _ => println!("{message}"),
            }
            Ok(())
        }
        _ => unexpected_response(),
    }
}

pub async fn ipc_playback_command(action: PlaybackCommand, format: OutputFormat) -> Result<()> {
    print_mutation(
        daemon_request(Request::PlaybackCommand { command: action }).await?,
        format,
    )
}

pub async fn daemon_current_playback() -> Result<Option<Playback>> {
    match daemon_request(Request::PlaybackGet).await? {
        ResponseData::Playback { playback } => Ok(Some(playback)),
        _ => unexpected_response(),
    }
}

pub async fn ipc_transfer(device: &str, format: OutputFormat) -> Result<()> {
    print_mutation(
        daemon_request(Request::DeviceTransfer {
            device: device.to_string(),
        })
        .await?,
        format,
    )
}

pub async fn ipc_playlist(command: crate::PlaylistCommand) -> Result<()> {
    match command {
        crate::PlaylistCommand::Plan { brief, format } => {
            let plan = crate::agent_playlists::build_playlist_plan(&brief)?;
            output::print_playlist_plan(&plan, format)
        }
        crate::PlaylistCommand::Create {
            name,
            from,
            dry_run,
            yes,
            provider,
            format,
        } => ipc_playlist_create(&name, &from, dry_run, yes, provider, format).await,
        crate::PlaylistCommand::Tracks {
            playlist,
            provider,
            format,
        } => {
            let router = ProviderRouter::load(provider).await?;
            let playlist =
                daemon_playlist_and_require(&playlist, &router, ResourceCapability::PlaylistItems)
                    .await?;
            match daemon_request(Request::PlaylistTracks {
                playlist: playlist.id,
                wait: true,
                provider: router.request_provider(),
            })
            .await?
            {
                ResponseData::MediaItems { items } => output::print_media_items(&items, format),
                _ => unexpected_response(),
            }
        }
        crate::PlaylistCommand::Play {
            playlist,
            provider,
            format,
        } => {
            let router = ProviderRouter::load(provider).await?;
            let playlist =
                daemon_playlist_and_require(&playlist, &router, ResourceCapability::Playback)
                    .await?;
            ipc_play_resolved_uri(&playlist.id, format).await
        }
        crate::PlaylistCommand::Add {
            playlist,
            uris,
            ids,
            dry_run,
            yes,
            provider,
            format,
        } => ipc_playlist_add(&playlist, uris, ids, dry_run, yes, provider, format).await,
        crate::PlaylistCommand::Remove {
            playlist,
            uris,
            ids,
            dry_run,
            yes,
            provider,
            format,
        } => ipc_playlist_remove(&playlist, uris, ids, dry_run, yes, provider, format).await,
        crate::PlaylistCommand::AddCurrent {
            playlist,
            provider,
            format,
        } => {
            let router = ProviderRouter::load(provider).await?;
            let playlist = daemon_playlist(&playlist, &router).await?;
            let item = match daemon_request(Request::PlaybackGet).await? {
                ResponseData::Playback { playback } => {
                    playback.item.context("nothing is playing")?
                }
                _ => return unexpected_response(),
            };
            let item_uri = router
                .resolve_required(&item.uri, vec![MediaKind::Track, MediaKind::Episode])
                .await?;
            router.require_resolved_capability(&playlist.id, ResourceCapability::PlaylistAdd)?;
            print_mutation(
                daemon_request(Request::PlaylistAddItems {
                    playlist: playlist.id,
                    uris: vec![item_uri],
                    provider: router.request_provider(),
                })
                .await?,
                format,
            )
        }
        crate::PlaylistCommand::Unfollow {
            playlist,
            yes,
            provider,
            format,
        } => ipc_playlist_unfollow(&playlist, yes, provider, format).await,
        crate::PlaylistCommand::SetImage {
            playlist,
            file,
            provider,
            format,
        } => ipc_playlist_set_image(&playlist, &file, provider, format).await,
    }
}

async fn ipc_playlist_set_image(
    playlist: &str,
    file: &Path,
    provider: Option<String>,
    format: OutputFormat,
) -> Result<()> {
    use base64::Engine as _;

    // The current image-upload contract accepts JPEG and caps the base64 body at
    // 256 KB. Reading raw bytes ~ 192 KB roughly produces a 256 KB
    // encoded payload (base64 inflates by 4/3). Reject early so we
    // don't hand the daemon a payload the provider will refuse anyway.
    const MAX_RAW_BYTES: usize = 192 * 1024;
    const MAX_ENCODED_BYTES: usize = 256 * 1024;

    let raw = if file == Path::new("-") {
        let mut buf = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buf)
            .context("failed to read JPEG bytes from stdin")?;
        buf
    } else {
        std::fs::read(file).with_context(|| format!("failed to read {}", file.display()))?
    };
    if raw.is_empty() {
        anyhow::bail!("playlist set-image: input file is empty");
    }
    // Sniff the JPEG SOI marker (FF D8 FF) before
    // we ship a non-JPEG that the daemon would round-trip just to get a
    // 400 back.
    if raw.len() < 3 || raw[0] != 0xff || raw[1] != 0xd8 || raw[2] != 0xff {
        anyhow::bail!(
            "playlist set-image: {} does not start with a JPEG SOI marker (FF D8 FF)",
            file.display()
        );
    }
    if raw.len() > MAX_RAW_BYTES {
        anyhow::bail!(
            "playlist set-image: {} is {} bytes; encoded payload would exceed the provider's 256 KB cap. Re-export at a smaller size.",
            file.display(),
            raw.len()
        );
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(&raw);
    if encoded.len() > MAX_ENCODED_BYTES {
        anyhow::bail!(
            "playlist set-image: encoded image is {} bytes, exceeds the provider's 256 KB cap",
            encoded.len()
        );
    }

    let router = ProviderRouter::load(provider).await?;
    let resolved =
        daemon_playlist_and_require(playlist, &router, ResourceCapability::PlaylistImage).await?;
    print_mutation(
        daemon_request(Request::PlaylistSetImage {
            playlist: resolved.id.clone(),
            image_base64: encoded,
            provider: router.request_provider(),
        })
        .await?,
        format,
    )
}

async fn ipc_playlist_unfollow(
    playlist: &str,
    yes: bool,
    provider: Option<String>,
    format: OutputFormat,
) -> Result<()> {
    let router = ProviderRouter::load(provider).await?;
    let resolved =
        daemon_playlist_and_require(playlist, &router, ResourceCapability::PlaylistUnfollow)
            .await?;
    if !yes {
        confirm_playlist_unfollow(&resolved)?;
    }
    print_mutation(
        daemon_request(Request::PlaylistUnfollow {
            playlist: resolved.id.clone(),
            provider: router.request_provider(),
        })
        .await?,
        format,
    )
}

fn confirm_playlist_unfollow(playlist: &Playlist) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        anyhow::bail!("Confirmation required for `playlist unfollow`. Re-run with --yes.");
    }
    println!(
        "Unfollow `{}` ({})? This removes it from your library and is not reversible.",
        playlist.name, playlist.id
    );
    print!("Continue? [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        return Ok(());
    }
    anyhow::bail!("Aborted")
}

async fn ipc_playlist_create(
    name: &str,
    from: &Path,
    dry_run: bool,
    yes: bool,
    provider: Option<String>,
    format: OutputFormat,
) -> Result<()> {
    let router = ProviderRouter::load(provider).await?;
    router.require("playlist creation", |caps| caps.playlists.create)?;
    crate::agent_playlists::ensure_playlist_create_allowed(dry_run, yes)?;
    let raw = read_input(from)?;
    let candidates = crate::agent_playlists::parse_candidates_jsonl(&raw)?;
    let preview = crate::agent_playlists::build_playlist_preview(name, &candidates);
    let uris = crate::agent_playlists::selected_track_uris(&candidates);
    if uris.is_empty() {
        anyhow::bail!("no resolved track URIs to add");
    }
    let uris = router.resolve_many(uris, vec![MediaKind::Track]).await?;
    if dry_run {
        return match daemon_request(Request::PlaylistCreatePreview {
            name: name.to_string(),
            description: None,
            uris,
            provider: router.request_provider(),
        })
        .await?
        {
            ResponseData::Playlists { playlists } if playlists.is_empty() => {
                output::print_playlist_preview(&preview, format)
            }
            _ => unexpected_response(),
        };
    }
    match daemon_request(Request::PlaylistCreate {
        name: name.to_string(),
        description: None,
        uris,
        provider: router.request_provider(),
    })
    .await?
    {
        ResponseData::PlaylistCreate { receipt } => {
            output::print_playlist_create_receipt(&receipt, format)
        }
        _ => unexpected_response(),
    }
}

pub async fn ipc_library(command: crate::LibraryCommand) -> Result<()> {
    let (request, format) = match command {
        crate::LibraryCommand::Tracks {
            limit,
            provider,
            format,
        } => {
            let router = ProviderRouter::load(provider).await?;
            router.require("library listing", |caps| {
                !caps.library.read_kinds.is_empty()
            })?;
            (
                Request::LibraryList {
                    limit,
                    provider: router.request_provider(),
                },
                format,
            )
        }
        crate::LibraryCommand::SavedTracks {
            limit,
            offset,
            provider,
            format,
        } => {
            let router = ProviderRouter::load(provider).await?;
            router.require("saved-track listing", |caps| {
                caps.library.can_read(&MediaKind::Track)
            })?;
            (
                Request::SavedTracks {
                    limit,
                    offset,
                    provider: router.request_provider(),
                },
                format,
            )
        }
        crate::LibraryCommand::Shows {
            limit,
            provider,
            format,
        } => {
            let router = ProviderRouter::load(provider).await?;
            router.require("saved-show listing", |caps| {
                caps.library.can_read(&MediaKind::Show)
            })?;
            (
                Request::SavedShows {
                    limit,
                    provider: router.request_provider(),
                },
                format,
            )
        }
    };
    match daemon_request(request).await? {
        ResponseData::MediaItems { items } => output::print_media_items(&items, format),
        // Saved tracks now answer with a paged variant carrying `total`; the
        // CLI keeps printing the page's items so its output stays stable.
        ResponseData::SavedTracksPage { items, .. } => output::print_media_items(&items, format),
        _ => unexpected_response(),
    }
}

pub async fn ipc_show(command: crate::ShowCommand) -> Result<()> {
    let crate::ShowCommand::Episodes {
        show,
        limit,
        offset,
        provider,
        format,
    } = command;
    let router = ProviderRouter::load(provider).await?;
    let show = router
        .resolve_and_require(
            &show,
            vec![MediaKind::Show],
            ResourceCapability::ShowEpisodes,
        )
        .await?;
    match daemon_request(Request::ShowEpisodes {
        show,
        limit,
        offset,
    })
    .await?
    {
        ResponseData::MediaItems { items } => output::print_media_items(&items, format),
        _ => unexpected_response(),
    }
}

pub async fn ipc_album(command: crate::AlbumCommand) -> Result<()> {
    let crate::AlbumCommand::Tracks {
        album,
        provider,
        format,
    } = command;
    let router = ProviderRouter::load(provider).await?;
    let album = router
        .resolve_and_require(
            &album,
            vec![MediaKind::Album],
            ResourceCapability::AlbumTracks,
        )
        .await?;
    match daemon_request(Request::AlbumTracks { album }).await? {
        ResponseData::MediaItems { items } => output::print_media_items(&items, format),
        _ => unexpected_response(),
    }
}

/// Listening history grouped into sessions (or flattened to a chronological
/// track list with `--flat`). Merges local and provider-reported plays.
pub async fn ipc_history(limit: u32, flat: bool, format: OutputFormat) -> Result<()> {
    match daemon_request(Request::ListenSessions { limit }).await? {
        ResponseData::ListenSessions { sessions } => {
            if flat {
                let tracks: Vec<_> = sessions.into_iter().flat_map(|s| s.tracks).collect();
                output::print_media_items(&tracks, format)
            } else {
                output::print_listen_sessions(&sessions, format)
            }
        }
        _ => unexpected_response(),
    }
}

/// The cross-show episode feed: a flat, date-ordered list of episodes from all
/// followed podcasts.
pub async fn ipc_episodes(
    limit: u32,
    sort: spotuify_protocol::EpisodeSort,
    refresh: bool,
    provider: Option<String>,
    format: OutputFormat,
) -> Result<()> {
    let router = ProviderRouter::load(provider).await?;
    router.require("episode feed", |caps| {
        caps.catalog.show_episodes && caps.library.can_read(&MediaKind::Show)
    })?;
    match daemon_request(Request::EpisodeFeed {
        limit,
        sort,
        refresh,
        provider: router.request_provider(),
    })
    .await?
    {
        ResponseData::MediaItems { items } => output::print_media_items(&items, format),
        _ => unexpected_response(),
    }
}

/// Report whether a newer spotuify release exists and how to upgrade.
pub async fn ipc_update(force: bool, format: OutputFormat) -> Result<()> {
    match daemon_request(Request::CheckUpdate { force }).await? {
        ResponseData::UpdateStatus {
            update_available,
            current_version,
            latest_version,
            release_url,
            upgrade,
            checked_at_ms,
        } => output::print_update_status(
            update_available,
            &current_version,
            latest_version.as_deref(),
            release_url.as_deref(),
            &upgrade,
            checked_at_ms,
            format,
        ),
        _ => unexpected_response(),
    }
}

pub async fn ipc_artist(command: crate::ArtistCommand) -> Result<()> {
    match command {
        crate::ArtistCommand::Albums {
            artist,
            library_only,
            groups,
            provider,
            format,
        } => {
            let router = ProviderRouter::load(provider).await?;
            let artist = router
                .resolve_and_require(
                    &artist,
                    vec![MediaKind::Artist],
                    ResourceCapability::ArtistAlbums,
                )
                .await?;
            match daemon_request(Request::ArtistAlbums { artist }).await? {
                ResponseData::MediaItems { mut items } => {
                    // The daemon returns the full tagged discography; the toggle
                    // and group filters are applied client-side (no refetch).
                    if library_only {
                        items.retain(|item| item.in_library == Some(true));
                    }
                    if !groups.is_empty() {
                        let allowed: Vec<&str> = groups.iter().map(|g| g.as_api_str()).collect();
                        items.retain(|item| {
                            item.album_group
                                .as_ref()
                                .map(|group| group.as_str())
                                .is_some_and(|group| allowed.contains(&group))
                        });
                    }
                    output::print_discography(&items, format)
                }
                _ => unexpected_response(),
            }
        }
        crate::ArtistCommand::Followed { provider, format } => {
            let router = ProviderRouter::load(provider).await?;
            router.require("followed-artist listing", |caps| {
                caps.library.can_read(&MediaKind::Artist)
            })?;
            match daemon_request(Request::FollowedArtists {
                limit: 500,
                provider: router.request_provider(),
            })
            .await?
            {
                ResponseData::MediaItems { items } => output::print_media_items(&items, format),
                _ => unexpected_response(),
            }
        }
        crate::ArtistCommand::Follow {
            artist,
            provider,
            format,
        } => {
            let router = ProviderRouter::load(provider).await?;
            let artist = router
                .resolve_and_require(
                    &artist,
                    vec![MediaKind::Artist],
                    ResourceCapability::ArtistFollow,
                )
                .await?;
            let data = daemon_request(Request::ArtistFollow { artist }).await?;
            print_mutation(data, format)
        }
        crate::ArtistCommand::Unfollow {
            artist,
            provider,
            format,
        } => {
            let router = ProviderRouter::load(provider).await?;
            let artist = router
                .resolve_and_require(
                    &artist,
                    vec![MediaKind::Artist],
                    ResourceCapability::ArtistFollow,
                )
                .await?;
            let data = daemon_request(Request::ArtistUnfollow { artist }).await?;
            print_mutation(data, format)
        }
        crate::ArtistCommand::Related {
            artist,
            provider,
            format,
        } => {
            let router = ProviderRouter::load(provider).await?;
            let artist = router
                .resolve_and_require(
                    &artist,
                    vec![MediaKind::Artist],
                    ResourceCapability::RelatedArtists,
                )
                .await?;
            match daemon_request(Request::RelatedArtists { artist }).await? {
                ResponseData::MediaItems { items } => output::print_media_items(&items, format),
                _ => unexpected_response(),
            }
        }
    }
}

pub async fn ipc_radio(command: crate::RadioCommand) -> Result<()> {
    match command {
        crate::RadioCommand::Start {
            seed,
            dry_run,
            provider,
            format,
        } => {
            let router = ProviderRouter::load(provider).await?;
            let seed_uri = router
                .resolve_required(
                    &seed,
                    vec![
                        MediaKind::Track,
                        MediaKind::Artist,
                        MediaKind::Album,
                        MediaKind::Playlist,
                    ],
                )
                .await?;
            router.require_radio_start(&seed_uri, dry_run)?;
            let request = Request::RadioStart { seed_uri, dry_run };
            let response = if dry_run {
                daemon_request(request).await?
            } else {
                daemon_request_finalized(request).await?
            };
            match response {
                ResponseData::MediaItems { items } => output::print_media_items(&items, format),
                ResponseData::Mutation { receipt } if !dry_run => {
                    output::print_basic_receipt(&receipt.action, &receipt.message, format)
                }
                _ => unexpected_response(),
            }
        }
    }
}

pub async fn ipc_reminder(command: crate::ReminderCommand) -> Result<()> {
    match command {
        crate::ReminderCommand::Create {
            uri,
            provider,
            at,
            repeat,
            message,
            format,
        } => {
            let router = ProviderRouter::load(provider).await?;
            let uri = router
                .resolve_required(
                    &uri,
                    vec![
                        MediaKind::Track,
                        MediaKind::Episode,
                        MediaKind::Album,
                        MediaKind::Artist,
                        MediaKind::Playlist,
                        MediaKind::Show,
                    ],
                )
                .await?;
            let anchor_at_ms = parse_when(&at)?;
            let recurrence = spotuify_core::Recurrence::parse(&repeat).ok_or_else(|| {
                anyhow::anyhow!("invalid --repeat '{repeat}' (none|daily|weekly|monthly)")
            })?;
            match daemon_request(Request::ReminderCreate {
                media_uri: uri,
                anchor_at_ms,
                recurrence,
                tz: local_timezone_name(),
                message,
            })
            .await?
            {
                ResponseData::ReminderCreated { reminder } => {
                    output::print_reminders(std::slice::from_ref(&reminder), format)
                }
                _ => unexpected_response(),
            }
        }
        crate::ReminderCommand::List { all, format } => {
            match daemon_request(Request::RemindersList {
                include_inactive: all,
            })
            .await?
            {
                ResponseData::Reminders { reminders } => {
                    output::print_reminders(&reminders, format)
                }
                _ => unexpected_response(),
            }
        }
        crate::ReminderCommand::Cancel { id, format } => {
            print_ack_formatted(Request::ReminderCancel { id }, format).await
        }
    }
}

pub async fn ipc_notifications(command: crate::NotificationCommand) -> Result<()> {
    use spotuify_protocol::NotificationAction as NA;
    match command {
        crate::NotificationCommand::List { all, format } => {
            match daemon_request(Request::NotificationsList {
                include_archived: all,
            })
            .await?
            {
                ResponseData::Notifications { notifications } => {
                    output::print_notifications(&notifications, format)
                }
                _ => unexpected_response(),
            }
        }
        crate::NotificationCommand::Play { id, format } => {
            print_ack_formatted(
                Request::NotificationAct {
                    id,
                    action: NA::Play,
                    snooze_until_ms: None,
                },
                format,
            )
            .await
        }
        crate::NotificationCommand::Queue { id, format } => {
            print_ack_formatted(
                Request::NotificationAct {
                    id,
                    action: NA::Queue,
                    snooze_until_ms: None,
                },
                format,
            )
            .await
        }
        crate::NotificationCommand::Dismiss { id, format } => {
            print_ack_formatted(
                Request::NotificationAct {
                    id,
                    action: NA::Dismiss,
                    snooze_until_ms: None,
                },
                format,
            )
            .await
        }
        crate::NotificationCommand::Snooze {
            id,
            snooze_for,
            format,
        } => {
            let dur = snooze_for
                .as_deref()
                .map(parse_duration_ms)
                .transpose()?
                .unwrap_or(3_600_000);
            print_ack_formatted(
                Request::NotificationAct {
                    id,
                    action: NA::Snooze,
                    snooze_until_ms: Some(spotuify_core::now_ms() + dur),
                },
                format,
            )
            .await
        }
    }
}

/// Parse a `--at` value: `+2h`/`+30m`/`+3d`/`+1w`/`+45s`, `now`, `tomorrow`, or
/// an ISO-8601 datetime. Offsets/keywords are relative to local now; the result
/// is an absolute Unix epoch (ms).
fn parse_when(input: &str) -> Result<i64> {
    let s = input.trim();
    let now = chrono::Local::now();
    if let Some(rest) = s.strip_prefix('+') {
        return Ok(now.timestamp_millis() + parse_duration_ms(rest)?);
    }
    match s.to_ascii_lowercase().as_str() {
        "now" => return Ok(now.timestamp_millis()),
        "tomorrow" => return Ok((now + chrono::Duration::days(1)).timestamp_millis()),
        _ => {}
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(dt.timestamp_millis());
    }
    // Date-only ISO ("2026-07-01") — the help has always advertised
    // ISO-8601 but only full datetimes parsed. Interpret as local 09:00
    // (a reminder at midnight is never what anyone meant).
    if let Ok(date) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        use chrono::TimeZone as _;
        let at = date.and_hms_opt(9, 0, 0).unwrap_or_default();
        if let Some(local) = chrono::Local.from_local_datetime(&at).earliest() {
            return Ok(local.timestamp_millis());
        }
    }
    anyhow::bail!("could not parse --at '{input}'; use +2h / +3d / +1w / tomorrow / ISO-8601")
}

/// The machine's IANA timezone name (recurring reminders anchored to
/// hardcoded UTC drifted by an hour across every DST change).
fn local_timezone_name() -> String {
    iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_string())
}

/// Parse a bare duration like `15m`, `1h`, `4h`, `1d`, `1w`, `45s` into ms.
fn parse_duration_ms(input: &str) -> Result<i64> {
    let s = input.trim().trim_start_matches('+');
    if s.len() < 2 {
        anyhow::bail!("bad duration '{input}'");
    }
    let (num, unit) = s.split_at(s.len() - 1);
    let n: i64 = num
        .parse()
        .with_context(|| format!("bad duration number in '{input}'"))?;
    let ms = match unit {
        "s" => n * 1_000,
        "m" => n * 60_000,
        "h" => n * 3_600_000,
        "d" => n * 86_400_000,
        "w" => n * 604_800_000,
        other => anyhow::bail!("unknown duration unit '{other}' (use s/m/h/d/w)"),
    };
    Ok(ms)
}

pub async fn ipc_save_target(
    action: &str,
    target: &str,
    wait: bool,
    provider: Option<String>,
    format: OutputFormat,
) -> Result<()> {
    let router = ProviderRouter::load(provider).await?;
    let current = target.eq_ignore_ascii_case("current");
    let normalized = if current {
        let item = match daemon_request(Request::PlaybackGet).await? {
            ResponseData::Playback { playback } => playback.item.context("nothing is playing")?,
            _ => return unexpected_response(),
        };
        Some(
            router
                .resolve_and_require(
                    &item.uri,
                    vec![MediaKind::Track, MediaKind::Episode],
                    ResourceCapability::LibrarySave,
                )
                .await?,
        )
    } else {
        Some(
            router
                .resolve_and_require(
                    target,
                    vec![
                        MediaKind::Track,
                        MediaKind::Episode,
                        MediaKind::Album,
                        MediaKind::Show,
                        MediaKind::Artist,
                    ],
                    ResourceCapability::LibrarySave,
                )
                .await?,
        )
    };
    let request = Request::LibrarySave {
        uri: normalized,
        // `current` means the item observed for this invocation. Pinning its
        // canonical URI prevents a track/provider change between IPC calls
        // from mutating a different provider than the one just validated.
        current: false,
    };
    let data = if wait {
        daemon_request_finalized(request).await?
    } else {
        daemon_request(request).await?
    };
    match data {
        ResponseData::Mutation { mut receipt } => {
            receipt.action = action.to_string();
            output::print_basic_receipt(&receipt.action, &receipt.message, format)
        }
        _ => unexpected_response(),
    }
}

fn print_mutation(data: ResponseData, format: OutputFormat) -> Result<()> {
    match data {
        ResponseData::Mutation { receipt } => {
            output::print_basic_receipt(&receipt.action, &receipt.message, format)
        }
        _ => unexpected_response(),
    }
}

async fn ipc_queue_add(
    uris: Vec<String>,
    ids: Option<PathBuf>,
    search: Option<String>,
    many: bool,
    wait: bool,
    provider: Option<String>,
    format: OutputFormat,
) -> Result<()> {
    let router = ProviderRouter::load(provider).await?;
    let queue_request = |request: Request| async move {
        if wait {
            daemon_request_finalized(request).await
        } else {
            daemon_request(request).await
        }
    };
    match search {
        Some(query) => {
            router.require_search_kinds(&[MediaKind::Track])?;
            if !uris.is_empty() || ids.is_some() {
                anyhow::bail!("provide URI(s), --ids, or --search, not more than one");
            }
            let items = match daemon_request(Request::Search {
                query: query.clone(),
                scope: SearchScopeData::Track,
                source: router.remote_source()?,
                limit: 50,
                provider: router.request_provider(),
                kinds: None,
                sort: None,
            })
            .await?
            {
                ResponseData::SearchResults { items } => items,
                _ => return unexpected_response(),
            };
            let item = selection::media_item_at_index(items, &query, 1)?;
            router.require_resolved_capability(&item.uri, ResourceCapability::QueueAdd)?;
            queue_request(Request::QueueAdd {
                uri: item.uri.clone(),
            })
            .await?;
            output::print_item_receipt("queue", &item, format)
        }
        None => {
            let mut selection = selection::resolve_uri_selection(
                uris,
                ids.as_deref(),
                "provide a URI or --search QUERY",
            )?;
            selection.uris = router
                .resolve_many(selection.uris, vec![MediaKind::Track, MediaKind::Episode])
                .await?;
            router.require_resolved_capabilities(
                selection.uris.iter().map(String::as_str),
                ResourceCapability::QueueAdd,
            )?;
            if many {
                // One aggregate request + receipt + undo entry.
                return match queue_request(Request::QueueAddMany {
                    uris: selection.uris.clone(),
                })
                .await?
                {
                    ResponseData::Mutation { receipt } => {
                        output::print_basic_receipt(&receipt.action, &receipt.message, format)
                    }
                    _ => unexpected_response(),
                };
            }
            let mut errors = Vec::new();
            let mut succeeded = 0;
            for uri in &selection.uris {
                match queue_request(Request::QueueAdd { uri: uri.clone() }).await {
                    Ok(ResponseData::Mutation { .. }) => succeeded += 1,
                    Ok(_) => errors.push(output::MutationOutputError {
                        uri: uri.clone(),
                        error: "unexpected response from daemon".to_string(),
                    }),
                    Err(err) => errors.push(output::MutationOutputError {
                        uri: uri.clone(),
                        error: err.to_string(),
                    }),
                }
            }
            let failed = errors.len();
            let receipt = output::MutationOutput {
                ok: failed == 0,
                action: "queue".to_string(),
                dry_run: Some(false),
                playlist: None,
                playlist_name: None,
                requested: selection.uris.len(),
                succeeded,
                failed,
                uris: selection.uris,
                errors,
                message: format!("Queued {succeeded} item(s)"),
            };
            output::print_mutation_output(&receipt, format)?;
            if receipt.failed > 0 {
                anyhow::bail!(
                    "partial mutation failure: queued {}, failed {}",
                    receipt.succeeded,
                    receipt.failed
                );
            }
            Ok(())
        }
    }
}

async fn ipc_playlist_add(
    playlist: &str,
    uris: Vec<String>,
    ids: Option<PathBuf>,
    dry_run: bool,
    yes: bool,
    provider: Option<String>,
    format: OutputFormat,
) -> Result<()> {
    let router = ProviderRouter::load(provider).await?;
    let mut selection = selection::resolve_uri_selection(
        uris,
        ids.as_deref(),
        "provide playlist URI(s), --ids FILE, or pipe IDs on stdin",
    )?;
    let playlist = daemon_playlist(playlist, &router).await?;
    selection.uris = router
        .resolve_many(selection.uris, vec![MediaKind::Track, MediaKind::Episode])
        .await?;
    selection::ensure_track_or_episode_uris(&selection.uris)?;
    router.require_resolved_capability(&playlist.id, ResourceCapability::PlaylistAdd)?;

    if dry_run {
        let playlist = match daemon_request(Request::PlaylistItemsPreview {
            playlist: playlist.id.clone(),
            uris: selection.uris.clone(),
            action: PlaylistItemMutationAction::Add,
            provider: router.request_provider(),
        })
        .await?
        {
            ResponseData::Playlists { mut playlists } if playlists.len() == 1 => {
                playlists.remove(0)
            }
            _ => return unexpected_response(),
        };
        return output::print_mutation_output(
            &playlist_add_receipt(&playlist, &selection.uris, true, 0, Vec::new()),
            format,
        );
    }

    if selection.requires_confirmation() && !yes {
        confirm_playlist_add(&playlist, &selection.uris)?;
    }

    match daemon_request(Request::PlaylistAddItems {
        playlist: playlist.id.clone(),
        uris: selection.uris.clone(),
        provider: router.request_provider(),
    })
    .await?
    {
        ResponseData::Mutation { .. } => output::print_mutation_output(
            &playlist_add_receipt(
                &playlist,
                &selection.uris,
                false,
                selection.uris.len(),
                Vec::new(),
            ),
            format,
        ),
        _ => unexpected_response(),
    }
}

async fn ipc_playlist_remove(
    playlist: &str,
    uris: Vec<String>,
    ids: Option<PathBuf>,
    dry_run: bool,
    yes: bool,
    provider: Option<String>,
    format: OutputFormat,
) -> Result<()> {
    let router = ProviderRouter::load(provider).await?;
    let mut selection = selection::resolve_uri_selection(
        uris,
        ids.as_deref(),
        "provide playlist URI(s), --ids FILE, or pipe IDs on stdin",
    )?;
    let playlist = daemon_playlist(playlist, &router).await?;
    selection.uris = router
        .resolve_many(selection.uris, vec![MediaKind::Track, MediaKind::Episode])
        .await?;
    selection::ensure_track_or_episode_uris(&selection.uris)?;
    router.require_resolved_capability(&playlist.id, ResourceCapability::PlaylistRemove)?;

    if dry_run {
        let playlist = match daemon_request(Request::PlaylistItemsPreview {
            playlist: playlist.id.clone(),
            uris: selection.uris.clone(),
            action: PlaylistItemMutationAction::Remove,
            provider: router.request_provider(),
        })
        .await?
        {
            ResponseData::Playlists { mut playlists } if playlists.len() == 1 => {
                playlists.remove(0)
            }
            _ => return unexpected_response(),
        };
        return output::print_mutation_output(
            &playlist_remove_receipt(&playlist, &selection.uris, true, 0, Vec::new()),
            format,
        );
    }

    if selection.requires_confirmation() && !yes {
        confirm_playlist_remove(&playlist, &selection.uris)?;
    }

    match daemon_request(Request::PlaylistRemoveItems {
        playlist: playlist.id.clone(),
        uris: selection.uris.clone(),
        provider: router.request_provider(),
    })
    .await?
    {
        ResponseData::Mutation { .. } => output::print_mutation_output(
            &playlist_remove_receipt(
                &playlist,
                &selection.uris,
                false,
                selection.uris.len(),
                Vec::new(),
            ),
            format,
        ),
        _ => unexpected_response(),
    }
}

async fn daemon_playlist(value: &str, router: &ProviderRouter) -> Result<Playlist> {
    let resolved = router
        .resolve_optional(value, vec![MediaKind::Playlist])
        .await?;
    let value = resolved.as_deref().unwrap_or(value);
    let provider = router.provider_for_resource(value)?;
    let playlists = match daemon_request(Request::PlaylistsList { provider }).await? {
        ResponseData::Playlists { playlists } => playlists,
        _ => return unexpected_response(),
    };
    let mut playlist = selection::resolve_playlist(&playlists, value)?;
    playlist.id = router
        .resolve_required(&playlist.id, vec![MediaKind::Playlist])
        .await?;
    Ok(playlist)
}

async fn daemon_playlist_and_require(
    value: &str,
    router: &ProviderRouter,
    capability: ResourceCapability,
) -> Result<Playlist> {
    let playlist = daemon_playlist(value, router).await?;
    router.require_resolved_capability(&playlist.id, capability)?;
    Ok(playlist)
}

fn playlist_add_receipt(
    playlist: &Playlist,
    uris: &[String],
    dry_run: bool,
    succeeded: usize,
    errors: Vec<output::MutationOutputError>,
) -> output::MutationOutput {
    let failed = errors.len();
    let message = if dry_run {
        format!("Would add {} item(s) to {}", uris.len(), playlist.name)
    } else {
        format!("Added {succeeded} item(s) to {}", playlist.name)
    };
    output::MutationOutput {
        ok: failed == 0,
        action: "playlist-add".to_string(),
        dry_run: Some(dry_run),
        playlist: Some(playlist.id.clone()),
        playlist_name: Some(playlist.name.clone()),
        requested: uris.len(),
        succeeded,
        failed,
        uris: uris.to_vec(),
        errors,
        message,
    }
}

fn playlist_remove_receipt(
    playlist: &Playlist,
    uris: &[String],
    dry_run: bool,
    succeeded: usize,
    errors: Vec<output::MutationOutputError>,
) -> output::MutationOutput {
    let failed = errors.len();
    let message = if dry_run {
        format!("Would remove {} item(s) from {}", uris.len(), playlist.name)
    } else {
        format!("Removed {succeeded} item(s) from {}", playlist.name)
    };
    output::MutationOutput {
        ok: failed == 0,
        action: "playlist-remove".to_string(),
        dry_run: Some(dry_run),
        playlist: Some(playlist.id.clone()),
        playlist_name: Some(playlist.name.clone()),
        requested: uris.len(),
        succeeded,
        failed,
        uris: uris.to_vec(),
        errors,
        message,
    }
}

fn confirm_playlist_add(playlist: &Playlist, uris: &[String]) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        anyhow::bail!(
            "Confirmation required for `playlist add`. Re-run with --yes or inspect with --dry-run."
        );
    }
    println!("Would add {} item(s) to {}", uris.len(), playlist.name);
    for uri in uris.iter().take(8) {
        println!("- {uri}");
    }
    if uris.len() > 8 {
        println!("... and {} more", uris.len() - 8);
    }
    print!("\nContinue? [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        return Ok(());
    }
    anyhow::bail!("Aborted")
}

fn confirm_playlist_remove(playlist: &Playlist, uris: &[String]) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        anyhow::bail!(
            "Confirmation required for `playlist remove`. Re-run with --yes or inspect with --dry-run."
        );
    }
    println!("Would remove {} item(s) from {}", uris.len(), playlist.name);
    for uri in uris.iter().take(8) {
        println!("- {uri}");
    }
    if uris.len() > 8 {
        println!("... and {} more", uris.len() - 8);
    }
    print!("\nContinue? [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        return Ok(());
    }
    anyhow::bail!("Aborted")
}

/// A daemon error with its STRUCTURED kind preserved. The exit-code
/// mapper downcasts to this instead of substring-matching prose that
/// can contain user input (a search for "login to my heart" used to
/// exit with the auth code).
#[derive(Debug)]
pub struct DaemonRequestError {
    pub kind: spotuify_protocol::IpcErrorKind,
    pub message: String,
    pub retryable: bool,
    pub provider: Option<ProviderId>,
    pub detail: Option<String>,
    pub retry_after_secs: Option<u64>,
}

impl DaemonRequestError {
    fn new(kind: spotuify_protocol::IpcErrorKind, message: String) -> Self {
        Self {
            kind,
            message,
            retryable: kind.is_retryable(),
            provider: None,
            detail: None,
            retry_after_secs: None,
        }
    }

    fn from_response(
        kind: spotuify_protocol::IpcErrorKind,
        message: String,
        retryable: bool,
        provider: Option<ProviderId>,
        detail: Option<String>,
    ) -> Self {
        Self {
            kind,
            message,
            retryable,
            provider,
            detail,
            retry_after_secs: None,
        }
    }
}

impl std::fmt::Display for DaemonRequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DaemonRequestError {}

/// Run provider authentication through the daemon-owned session registry.
/// The daemon owns the callback listener, credential persistence, and auth
/// reload; this client only renders and polls serializable session state.
pub async fn ipc_login(provider: Option<String>, method: Option<String>) -> Result<()> {
    ipc_login_with_progress(provider, method, print_auth_session_state).await
}

pub async fn ipc_auth_status(provider: Option<String>) -> Result<AuthStatusData> {
    match daemon_request(Request::AuthStatus {
        provider: parse_provider_id(provider)?,
    })
    .await?
    {
        ResponseData::AuthStatus { status } => Ok(status),
        other => anyhow::bail!("unexpected auth-status response: {other:?}"),
    }
}

pub async fn ipc_logout(provider: Option<String>) -> Result<AuthLogoutData> {
    match daemon_request(Request::AuthLogout {
        provider: parse_provider_id(provider)?,
    })
    .await?
    {
        ResponseData::AuthLogout { result } => Ok(result),
        other => anyhow::bail!("unexpected auth-logout response: {other:?}"),
    }
}

pub async fn ipc_login_with_progress<F>(
    provider: Option<String>,
    method: Option<String>,
    mut on_state: F,
) -> Result<()>
where
    F: FnMut(&AuthSessionData),
{
    let provider = parse_provider_id(provider)?;
    spotuify_launcher::ensure_daemon_running().await?;
    let mut client = IpcClient::connect_with_source(OperationSource::Cli).await?;
    let response = client
        .request(Request::AuthStart { provider, method })
        .await?;
    let mut session = auth_session_from_response(response)?;
    let mut last_state = None;
    let mut interrupt_seen = false;
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);

    loop {
        if last_state.as_ref() != Some(&session.state) {
            on_state(&session);
            last_state = Some(session.state.clone());
        }
        match auth_poll_outcome(&session.state) {
            AuthPollOutcome::Continue => {}
            AuthPollOutcome::Authorized => return Ok(()),
            AuthPollOutcome::Failed(message) => anyhow::bail!(message.to_string()),
            AuthPollOutcome::Cancelled => anyhow::bail!("authentication cancelled"),
        }

        tokio::select! {
            result = &mut interrupt, if !interrupt_seen => {
                result.context("failed to listen for Ctrl-C")?;
                interrupt_seen = true;
                let response = client.request(Request::AuthCancel {
                    session_id: session.session_id,
                }).await?;
                session = auth_session_from_response(response)?;
                match auth_poll_outcome(&session.state) {
                    AuthPollOutcome::Cancelled => anyhow::bail!("authentication cancelled"),
                    AuthPollOutcome::Authorized => return Ok(()),
                    AuthPollOutcome::Failed(message) => anyhow::bail!(message.to_string()),
                    AuthPollOutcome::Continue => {
                        eprintln!("Authentication commit already started; waiting for its result...");
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
        let response = client
            .request(Request::AuthPoll {
                session_id: session.session_id,
            })
            .await?;
        session = auth_session_from_response(response)?;
    }
}

fn parse_provider_id(provider: Option<String>) -> Result<Option<ProviderId>> {
    provider
        .map(ProviderId::new)
        .transpose()
        .map_err(|error| provider_error(spotuify_protocol::IpcErrorKind::InvalidRequest, error))
}

fn auth_session_from_response(response: Response) -> Result<AuthSessionData> {
    match response {
        Response::Ok {
            data: ResponseData::AuthSession { session },
        } => Ok(session),
        Response::Error {
            kind,
            message,
            retryable,
            provider,
            detail,
            ..
        } => Err(anyhow::Error::new(DaemonRequestError::from_response(
            kind, message, retryable, provider, detail,
        ))),
        other => anyhow::bail!("unexpected authentication response: {other:?}"),
    }
}

enum AuthPollOutcome<'a> {
    Continue,
    Authorized,
    Failed(&'a str),
    Cancelled,
}

fn auth_poll_outcome(state: &AuthSessionState) -> AuthPollOutcome<'_> {
    match state {
        AuthSessionState::Starting
        | AuthSessionState::AwaitingUser { .. }
        | AuthSessionState::Waiting { .. } => AuthPollOutcome::Continue,
        AuthSessionState::Authorized => AuthPollOutcome::Authorized,
        AuthSessionState::Failed { message } => AuthPollOutcome::Failed(message),
        AuthSessionState::Cancelled => AuthPollOutcome::Cancelled,
    }
}

fn print_auth_session_state(session: &AuthSessionData) {
    match &session.state {
        AuthSessionState::AwaitingUser {
            authorization_url,
            redirect_uri,
            browser_error,
        }
        | AuthSessionState::Waiting {
            authorization_url,
            redirect_uri,
            browser_error,
        } => {
            if let Some(error) = browser_error {
                eprintln!(
                    "Could not launch a browser automatically ({error}).\nOpen this URL in any browser:\n  {authorization_url}\n(Waiting for the OAuth callback on {redirect_uri})"
                );
            } else {
                eprintln!("Opening provider authorization in your browser...");
                eprintln!("If it does not open, visit:\n{authorization_url}\n");
            }
        }
        AuthSessionState::Authorized => {
            if session.method == "none" {
                eprintln!("{} requires no authentication.", session.provider);
            } else {
                eprintln!("{} auth saved in the local auth file.", session.provider);
            }
        }
        AuthSessionState::Starting
        | AuthSessionState::Failed { .. }
        | AuthSessionState::Cancelled => {}
    }
}

#[cfg(test)]
type TestDaemonHandler =
    std::sync::Arc<dyn Fn(Request) -> Result<ResponseData> + Send + Sync + 'static>;

#[cfg(test)]
static TEST_DAEMON_HANDLER: std::sync::Mutex<Option<TestDaemonHandler>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
fn test_daemon_response(request: &Request) -> Option<Result<ResponseData>> {
    let handler = TEST_DAEMON_HANDLER
        .lock()
        .expect("test daemon handler lock")
        .clone()?;
    Some(handler(request.clone()))
}

async fn daemon_request(request: Request) -> Result<ResponseData> {
    #[cfg(test)]
    if let Some(response) = test_daemon_response(&request) {
        return response;
    }
    spotuify_launcher::ensure_daemon_running().await?;
    let mut client = IpcClient::connect_with_source(OperationSource::Cli).await?;
    let mutation_id = mutation_id_for_request(&request);
    let response = client
        .request_with_mutation_id(request.clone(), mutation_id)
        .await?;
    match response {
        Response::Ok { data } => Ok(data),
        Response::Error {
            kind: spotuify_protocol::IpcErrorKind::AuthRevoked,
            message,
            retryable,
            provider,
            detail,
            ..
        } if request.requires_mutation_id() => {
            Err(anyhow::Error::new(DaemonRequestError::from_response(
                spotuify_protocol::IpcErrorKind::AuthRevoked,
                terminal_auth_revoked_message(&message, provider.as_ref()),
                retryable,
                provider,
                detail,
            )))
        }
        Response::Error {
            kind: spotuify_protocol::IpcErrorKind::AuthRevoked,
            message,
            retryable,
            provider,
            detail,
            ..
        } => {
            handle_auth_revoked_then_retry(
                request,
                mutation_id,
                provider,
                retryable,
                detail,
                &message,
            )
            .await
        }
        Response::Error {
            kind,
            message,
            retryable,
            provider,
            detail,
            ..
        } => Err(anyhow::Error::new(DaemonRequestError::from_response(
            kind, message, retryable, provider, detail,
        ))),
    }
}

fn mutation_id_for_request(request: &Request) -> Option<spotuify_protocol::MutationId> {
    request
        .requires_mutation_id()
        .then(spotuify_protocol::MutationId::new_v7)
}

/// Interactive recovery for auth-revoked reads. Prompts on stdin; on
/// consent, polls the same daemon-owned flow as `spotuify login`, then
/// retries the original request exactly once.
///
/// Non-TTY callers (scripts, pipes) skip the prompt and exit with
/// the actionable error message — they have no way to answer "Y".
async fn handle_auth_revoked_then_retry(
    request: Request,
    mutation_id: Option<spotuify_protocol::MutationId>,
    provider: Option<ProviderId>,
    retryable: bool,
    detail: Option<String>,
    original_message: &str,
) -> Result<ResponseData> {
    use std::io::{BufRead, IsTerminal, Write};

    let recovery_command = auth_recovery_command(provider.as_ref());
    eprintln!("Provider session expired ({original_message}).");

    if !std::io::stdin().is_terminal() {
        return Err(anyhow::Error::new(DaemonRequestError::from_response(
            spotuify_protocol::IpcErrorKind::AuthRevoked,
            format!(
                "Provider session expired and stdin is not a TTY; run `{recovery_command}` to recover"
            ),
            retryable,
            provider,
            detail,
        )));
    }

    eprint!("Re-authenticate now? [Y/n] ");
    std::io::stderr().flush().ok();
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .context("failed to read stdin")?;
    let answer = answer.trim();
    let consent = answer.is_empty() || matches!(answer, "y" | "Y" | "yes" | "Yes" | "YES");
    if !consent {
        return Err(anyhow::Error::new(DaemonRequestError::from_response(
            spotuify_protocol::IpcErrorKind::AuthRevoked,
            format!("Aborted. Run `{recovery_command}` when you're ready to re-authenticate."),
            retryable,
            provider,
            detail,
        )));
    }

    eprintln!("Re-authenticating…");
    ipc_login(auth_recovery_provider(provider), None)
        .await
        .context("OAuth flow failed")?;

    eprintln!("Retrying original command…");
    let mut retry_client = IpcClient::connect_with_source(OperationSource::Cli).await?;
    match retry_client
        .request_with_mutation_id(request, mutation_id)
        .await?
    {
        Response::Ok { data } => Ok(data),
        Response::Error {
            kind,
            message,
            retryable,
            provider,
            detail,
            ..
        } => Err(anyhow::Error::new(DaemonRequestError::from_response(
            kind, message, retryable, provider, detail,
        ))),
    }
}

fn auth_recovery_command(provider: Option<&ProviderId>) -> String {
    provider.map_or_else(
        || "spotuify login".to_string(),
        |provider| format!("spotuify login --provider {provider}"),
    )
}

fn terminal_auth_revoked_message(message: &str, provider: Option<&ProviderId>) -> String {
    let recovery_command = auth_recovery_command(provider);
    format!(
        "{message}. Mutation outcome is terminal under its mutation id; inspect remote state before issuing a new mutation after running `{recovery_command}`"
    )
}

fn auth_recovery_provider(provider: Option<ProviderId>) -> Option<String> {
    provider.map(|provider| provider.to_string())
}

/// Phase 13 (P13-I) — reload the daemon's view of the config file
/// without a restart. Player backend swaps still require a restart;
/// the daemon returns a clear Ack with the message.
pub async fn ipc_reload() -> Result<()> {
    match daemon_request(Request::Reload).await? {
        ResponseData::Ack { message } => {
            println!("{message}");
            Ok(())
        }
        _ => unexpected_response(),
    }
}

/// Phase 13 (P13-I) — request the daemon re-register its active player
/// backend (useful after a VPN flap).
pub async fn ipc_reconnect() -> Result<()> {
    match daemon_request(Request::Reconnect).await? {
        ResponseData::Ack { message } => {
            println!("{message}");
            Ok(())
        }
        _ => unexpected_response(),
    }
}

fn unexpected_response<T>() -> Result<T> {
    anyhow::bail!("unexpected response from daemon")
}

fn parse_lyrics_offset(value: &str) -> Result<i64> {
    let raw = value.trim().strip_suffix("ms").unwrap_or(value.trim());
    raw.parse::<i64>()
        .with_context(|| format!("expected offset like +50ms or -200ms, got `{value}`"))
}

fn read_input(path: &Path) -> Result<String> {
    if path == Path::new("-") {
        let mut raw = String::new();
        std::io::stdin().read_to_string(&mut raw)?;
        return Ok(raw);
    }
    std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic, clippy::unwrap_used)]

    use super::*;
    use spotuify_core::{LyricLine, LyricsProvider, MediaKind, ProviderExtrasCaps, TransportCaps};

    static TEST_DAEMON_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct TestDaemonGuard;

    impl Drop for TestDaemonGuard {
        fn drop(&mut self) {
            *TEST_DAEMON_HANDLER
                .lock()
                .expect("test daemon handler lock") = None;
        }
    }

    fn install_test_daemon(
        handler: impl Fn(Request) -> Result<ResponseData> + Send + Sync + 'static,
    ) -> TestDaemonGuard {
        *TEST_DAEMON_HANDLER
            .lock()
            .expect("test daemon handler lock") = Some(std::sync::Arc::new(handler));
        TestDaemonGuard
    }

    fn routed_catalog(default_caps: ProviderCaps, owner_caps: ProviderCaps) -> ProviderCatalog {
        ProviderCatalog {
            default_provider: Some(ProviderId::new("default").unwrap()),
            providers: vec![
                ProviderDescriptor {
                    id: ProviderId::new("default").unwrap(),
                    uri_scheme: spotuify_core::UriScheme::new("default").unwrap(),
                    display_name: "Default".to_string(),
                    capabilities: default_caps,
                    is_default: true,
                },
                ProviderDescriptor {
                    id: ProviderId::new("owner").unwrap(),
                    uri_scheme: spotuify_core::UriScheme::new("owner").unwrap(),
                    display_name: "Owner".to_string(),
                    capabilities: owner_caps,
                    is_default: false,
                },
            ],
        }
    }

    fn successful_mutation(action: &str) -> ResponseData {
        ResponseData::Mutation {
            receipt: spotuify_protocol::CommandReceipt {
                ok: true,
                action: action.to_string(),
                message: "ok".to_string(),
                receipt_id: None,
                mutation_id: None,
                status: Some(spotuify_protocol::ReceiptStatus::Confirmed),
                error: None,
                replayed: false,
            },
        }
    }

    fn router_with_caps(capabilities: ProviderCaps) -> ProviderRouter {
        let provider = ProviderId::new("music").unwrap();
        ProviderRouter {
            catalog: Some(ProviderCatalog {
                default_provider: Some(provider.clone()),
                providers: vec![ProviderDescriptor {
                    id: provider.clone(),
                    uri_scheme: spotuify_core::UriScheme::new("music").unwrap(),
                    display_name: "Music".to_string(),
                    capabilities,
                    is_default: true,
                }],
            }),
            selected: Some(provider),
        }
    }

    #[test]
    fn auth_recovery_preserves_the_typed_error_provider() {
        let provider = ProviderId::new("nondefault").unwrap();
        assert_eq!(
            auth_recovery_command(Some(&provider)),
            "spotuify login --provider nondefault"
        );
        assert_eq!(
            auth_recovery_provider(Some(provider.clone())),
            Some("nondefault".to_string())
        );
        assert_eq!(auth_recovery_command(None), "spotuify login");
        assert_eq!(auth_recovery_provider(None), None);
        let guidance = terminal_auth_revoked_message("authorization revoked", Some(&provider));
        assert!(guidance.contains("terminal under its mutation id"));
        assert!(guidance.contains("spotuify login --provider nondefault"));
    }

    #[test]
    fn structured_search_ack_error_preserves_typed_metadata_and_auth_guidance() {
        let provider = ProviderId::new("nondefault").unwrap();
        let error = structured_daemon_error(
            spotuify_protocol::IpcErrorKind::AuthRevoked,
            "authorization revoked".to_string(),
            false,
            Some(provider.clone()),
            Some("refresh token rejected".to_string()),
            Some(19),
            Some("search-page request failed"),
            false,
        );
        let structured = error.downcast_ref::<DaemonRequestError>().unwrap();

        assert_eq!(
            structured.kind,
            spotuify_protocol::IpcErrorKind::AuthRevoked
        );
        assert!(!structured.retryable);
        assert_eq!(structured.provider.as_ref(), Some(&provider));
        assert_eq!(structured.detail.as_deref(), Some("refresh token rejected"));
        assert_eq!(structured.retry_after_secs, Some(19));
        assert!(structured.message.contains("search-page request failed"));
        assert!(structured
            .message
            .contains("spotuify login --provider nondefault"));
        assert!(!structured
            .message
            .contains("terminal under its mutation id"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn play_uri_uses_the_resolved_owner_capability_not_the_default() {
        let _serial = TEST_DAEMON_SERIAL.lock().await;
        let catalog = routed_catalog(
            ProviderCaps::default(),
            ProviderCaps {
                transport: Some(TransportCaps {
                    play: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let _daemon = install_test_daemon(move |request| match request {
            Request::ProvidersList => Ok(ResponseData::ProviderList {
                default_provider: catalog.default_provider.clone(),
                providers: catalog.providers.clone(),
            }),
            Request::PlaybackCommand {
                command: PlaybackCommand::PlayUri { uri, .. },
            } if uri == "owner:track:one" => Ok(successful_mutation("play")),
            request => panic!("unexpected test daemon request: {request:?}"),
        });

        ipc_play_uri("owner:track:one", None, OutputFormat::Json)
            .await
            .expect("the canonical URI owner supports playback");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn play_uri_reports_provider_conflict_before_capability_denial() {
        let _serial = TEST_DAEMON_SERIAL.lock().await;
        let catalog = routed_catalog(ProviderCaps::default(), ProviderCaps::default());
        let _daemon = install_test_daemon(move |request| match request {
            Request::ProvidersList => Ok(ResponseData::ProviderList {
                default_provider: catalog.default_provider.clone(),
                providers: catalog.providers.clone(),
            }),
            request => panic!("unexpected test daemon request: {request:?}"),
        });

        let error = ipc_play_uri(
            "owner:track:one",
            Some("default".to_string()),
            OutputFormat::Json,
        )
        .await
        .expect_err("explicit provider conflicts must fail before capability checks");
        let error = error
            .downcast_ref::<DaemonRequestError>()
            .expect("structured CLI error");
        assert_eq!(error.kind, spotuify_protocol::IpcErrorKind::InvalidRequest);
        assert!(error.message.contains("conflicts with URI scheme `owner`"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn authoritative_empty_catalog_rejects_default_scoped_commands() {
        let _serial = TEST_DAEMON_SERIAL.lock().await;
        let _daemon = install_test_daemon(|request| match request {
            Request::ProvidersList => Ok(ResponseData::ProviderList {
                default_provider: None,
                providers: Vec::new(),
            }),
            request => panic!("authoritative empty catalog must stop before {request:?}"),
        });

        for error in [
            ipc_playlists(None, OutputFormat::Json)
                .await
                .expect_err("playlist listing requires a configured default provider"),
            ipc_sync(SyncTargetData::Library, None, OutputFormat::Json)
                .await
                .expect_err("provider sync requires a configured default provider"),
        ] {
            let error = error
                .downcast_ref::<DaemonRequestError>()
                .expect("structured CLI error");
            assert_eq!(error.kind, spotuify_protocol::IpcErrorKind::Unsupported);
            assert!(error.message.contains("no default provider is configured"));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn play_uri_keeps_legacy_bare_ids_when_provider_catalog_is_unavailable() {
        let _serial = TEST_DAEMON_SERIAL.lock().await;
        let _daemon = install_test_daemon(|request| match request {
            Request::ProvidersList | Request::ResolveTarget { .. } => Err(provider_error(
                spotuify_protocol::IpcErrorKind::Unsupported,
                "legacy daemon does not support provider discovery",
            )),
            Request::PlaybackCommand {
                command: PlaybackCommand::PlayUri { uri, .. },
            } if uri == "legacy-track-id" => Ok(successful_mutation("play")),
            request => panic!("unexpected test daemon request: {request:?}"),
        });

        ipc_play_uri("legacy-track-id", None, OutputFormat::Json)
            .await
            .expect("provider discovery must remain additive for legacy daemons");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn play_query_gates_the_selected_search_result_owner() {
        let _serial = TEST_DAEMON_SERIAL.lock().await;
        let catalog = routed_catalog(
            ProviderCaps {
                search: spotuify_core::SearchCaps {
                    remote: true,
                    kinds: vec![MediaKind::Track],
                    ..Default::default()
                },
                ..Default::default()
            },
            ProviderCaps {
                transport: Some(TransportCaps {
                    play: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let _daemon = install_test_daemon(move |request| match request {
            Request::ProvidersList => Ok(ResponseData::ProviderList {
                default_provider: catalog.default_provider.clone(),
                providers: catalog.providers.clone(),
            }),
            Request::ResolveTarget {
                input,
                provider: None,
                ..
            } if input == "owner song" => Ok(ResponseData::TargetResolved { target: None }),
            Request::Search { query, .. } if query == "owner song" => {
                Ok(ResponseData::SearchResults {
                    items: vec![MediaItem {
                        uri: "owner:track:one".to_string(),
                        name: "Owner Song".to_string(),
                        kind: MediaKind::Track,
                        ..Default::default()
                    }],
                })
            }
            Request::PlaybackCommand {
                command: PlaybackCommand::PlayUri { uri, .. },
            } if uri == "owner:track:one" => Ok(successful_mutation("play")),
            request => panic!("unexpected test daemon request: {request:?}"),
        });

        ipc_play_query(
            "owner song",
            SearchScopeData::Track,
            None,
            OutputFormat::Json,
        )
        .await
        .expect("playback capability follows the search result owner");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn playlist_tracks_uses_the_canonical_playlist_owner_capability() {
        let _serial = TEST_DAEMON_SERIAL.lock().await;
        let catalog = routed_catalog(
            ProviderCaps::default(),
            ProviderCaps {
                playlists: spotuify_core::PlaylistCaps {
                    list: true,
                    item_read: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let _daemon = install_test_daemon(move |request| match request {
            Request::ProvidersList => Ok(ResponseData::ProviderList {
                default_provider: catalog.default_provider.clone(),
                providers: catalog.providers.clone(),
            }),
            Request::PlaylistsList {
                provider: Some(provider),
            } if provider.as_str() == "owner" => Ok(ResponseData::Playlists {
                playlists: vec![Playlist {
                    id: "owner:playlist:focus".to_string(),
                    name: "Focus".to_string(),
                    owner: "Owner".to_string(),
                    tracks_total: 0,
                    image_url: None,
                    version_token: None,
                }],
            }),
            Request::PlaylistTracks {
                playlist,
                provider: None,
                wait: true,
            } if playlist == "owner:playlist:focus" => {
                Ok(ResponseData::MediaItems { items: Vec::new() })
            }
            request => panic!("unexpected test daemon request: {request:?}"),
        });

        ipc_playlist(crate::PlaylistCommand::Tracks {
            playlist: "owner:playlist:focus".to_string(),
            provider: None,
            format: OutputFormat::Json,
        })
        .await
        .expect("playlist reads must use the canonical owner capability");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn playlist_bare_names_preserve_explicit_and_default_resolution_scope() {
        let _serial = TEST_DAEMON_SERIAL.lock().await;
        for selected in [Some("owner".to_string()), None] {
            let catalog = routed_catalog(
                ProviderCaps::default(),
                ProviderCaps {
                    playlists: spotuify_core::PlaylistCaps {
                        list: true,
                        item_read: true,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            let expected_for_daemon = selected
                .as_deref()
                .map(|provider| ProviderId::new(provider).unwrap());
            let _daemon = install_test_daemon(move |request| match request {
                Request::ProvidersList => Ok(ResponseData::ProviderList {
                    default_provider: catalog.default_provider.clone(),
                    providers: catalog.providers.clone(),
                }),
                Request::ResolveTarget {
                    input, provider, ..
                } if input == "Focus" => {
                    assert_eq!(provider, expected_for_daemon);
                    Ok(ResponseData::TargetResolved { target: None })
                }
                Request::PlaylistsList { provider } => {
                    assert_eq!(provider, expected_for_daemon);
                    Ok(ResponseData::Playlists {
                        playlists: vec![Playlist {
                            id: "owner:playlist:focus".to_string(),
                            name: "Focus".to_string(),
                            owner: "Owner".to_string(),
                            tracks_total: 0,
                            image_url: None,
                            version_token: None,
                        }],
                    })
                }
                Request::PlaylistTracks {
                    playlist,
                    provider,
                    wait: true,
                } if playlist == "owner:playlist:focus" => {
                    assert_eq!(provider, expected_for_daemon);
                    Ok(ResponseData::MediaItems { items: Vec::new() })
                }
                request => panic!("unexpected test daemon request: {request:?}"),
            });

            ipc_playlist(crate::PlaylistCommand::Tracks {
                playlist: "Focus".to_string(),
                provider: selected,
                format: OutputFormat::Json,
            })
            .await
            .expect("bare playlist names retain their resolution scope");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn playlist_dry_runs_use_only_read_only_daemon_preview_requests() {
        let _serial = TEST_DAEMON_SERIAL.lock().await;
        let catalog = routed_catalog(
            ProviderCaps::default(),
            ProviderCaps {
                playlists: spotuify_core::PlaylistCaps {
                    list: true,
                    item_read: true,
                    create: true,
                    add: true,
                    remove: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed_for_daemon = observed.clone();
        let _daemon = install_test_daemon(move |request| match request {
            Request::ProvidersList => Ok(ResponseData::ProviderList {
                default_provider: catalog.default_provider.clone(),
                providers: catalog.providers.clone(),
            }),
            Request::PlaylistsList {
                provider: Some(provider),
            } if provider.as_str() == "owner" => Ok(ResponseData::Playlists {
                playlists: vec![Playlist {
                    id: "owner:playlist:focus".to_string(),
                    name: "Focus".to_string(),
                    owner: "Owner".to_string(),
                    tracks_total: 1,
                    image_url: None,
                    version_token: Some("v1".to_string()),
                }],
            }),
            Request::PlaylistCreatePreview {
                name,
                description: None,
                uris,
                provider: Some(provider),
            } if name == "Focus"
                && uris.len() == 1
                && uris[0] == "owner:track:one"
                && provider.as_str() == "owner" =>
            {
                observed_for_daemon.lock().unwrap().push("create-preview");
                Ok(ResponseData::Playlists {
                    playlists: Vec::new(),
                })
            }
            Request::PlaylistItemsPreview {
                playlist,
                uris,
                action,
                provider: None,
            } if playlist == "owner:playlist:focus"
                && uris.len() == 1
                && uris[0] == "owner:track:one" =>
            {
                observed_for_daemon.lock().unwrap().push(match action {
                    PlaylistItemMutationAction::Add => "add-preview",
                    PlaylistItemMutationAction::Remove => "remove-preview",
                });
                Ok(ResponseData::Playlists {
                    playlists: vec![Playlist {
                        id: playlist,
                        name: "Focus".to_string(),
                        owner: "Owner".to_string(),
                        tracks_total: 1,
                        image_url: None,
                        version_token: Some("v1".to_string()),
                    }],
                })
            }
            request => panic!("dry-run issued unexpected daemon request: {request:?}"),
        });

        let path = std::env::temp_dir().join(format!(
            "spotuify-cli-playlist-preview-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(
            &path,
            concat!(
                r#"{"position":1,"query":"one","status":"resolved","chosen_uri":"owner:track:one","confidence":1.0,"reason":"test","alternatives":[],"source":"test"}"#,
                "\n"
            ),
        )
        .unwrap();
        let create = ipc_playlist_create(
            "Focus",
            &path,
            true,
            false,
            Some("owner".to_string()),
            OutputFormat::Json,
        )
        .await;
        let _ = std::fs::remove_file(&path);
        create.expect("playlist create dry-run must use the daemon preview command");

        ipc_playlist_add(
            "owner:playlist:focus",
            vec!["owner:track:one".to_string()],
            None,
            true,
            false,
            None,
            OutputFormat::Json,
        )
        .await
        .expect("playlist add dry-run must use the daemon preview command");
        ipc_playlist_remove(
            "owner:playlist:focus",
            vec!["owner:track:one".to_string()],
            None,
            true,
            false,
            None,
            OutputFormat::Json,
        )
        .await
        .expect("playlist remove dry-run must use the daemon preview command");

        assert_eq!(
            *observed.lock().unwrap(),
            ["create-preview", "add-preview", "remove-preview"]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn save_current_pins_the_playing_items_owner_and_capability() {
        let _serial = TEST_DAEMON_SERIAL.lock().await;
        let catalog = routed_catalog(
            ProviderCaps::default(),
            ProviderCaps {
                library: spotuify_core::LibraryCaps {
                    save_kinds: vec![MediaKind::Track],
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let _daemon = install_test_daemon(move |request| match request {
            Request::ProvidersList => Ok(ResponseData::ProviderList {
                default_provider: catalog.default_provider.clone(),
                providers: catalog.providers.clone(),
            }),
            Request::PlaybackGet => Ok(ResponseData::Playback {
                playback: Playback {
                    item: Some(MediaItem {
                        uri: "owner:track:one".to_string(),
                        name: "Owner Song".to_string(),
                        kind: MediaKind::Track,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            }),
            Request::LibrarySave {
                uri: Some(uri),
                current: false,
            } if uri == "owner:track:one" => Ok(successful_mutation("save")),
            request => panic!("unexpected test daemon request: {request:?}"),
        });

        ipc_save_target("like", "current", false, None, OutputFormat::Json)
            .await
            .expect("save current must gate the playing item's owner");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn save_current_rejects_explicit_provider_conflict_before_mutation() {
        let _serial = TEST_DAEMON_SERIAL.lock().await;
        let catalog = routed_catalog(ProviderCaps::default(), ProviderCaps::default());
        let _daemon = install_test_daemon(move |request| match request {
            Request::ProvidersList => Ok(ResponseData::ProviderList {
                default_provider: catalog.default_provider.clone(),
                providers: catalog.providers.clone(),
            }),
            Request::PlaybackGet => Ok(ResponseData::Playback {
                playback: Playback {
                    item: Some(MediaItem {
                        uri: "owner:track:one".to_string(),
                        name: "Owner Song".to_string(),
                        kind: MediaKind::Track,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            }),
            request => panic!("provider conflict must stop before mutation: {request:?}"),
        });

        let error = ipc_save_target(
            "like",
            "current",
            false,
            Some("default".to_string()),
            OutputFormat::Json,
        )
        .await
        .expect_err("the selected provider must own the current item");
        let error = error
            .downcast_ref::<DaemonRequestError>()
            .expect("structured CLI error");
        assert_eq!(error.kind, spotuify_protocol::IpcErrorKind::InvalidRequest);
        assert!(error.message.contains("conflicts with URI scheme `owner`"));
    }

    #[test]
    fn client_attaches_ids_only_to_committed_ops_mutations() {
        let operation_id = spotuify_protocol::OperationId::new_v7();
        assert!(mutation_id_for_request(&Request::OpsUndo {
            operation_id: Some(operation_id),
            dry_run: true,
            force: false,
            bulk_since_ms: None,
        })
        .is_none());
        assert!(mutation_id_for_request(&Request::OpsUndo {
            operation_id: Some(operation_id),
            dry_run: false,
            force: false,
            bulk_since_ms: None,
        })
        .is_some());
        assert!(mutation_id_for_request(&Request::OpsRedo {
            operation_id: Some(operation_id),
        })
        .is_some());
    }

    #[test]
    fn auth_poll_outcome_tracks_terminal_states() {
        assert!(matches!(
            auth_poll_outcome(&AuthSessionState::Starting),
            AuthPollOutcome::Continue
        ));
        assert!(matches!(
            auth_poll_outcome(&AuthSessionState::Authorized),
            AuthPollOutcome::Authorized
        ));
        assert!(matches!(
            auth_poll_outcome(&AuthSessionState::Failed {
                message: "denied".to_string(),
            }),
            AuthPollOutcome::Failed("denied")
        ));
        assert!(matches!(
            auth_poll_outcome(&AuthSessionState::Cancelled),
            AuthPollOutcome::Cancelled
        ));
    }

    #[test]
    fn remote_search_uses_catalog_default_provider() {
        let provider = ProviderId::new("music").unwrap();
        let router = ProviderRouter {
            catalog: Some(ProviderCatalog {
                default_provider: Some(provider.clone()),
                providers: vec![ProviderDescriptor {
                    id: provider.clone(),
                    uri_scheme: spotuify_core::UriScheme::new("music").unwrap(),
                    display_name: "Music".to_string(),
                    capabilities: ProviderCaps {
                        search: spotuify_core::SearchCaps {
                            remote: true,
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    is_default: true,
                }],
            }),
            selected: None,
        };

        assert_eq!(
            router.remote_source().unwrap(),
            SearchSourceData::Remote(provider)
        );
        assert_eq!(router.request_provider(), None);
    }

    #[test]
    fn capability_gate_returns_structured_unsupported_error() {
        let provider = ProviderId::new("readonly").unwrap();
        let router = ProviderRouter {
            catalog: Some(ProviderCatalog {
                default_provider: Some(provider.clone()),
                providers: vec![ProviderDescriptor {
                    id: provider.clone(),
                    uri_scheme: spotuify_core::UriScheme::new("readonly").unwrap(),
                    display_name: "Read Only".to_string(),
                    capabilities: ProviderCaps::default(),
                    is_default: true,
                }],
            }),
            selected: Some(provider),
        };

        let error = router
            .require("playlist creation", |caps| caps.playlists.create)
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<DaemonRequestError>().unwrap().kind,
            spotuify_protocol::IpcErrorKind::Unsupported
        );
    }

    #[test]
    fn all_search_accepts_any_advertised_kind_but_exact_kinds_fail_closed() {
        let router = router_with_caps(ProviderCaps {
            search: spotuify_core::SearchCaps {
                remote: true,
                kinds: vec![MediaKind::Track],
                ..Default::default()
            },
            ..Default::default()
        });

        assert!(router.require_search_scope(&SearchScopeData::All).is_ok());
        assert!(router.require_search_scope(&SearchScopeData::Track).is_ok());
        assert!(router
            .require_search_scope(&SearchScopeData::Episode)
            .is_err());
        assert!(router_with_caps(ProviderCaps {
            search: spotuify_core::SearchCaps {
                remote: true,
                kinds: Vec::new(),
                ..Default::default()
            },
            ..Default::default()
        })
        .require_search_scope(&SearchScopeData::All)
        .is_err());
    }

    #[test]
    fn related_artists_requires_its_semantic_capability() {
        assert!(router_with_caps(ProviderCaps::default())
            .require_resource("music:artist:one", "related artists", |caps| {
                caps.extras.related_artists
            })
            .is_err());
        assert!(router_with_caps(ProviderCaps {
            extras: ProviderExtrasCaps {
                related_artists: true,
                ..Default::default()
            },
            ..Default::default()
        })
        .require_resource("music:artist:one", "related artists", |caps| {
            caps.extras.related_artists
        })
        .is_ok());
    }

    #[test]
    fn transportless_radio_allows_preview_but_rejects_live_start() {
        let router = router_with_caps(ProviderCaps {
            extras: ProviderExtrasCaps {
                radio: true,
                ..Default::default()
            },
            ..Default::default()
        });
        assert!(router.require_radio_start("music:track:one", true).is_ok());
        let error = router
            .require_radio_start("music:track:one", false)
            .unwrap_err();
        assert!(error.to_string().contains("radio queue additions"));
    }

    #[test]
    fn live_radio_and_playlist_play_require_and_accept_transport_capabilities() {
        let extras_only = router_with_caps(ProviderCaps {
            extras: ProviderExtrasCaps {
                radio: true,
                ..Default::default()
            },
            ..Default::default()
        });
        assert!(extras_only
            .require_resource("music:playlist:focus", "playlist playback", |caps| {
                caps.transport
                    .as_ref()
                    .is_some_and(|transport| transport.play)
            })
            .is_err());

        let queue_only = router_with_caps(ProviderCaps {
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
        assert!(queue_only
            .require_radio_start("music:track:one", false)
            .is_ok());

        let playable = router_with_caps(ProviderCaps {
            transport: Some(TransportCaps {
                play: true,
                queue_add: true,
                ..Default::default()
            }),
            extras: ProviderExtrasCaps {
                radio: true,
                ..Default::default()
            },
            ..Default::default()
        });
        assert!(playable
            .require_resource("music:playlist:focus", "playlist playback", |caps| {
                caps.transport
                    .as_ref()
                    .is_some_and(|transport| transport.play)
            })
            .is_ok());
        assert!(playable
            .require_radio_start("music:track:one", false)
            .is_ok());
    }

    #[test]
    fn resource_capability_gates_follow_uri_owner_and_retain_playlist_scope() {
        let default_provider = ProviderId::new("default").unwrap();
        let routed_provider = ProviderId::new("routed").unwrap();
        let routed_caps = ProviderCaps {
            extras: ProviderExtrasCaps {
                related_artists: true,
                ..Default::default()
            },
            transport: Some(TransportCaps {
                play: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let catalog = ProviderCatalog {
            default_provider: Some(default_provider.clone()),
            providers: vec![
                ProviderDescriptor {
                    id: default_provider.clone(),
                    uri_scheme: spotuify_core::UriScheme::new("default").unwrap(),
                    display_name: "Default".to_string(),
                    capabilities: ProviderCaps::default(),
                    is_default: true,
                },
                ProviderDescriptor {
                    id: routed_provider.clone(),
                    uri_scheme: spotuify_core::UriScheme::new("routed").unwrap(),
                    display_name: "Routed".to_string(),
                    capabilities: routed_caps,
                    is_default: false,
                },
            ],
        };
        let router = ProviderRouter {
            catalog: Some(catalog.clone()),
            selected: None,
        };

        assert!(router
            .require_resource("routed:artist:one", "related artists", |caps| {
                caps.extras.related_artists
            })
            .is_ok());
        assert!(router
            .require_resource("default:artist:one", "related artists", |caps| {
                caps.extras.related_artists
            })
            .is_err());
        assert_eq!(
            router
                .provider_for_resource("routed:playlist:focus")
                .unwrap(),
            Some(routed_provider)
        );

        let explicitly_default = ProviderRouter {
            catalog: Some(catalog),
            selected: Some(default_provider),
        };
        let error = explicitly_default
            .provider_for_resource("routed:playlist:focus")
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("conflicts with URI scheme `routed`"));
    }

    #[test]
    fn invalid_provider_id_returns_structured_usage_error() {
        let error = parse_provider_id(Some("Not Valid".to_string())).unwrap_err();
        let error = error.downcast_ref::<DaemonRequestError>().unwrap();
        assert_eq!(error.kind, spotuify_protocol::IpcErrorKind::InvalidRequest);
        assert!(error.message.contains("invalid provider id"));
    }

    #[test]
    fn resolve_request_preserves_bare_id_and_expected_kind() {
        let provider = ProviderId::new("music").unwrap();
        let request = ProviderRouter {
            catalog: None,
            selected: Some(provider.clone()),
        }
        .resolve_request("bare-id", vec![MediaKind::Playlist]);

        assert!(matches!(
            request,
            Request::ResolveTarget {
                input,
                provider: Some(actual_provider),
                expected_kinds: Some(kinds),
            } if input == "bare-id"
                && actual_provider == provider
                && kinds == vec![MediaKind::Playlist]
        ));
    }

    #[test]
    fn terminal_mutation_result_preserves_structured_auth_error() {
        let provider = ProviderId::new("nondefault").unwrap();
        let data = ResponseData::Mutation {
            receipt: spotuify_protocol::CommandReceipt {
                ok: false,
                action: "queue".into(),
                message: "authorization revoked".into(),
                receipt_id: Some(spotuify_protocol::ReceiptId::new_v7()),
                mutation_id: Some(spotuify_protocol::MutationId::new_v7()),
                status: Some(spotuify_protocol::ReceiptStatus::Failed),
                error: Some(spotuify_protocol::ApiErrorSummary {
                    kind: spotuify_protocol::IpcErrorKind::AuthRevoked,
                    message: "authorization revoked".into(),
                    retry_after_secs: Some(17),
                    provider: Some(provider.clone()),
                    detail: Some("refresh token rejected".into()),
                }),
                replayed: true,
            },
        };

        let err = terminal_mutation_result(data).unwrap_err();
        let structured = err.downcast_ref::<DaemonRequestError>().unwrap();
        assert_eq!(
            structured.kind,
            spotuify_protocol::IpcErrorKind::AuthRevoked
        );
        assert_eq!(structured.provider.as_ref(), Some(&provider));
        assert_eq!(structured.retry_after_secs, Some(17));
        assert_eq!(structured.detail.as_deref(), Some("refresh token rejected"));
        assert!(!structured.retryable);
        assert!(structured
            .message
            .contains("spotuify login --provider nondefault"));
        assert!(structured
            .message
            .contains("terminal under its mutation id"));
    }

    #[test]
    fn terminal_mutation_result_returns_confirmed_receipt_not_pending_data() {
        let receipt_id = spotuify_protocol::ReceiptId::new_v7();
        let data = ResponseData::Mutation {
            receipt: spotuify_protocol::CommandReceipt {
                ok: true,
                action: "save".into(),
                message: "save confirmed".into(),
                receipt_id: Some(receipt_id),
                mutation_id: Some(spotuify_protocol::MutationId::new_v7()),
                status: Some(spotuify_protocol::ReceiptStatus::Confirmed),
                error: None,
                replayed: true,
            },
        };

        let terminal = terminal_mutation_result(data).unwrap();
        assert!(matches!(
            terminal,
            ResponseData::Mutation {
                receipt: spotuify_protocol::CommandReceipt {
                    receipt_id: Some(id),
                    status: Some(spotuify_protocol::ReceiptStatus::Confirmed),
                    replayed: true,
                    ..
                }
            } if id == receipt_id
        ));
    }

    fn media_item(uri: &str, name: &str, duration_ms: u64) -> MediaItem {
        MediaItem {
            id: Some(
                ResourceUri::parse(uri)
                    .map(|resource| resource.bare_id().to_string())
                    .unwrap_or_else(|_| uri.to_string()),
            ),
            uri: uri.to_string(),
            name: name.to_string(),
            subtitle: "Artist".to_string(),
            context: "Album".to_string(),
            duration_ms,
            image_url: None,
            kind: MediaKind::Track,
            source: None,
            freshness: None,
            explicit: None,
            is_playable: None,
            ..Default::default()
        }
    }

    fn line(start_ms: u64, text: &str) -> LyricLine {
        LyricLine {
            start_ms,
            text: text.to_string(),
            is_rtl: false,
        }
    }

    fn lyrics() -> SyncedLyrics {
        SyncedLyrics {
            provider: LyricsProvider::Lrclib,
            track_uri: "spotify:track:one".to_string(),
            lines: vec![
                line(0, "first"),
                line(1_000, "second"),
                line(2_000, "third"),
                line(3_000, "fourth"),
            ],
            fetched_at_ms: 1,
            synced: true,
            language: None,
            source_url: None,
        }
    }

    #[test]
    fn lyric_window_keeps_active_line_centered_when_possible() {
        let lines = lyrics().lines;

        assert_eq!(lyric_window(&lines, 2, 3), 1..4);
        assert_eq!(lyric_window(&lines, 0, 3), 0..3);
        assert_eq!(lyric_window(&lines, 3, 3), 1..4);
    }

    #[test]
    fn playback_progress_advances_while_playing_and_clamps_to_duration() {
        let anchor = Instant::now();
        let playback = Playback {
            item: Some(media_item("spotify:track:one", "One", 2_000)),
            is_playing: true,
            progress_ms: 1_500,
            ..Playback::default()
        };

        assert_eq!(
            playback_progress_at(&playback, anchor, anchor + Duration::from_secs(1)),
            2_000
        );

        let paused = Playback {
            is_playing: false,
            progress_ms: 1_500,
            ..playback
        };
        assert_eq!(
            playback_progress_at(&paused, anchor, anchor + Duration::from_secs(1)),
            1_500
        );
    }

    #[test]
    fn follow_view_applies_display_lead_to_active_line() {
        let playback = Playback {
            item: Some(media_item("spotify:track:one", "One", 4_000)),
            is_playing: false,
            progress_ms: 1_500,
            ..Playback::default()
        };
        let mut follower = LyricsFollower::new(playback, 700);
        follower.lyrics = Some(lyrics());

        let view = follower.view_at(follower.anchored_at);

        assert_eq!(view.active_line, Some(2));
    }

    #[test]
    fn jsonl_follow_output_emits_active_line_payload() {
        let playback = Playback {
            item: Some(media_item("spotify:track:one", "One", 4_000)),
            is_playing: true,
            progress_ms: 1_250,
            ..Playback::default()
        };
        let lyrics = lyrics();
        let view = FollowView {
            playback: &playback,
            lyrics: Some(&lyrics),
            progress_ms: 1_250,
            active_line: Some(1),
            status: None,
        };
        let mut out = Vec::new();

        write_follow_jsonl(&mut out, &view).expect("jsonl should write");

        let json: serde_json::Value =
            serde_json::from_slice(&out).expect("output should be valid JSON");
        assert_eq!(json["event"], "line");
        assert_eq!(json["track_uri"], "spotify:track:one");
        assert_eq!(json["line_index"], 1);
        assert_eq!(json["text"], "second");
    }

    #[test]
    fn table_follow_output_marks_current_line() {
        let playback = Playback {
            item: Some(media_item("spotify:track:one", "One", 4_000)),
            is_playing: false,
            progress_ms: 2_000,
            ..Playback::default()
        };
        let lyrics = lyrics();
        let view = FollowView {
            playback: &playback,
            lyrics: Some(&lyrics),
            progress_ms: 2_000,
            active_line: Some(2),
            status: None,
        };
        let mut out = Vec::new();

        write_follow_table(&mut out, &view, 3, false).expect("table should write");

        let rendered = String::from_utf8(out).expect("utf8 output");
        assert!(rendered.contains("paused  00:02"));
        assert!(rendered.contains("> third"));
    }
}
