//! Provider-neutral catalog, library, playlist, and remote-transport contracts.
//!
//! Provider adapters translate their native APIs into these types. Provider
//! quirks and authentication details stay below this boundary.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    Device, MediaItem, MediaKind, Playback, Playlist, Queue, RepeatMode, ResourceUri, SyncedLyrics,
    UriScheme,
};

/// Stable registry identity for a provider adapter.
///
/// This is deliberately distinct from [`crate::UriScheme`]: a registry entry
/// identifies an adapter instance, while a URI scheme identifies a resource
/// namespace. They currently share labels for the built-in adapters, but that
/// is not a type-level invariant.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ProviderId(String);

impl ProviderId {
    /// Build a lowercase, configuration-safe provider identifier.
    pub fn new(value: impl Into<String>) -> Result<Self, ProviderIdError> {
        let value = value.into();
        let mut bytes = value.bytes();
        let valid = bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
            && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if !valid {
            return Err(ProviderIdError { value });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ProviderId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ProviderId {
    type Err = ProviderIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for ProviderId {
    type Error = ProviderIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("invalid provider id `{value}`; expected lowercase ASCII letters, digits, or hyphens")]
pub struct ProviderIdError {
    pub value: String,
}

/// Search behavior an adapter can provide.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct SearchCaps {
    pub remote: bool,
    /// Empty means no media kinds are searchable.
    pub kinds: Vec<MediaKind>,
    pub max_page_size: Option<usize>,
    /// Maximum query length accepted by the adapter, when bounded.
    pub max_query_chars: Option<usize>,
}

/// Catalog-read behavior outside search.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct CatalogCaps {
    /// Media kinds supported by direct URI lookup.
    pub lookup_kinds: Vec<MediaKind>,
    pub recently_played: bool,
    pub recently_played_max_page_size: Option<usize>,
    pub album_tracks: bool,
    pub album_tracks_max_page_size: Option<usize>,
    pub artist_albums: bool,
    pub artist_albums_max_page_size: Option<usize>,
    pub show_episodes: bool,
    pub show_episodes_max_page_size: Option<usize>,
}

/// Saved-library behavior an adapter can provide.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct LibraryCaps {
    /// Kinds the provider can enumerate from the user's library.
    pub read_kinds: Vec<MediaKind>,
    /// Kinds the provider can save and unsave.
    #[serde(alias = "write_kinds")]
    pub save_kinds: Vec<MediaKind>,
    /// Kinds the provider can follow and unfollow.
    pub follow_kinds: Vec<MediaKind>,
    pub mutation_max_batch: Option<usize>,
    pub max_page_size: Option<usize>,
    pub freshness_probe: bool,
}

impl LibraryCaps {
    pub fn can_read(&self, kind: &MediaKind) -> bool {
        self.read_kinds.contains(kind)
    }

    pub fn can_save(&self, kind: &MediaKind) -> bool {
        self.save_kinds.contains(kind)
    }

    pub fn can_follow(&self, kind: &MediaKind) -> bool {
        self.follow_kinds.contains(kind)
    }
}

/// Playlist behavior an adapter can provide.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct PlaylistCaps {
    pub list: bool,
    pub item_read: bool,
    pub create: bool,
    pub add: bool,
    pub remove: bool,
    pub reorder: bool,
    pub image: bool,
    pub unfollow: bool,
    pub version_tokens: bool,
    pub list_max_page_size: Option<usize>,
    pub items_max_page_size: Option<usize>,
    pub add_max_batch: Option<usize>,
    pub remove_max_batch: Option<usize>,
}

/// Remote playback-control behavior. `None` on [`ProviderCaps::transport`]
/// means the provider has no remote transport at all.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct TransportCaps {
    pub playback_state: bool,
    pub play: bool,
    pub pause: bool,
    pub resume: bool,
    pub next: bool,
    pub previous: bool,
    pub seek: bool,
    pub volume: bool,
    pub shuffle: bool,
    pub repeat: bool,
    pub queue_read: bool,
    /// `true` when a successful queue read is authoritative for the entire
    /// upcoming queue. Providers with truncated/windowed queue APIs leave this
    /// false so clients may preserve a cached tail.
    pub queue_snapshots_complete: bool,
    pub queue_add: bool,
    pub devices: bool,
    pub transfer: bool,
}

/// Optional provider-native workflows that do not belong to the catalog,
/// library, playlist, or transport contracts.
///
/// These flags describe semantic results. They do not expose the adapter's
/// underlying protocol (for example, a proprietary request bus or HTTP
/// endpoint) to callers.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct ProviderExtrasCaps {
    pub native_lyrics: bool,
    pub related_artists: bool,
    pub radio: bool,
}

/// Semantic capability declaration for one provider adapter.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct ProviderCaps {
    pub search: SearchCaps,
    pub catalog: CatalogCaps,
    pub library: LibraryCaps,
    pub playlists: PlaylistCaps,
    pub transport: Option<TransportCaps>,
    pub extras: ProviderExtrasCaps,
}

/// Stable provider metadata exposed to daemon-backed clients.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderDescriptor {
    pub id: ProviderId,
    pub uri_scheme: UriScheme,
    pub display_name: String,
    pub capabilities: ProviderCaps,
    #[serde(default)]
    pub is_default: bool,
}

/// Capability catalog for the current daemon provider registry.
///
/// `default_provider` is optional so an explicitly empty catalog remains a
/// valid, distinguishable value during startup and compatibility handling.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderCatalog {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_provider: Option<ProviderId>,
    #[serde(default)]
    pub providers: Vec<ProviderDescriptor>,
}

impl ProviderCatalog {
    /// Validate that the catalog's default marker and default identity agree.
    pub fn validate(&self) -> Result<(), String> {
        let mut ids = std::collections::BTreeSet::new();
        let mut schemes = std::collections::BTreeSet::new();
        for provider in &self.providers {
            if !ids.insert(&provider.id) {
                return Err(format!(
                    "provider catalog contains duplicate id `{}`",
                    provider.id
                ));
            }
            if !schemes.insert(&provider.uri_scheme) {
                return Err(format!(
                    "provider catalog contains duplicate URI scheme `{}`",
                    provider.uri_scheme
                ));
            }
        }
        let marked = self
            .providers
            .iter()
            .filter(|provider| provider.is_default)
            .collect::<Vec<_>>();
        match &self.default_provider {
            None if marked.is_empty() => Ok(()),
            Some(default) if marked.len() == 1 && &marked[0].id == default => Ok(()),
            None => Err("provider catalog marks a default without default_provider".to_string()),
            Some(default) => Err(format!(
                "provider catalog default `{default}` must identify exactly one marked provider"
            )),
        }
    }
}

/// Provider-owned client preferences that clients cannot infer from runtime
/// diagnostics alone.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClientPreferences {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viz_color_scheme: Option<String>,
}

/// Successful result of provider-owned input normalization.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolvedTarget {
    pub provider: ProviderId,
    pub uri: ResourceUri,
}

/// Tri-state claim result used to route raw user input across adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TargetClaim {
    /// The input belongs to another provider or is ordinary search text.
    NotMine,
    /// The provider recognized and canonicalized the input.
    Resolved(ResourceUri),
    /// The provider recognized its namespace but the input was malformed.
    Invalid { message: String },
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ProviderError {
    #[error("provider operation `{operation}` is unsupported")]
    Unsupported { operation: String },
    #[error("provider rate limited the request")]
    RateLimited {
        scope: Option<String>,
        retry_after: Option<Duration>,
    },
    #[error("provider authentication is required")]
    AuthRequired,
    #[error("provider authentication has expired")]
    AuthExpired,
    #[error("provider authentication was revoked")]
    AuthRevoked,
    #[error("provider denied `{operation}`")]
    Forbidden { operation: String },
    #[error("provider has no active playback device")]
    NoActiveDevice,
    #[error("invalid provider input `{field}`: {message}")]
    InvalidInput { field: String, message: String },
    #[error("provider network error: {0}")]
    Network(String),
    #[error("provider transient error (status {status:?}): {message}")]
    Transient {
        status: Option<u16>,
        message: String,
    },
    #[error("provider upstream error (HTTP {status}): {message}")]
    Upstream { status: u16, message: String },
    #[error("provider response decode error: {0}")]
    Decode(String),
    #[error("provider resource not found: {resource}")]
    NotFound { resource: String },
    #[error("provider sync token expired: {reason}")]
    SyncTokenExpired { reason: String },
    #[error("provider version conflict (expected {expected:?}, actual {actual:?})")]
    VersionConflict {
        expected: Option<String>,
        actual: Option<String>,
    },
    #[error("provider error: {0}")]
    Provider(String),
}

impl ProviderError {
    pub fn unsupported(operation: impl Into<String>) -> Self {
        Self::Unsupported {
            operation: operation.into(),
        }
    }

    /// Whether retry/backoff can plausibly succeed without user intervention.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited { .. }
                | Self::Network(_)
                | Self::Transient { .. }
                | Self::AuthExpired
        ) || matches!(self, Self::Upstream { status, .. } if (500..=599).contains(status))
    }

    /// Whether the caller should enter an authentication recovery flow.
    pub fn is_auth_error(&self) -> bool {
        matches!(
            self,
            Self::AuthRequired | Self::AuthExpired | Self::AuthRevoked
        )
    }
}

pub type ProviderResult<T> = Result<T, ProviderError>;

/// Expected access result for resources that can exist but be unreadable
/// (private playlists, regional restrictions, or account policy).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "value")]
pub enum AccessOutcome<T> {
    Available(T),
    Unavailable(AccessUnavailable),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessUnavailable {
    Private,
    RegionRestricted,
    SubscriptionRequired,
    TemporarilyUnavailable,
}

/// Opaque provider-owned freshness value. Callers persist and compare bytes;
/// only the originating adapter interprets them.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct FreshnessProbe(pub Vec<u8>);

/// Scheduling lane for one provider call. Adapters map this to their own
/// rate-limit and concurrency controls.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestPriority {
    #[default]
    Foreground,
    BackgroundSync,
    PlaybackControl,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct RequestContext {
    pub priority: RequestPriority,
}

impl RequestContext {
    pub const FOREGROUND: Self = Self {
        priority: RequestPriority::Foreground,
    };
    pub const BACKGROUND_SYNC: Self = Self {
        priority: RequestPriority::BackgroundSync,
    };
    pub const PLAYBACK_CONTROL: Self = Self {
        priority: RequestPriority::PlaybackControl,
    };
}

/// Provider-neutral pagination input.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct PageRequest {
    pub limit: u32,
    /// Logical requested offset, echoed in [`ProviderPage`]. Cursor-native
    /// providers may keep this at the accumulated item count.
    pub offset: u64,
    /// Opaque provider continuation from a previous page.
    pub cursor: Option<String>,
}

impl PageRequest {
    pub const fn new(limit: u32, offset: u64) -> Self {
        Self {
            limit,
            offset,
            cursor: None,
        }
    }

    pub fn with_cursor(limit: u32, offset: u64, cursor: impl Into<String>) -> Self {
        Self {
            limit,
            offset,
            cursor: Some(cursor.into()),
        }
    }
}

impl Default for PageRequest {
    fn default() -> Self {
        Self {
            limit: 50,
            offset: 0,
            cursor: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PageContinuation {
    Offset(u64),
    Cursor(String),
}

/// Provider-safe page. Totals are optional because many remote APIs cannot
/// compute them, while the next continuation preserves cursor-native paging.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProviderPage<T> {
    pub items: Vec<T>,
    pub requested_offset: u64,
    pub total: Option<u64>,
    pub next: Option<PageContinuation>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct SearchRequest {
    pub query: String,
    /// One kind per page keeps totals and offsets unambiguous across provider
    /// APIs whose native endpoints paginate each kind independently.
    pub kind: MediaKind,
    pub page: PageRequest,
}

impl Default for SearchRequest {
    fn default() -> Self {
        Self {
            query: String::new(),
            kind: MediaKind::Track,
            page: PageRequest::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct LibraryRequest {
    pub kind: MediaKind,
    pub page: PageRequest,
}

impl Default for LibraryRequest {
    fn default() -> Self {
        Self {
            kind: MediaKind::Track,
            page: PageRequest::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CollectionRequest {
    pub uri: ResourceUri,
    pub page: PageRequest,
}

/// One playlist item targeted for removal. Empty `positions` means every
/// occurrence of `uri`; otherwise only the listed zero-based positions.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlaylistItemRef {
    pub uri: ResourceUri,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub positions: Vec<u32>,
}

/// One item insertion. Per-item positions preserve non-contiguous undo plans;
/// `None` appends in mutation order.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlaylistInsertion {
    pub uri: ResourceUri,
    pub position: Option<u32>,
}

/// Consolidated provider mutation surface.
///
/// `mutation_id` is a correlation/deduplication key, not a promise that a
/// remote API supplies exactly-once delivery. The daemon owns durable claims;
/// adapters should cache receipts where they can safely suppress local replay.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mutation {
    PlaylistCreate {
        name: String,
        public: Option<bool>,
        description: Option<String>,
    },
    PlaylistAdd {
        playlist_uri: ResourceUri,
        items: Vec<PlaylistInsertion>,
        expected_version: Option<String>,
    },
    PlaylistRemove {
        playlist_uri: ResourceUri,
        items: Vec<PlaylistItemRef>,
        expected_version: Option<String>,
    },
    PlaylistReorder {
        playlist_uri: ResourceUri,
        range_start: u32,
        insert_before: u32,
        range_length: u32,
        expected_version: Option<String>,
    },
    PlaylistSetImage {
        playlist_uri: ResourceUri,
        jpeg: Vec<u8>,
    },
    PlaylistUnfollow {
        playlist_uri: ResourceUri,
    },
    LibrarySave {
        uris: Vec<ResourceUri>,
    },
    LibraryUnsave {
        uris: Vec<ResourceUri>,
    },
    Follow {
        uris: Vec<ResourceUri>,
    },
    Unfollow {
        uris: Vec<ResourceUri>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationOutcome {
    PlaylistCreated {
        playlist: Playlist,
    },
    PlaylistChanged {
        playlist_uri: ResourceUri,
    },
    PlaylistImageSet {
        playlist_uri: ResourceUri,
    },
    PlaylistUnfollowed {
        playlist_uri: ResourceUri,
    },
    LibraryChanged {
        uris: Vec<ResourceUri>,
        saved: bool,
    },
    FollowChanged {
        uris: Vec<ResourceUri>,
        following: bool,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationCompletion {
    Applied,
    PartiallyApplied,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MutationFailure {
    pub uri: Option<ResourceUri>,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MutationReceipt {
    pub mutation_id: Uuid,
    pub provider: ProviderId,
    pub completion: MutationCompletion,
    pub outcome: MutationOutcome,
    /// Resulting opaque playlist version, when the mutation affects one and
    /// the provider exposes version tokens.
    pub version_token: Option<String>,
    /// Non-empty only when `completion == PartiallyApplied`.
    pub failures: Vec<MutationFailure>,
}

/// Device selection for remote transport. `Active` never silently transfers;
/// adapters return [`ProviderError::NoActiveDevice`] when none exists.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportDevice {
    Active,
    Id(String),
}

/// Source for a remote play request. The enum makes provider context and an
/// explicit ordered window mutually exclusive.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaySource {
    Single,
    Context(ResourceUri),
    /// Explicit playback window for sources without a provider context URI
    /// (for example, Liked Songs). Must contain `PlayRequest::start_uri`.
    Ordered(Vec<ResourceUri>),
}

/// Remote play request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlayRequest {
    pub start_uri: ResourceUri,
    pub source: PlaySource,
    pub device: TransportDevice,
    pub position_ms: u64,
}

impl PlayRequest {
    pub fn validate(&self) -> ProviderResult<()> {
        if let PlaySource::Ordered(uris) = &self.source {
            if uris.is_empty() || !uris.contains(&self.start_uri) {
                return Err(ProviderError::InvalidInput {
                    field: "source".to_string(),
                    message: "ordered playback source must contain start_uri".to_string(),
                });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QueueAddRequest {
    pub uri: ResourceUri,
    pub device: TransportDevice,
}

/// A provider-neutral remote transport mutation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportCommand {
    Play(PlayRequest),
    Pause,
    Resume,
    Next,
    Previous,
    Seek { position_ms: u64 },
    Volume { percent: u8 },
    Shuffle { enabled: bool },
    Repeat { mode: RepeatMode },
    QueueAdd(QueueAddRequest),
    Transfer { device_id: String, play: bool },
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct TransportOutcome {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playback: Option<Playback>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<Queue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub devices: Option<Vec<Device>>,
}

/// Catalog, library, and playlist adapter boundary.
///
/// [`RemoteTransport`] remains an independent object. Registry construction
/// pairs the two by [`ProviderId`] and must reject mismatched IDs/schemes or a
/// transport object whose provider declares `transport: None`.
#[async_trait]
pub trait MusicProvider: Send + Sync {
    fn id(&self) -> &ProviderId;
    /// Canonical resource namespace owned by this provider instance. Provider
    /// registries must reject duplicate schemes so URI routing is unambiguous.
    fn uri_scheme(&self) -> &UriScheme;
    fn display_name(&self) -> &str;
    fn capabilities(&self) -> ProviderCaps;

    /// Claim and canonicalize raw user input owned by this adapter.
    ///
    /// The default implementation handles strict canonical resource URIs.
    /// Adapters override it for provider share URLs and legacy URI forms.
    fn claim_target(&self, input: &str) -> TargetClaim {
        let input = input.trim();
        match ResourceUri::parse(input) {
            Ok(resource) if resource.scheme() == self.uri_scheme() => {
                TargetClaim::Resolved(resource)
            }
            Ok(_) => TargetClaim::NotMine,
            Err(error)
                if input
                    .split_once(':')
                    .is_some_and(|(scheme, _)| scheme == self.uri_scheme().label()) =>
            {
                TargetClaim::Invalid {
                    message: error.to_string(),
                }
            }
            Err(_) => TargetClaim::NotMine,
        }
    }

    async fn search(
        &self,
        _context: RequestContext,
        _request: SearchRequest,
    ) -> ProviderResult<ProviderPage<MediaItem>> {
        Err(ProviderError::unsupported("search"))
    }

    async fn media_item(
        &self,
        _context: RequestContext,
        _uri: &ResourceUri,
    ) -> ProviderResult<Option<MediaItem>> {
        Err(ProviderError::unsupported("media_item"))
    }

    async fn recently_played(
        &self,
        _context: RequestContext,
        _page: PageRequest,
    ) -> ProviderResult<ProviderPage<MediaItem>> {
        Err(ProviderError::unsupported("recently_played"))
    }

    async fn library_items(
        &self,
        _context: RequestContext,
        _request: LibraryRequest,
    ) -> ProviderResult<ProviderPage<MediaItem>> {
        Err(ProviderError::unsupported("library_items"))
    }

    async fn library_freshness_probe(
        &self,
        _context: RequestContext,
        _kind: MediaKind,
    ) -> ProviderResult<FreshnessProbe> {
        Err(ProviderError::unsupported("library_freshness_probe"))
    }

    /// Provider-owned freshness comparison. Adapters may override for opaque
    /// tokens whose semantics are richer than byte inequality.
    fn library_freshness_changed(
        &self,
        previous: &FreshnessProbe,
        current: &FreshnessProbe,
    ) -> bool {
        previous != current
    }

    /// Provider-owned playlist-version comparison.
    ///
    /// Fail-open: a missing token on either side means we can't prove the
    /// playlist is unchanged, so report changed and force a refetch (matches
    /// `should_refetch_playlist_tracks` in `spotuify-sync`). Only two present,
    /// differing tokens are treated as a genuine change; two present, equal
    /// tokens as unchanged.
    fn playlist_version_changed(&self, previous: Option<&str>, current: Option<&str>) -> bool {
        match (previous, current) {
            (Some(previous), Some(current)) => previous != current,
            _ => true,
        }
    }

    async fn playlists(
        &self,
        _context: RequestContext,
        _page: PageRequest,
    ) -> ProviderResult<ProviderPage<Playlist>> {
        Err(ProviderError::unsupported("playlists"))
    }

    async fn playlist(
        &self,
        _context: RequestContext,
        _uri: &ResourceUri,
    ) -> ProviderResult<Option<Playlist>> {
        Err(ProviderError::unsupported("playlist"))
    }

    async fn playlist_items(
        &self,
        _context: RequestContext,
        _request: CollectionRequest,
    ) -> ProviderResult<AccessOutcome<ProviderPage<MediaItem>>> {
        Err(ProviderError::unsupported("playlist_items"))
    }

    async fn album_tracks(
        &self,
        _context: RequestContext,
        _request: CollectionRequest,
    ) -> ProviderResult<ProviderPage<MediaItem>> {
        Err(ProviderError::unsupported("album_tracks"))
    }

    async fn artist_albums(
        &self,
        _context: RequestContext,
        _request: CollectionRequest,
    ) -> ProviderResult<ProviderPage<MediaItem>> {
        Err(ProviderError::unsupported("artist_albums"))
    }

    async fn show_episodes(
        &self,
        _context: RequestContext,
        _request: CollectionRequest,
    ) -> ProviderResult<ProviderPage<MediaItem>> {
        Err(ProviderError::unsupported("show_episodes"))
    }

    async fn apply_mutation(
        &self,
        _context: RequestContext,
        _mutation_id: Uuid,
        _mutation: &Mutation,
    ) -> ProviderResult<MutationReceipt> {
        Err(ProviderError::unsupported("apply_mutation"))
    }
}

/// Optional provider facet for remote playback control. Local audio playback
/// remains the responsibility of `spotuify-player::PlayerBackend`.
#[async_trait]
pub trait RemoteTransport: Send + Sync {
    fn provider_id(&self) -> &ProviderId;
    fn uri_scheme(&self) -> &UriScheme;

    async fn playback(&self, _context: RequestContext) -> ProviderResult<Playback> {
        Err(ProviderError::unsupported("transport.playback"))
    }

    async fn devices(&self, _context: RequestContext) -> ProviderResult<Vec<Device>> {
        Err(ProviderError::unsupported("transport.devices"))
    }

    async fn queue(&self, _context: RequestContext) -> ProviderResult<Queue> {
        Err(ProviderError::unsupported("transport.queue"))
    }

    /// Transport mutations should be dispatched through the playback-control
    /// lane. Adapters must override weaker caller priorities for this method.
    async fn execute(
        &self,
        _context: RequestContext,
        _command: TransportCommand,
    ) -> ProviderResult<TransportOutcome> {
        Err(ProviderError::unsupported("transport.execute"))
    }
}

/// Optional provider-native workflows with provider-neutral inputs and
/// outputs.
///
/// Registries pair this object with [`MusicProvider`] by provider identity and
/// URI scheme, just like [`RemoteTransport`]. A provider without extras does
/// not install an object; callers must gate on [`ProviderExtrasCaps`] before
/// dispatch. Default methods remain defensive and return a typed unsupported
/// error if a capability declaration and implementation drift apart.
#[async_trait]
pub trait ProviderExtras: Send + Sync {
    fn provider_id(&self) -> &ProviderId;
    fn uri_scheme(&self) -> &UriScheme;
    fn capabilities(&self) -> ProviderExtrasCaps;

    async fn native_lyrics(
        &self,
        _context: RequestContext,
        _track: &ResourceUri,
    ) -> ProviderResult<Option<SyncedLyrics>> {
        Err(ProviderError::unsupported("extras.native_lyrics"))
    }

    async fn related_artists(
        &self,
        _context: RequestContext,
        _artist: &ResourceUri,
    ) -> ProviderResult<Vec<MediaItem>> {
        Err(ProviderError::unsupported("extras.related_artists"))
    }

    async fn radio(
        &self,
        _context: RequestContext,
        _seed: &ResourceUri,
    ) -> ProviderResult<Vec<ResourceUri>> {
        Err(ProviderError::unsupported("extras.radio"))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    struct MinimalProvider {
        id: ProviderId,
        scheme: UriScheme,
    }

    struct MinimalExtras {
        id: ProviderId,
        scheme: UriScheme,
    }

    #[async_trait]
    impl MusicProvider for MinimalProvider {
        fn id(&self) -> &ProviderId {
            &self.id
        }

        fn uri_scheme(&self) -> &UriScheme {
            &self.scheme
        }

        fn display_name(&self) -> &str {
            "Minimal"
        }

        fn capabilities(&self) -> ProviderCaps {
            ProviderCaps::default()
        }
    }

    #[async_trait]
    impl ProviderExtras for MinimalExtras {
        fn provider_id(&self) -> &ProviderId {
            &self.id
        }

        fn uri_scheme(&self) -> &UriScheme {
            &self.scheme
        }

        fn capabilities(&self) -> ProviderExtrasCaps {
            ProviderExtrasCaps::default()
        }
    }

    #[test]
    fn provider_id_is_distinct_and_validated() {
        let id = ProviderId::new("apple-music").expect("valid provider id");
        assert_eq!(id.as_str(), "apple-music");
        assert!(ProviderId::new("Apple Music").is_err());
        assert!(ProviderId::new("").is_err());
    }

    #[test]
    fn provider_traits_are_object_safe() {
        let provider = MinimalProvider {
            id: ProviderId::new("minimal").expect("valid provider id"),
            scheme: UriScheme::new("minimal").expect("valid URI scheme"),
        };
        let object: &dyn MusicProvider = &provider;
        assert_eq!(object.id().as_str(), "minimal");
        assert_eq!(object.uri_scheme().label(), "minimal");
    }

    #[test]
    fn capability_defaults_are_additive_and_disabled() {
        let caps: ProviderCaps = serde_json::from_str("{}").expect("default caps deserialize");
        assert_eq!(caps, ProviderCaps::default());
        assert!(!caps.search.remote);
        assert_eq!(caps.search.max_query_chars, None);
        assert!(caps.transport.is_none());
        assert_eq!(caps.extras, ProviderExtrasCaps::default());

        let with_radio: ProviderCaps = serde_json::from_str(r#"{"extras":{"radio":true}}"#)
            .expect("additive extras capability should deserialize");
        assert!(with_radio.extras.radio);
        assert!(!with_radio.extras.native_lyrics);
        assert!(!with_radio.extras.related_artists);
    }

    #[test]
    fn provider_extras_are_object_safe_and_transportless_by_default() {
        let extras = MinimalExtras {
            id: ProviderId::new("minimal").expect("valid provider id"),
            scheme: UriScheme::new("minimal").expect("valid URI scheme"),
        };
        let object: &dyn ProviderExtras = &extras;
        assert_eq!(object.provider_id().as_str(), "minimal");
        assert_eq!(object.uri_scheme().label(), "minimal");
        assert_eq!(object.capabilities(), ProviderExtrasCaps::default());
        let track = ResourceUri::parse("minimal:track:one").expect("valid URI");
        let future = object.native_lyrics(RequestContext::FOREGROUND, &track);
        drop(future);
        assert!(ProviderCaps::default().transport.is_none());
    }

    #[test]
    fn default_target_claim_only_owns_its_canonical_namespace() {
        let provider = MinimalProvider {
            id: ProviderId::new("minimal").expect("valid provider id"),
            scheme: UriScheme::new("minimal").expect("valid URI scheme"),
        };
        assert!(matches!(
            provider.claim_target("minimal:track:one"),
            TargetClaim::Resolved(uri) if uri.as_uri() == "minimal:track:one"
        ));
        assert_eq!(
            provider.claim_target("other:track:one"),
            TargetClaim::NotMine
        );
        assert!(matches!(
            provider.claim_target("minimal:not-a-kind:one"),
            TargetClaim::Invalid { .. }
        ));
        assert_eq!(
            provider.claim_target("ordinary search"),
            TargetClaim::NotMine
        );
    }

    #[test]
    fn absent_catalog_and_explicit_empty_catalog_remain_distinct() {
        #[derive(Debug, Default, Deserialize, PartialEq, Serialize)]
        struct SeedMetadata {
            #[serde(default, skip_serializing_if = "Option::is_none")]
            provider_catalog: Option<ProviderCatalog>,
        }

        let absent: SeedMetadata = serde_json::from_str("{}").expect("absent catalog");
        let empty: SeedMetadata =
            serde_json::from_str(r#"{"provider_catalog":{}}"#).expect("explicit empty catalog");
        assert_eq!(absent.provider_catalog, None);
        assert_eq!(empty.provider_catalog, Some(ProviderCatalog::default()));
        assert_eq!(serde_json::to_string(&absent).unwrap(), "{}");
        assert_eq!(
            serde_json::to_string(&empty).unwrap(),
            r#"{"provider_catalog":{"providers":[]}}"#
        );
    }

    #[test]
    fn provider_catalog_default_identity_and_marker_must_agree() {
        let descriptor = |id: &str, is_default| ProviderDescriptor {
            id: ProviderId::new(id).unwrap(),
            uri_scheme: UriScheme::new(id).unwrap(),
            display_name: id.to_string(),
            capabilities: ProviderCaps::default(),
            is_default,
        };
        let valid = ProviderCatalog {
            default_provider: Some(ProviderId::new("primary").unwrap()),
            providers: vec![descriptor("primary", true), descriptor("secondary", false)],
        };
        assert!(valid.validate().is_ok());

        let missing_marker = ProviderCatalog {
            default_provider: valid.default_provider.clone(),
            providers: vec![descriptor("primary", false)],
        };
        assert!(missing_marker.validate().is_err());

        let unexpected_marker = ProviderCatalog {
            default_provider: None,
            providers: vec![descriptor("primary", true)],
        };
        assert!(unexpected_marker.validate().is_err());

        let duplicate_markers = ProviderCatalog {
            default_provider: valid.default_provider,
            providers: vec![descriptor("primary", true), descriptor("secondary", true)],
        };
        assert!(duplicate_markers.validate().is_err());

        let duplicate_ids = ProviderCatalog {
            default_provider: None,
            providers: vec![
                descriptor("primary", false),
                ProviderDescriptor {
                    id: ProviderId::new("primary").unwrap(),
                    uri_scheme: UriScheme::new("other").unwrap(),
                    display_name: "other".to_string(),
                    capabilities: ProviderCaps::default(),
                    is_default: false,
                },
            ],
        };
        assert!(duplicate_ids
            .validate()
            .unwrap_err()
            .contains("duplicate id"));

        let duplicate_schemes = ProviderCatalog {
            default_provider: None,
            providers: vec![
                descriptor("primary", false),
                ProviderDescriptor {
                    id: ProviderId::new("other").unwrap(),
                    uri_scheme: UriScheme::new("primary").unwrap(),
                    display_name: "other".to_string(),
                    capabilities: ProviderCaps::default(),
                    is_default: false,
                },
            ],
        };
        assert!(duplicate_schemes
            .validate()
            .unwrap_err()
            .contains("duplicate URI scheme"));
    }

    #[test]
    fn request_context_has_explicit_scheduling_lanes() {
        assert_eq!(
            RequestContext::BACKGROUND_SYNC.priority,
            RequestPriority::BackgroundSync
        );
        assert_eq!(
            RequestContext::PLAYBACK_CONTROL.priority,
            RequestPriority::PlaybackControl
        );
    }

    #[test]
    fn provider_errors_classify_retry_and_auth_recovery() {
        assert!(ProviderError::Transient {
            status: Some(503),
            message: "busy".to_string(),
        }
        .is_retryable());
        assert!(ProviderError::Upstream {
            status: 500,
            message: "failed".to_string(),
        }
        .is_retryable());
        assert!(!ProviderError::Upstream {
            status: 400,
            message: "bad request".to_string(),
        }
        .is_retryable());
        assert!(ProviderError::AuthExpired.is_auth_error());
        assert!(ProviderError::AuthExpired.is_retryable());
        assert!(!ProviderError::Forbidden {
            operation: "search".to_string(),
        }
        .is_auth_error());
    }

    #[test]
    fn play_request_rejects_invalid_ordered_sources() {
        let request = PlayRequest {
            start_uri: ResourceUri::parse("minimal:track:one").expect("valid URI"),
            source: PlaySource::Ordered(Vec::new()),
            device: TransportDevice::Active,
            position_ms: 0,
        };
        assert!(matches!(
            request.validate(),
            Err(ProviderError::InvalidInput { ref field, .. }) if field == "source"
        ));
    }

    #[test]
    fn default_freshness_comparison_is_value_based() {
        let provider = MinimalProvider {
            id: ProviderId::new("minimal").expect("valid provider id"),
            scheme: UriScheme::new("minimal").expect("valid URI scheme"),
        };
        assert!(!provider
            .library_freshness_changed(&FreshnessProbe(vec![1]), &FreshnessProbe(vec![1]),));
        assert!(
            provider.library_freshness_changed(&FreshnessProbe(vec![1]), &FreshnessProbe(vec![2]),)
        );
        assert!(!provider.playlist_version_changed(Some("v1"), Some("v1")));
        assert!(provider.playlist_version_changed(Some("v1"), Some("v2")));
    }
}
