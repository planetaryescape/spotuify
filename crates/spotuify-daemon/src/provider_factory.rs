//! Construction of validated provider runtimes for the daemon.
//!
//! This is the only daemon module allowed to construct or name the concrete
//! Spotify adapter. State and handlers receive provider-neutral registry and
//! auth outcomes.

use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Deserialize;
#[cfg(test)]
use spotuify_core::RemoteTransport;
#[cfg(feature = "embedded-playback")]
use spotuify_core::{MediaKind, ProviderExtrasCaps, RequestContext, ResourceUri};
use spotuify_core::{MusicProvider, ProviderError, ProviderExtras, ProviderId, UriScheme};
use spotuify_provider_fake::{FakeDataset, FakeProvider};
use spotuify_spotify::auth::StoredToken;
use spotuify_spotify::client::SchemaCompatReporter;
use spotuify_spotify::provider::provider_error;
use spotuify_spotify::rate_limit::RateLimitedClient;
use spotuify_spotify::{SpotifyClient, WebApiBearerProvider};
use tokio::sync::Mutex;

use crate::analytics::{AnalyticsSource, AnalyticsStore};
use crate::provider_registry::{
    ProviderPlayer, ProviderPlayerSlot, ProviderRegistry, ProviderRuntime, TransportRecovery,
};

#[cfg(feature = "embedded-playback")]
const PROVIDER_RESOURCE_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(60);
#[cfg(feature = "embedded-playback")]
const PROVIDER_RESOURCE_CACHE_MAX: usize = 128;

#[cfg(feature = "embedded-playback")]
#[derive(Clone)]
enum ProviderResourceCacheEntry {
    Ready {
        value: bytes::Bytes,
        expires_at: std::time::Instant,
    },
    Loading(tokio::sync::watch::Receiver<Option<spotuify_core::ProviderResult<bytes::Bytes>>>),
}

#[cfg(feature = "embedded-playback")]
#[derive(Clone)]
struct ProviderResourceCache {
    entries: Arc<tokio::sync::Mutex<std::collections::HashMap<String, ProviderResourceCacheEntry>>>,
    ttl: std::time::Duration,
    max_entries: usize,
}

#[cfg(feature = "embedded-playback")]
impl Default for ProviderResourceCache {
    fn default() -> Self {
        Self {
            entries: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            ttl: PROVIDER_RESOURCE_CACHE_TTL,
            max_entries: PROVIDER_RESOURCE_CACHE_MAX,
        }
    }
}

#[cfg(feature = "embedded-playback")]
impl ProviderResourceCache {
    async fn get_or_fetch<F, Fut>(
        &self,
        key: String,
        fetch: F,
    ) -> spotuify_core::ProviderResult<bytes::Bytes>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = spotuify_core::ProviderResult<bytes::Bytes>>
            + Send
            + 'static,
    {
        let mut receiver = {
            let mut entries = self.entries.lock().await;
            let now = std::time::Instant::now();
            entries.retain(|_, entry| {
                !matches!(entry, ProviderResourceCacheEntry::Ready { expires_at, .. } if *expires_at <= now)
            });
            match entries.get(&key) {
                Some(ProviderResourceCacheEntry::Ready { value, .. }) => return Ok(value.clone()),
                Some(ProviderResourceCacheEntry::Loading(receiver)) => receiver.clone(),
                None => {
                    if entries.len() >= self.max_entries {
                        if let Some(evicted) = entries.iter().find_map(|(key, entry)| {
                            matches!(entry, ProviderResourceCacheEntry::Ready { .. })
                                .then(|| key.clone())
                        }) {
                            entries.remove(&evicted);
                        } else {
                            return Err(ProviderError::Transient {
                                status: None,
                                message:
                                    "provider resource cache is saturated by in-flight fetches"
                                        .to_string(),
                            });
                        }
                    }
                    let (sender, receiver) = tokio::sync::watch::channel(None);
                    entries.insert(
                        key.clone(),
                        ProviderResourceCacheEntry::Loading(receiver.clone()),
                    );
                    let cache = self.clone();
                    tokio::spawn(async move {
                        let result = fetch().await;
                        let mut entries = cache.entries.lock().await;
                        if let Ok(value) = &result {
                            entries.insert(
                                key,
                                ProviderResourceCacheEntry::Ready {
                                    value: value.clone(),
                                    expires_at: std::time::Instant::now() + cache.ttl,
                                },
                            );
                        } else {
                            entries.remove(&key);
                        }
                        drop(entries);
                        let _ = sender.send(Some(result));
                    });
                    receiver
                }
            }
        };

        loop {
            if let Some(result) = receiver.borrow().clone() {
                return result;
            }
            receiver
                .changed()
                .await
                .map_err(|_| ProviderError::Transient {
                    status: None,
                    message: "provider resource fetch task stopped".to_string(),
                })?;
        }
    }
}

#[cfg(feature = "embedded-playback")]
pub(crate) struct SpotifySessionExtras {
    provider_id: ProviderId,
    uri_scheme: UriScheme,
    session: spotuify_player::backends::embedded::EmbeddedSessionHandle,
    cache: ProviderResourceCache,
}

#[cfg(feature = "embedded-playback")]
impl SpotifySessionExtras {
    pub(crate) fn new(
        provider_id: ProviderId,
        session: spotuify_player::backends::embedded::EmbeddedSessionHandle,
    ) -> Self {
        Self {
            provider_id,
            uri_scheme: UriScheme::Spotify,
            session,
            cache: ProviderResourceCache::default(),
        }
    }

    pub(crate) async fn mint_web_api_bearer(&self) -> Option<String> {
        self.session.mint_web_api_bearer().await
    }

    async fn fetch(
        &self,
        uri: &str,
        operation: &str,
    ) -> spotuify_core::ProviderResult<bytes::Bytes> {
        let session = self.session.clone();
        let uri = uri.to_string();
        let operation = operation.to_string();
        self.cache
            .get_or_fetch(uri.clone(), move || async move {
                session
                    .fetch_provider_resource(&uri)
                    .await
                    .map_err(|error| player_provider_error(error, &operation))
            })
            .await
    }
}

#[cfg(feature = "embedded-playback")]
#[async_trait::async_trait]
impl WebApiBearerProvider for SpotifySessionExtras {
    async fn bearer(&self, _force_refresh: bool) -> spotuify_spotify::SpotifyResult<String> {
        self.mint_web_api_bearer()
            .await
            .ok_or(spotuify_spotify::SpotifyError::AuthRequired)
    }
}

#[cfg(feature = "embedded-playback")]
#[async_trait::async_trait]
impl ProviderExtras for SpotifySessionExtras {
    fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    fn uri_scheme(&self) -> &UriScheme {
        &self.uri_scheme
    }

    fn capabilities(&self) -> ProviderExtrasCaps {
        ProviderExtrasCaps {
            native_lyrics: true,
            related_artists: true,
            radio: true,
        }
    }

    async fn native_lyrics(
        &self,
        _context: RequestContext,
        track: &ResourceUri,
    ) -> spotuify_core::ProviderResult<Option<spotuify_core::SyncedLyrics>> {
        require_extra_resource(self, track, &[MediaKind::Track], "native_lyrics.track")?;
        let mercury_uri =
            spotuify_lyrics::mercury_uri_for_track_uri(&track.as_uri()).ok_or_else(|| {
                ProviderError::InvalidInput {
                    field: "native_lyrics.track".to_string(),
                    message: format!(
                        "provider {} cannot fetch lyrics for `{track}`",
                        self.provider_id
                    ),
                }
            })?;
        let bytes = self.fetch(&mercury_uri, "native_lyrics").await?;
        spotuify_lyrics::parse_spotify_mercury(bytes, &track.as_uri(), spotuify_core::now_ms())
            .map_err(|error| ProviderError::Decode(format!("native lyrics: {error}")))
    }

    async fn related_artists(
        &self,
        _context: RequestContext,
        artist: &ResourceUri,
    ) -> spotuify_core::ProviderResult<Vec<spotuify_core::MediaItem>> {
        require_extra_resource(self, artist, &[MediaKind::Artist], "related_artists.artist")?;
        let mercury_uri = spotuify_spotify::mercury::related_artists_mercury_uri(&artist.as_uri())
            .ok_or_else(|| ProviderError::InvalidInput {
                field: "related_artists.artist".to_string(),
                message: format!("provider {} cannot resolve `{artist}`", self.provider_id),
            })?;
        let bytes = self.fetch(&mercury_uri, "related_artists").await?;
        Ok(spotuify_spotify::mercury::parse_related_artists(&bytes))
    }

    async fn radio(
        &self,
        _context: RequestContext,
        seed: &ResourceUri,
    ) -> spotuify_core::ProviderResult<Vec<ResourceUri>> {
        require_extra_resource(
            self,
            seed,
            &[
                MediaKind::Track,
                MediaKind::Album,
                MediaKind::Artist,
                MediaKind::Playlist,
            ],
            "radio.seed",
        )?;
        let mercury_uri = spotuify_spotify::mercury::radio_station_mercury_uri(&seed.as_uri());
        let bytes = self.fetch(&mercury_uri, "radio").await?;
        spotuify_spotify::mercury::parse_radio_station(&bytes)
            .into_iter()
            .map(|uri| {
                ResourceUri::parse(&uri).map_err(|error| {
                    ProviderError::Decode(format!("radio returned invalid URI `{uri}`: {error}"))
                })
            })
            .collect()
    }
}

#[cfg(feature = "embedded-playback")]
fn require_extra_resource(
    extras: &SpotifySessionExtras,
    resource: &ResourceUri,
    allowed: &[MediaKind],
    field: &str,
) -> spotuify_core::ProviderResult<()> {
    if resource.scheme() != extras.uri_scheme() || !allowed.contains(&resource.kind()) {
        return Err(ProviderError::InvalidInput {
            field: field.to_string(),
            message: format!(
                "provider {} cannot use `{resource}` for this operation",
                extras.provider_id()
            ),
        });
    }
    Ok(())
}

#[cfg(feature = "embedded-playback")]
fn player_provider_error(error: spotuify_player::PlayerError, operation: &str) -> ProviderError {
    match error {
        spotuify_player::PlayerError::Auth(_) => ProviderError::AuthRequired,
        spotuify_player::PlayerError::Network(message) => ProviderError::Network(message),
        spotuify_player::PlayerError::Timeout(duration) => ProviderError::Transient {
            status: None,
            message: format!("{operation} timed out after {duration:?}"),
        },
        spotuify_player::PlayerError::InvalidArg(message) => ProviderError::InvalidInput {
            field: operation.to_string(),
            message,
        },
        spotuify_player::PlayerError::ProviderPolicy(message) => ProviderError::InvalidInput {
            field: operation.to_string(),
            message: spotuify_protocol::sanitize_provider_policy_reason(&message),
        },
        spotuify_player::PlayerError::Unsupported(operation) => {
            ProviderError::unsupported(operation)
        }
        other => ProviderError::Provider(other.to_string()),
    }
}

/// Shared Spotify HTTP/backpressure runtime and concrete adapter factory.
/// Cloning preserves the reqwest pools, semaphores, and rate-limit state.
#[derive(Clone)]
pub(crate) struct ProviderFactory {
    rate_limiter: RateLimitedClient,
    local_facets: Arc<parking_lot::Mutex<std::collections::BTreeMap<ProviderId, LocalFacets>>>,
}

#[derive(Clone)]
struct LocalFacets {
    extras: Option<Arc<dyn ProviderExtras>>,
    session_bearer: Option<Arc<dyn WebApiBearerProvider>>,
    player: ProviderPlayerSlot,
}

/// Provider-neutral result of probing the configured adapter's auth path.
#[derive(Debug)]
pub(crate) enum ProviderAuthOutcome {
    /// The deterministic fake provider has no remote authentication step.
    NotRequired,
    Authenticated {
        access_token: String,
        first_party: bool,
    },
    Unavailable {
        error: ProviderError,
        first_party: bool,
    },
}

pub(crate) struct ProviderBuildOutcome {
    pub registry: ProviderRegistry,
    pub auth: ProviderAuthOutcome,
    pub session_bearer: Option<(ProviderId, Arc<dyn WebApiBearerProvider>)>,
}

/// Authentication is selected from the concrete adapter branch, never from
/// a provider-id spelling. An injected adapter named `spotify` therefore does
/// not accidentally inherit Spotify OAuth behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProviderAuthStrategy {
    None,
    SpotifyOauth,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProviderAuthTarget {
    pub provider_id: ProviderId,
    pub strategy: ProviderAuthStrategy,
}

/// Shared inputs needed by both registry construction and the lightweight
/// daemon auth-health probe.
#[derive(Clone)]
pub(crate) struct ProviderAuthInputs {
    // Config validation rejects multiple Spotify adapters because they would
    // collide on the `spotify:` URI namespace. The single cache/bearer pair
    // therefore belongs to the sole configured Spotify adapter, whose
    // ProviderId may still be custom (for example `custom-cloud`).
    pub token_cache: Arc<Mutex<Option<StoredToken>>>,
    pub first_party_bearer: Option<(ProviderId, Arc<dyn WebApiBearerProvider>)>,
}

pub(crate) struct ProviderBuildInputs {
    /// Last config snapshot accepted by runtime reload. Registry construction
    /// must use this exact value rather than re-reading a file that may have
    /// changed after validation.
    pub config: Option<spotuify_config::AppConfig>,
    pub auth: ProviderAuthInputs,
    pub schema_compat_reporter: Arc<dyn SchemaCompatReporter>,
    pub player_token_slot: Arc<parking_lot::RwLock<Option<String>>>,
    pub viz_analyzer: Option<spotuify_audio::SharedAnalyzer>,
}

/// Build the deterministic adapter behind the legacy
/// `SPOTUIFY_FAKE_SPOTIFY` switch. The switch substitutes Spotify's network
/// implementation only; every daemon boundary must still see Spotify's
/// provider id and URI scheme.
pub(crate) fn fake_spotify_provider() -> Result<FakeProvider> {
    let dataset = std::env::var(spotuify_provider_fake::FAKE_DATASET_ENV)
        .ok()
        .map(|value| value.parse::<FakeDataset>())
        .transpose()
        .context("invalid fake provider dataset")?
        .unwrap_or_default();
    let dataset = match dataset {
        FakeDataset::Standard => FakeDataset::SpotifyCompatibility,
        other => other,
    };
    Ok(FakeProvider::with_identity(
        ProviderId::new("spotify").expect("built-in Spotify provider id is valid"),
        UriScheme::Spotify,
        dataset,
    ))
}

impl ProviderFactory {
    pub fn new() -> Result<Self> {
        let rate_limiter =
            SpotifyClient::default_rate_limiter().context("failed to build Spotify runtime")?;
        Ok(Self {
            rate_limiter,
            local_facets: Arc::new(parking_lot::Mutex::new(std::collections::BTreeMap::new())),
        })
    }

    fn local_facets_or_try_insert(
        &self,
        provider_id: &ProviderId,
        build: impl FnOnce() -> Result<LocalFacets>,
    ) -> Result<LocalFacets> {
        let mut facets = self.local_facets.lock();
        if let Some(existing) = facets.get(provider_id) {
            return Ok(existing.clone());
        }
        let built = build()?;
        facets.insert(provider_id.clone(), built.clone());
        Ok(built)
    }

    fn configured_fake_runtime(
        &self,
        entry: &spotuify_config::ProviderEntry,
        default_id: &ProviderId,
    ) -> Result<ProviderRuntime> {
        let provider = Arc::new(fake_provider(entry)?);
        let runtime = if provider.id() == default_id {
            let local = self.local_facets_or_try_insert(provider.id(), || {
                let (backend, events) =
                    spotuify_player::backends::mock::MockPlayerBackend::new_for_provider(
                        provider.id().clone(),
                        MusicProvider::uri_scheme(provider.as_ref()).clone(),
                    );
                Ok(LocalFacets {
                    extras: None,
                    session_bearer: None,
                    player: ProviderPlayerSlot::new(ProviderPlayer::new(Box::new(backend), events)),
                })
            })?;
            ProviderRuntime::with_player_slot(
                provider,
                local.extras,
                local.player,
                TransportRecovery::RemoteOnly,
            )
        } else {
            ProviderRuntime::with_transport(provider)
        };
        runtime.context("fake provider facets failed registry validation")
    }

    /// Validate every adapter-specific config table without touching auth,
    /// player ownership, or the currently installed registry. Runtime reload
    /// uses this as its prepare phase so malformed settings cannot evict a
    /// working registry and fail only on the next command.
    pub(crate) fn validate_config(&self, config: &spotuify_config::AppConfig) -> Result<()> {
        let default_id = config
            .default_provider
            .as_ref()
            .context("provider config does not select a default provider")?;
        for entry in &config.providers {
            match entry.kind.as_str() {
                "fake" => {
                    fake_provider(entry)?;
                    if &entry.id == default_id {
                        entry
                            .player_settings()
                            .context("failed to decode fake provider player config")?;
                    }
                }
                "spotify" => {
                    spotuify_spotify::config::provider_config_from_table(
                        entry.raw_table(),
                        config.path.clone(),
                    )
                    .context("failed to decode Spotify provider config")?;
                    if &entry.id == default_id {
                        entry
                            .player_settings()
                            .context("failed to decode Spotify provider player config")?;
                    }
                }
                kind => anyhow::bail!("provider `{}` has unsupported type `{kind}`", entry.id),
            }
        }
        Ok(())
    }

    /// Resolve the identity configured by the built-in provider factory
    /// without probing credentials. Auth recovery must be able to validate
    /// its target while the normal provider registry is auth-gated.
    pub(crate) fn configured_auth_target(requested: Option<&str>) -> Result<ProviderAuthTarget> {
        if std::env::var_os("SPOTUIFY_FAKE_SPOTIFY").is_some() {
            let provider_id = fake_spotify_provider()?.id().clone();
            validate_requested_provider_id(&provider_id, requested)?;
            return Ok(ProviderAuthTarget {
                provider_id,
                // Never let the deterministic smoke adapter touch the user's
                // real Spotify credential inventory. It keeps Spotify's wire
                // identity and URI scheme, but deliberately requires no auth.
                strategy: ProviderAuthStrategy::None,
            });
        }
        let loaded = spotuify_config::load().context("failed to load provider config")?;
        Self::auth_target_from_config(&loaded.config, requested)
    }

    pub(crate) fn auth_target_from_config(
        config: &spotuify_config::AppConfig,
        requested: Option<&str>,
    ) -> Result<ProviderAuthTarget> {
        let entry = match requested {
            Some(requested) => {
                let provider_id =
                    ProviderId::new(requested).map_err(|error| ProviderError::InvalidInput {
                        field: "provider".to_string(),
                        message: error.to_string(),
                    })?;
                config
                    .provider(&provider_id)
                    .ok_or_else(|| ProviderError::InvalidInput {
                        field: "provider".to_string(),
                        message: format!("provider `{provider_id}` is not configured"),
                    })?
            }
            None => config.default_provider().ok_or_else(|| {
                anyhow::anyhow!("provider config does not select a default provider")
            })?,
        };
        let strategy = match entry.kind.as_str() {
            "spotify" => ProviderAuthStrategy::SpotifyOauth,
            "fake" => ProviderAuthStrategy::None,
            kind => anyhow::bail!("provider `{}` has unsupported type `{kind}`", entry.id),
        };
        Ok(ProviderAuthTarget {
            provider_id: entry.id.clone(),
            strategy,
        })
    }

    /// Auth-health target. A music-only/fake default may coexist with one
    /// secondary Spotify adapter; config validation guarantees there is at
    /// most one such adapter, so probe it rather than falsely declaring the
    /// default's no-auth strategy healthy.
    pub(crate) fn configured_health_auth_target() -> Result<ProviderAuthTarget> {
        if std::env::var_os("SPOTUIFY_FAKE_SPOTIFY").is_some() {
            return Self::configured_auth_target(None);
        }
        let loaded = spotuify_config::load().context("failed to load provider config")?;
        Self::health_auth_target_from_config(&loaded.config)
    }

    pub(crate) fn health_auth_target_from_config(
        config: &spotuify_config::AppConfig,
    ) -> Result<ProviderAuthTarget> {
        let requested = config
            .providers
            .iter()
            .find(|provider| provider.kind == "spotify")
            .map(|provider| provider.id.as_str());
        Self::auth_target_from_config(config, requested)
    }

    /// Build the configured default provider, apply every adapter decorator,
    /// probe its primary auth path once, then erase the concrete type behind a
    /// validated registry.
    pub async fn build_default_registry(
        &self,
        inputs: ProviderBuildInputs,
    ) -> Result<ProviderBuildOutcome> {
        if std::env::var_os("SPOTUIFY_FAKE_SPOTIFY").is_some() {
            // This compatibility switch powers the CLI smoke suite and has
            // always meant "simulate Spotify", not "select the standalone
            // fake namespace". Preserve Spotify identity/URIs while using
            // the deterministic fake implementation underneath.
            let fake = Arc::new(fake_spotify_provider()?);
            let local = self.local_facets_or_try_insert(fake.id(), || {
                let (backend, events) =
                    spotuify_player::backends::mock::MockPlayerBackend::new_for_provider(
                        fake.id().clone(),
                        MusicProvider::uri_scheme(fake.as_ref()).clone(),
                    );
                Ok(LocalFacets {
                    extras: None,
                    session_bearer: None,
                    player: ProviderPlayerSlot::new(ProviderPlayer::new(Box::new(backend), events)),
                })
            })?;
            let default_id = fake.id().clone();
            let runtime = ProviderRuntime::with_player_slot(
                fake,
                local.extras,
                local.player,
                TransportRecovery::RemoteOnly,
            )
            .context("provider facets failed registry validation")?;
            return Ok(ProviderBuildOutcome {
                registry: ProviderRegistry::new(default_id, [runtime])
                    .context("invalid provider registry")?,
                auth: ProviderAuthOutcome::NotRequired,
                session_bearer: None,
            });
        }

        let config = match inputs.config.clone() {
            Some(config) => config,
            None => {
                let loaded = spotuify_config::load().context("failed to load provider config")?;
                for warning in &loaded.warnings {
                    tracing::warn!(warning = %warning, "deprecated configuration");
                }
                loaded.config
            }
        };
        let default_id = config
            .default_provider
            .clone()
            .context("provider config does not select a default provider")?;
        let analytics = match AnalyticsStore::open_default().await {
            Ok(store) => Some(Arc::new(store)),
            Err(err) => {
                tracing::warn!(error = %err, "analytics store unavailable");
                None
            }
        };
        let mut runtimes = Vec::with_capacity(config.providers.len());
        let mut auth = None;
        let mut session_bearer = None;
        for entry in &config.providers {
            match entry.kind.as_str() {
                "fake" => {
                    runtimes.push(self.configured_fake_runtime(entry, &default_id)?);
                    if entry.id == default_id {
                        auth = Some(ProviderAuthOutcome::NotRequired);
                    }
                }
                "spotify" => {
                    let local = if entry.id == default_id {
                        Some(self.local_facets_or_try_insert(&entry.id, || {
                            let player = crate::player_factory::build_player(
                                entry.id.clone(),
                                UriScheme::Spotify,
                                &entry.player_settings()?,
                                inputs.player_token_slot.clone(),
                                inputs.viz_analyzer.clone(),
                            )
                            .context("failed to build provider player")?;
                            #[cfg(feature = "embedded-playback")]
                            let (extras, session_bearer) = {
                                let extras = Arc::new(SpotifySessionExtras::new(
                                    entry.id.clone(),
                                    player.session.clone(),
                                ));
                                (
                                    Some(extras.clone() as Arc<dyn ProviderExtras>),
                                    Some(extras as Arc<dyn WebApiBearerProvider>),
                                )
                            };
                            #[cfg(not(feature = "embedded-playback"))]
                            let (extras, session_bearer) = (None, None);
                            Ok(LocalFacets {
                                extras,
                                session_bearer,
                                player: ProviderPlayerSlot::new(ProviderPlayer::new(
                                    player.backend,
                                    player.stream,
                                )),
                            })
                        })?)
                    } else {
                        None
                    };
                    let mut provider_auth = inputs.auth.clone();
                    if let Some(bearer) = local
                        .as_ref()
                        .and_then(|local| local.session_bearer.clone())
                    {
                        provider_auth.first_party_bearer = Some((entry.id.clone(), bearer.clone()));
                        session_bearer = Some((entry.id.clone(), bearer));
                    }
                    let (client, requested_first_party) =
                        spotuify_spotify::config::provider_client_from_table(
                            &entry.id,
                            entry.raw_table(),
                            config.path.clone(),
                            self.rate_limiter.clone(),
                        )
                        .context("failed to decode Spotify provider config")?;
                    let first_party = cfg!(feature = "embedded-playback") && requested_first_party;
                    let mut client = self
                        .configured_client(client, entry.id.clone(), &provider_auth, first_party)?
                        .with_schema_compat_reporter(inputs.schema_compat_reporter.clone());
                    if entry.id == default_id {
                        auth = Some(probe_client_auth(&client, first_party).await);
                    }
                    if let Some(store) = analytics.clone() {
                        client = client.with_analytics(store, AnalyticsSource::Daemon);
                    }
                    let client = Arc::new(client);
                    let runtime = if let Some(local) = local {
                        ProviderRuntime::with_player_slot(
                            client,
                            local.extras,
                            local.player,
                            TransportRecovery::EmbeddedPlayer,
                        )
                    } else {
                        ProviderRuntime::with_transport(client)
                    };
                    runtimes.push(
                        runtime.context("Spotify provider facets failed registry validation")?,
                    );
                }
                kind => anyhow::bail!("provider `{}` has unsupported type `{kind}`", entry.id),
            }
        }
        let registry = ProviderRegistry::new(default_id, runtimes)
            .context("invalid configured provider registry")?;
        Ok(ProviderBuildOutcome {
            registry,
            auth: auth.context("default provider was not constructed")?,
            session_bearer,
        })
    }

    /// Probe auth without constructing or exposing a registry. Used by the
    /// daemon health loop while no client is connected.
    pub async fn probe_auth(
        &self,
        config: &spotuify_config::AppConfig,
        inputs: ProviderAuthInputs,
        requested: Option<&ProviderId>,
    ) -> Result<ProviderAuthOutcome> {
        if std::env::var_os("SPOTUIFY_FAKE_SPOTIFY").is_some() {
            return Ok(ProviderAuthOutcome::NotRequired);
        }
        let entry = match requested {
            Some(provider) => {
                config
                    .provider(provider)
                    .ok_or_else(|| ProviderError::InvalidInput {
                        field: "provider".to_string(),
                        message: format!("provider `{provider}` is not configured"),
                    })?
            }
            None => config
                .default_provider()
                .context("provider config does not select a default provider")?,
        };
        match entry.kind.as_str() {
            "fake" => Ok(ProviderAuthOutcome::NotRequired),
            "spotify" => {
                let (client, requested_first_party) =
                    spotuify_spotify::config::provider_client_from_table(
                        &entry.id,
                        entry.raw_table(),
                        config.path.clone(),
                        self.rate_limiter.clone(),
                    )
                    .context("failed to decode Spotify provider config")?;
                let first_party = cfg!(feature = "embedded-playback") && requested_first_party;
                let client =
                    self.configured_client(client, entry.id.clone(), &inputs, first_party)?;
                Ok(probe_client_auth(&client, first_party).await)
            }
            kind => anyhow::bail!("provider `{}` has unsupported type `{kind}`", entry.id),
        }
    }

    fn configured_client(
        &self,
        client: SpotifyClient,
        provider_id: ProviderId,
        auth: &ProviderAuthInputs,
        first_party: bool,
    ) -> Result<SpotifyClient> {
        let first_party_credentials_present = first_party_credentials_present(&provider_id);
        let client = client
            .with_provider_id(provider_id.clone())
            .with_token_cache(auth.token_cache.clone());
        let bearer = first_party_bearer_for_provider(
            auth,
            &provider_id,
            first_party || first_party_credentials_present,
        )?;
        if first_party {
            return Ok(client.with_bearer_provider(
                bearer.context("first-party bearer validation did not return a bearer")?,
            ));
        }
        if first_party_credentials_present {
            return Ok(client.with_write_bearer_provider(
                bearer.context("hybrid bearer validation did not return a bearer")?,
            ));
        }
        Ok(client)
    }
}

fn first_party_bearer_for_provider(
    auth: &ProviderAuthInputs,
    provider_id: &ProviderId,
    required: bool,
) -> Result<Option<Arc<dyn WebApiBearerProvider>>> {
    match auth.first_party_bearer.as_ref() {
        Some((owner, bearer)) if owner == provider_id => Ok(Some(bearer.clone())),
        Some((owner, _)) if required => anyhow::bail!(
            "provider `{provider_id}` cannot use first-party auth through embedded provider `{owner}`"
        ),
        None if required => anyhow::bail!(
            "provider `{provider_id}` cannot use first-party auth without owning the embedded player"
        ),
        _ => Ok(None),
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct FakeProviderConfig {
    dataset: Option<String>,
}

fn fake_provider(entry: &spotuify_config::ProviderEntry) -> Result<FakeProvider> {
    let config = entry
        .deserialize::<FakeProviderConfig>()
        .context("failed to decode fake provider config")?;
    let dataset = config
        .dataset
        .as_deref()
        .map(str::parse::<FakeDataset>)
        .transpose()
        .context("invalid fake provider dataset")?
        .unwrap_or_default();
    let scheme =
        UriScheme::new(entry.id.as_str()).map_err(|error| ProviderError::InvalidInput {
            field: "uri_scheme".to_string(),
            message: error.to_string(),
        })?;
    Ok(FakeProvider::with_identity(
        entry.id.clone(),
        scheme,
        dataset,
    ))
}

pub(crate) fn validate_requested_provider_id(
    configured: &ProviderId,
    requested: Option<&str>,
) -> Result<ProviderId> {
    match requested {
        None => Ok(configured.clone()),
        Some(requested) if requested == configured.as_str() => Ok(configured.clone()),
        Some(requested) => Err(ProviderError::InvalidInput {
            field: "provider".to_string(),
            message: format!("provider `{requested}` is not configured"),
        }
        .into()),
    }
}

async fn probe_client_auth(client: &SpotifyClient, first_party: bool) -> ProviderAuthOutcome {
    match client.access_token().await {
        Ok(access_token) => ProviderAuthOutcome::Authenticated {
            access_token,
            first_party,
        },
        Err(error) => ProviderAuthOutcome::Unavailable {
            error: provider_error(error, "auth_probe"),
            first_party,
        },
    }
}

fn first_party_credentials_present(provider_id: &ProviderId) -> bool {
    cfg!(feature = "embedded-playback")
        && first_party_credentials_present_with(provider_id, |provider| {
            spotuify_spotify::auth::load_first_party_credentials_for(provider)
        })
}

fn first_party_credentials_present_with<T, E>(
    provider_id: &ProviderId,
    load: impl FnOnce(&str) -> std::result::Result<Option<T>, E>,
) -> bool {
    matches!(load(provider_id.as_str()), Ok(Some(_)))
}

#[cfg(test)]
fn registry_with_player<P>(provider: Arc<P>) -> Result<ProviderRegistry>
where
    P: MusicProvider + RemoteTransport + 'static,
{
    let default_id: ProviderId = provider.id().clone();
    let (backend, events) = spotuify_player::backends::mock::MockPlayerBackend::new_for_provider(
        provider.id().clone(),
        MusicProvider::uri_scheme(provider.as_ref()).clone(),
    );
    let runtime = ProviderRuntime::with_player(
        provider,
        None,
        ProviderPlayer::new(Box::new(backend), events),
        TransportRecovery::RemoteOnly,
    )
    .context("provider facets failed registry validation")?;
    ProviderRegistry::new(default_id, [runtime]).context("invalid provider registry")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic)]

    use super::*;

    #[cfg(feature = "embedded-playback")]
    fn resource_cache(ttl: std::time::Duration) -> ProviderResourceCache {
        ProviderResourceCache {
            entries: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            ttl,
            max_entries: 8,
        }
    }

    struct StaticBearer;

    #[async_trait::async_trait]
    impl WebApiBearerProvider for StaticBearer {
        async fn bearer(&self, _force_refresh: bool) -> spotuify_spotify::SpotifyResult<String> {
            Ok("token".to_string())
        }
    }

    #[test]
    fn injected_provider_builds_one_validated_runtime() {
        let provider = Arc::new(FakeProvider::isolated("factory-test").expect("valid fake"));
        let registry = registry_with_player(provider).expect("valid registry");

        assert_eq!(registry.len(), 1);
        assert_eq!(registry.default_id().as_str(), "factory-test");
        assert!(registry.default_provider().transport().is_ok());
    }

    #[cfg(feature = "embedded-playback")]
    #[test]
    fn provider_policy_errors_are_sanitized_before_provider_error_persistence() {
        let credential = "token=Ab1Cd2Ef3Gh4".to_string();
        let error = player_provider_error(
            spotuify_player::PlayerError::ProviderPolicy(format!(
                "account restriction reported {credential}"
            )),
            "provider_resource",
        );
        let rendered = error.to_string();

        assert!(!rendered.contains(&credential));
        assert!(rendered.contains("<redacted>"));
    }

    #[tokio::test]
    async fn dual_fake_config_builds_routed_registry_and_scopes_search() {
        let loaded = spotuify_config::load_str(
            std::path::Path::new("/tmp/spotuify-dual-fake.toml"),
            r#"
[providers]
default = "fake-b"

[providers.fake-a]
type = "fake"
dataset = "standard"

[providers.fake-b]
type = "fake"
dataset = "empty"
"#,
            &spotuify_config::EnvOverrides::default(),
        )
        .expect("dual-fake config");
        let factory = ProviderFactory::new().expect("provider factory");
        factory
            .validate_config(&loaded.config)
            .expect("adapter-owned config validation");
        let default_id = loaded
            .config
            .default_provider
            .clone()
            .expect("explicit default");
        let runtimes = loaded
            .config
            .providers
            .iter()
            .map(|entry| factory.configured_fake_runtime(entry, &default_id))
            .collect::<Result<Vec<_>>>()
            .expect("configured fake runtimes");
        let registry = ProviderRegistry::new(default_id.clone(), runtimes)
            .expect("dual-fake registry is valid");

        assert_eq!(registry.len(), 2);
        assert_eq!(registry.default_id(), &default_id);
        assert_eq!(registry.default_provider().id().as_str(), "fake-b");
        assert!(registry.default_provider().has_player());
        let catalog = registry.catalog();
        assert_eq!(
            catalog
                .providers
                .iter()
                .map(|provider| (provider.id.as_str(), provider.uri_scheme.label()))
                .collect::<Vec<_>>(),
            [("fake-a", "fake-a"), ("fake-b", "fake-b")]
        );

        let fake_a = ProviderId::new("fake-a").expect("valid provider id");
        let results = registry
            .provider_or_default(Some(&fake_a))
            .expect("selected provider")
            .music()
            .search(
                spotuify_core::RequestContext::FOREGROUND,
                spotuify_core::SearchRequest {
                    query: "track two".to_string(),
                    kind: spotuify_core::MediaKind::Track,
                    page: spotuify_core::PageRequest::default(),
                },
            )
            .await
            .expect("scoped search");
        assert_eq!(results.items.len(), 1);
        assert!(results.items[0].uri.starts_with("fake-a:"));
        let routed =
            spotuify_core::ResourceUri::parse(&results.items[0].uri).expect("canonical result URI");
        assert_eq!(
            registry
                .provider_for_uri(&routed)
                .expect("URI-routed provider")
                .id(),
            &fake_a
        );

        let default_results = registry
            .provider_or_default(None)
            .expect("default provider")
            .music()
            .search(
                spotuify_core::RequestContext::FOREGROUND,
                spotuify_core::SearchRequest {
                    query: "track two".to_string(),
                    kind: spotuify_core::MediaKind::Track,
                    page: spotuify_core::PageRequest::default(),
                },
            )
            .await
            .expect("default-scoped search");
        assert!(default_results.items.is_empty());
    }

    #[test]
    fn factory_owns_shared_spotify_runtime_creation() {
        ProviderFactory::new().expect("default provider factory");
    }

    #[test]
    fn registry_rebuild_reuses_one_local_player_slot() {
        let factory = ProviderFactory::new().expect("provider factory");
        let provider = FakeProvider::isolated("slot-owner").expect("valid fake");
        let builds = std::sync::atomic::AtomicUsize::new(0);
        let build = || {
            builds.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let (backend, events) =
                spotuify_player::backends::mock::MockPlayerBackend::new_for_provider(
                    provider.id().clone(),
                    MusicProvider::uri_scheme(&provider).clone(),
                );
            Ok(LocalFacets {
                extras: None,
                session_bearer: None,
                player: ProviderPlayerSlot::new(ProviderPlayer::new(Box::new(backend), events)),
            })
        };
        let first = factory
            .local_facets_or_try_insert(provider.id(), build)
            .expect("first facets");
        let second = factory
            .local_facets_or_try_insert(provider.id(), || {
                panic!("cached player slot must prevent a second backend construction")
            })
            .expect("cached facets");

        assert_eq!(builds.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(first.player.shares_allocation_with(&second.player));
    }

    #[test]
    fn requested_provider_must_match_factory_identity() {
        let configured = ProviderId::new("spotify").expect("valid provider id");
        assert_eq!(
            validate_requested_provider_id(&configured, None).expect("default provider"),
            configured
        );
        assert_eq!(
            validate_requested_provider_id(&configured, Some("spotify"))
                .expect("configured provider"),
            configured
        );

        let error = validate_requested_provider_id(&configured, Some("fake"))
            .expect_err("unconfigured provider must fail");
        assert!(matches!(
            error.downcast_ref::<ProviderError>(),
            Some(ProviderError::InvalidInput { field, .. }) if field == "provider"
        ));
    }

    #[test]
    fn auth_strategy_is_not_inferred_from_provider_id() {
        let provider_id = ProviderId::new("spotify").expect("valid provider id");
        let injected = ProviderAuthTarget {
            provider_id: provider_id.clone(),
            strategy: ProviderAuthStrategy::None,
        };
        let built_in = ProviderAuthTarget {
            provider_id,
            strategy: ProviderAuthStrategy::SpotifyOauth,
        };

        assert_ne!(injected.strategy, built_in.strategy);
    }

    #[test]
    fn custom_provider_id_can_explicitly_select_auth() {
        let target = ProviderAuthTarget {
            provider_id: ProviderId::new("custom-cloud").expect("valid provider id"),
            strategy: ProviderAuthStrategy::SpotifyOauth,
        };
        assert_eq!(target.strategy, ProviderAuthStrategy::SpotifyOauth);
    }

    #[test]
    fn custom_provider_id_scopes_first_party_credential_lookup() {
        let provider_id = ProviderId::new("spotify-work").expect("valid provider id");
        let mut observed = None;
        let present = first_party_credentials_present_with(&provider_id, |provider| {
            observed = Some(provider.to_string());
            Ok::<_, ()>(Some(()))
        });

        assert!(present);
        assert_eq!(observed.as_deref(), Some("spotify-work"));
    }

    #[test]
    fn non_embedded_secondary_provider_cannot_borrow_first_party_bearer() {
        let work = ProviderId::new("work").expect("valid provider id");
        let fake = ProviderId::new("local").expect("valid provider id");
        let no_embedded_bearer = ProviderAuthInputs {
            token_cache: Arc::new(Mutex::new(None)),
            first_party_bearer: None,
        };
        let missing = first_party_bearer_for_provider(&no_embedded_bearer, &work, true)
            .err()
            .expect("secondary Spotify first-party mode needs its own embedded player");
        assert!(missing.to_string().contains("provider `work`"));

        let wrong_owner = ProviderAuthInputs {
            token_cache: Arc::new(Mutex::new(None)),
            first_party_bearer: Some((fake, Arc::new(StaticBearer))),
        };
        let mismatch = first_party_bearer_for_provider(&wrong_owner, &work, true)
            .err()
            .expect("a bearer from the default fake adapter must never reach work");
        assert!(mismatch.to_string().contains("embedded provider `local`"));
    }

    #[cfg(feature = "embedded-playback")]
    #[tokio::test]
    async fn provider_resource_cache_hit_skips_fetch() {
        let cache = resource_cache(std::time::Duration::from_secs(60));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for _ in 0..2 {
            let calls = calls.clone();
            assert_eq!(
                cache
                    .get_or_fetch("hm://one".to_string(), move || async move {
                        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Ok(bytes::Bytes::from_static(b"one"))
                    })
                    .await
                    .expect("resource fetch"),
                bytes::Bytes::from_static(b"one")
            );
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[cfg(feature = "embedded-playback")]
    #[tokio::test]
    async fn provider_resource_cache_refetches_after_ttl() {
        let cache = resource_cache(std::time::Duration::from_millis(1));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for attempt in 0..2 {
            if attempt == 1 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            let calls = calls.clone();
            cache
                .get_or_fetch("hm://ttl".to_string(), move || async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(bytes::Bytes::from_static(b"ttl"))
                })
                .await
                .expect("resource fetch");
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[cfg(feature = "embedded-playback")]
    #[tokio::test]
    async fn provider_resource_cache_isolates_uris() {
        let cache = resource_cache(std::time::Duration::from_secs(60));
        for (uri, value) in [("hm://a", "a"), ("hm://b", "b")] {
            let value = value.to_string();
            let fetched = cache
                .get_or_fetch(uri.to_string(), move || async move {
                    Ok(bytes::Bytes::from(value))
                })
                .await
                .expect("resource fetch");
            assert_eq!(fetched.as_ref(), uri.trim_start_matches("hm://").as_bytes());
        }
    }

    #[cfg(feature = "embedded-playback")]
    #[tokio::test]
    async fn provider_resource_cache_errors_do_not_poison_retry() {
        let cache = resource_cache(std::time::Duration::from_secs(60));
        let first = cache
            .get_or_fetch("hm://retry".to_string(), || async {
                Err(ProviderError::Transient {
                    status: None,
                    message: "temporary".to_string(),
                })
            })
            .await;
        assert!(first.is_err());
        let second = cache
            .get_or_fetch("hm://retry".to_string(), || async {
                Ok(bytes::Bytes::from_static(b"recovered"))
            })
            .await
            .expect("failed fetch must be retried");
        assert_eq!(second, bytes::Bytes::from_static(b"recovered"));
    }

    #[cfg(feature = "embedded-playback")]
    #[tokio::test]
    async fn provider_resource_cache_coalesces_concurrent_fetches() {
        let cache = resource_cache(std::time::Duration::from_secs(60));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let first = {
            let cache = cache.clone();
            let calls = calls.clone();
            let started = started.clone();
            let release = release.clone();
            tokio::spawn(async move {
                cache
                    .get_or_fetch("hm://shared".to_string(), move || async move {
                        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        started.notify_one();
                        release.notified().await;
                        Ok(bytes::Bytes::from_static(b"shared"))
                    })
                    .await
            })
        };
        started.notified().await;
        let second = {
            let cache = cache.clone();
            let calls = calls.clone();
            tokio::spawn(async move {
                cache
                    .get_or_fetch("hm://shared".to_string(), move || async move {
                        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Ok(bytes::Bytes::from_static(b"unexpected"))
                    })
                    .await
            })
        };
        tokio::task::yield_now().await;
        release.notify_one();
        assert_eq!(
            first
                .await
                .expect("first task")
                .expect("first fetch")
                .as_ref(),
            b"shared"
        );
        assert_eq!(
            second
                .await
                .expect("second task")
                .expect("second fetch")
                .as_ref(),
            b"shared"
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
