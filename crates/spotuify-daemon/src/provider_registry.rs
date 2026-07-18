//! Validated provider handles and provider-neutral daemon routing.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use spotuify_core::{
    MediaKind, MusicProvider, ProviderCaps, ProviderCatalog, ProviderDescriptor, ProviderError,
    ProviderExtras, ProviderId, ProviderResult, RemoteTransport, ResolvedTarget, ResourceUri,
    TargetClaim, UriScheme,
};

/// Recovery behavior owned by the concrete transport integration.
///
/// URI namespaces do not identify adapter implementations. Only the built-in
/// Spotify factory may opt its runtime into the embedded librespot path;
/// injected and fake adapters remain remote-only even when they own
/// `spotify:` resources.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TransportRecovery {
    #[default]
    RemoteOnly,
    EmbeddedPlayer,
}

pub(crate) struct ProviderPlayer {
    pub(crate) backend: Box<dyn spotuify_player::PlayerBackend>,
    pub(crate) events:
        tokio_stream::wrappers::UnboundedReceiverStream<spotuify_player::PlayerEvent>,
}

impl ProviderPlayer {
    pub(crate) fn new(
        backend: Box<dyn spotuify_player::PlayerBackend>,
        events: tokio_stream::wrappers::UnboundedReceiverStream<spotuify_player::PlayerEvent>,
    ) -> Self {
        Self { backend, events }
    }
}

/// Shareable single-consumer slot retained by the provider factory across
/// registry rebuilds. Taking the player for actor installation does not sever
/// the registry's identity from its original session/extras allocation.
#[derive(Clone)]
pub(crate) struct ProviderPlayerSlot {
    player: Arc<Mutex<Option<ProviderPlayer>>>,
    available: bool,
}

impl ProviderPlayerSlot {
    pub(crate) fn new(player: ProviderPlayer) -> Self {
        Self {
            player: Arc::new(Mutex::new(Some(player))),
            available: true,
        }
    }

    fn empty() -> Self {
        Self {
            player: Arc::new(Mutex::new(None)),
            available: false,
        }
    }

    fn take(&self) -> ProviderResult<Option<ProviderPlayer>> {
        self.player
            .lock()
            .map(|mut player| player.take())
            .map_err(|_| ProviderError::Provider("provider player slot poisoned".to_string()))
    }

    fn restore(&self, player: ProviderPlayer) -> ProviderResult<()> {
        let mut slot = self
            .player
            .lock()
            .map_err(|_| ProviderError::Provider("provider player slot poisoned".to_string()))?;
        if slot.is_some() {
            return Err(invalid_input(
                "player",
                "provider player slot is already occupied".to_string(),
            ));
        }
        *slot = Some(player);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn shares_allocation_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.player, &other.player)
    }
}

/// Shared handles for one configured provider instance.
///
/// Construction validates the identity and capabilities once so callers can
/// route without repeating defensive checks at every dispatch site.
#[derive(Clone)]
pub struct ProviderRuntime {
    music: Arc<dyn MusicProvider>,
    transport: Option<Arc<dyn RemoteTransport>>,
    extras: Option<Arc<dyn ProviderExtras>>,
    player: ProviderPlayerSlot,
    capabilities: ProviderCaps,
    transport_recovery: TransportRecovery,
}

impl ProviderRuntime {
    /// Register an adapter that deliberately has no remote-transport facet.
    pub fn music_only(music: Arc<dyn MusicProvider>) -> ProviderResult<Self> {
        Self::from_facets(
            music,
            None,
            None,
            ProviderPlayerSlot::empty(),
            TransportRecovery::RemoteOnly,
        )
    }

    /// Register both facets from one adapter allocation.
    ///
    /// Accepting the concrete shared `Arc` here makes it impossible to pair a
    /// music provider with transport state from a different adapter instance.
    pub fn with_transport<P>(provider: Arc<P>) -> ProviderResult<Self>
    where
        P: MusicProvider + RemoteTransport + 'static,
    {
        let music: Arc<dyn MusicProvider> = provider.clone();
        let transport: Arc<dyn RemoteTransport> = provider;
        Self::from_facets(
            music,
            Some(transport),
            None,
            ProviderPlayerSlot::empty(),
            TransportRecovery::RemoteOnly,
        )
    }

    /// Register transport and semantic extras supplied by the same provider
    /// integration. Facet identity is validated before the runtime is exposed.
    pub fn with_transport_and_extras<P>(
        provider: Arc<P>,
        extras: Arc<dyn ProviderExtras>,
    ) -> ProviderResult<Self>
    where
        P: MusicProvider + RemoteTransport + 'static,
    {
        let music: Arc<dyn MusicProvider> = provider.clone();
        let transport: Arc<dyn RemoteTransport> = provider;
        Self::from_facets(
            music,
            Some(transport),
            Some(extras),
            ProviderPlayerSlot::empty(),
            TransportRecovery::RemoteOnly,
        )
    }

    #[cfg(test)]
    pub(crate) fn with_player<P>(
        provider: Arc<P>,
        extras: Option<Arc<dyn ProviderExtras>>,
        player: ProviderPlayer,
        transport_recovery: TransportRecovery,
    ) -> ProviderResult<Self>
    where
        P: MusicProvider + RemoteTransport + 'static,
    {
        let music: Arc<dyn MusicProvider> = provider.clone();
        let transport: Arc<dyn RemoteTransport> = provider;
        Self::from_facets(
            music,
            Some(transport),
            extras,
            ProviderPlayerSlot::new(player),
            transport_recovery,
        )
    }

    pub(crate) fn with_player_slot<P>(
        provider: Arc<P>,
        extras: Option<Arc<dyn ProviderExtras>>,
        player: ProviderPlayerSlot,
        transport_recovery: TransportRecovery,
    ) -> ProviderResult<Self>
    where
        P: MusicProvider + RemoteTransport + 'static,
    {
        let music: Arc<dyn MusicProvider> = provider.clone();
        let transport: Arc<dyn RemoteTransport> = provider;
        Self::from_facets(music, Some(transport), extras, player, transport_recovery)
    }

    fn from_facets(
        music: Arc<dyn MusicProvider>,
        transport: Option<Arc<dyn RemoteTransport>>,
        extras: Option<Arc<dyn ProviderExtras>>,
        player: ProviderPlayerSlot,
        transport_recovery: TransportRecovery,
    ) -> ProviderResult<Self> {
        let provider_id = music.id();
        let uri_scheme = music.uri_scheme();
        let mut capabilities = music.capabilities();

        let expects_transport = capabilities.transport.is_some();
        validate_transport_presence("transport", expects_transport, transport.is_some())?;

        if let Some(transport) = transport.as_ref() {
            validate_transport_identity("transport", provider_id, uri_scheme, transport)?;
        }
        if let Some(extras) = extras.as_ref() {
            validate_extras_identity("extras", provider_id, uri_scheme, extras)?;
            capabilities.extras = extras.capabilities();
        } else {
            capabilities.extras = Default::default();
        }
        if let Some(player) = player
            .player
            .lock()
            .map_err(|_| ProviderError::Provider("provider player slot poisoned".to_string()))?
            .as_ref()
        {
            if transport.is_none() {
                return Err(invalid_input(
                    "player",
                    "a local player cannot be paired to a transportless provider".to_string(),
                ));
            }
            spotuify_player::validate_backend_pairing(
                provider_id,
                uri_scheme,
                Some(player.backend.as_ref()),
            )
            .map_err(|error| invalid_input("player", error.to_string()))?;
        }
        if transport_recovery == TransportRecovery::EmbeddedPlayer && !player.available {
            return Err(invalid_input(
                "transport_recovery",
                "embedded-player recovery requires a paired local player".to_string(),
            ));
        }

        Ok(Self {
            music,
            transport,
            extras,
            player,
            capabilities,
            transport_recovery,
        })
    }

    pub fn id(&self) -> &ProviderId {
        self.music.id()
    }

    pub fn uri_scheme(&self) -> &UriScheme {
        self.music.uri_scheme()
    }

    pub fn capabilities(&self) -> &ProviderCaps {
        &self.capabilities
    }

    pub fn music(&self) -> Arc<dyn MusicProvider> {
        self.music.clone()
    }

    pub fn transport(&self) -> ProviderResult<Arc<dyn RemoteTransport>> {
        self.transport
            .clone()
            .ok_or_else(|| ProviderError::unsupported("remote_transport"))
    }

    pub fn extras(&self) -> ProviderResult<Arc<dyn ProviderExtras>> {
        self.extras
            .clone()
            .ok_or_else(|| ProviderError::unsupported("provider_extras"))
    }

    pub(crate) fn has_player(&self) -> bool {
        self.player.available
    }

    pub(crate) fn take_player(&self) -> ProviderResult<Option<ProviderPlayer>> {
        self.player.take()
    }

    pub(crate) fn restore_player(&self, player: ProviderPlayer) -> ProviderResult<()> {
        self.player.restore(player)
    }

    pub(crate) fn transport_recovery(&self) -> TransportRecovery {
        self.transport_recovery
    }
}

/// Provider runtimes indexed by both configured identity and URI namespace.
#[derive(Clone)]
pub struct ProviderRegistry {
    default_id: ProviderId,
    providers: BTreeMap<ProviderId, ProviderRuntime>,
    schemes: BTreeMap<UriScheme, ProviderId>,
}

impl ProviderRegistry {
    pub fn new(
        default_id: ProviderId,
        runtimes: impl IntoIterator<Item = ProviderRuntime>,
    ) -> ProviderResult<Self> {
        let mut providers = BTreeMap::new();
        let mut schemes = BTreeMap::new();

        for runtime in runtimes {
            let provider_id = runtime.id().clone();
            let uri_scheme = runtime.uri_scheme().clone();
            if providers.contains_key(&provider_id) {
                return Err(invalid_input(
                    "provider_id",
                    format!("duplicate provider id `{provider_id}`"),
                ));
            }
            if let Some(existing) = schemes.get(&uri_scheme) {
                return Err(invalid_input(
                    "uri_scheme",
                    format!(
                        "URI scheme `{uri_scheme}` is already registered to provider `{existing}`"
                    ),
                ));
            }
            schemes.insert(uri_scheme, provider_id.clone());
            providers.insert(provider_id, runtime);
        }

        if !providers.contains_key(&default_id) {
            return Err(invalid_input(
                "default_provider",
                format!("provider `{default_id}` is not registered"),
            ));
        }
        let player_providers = providers
            .values()
            .filter(|runtime| runtime.has_player())
            .map(ProviderRuntime::id)
            .collect::<Vec<_>>();
        if player_providers.len() > 1 {
            return Err(invalid_input(
                "player",
                "only one provider player can be installed by this daemon".to_string(),
            ));
        }
        if player_providers
            .first()
            .is_some_and(|provider| *provider != &default_id)
        {
            return Err(invalid_input(
                "player",
                "the provider player must belong to the default provider".to_string(),
            ));
        }

        Ok(Self {
            default_id,
            providers,
            schemes,
        })
    }

    pub fn default_id(&self) -> &ProviderId {
        &self.default_id
    }

    pub fn default_provider(&self) -> &ProviderRuntime {
        // `new` rejects a missing default provider.
        &self.providers[&self.default_id]
    }

    pub(crate) fn embedded_player_provider_id(&self) -> Option<&ProviderId> {
        self.providers
            .values()
            .find(|runtime| runtime.has_player())
            .map(ProviderRuntime::id)
    }

    pub fn provider(&self, provider_id: &ProviderId) -> ProviderResult<&ProviderRuntime> {
        self.providers
            .get(provider_id)
            .ok_or_else(|| ProviderError::NotFound {
                resource: format!("provider:{provider_id}"),
            })
    }

    pub fn provider_for_scheme(&self, scheme: &UriScheme) -> ProviderResult<&ProviderRuntime> {
        let provider_id = self
            .schemes
            .get(scheme)
            .ok_or_else(|| ProviderError::NotFound {
                resource: format!("provider-scheme:{scheme}"),
            })?;
        self.provider(provider_id)
    }

    pub fn provider_for_uri(&self, uri: &ResourceUri) -> ProviderResult<&ProviderRuntime> {
        self.provider_for_scheme(uri.scheme())
    }

    /// Resolve an optional request route without conflating adapter identity
    /// with the URI namespace it owns.
    pub fn provider_or_default(
        &self,
        provider_id: Option<&ProviderId>,
    ) -> ProviderResult<&ProviderRuntime> {
        match provider_id {
            Some(provider_id) => self.provider(provider_id),
            None => Ok(self.default_provider()),
        }
    }

    /// Stable client-facing catalog derived from the validated registry.
    pub fn catalog(&self) -> ProviderCatalog {
        let catalog = ProviderCatalog {
            default_provider: Some(self.default_id.clone()),
            providers: self
                .iter()
                .map(|(provider_id, runtime)| ProviderDescriptor {
                    id: provider_id.clone(),
                    uri_scheme: runtime.uri_scheme().clone(),
                    display_name: runtime.music().display_name().to_string(),
                    capabilities: runtime.capabilities().clone(),
                    is_default: provider_id == &self.default_id,
                })
                .collect(),
        };
        debug_assert!(catalog.validate().is_ok());
        catalog
    }

    /// Normalize raw user input through one explicit adapter or all adapters.
    ///
    /// The all-provider path is deterministic because the registry is a
    /// `BTreeMap`. Multiple successful claims are rejected instead of picking
    /// whichever adapter happened to answer first.
    pub fn resolve_target(
        &self,
        input: &str,
        provider_id: Option<&ProviderId>,
        expected_kinds: Option<&[MediaKind]>,
    ) -> ProviderResult<Option<ResolvedTarget>> {
        if let Some(provider_id) = provider_id {
            let runtime = self.provider(provider_id)?;
            return match runtime.music().claim_target(input) {
                TargetClaim::NotMine => Ok(None),
                TargetClaim::Resolved(uri) => {
                    validate_claimed_uri(runtime, &uri)?;
                    enforce_expected_kind(&uri, expected_kinds)?;
                    Ok(Some(ResolvedTarget {
                        provider: provider_id.clone(),
                        uri,
                    }))
                }
                TargetClaim::Invalid { message } => Err(invalid_input(
                    "input",
                    format!("provider `{provider_id}` rejected target: {message}"),
                )),
            };
        }

        let mut resolved = Vec::new();
        let mut invalid = Vec::new();
        for (provider_id, runtime) in self.iter() {
            match runtime.music().claim_target(input) {
                TargetClaim::NotMine => {}
                TargetClaim::Resolved(uri) => {
                    validate_claimed_uri(runtime, &uri)?;
                    resolved.push(ResolvedTarget {
                        provider: provider_id.clone(),
                        uri,
                    });
                }
                TargetClaim::Invalid { message } => invalid.push((provider_id, message)),
            }
        }

        if !resolved.is_empty() && !invalid.is_empty() {
            return Err(invalid_input(
                "input",
                format!(
                    "target claim conflict: providers {} resolved it, while {} rejected it as malformed",
                    resolved
                        .iter()
                        .map(|target| format!("`{}`", target.provider))
                        .collect::<Vec<_>>()
                        .join(", "),
                    invalid
                        .iter()
                        .map(|(provider, _)| format!("`{provider}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }

        match resolved.as_slice() {
            [] if invalid.is_empty() => Ok(None),
            [] => Err(invalid_input(
                "input",
                invalid
                    .into_iter()
                    .map(|(provider, message)| format!("provider `{provider}`: {message}"))
                    .collect::<Vec<_>>()
                    .join("; "),
            )),
            [target] => {
                enforce_expected_kind(&target.uri, expected_kinds)?;
                Ok(Some(target.clone()))
            }
            targets => Err(invalid_input(
                "input",
                format!(
                    "target is ambiguous between providers {}",
                    targets
                        .iter()
                        .map(|target| format!("`{}`", target.provider))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )),
        }
    }

    /// Iterate providers in stable [`ProviderId`] order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&ProviderId, &ProviderRuntime)> {
        self.providers.iter()
    }

    pub fn len(&self) -> usize {
        self.providers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

fn validate_claimed_uri(runtime: &ProviderRuntime, uri: &ResourceUri) -> ProviderResult<()> {
    if uri.scheme() == runtime.uri_scheme() {
        return Ok(());
    }
    Err(invalid_input(
        "input",
        format!(
            "provider `{}` claimed foreign URI namespace `{}`",
            runtime.id(),
            uri.scheme()
        ),
    ))
}

fn enforce_expected_kind(
    uri: &ResourceUri,
    expected_kinds: Option<&[MediaKind]>,
) -> ProviderResult<()> {
    let Some(expected_kinds) = expected_kinds else {
        return Ok(());
    };
    let actual = uri.kind();
    if expected_kinds.contains(&actual) {
        return Ok(());
    }
    Err(invalid_input(
        "expected_kinds",
        format!("resolved target has kind `{actual}`, which is not allowed"),
    ))
}

fn validate_transport_identity(
    field: &str,
    expected_id: &ProviderId,
    expected_scheme: &UriScheme,
    transport: &Arc<dyn RemoteTransport>,
) -> ProviderResult<()> {
    validate_value(
        &format!("{field}.provider_id"),
        expected_id,
        transport.provider_id(),
    )?;
    validate_value(
        &format!("{field}.uri_scheme"),
        expected_scheme,
        transport.uri_scheme(),
    )
}

fn validate_extras_identity(
    field: &str,
    expected_id: &ProviderId,
    expected_scheme: &UriScheme,
    extras: &Arc<dyn ProviderExtras>,
) -> ProviderResult<()> {
    validate_value(
        &format!("{field}.provider_id"),
        expected_id,
        extras.provider_id(),
    )?;
    validate_value(
        &format!("{field}.uri_scheme"),
        expected_scheme,
        extras.uri_scheme(),
    )
}

fn validate_transport_presence(field: &str, expected: bool, actual: bool) -> ProviderResult<()> {
    if expected == actual {
        return Ok(());
    }
    let expectation = if expected { "present" } else { "absent" };
    Err(invalid_input(
        field,
        format!("transport handle must be {expectation} to match provider capabilities"),
    ))
}

fn validate_value<T>(field: &str, expected: &T, actual: &T) -> ProviderResult<()>
where
    T: std::fmt::Display + PartialEq,
{
    if expected == actual {
        return Ok(());
    }
    Err(invalid_input(
        field,
        format!("expected `{expected}`, got `{actual}`"),
    ))
}

fn invalid_input(field: impl Into<String>, message: impl Into<String>) -> ProviderError {
    ProviderError::InvalidInput {
        field: field.into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use spotuify_core::{
        LibraryRequest, MediaKind, MusicProvider, Mutation, PageRequest, ProviderCaps,
        ProviderError, ProviderId, RemoteTransport, RequestContext, ResourceUri, TargetClaim,
        TransportCommand, UriScheme,
    };
    use spotuify_provider_fake::{FakeDataset, FakeProvider};
    use uuid::Uuid;

    use super::{ProviderPlayer, ProviderRegistry, ProviderRuntime, TransportRecovery};

    #[derive(Clone)]
    struct CapsOverride {
        inner: FakeProvider,
        capabilities: ProviderCaps,
    }

    impl MusicProvider for CapsOverride {
        fn id(&self) -> &ProviderId {
            MusicProvider::id(&self.inner)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(&self.inner)
        }

        fn display_name(&self) -> &str {
            self.inner.display_name()
        }

        fn capabilities(&self) -> ProviderCaps {
            self.capabilities.clone()
        }
    }

    impl RemoteTransport for CapsOverride {
        fn provider_id(&self) -> &ProviderId {
            MusicProvider::id(self)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(self)
        }
    }

    #[derive(Clone)]
    struct TransportIdentityOverride {
        inner: FakeProvider,
        transport_id: ProviderId,
        transport_scheme: UriScheme,
    }

    impl MusicProvider for TransportIdentityOverride {
        fn id(&self) -> &ProviderId {
            MusicProvider::id(&self.inner)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(&self.inner)
        }

        fn display_name(&self) -> &str {
            self.inner.display_name()
        }

        fn capabilities(&self) -> ProviderCaps {
            self.inner.capabilities()
        }
    }

    impl RemoteTransport for TransportIdentityOverride {
        fn provider_id(&self) -> &ProviderId {
            &self.transport_id
        }

        fn uri_scheme(&self) -> &UriScheme {
            &self.transport_scheme
        }
    }

    #[derive(Clone)]
    struct ClaimOverride {
        inner: FakeProvider,
        claimed_input: String,
        invalid_input: String,
        claimed_scheme: Option<UriScheme>,
    }

    impl MusicProvider for ClaimOverride {
        fn id(&self) -> &ProviderId {
            MusicProvider::id(&self.inner)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(&self.inner)
        }

        fn display_name(&self) -> &str {
            self.inner.display_name()
        }

        fn capabilities(&self) -> ProviderCaps {
            self.inner.capabilities()
        }

        fn claim_target(&self, input: &str) -> TargetClaim {
            if input == self.claimed_input {
                return TargetClaim::Resolved(
                    ResourceUri::new(
                        self.claimed_scheme
                            .clone()
                            .unwrap_or_else(|| MusicProvider::uri_scheme(self).clone()),
                        MediaKind::Track,
                        "claimed-track",
                    )
                    .expect("claim URI is valid"),
                );
            }
            if input == self.invalid_input {
                return TargetClaim::Invalid {
                    message: "malformed share URL".to_string(),
                };
            }
            TargetClaim::NotMine
        }
    }

    impl RemoteTransport for ClaimOverride {
        fn provider_id(&self) -> &ProviderId {
            MusicProvider::id(self)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(self)
        }
    }

    fn fake_runtime(namespace: &str) -> ProviderRuntime {
        let id = ProviderId::new(namespace).expect("valid provider id");
        let scheme = UriScheme::new(namespace).expect("valid URI scheme");
        fake_runtime_with_identity(id, scheme)
    }

    fn fake_runtime_with_identity(id: ProviderId, scheme: UriScheme) -> ProviderRuntime {
        let provider = Arc::new(FakeProvider::with_identity(
            id,
            scheme,
            FakeDataset::Standard,
        ));
        ProviderRuntime::with_transport(provider).expect("fake runtime is internally consistent")
    }

    fn claiming_runtime(namespace: &str) -> ProviderRuntime {
        let provider = Arc::new(ClaimOverride {
            inner: FakeProvider::isolated(namespace).expect("valid fake"),
            claimed_input: "https://shared.example/track/1".to_string(),
            invalid_input: "https://shared.example/broken".to_string(),
            claimed_scheme: None,
        });
        ProviderRuntime::with_transport(provider).expect("claiming runtime is valid")
    }

    #[test]
    fn routes_default_id_scheme_and_uri_across_isolated_providers() {
        let provider_a = fake_runtime("fake-a");
        let provider_b = fake_runtime("fake-b");
        let default_id = ProviderId::new("fake-b").expect("valid provider id");
        let registry = ProviderRegistry::new(default_id.clone(), [provider_a, provider_b])
            .expect("two isolated providers are valid");

        assert_eq!(registry.len(), 2);
        assert_eq!(registry.default_id(), &default_id);
        assert_eq!(registry.default_provider().music().id(), &default_id);
        assert_eq!(
            registry
                .provider(&ProviderId::new("fake-a").expect("valid provider id"))
                .expect("provider exists")
                .id()
                .as_str(),
            "fake-a"
        );
        assert_eq!(
            registry
                .provider_for_scheme(&UriScheme::new("fake-b").expect("valid URI scheme"))
                .expect("scheme is registered")
                .id()
                .as_str(),
            "fake-b"
        );
        let uri = ResourceUri::new(
            UriScheme::new("fake-a").expect("valid URI scheme"),
            spotuify_core::MediaKind::Track,
            "track-1",
        )
        .expect("valid resource URI");
        assert_eq!(
            registry
                .provider_for_uri(&uri)
                .expect("URI is routed")
                .id()
                .as_str(),
            "fake-a"
        );
    }

    #[test]
    fn iterates_providers_in_stable_id_order() {
        let registry = ProviderRegistry::new(
            ProviderId::new("fake-b").expect("valid provider id"),
            [
                fake_runtime("fake-c"),
                fake_runtime("fake-a"),
                fake_runtime("fake-b"),
            ],
        )
        .expect("registry is valid");

        let ids = registry
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["fake-a", "fake-b", "fake-c"]);
    }

    #[test]
    fn catalog_preserves_custom_identity_scheme_and_one_default_marker() {
        let registry = ProviderRegistry::new(
            ProviderId::new("custom-cloud").expect("valid provider id"),
            [
                fake_runtime_with_identity(
                    ProviderId::new("custom-cloud").expect("valid provider id"),
                    UriScheme::Spotify,
                ),
                fake_runtime("fake-b"),
            ],
        )
        .expect("registry is valid");

        let catalog = registry.catalog();
        assert_eq!(
            catalog.default_provider.as_ref().map(ProviderId::as_str),
            Some("custom-cloud")
        );
        assert!(catalog.validate().is_ok());
        assert_eq!(catalog.providers.len(), 2);
        let custom = catalog
            .providers
            .iter()
            .find(|provider| provider.id.as_str() == "custom-cloud")
            .expect("custom provider is catalogued");
        assert_eq!(custom.uri_scheme, UriScheme::Spotify);
        assert!(custom.is_default);
        assert_eq!(
            catalog
                .providers
                .iter()
                .filter(|provider| provider.is_default)
                .count(),
            1
        );
    }

    #[test]
    fn resolve_target_is_tri_state_kind_checked_and_deterministic() {
        let registry = ProviderRegistry::new(
            ProviderId::new("fake-b").expect("valid provider id"),
            [claiming_runtime("fake-b"), claiming_runtime("fake-a")],
        )
        .expect("registry is valid");
        let fake_b = ProviderId::new("fake-b").expect("valid provider id");

        let explicit = registry
            .resolve_target(
                "https://shared.example/track/1",
                Some(&fake_b),
                Some(&[MediaKind::Track]),
            )
            .expect("explicit claim resolves")
            .expect("target is claimed");
        assert_eq!(explicit.provider, fake_b);
        assert_eq!(explicit.uri.as_uri(), "fake-b:track:claimed-track");
        assert!(registry
            .resolve_target("ordinary search", Some(&explicit.provider), None)
            .expect("not-mine is not an error")
            .is_none());

        let wrong_kind = registry
            .resolve_target(
                "https://shared.example/track/1",
                Some(&explicit.provider),
                Some(&[MediaKind::Album]),
            )
            .expect_err("unexpected kinds must fail");
        assert!(matches!(
            wrong_kind,
            ProviderError::InvalidInput { field, .. } if field == "expected_kinds"
        ));

        let ambiguous = registry
            .resolve_target("https://shared.example/track/1", None, None)
            .expect_err("multiple claims must fail");
        assert!(matches!(
            ambiguous,
            ProviderError::InvalidInput { field, message }
                if field == "input"
                    && message.contains("`fake-a`, `fake-b`")
        ));

        let invalid = registry
            .resolve_target("https://shared.example/broken", None, None)
            .expect_err("recognized malformed targets must fail");
        assert!(matches!(
            invalid,
            ProviderError::InvalidInput { field, message }
                if field == "input"
                    && message.starts_with("provider `fake-a`: malformed share URL")
        ));
    }

    #[test]
    fn resolve_target_rejects_a_provider_claiming_a_foreign_namespace() {
        let provider = Arc::new(ClaimOverride {
            inner: FakeProvider::isolated("fake-a").expect("valid fake"),
            claimed_input: "https://foreign.example/track/1".to_string(),
            invalid_input: String::new(),
            claimed_scheme: Some(UriScheme::new("fake-b").expect("valid URI scheme")),
        });
        let runtime = ProviderRuntime::with_transport(provider).expect("runtime is valid");
        let provider_id = runtime.id().clone();
        let registry =
            ProviderRegistry::new(provider_id.clone(), [runtime]).expect("registry is valid");

        let error = registry
            .resolve_target("https://foreign.example/track/1", Some(&provider_id), None)
            .expect_err("foreign namespace claims must fail closed");
        assert!(matches!(
            error,
            ProviderError::InvalidInput { field, message }
                if field == "input" && message.contains("foreign URI namespace `fake-b`")
        ));
    }

    #[test]
    fn resolve_target_rejects_resolved_and_invalid_claim_conflicts() {
        let input = "https://shared.example/conflict";
        let resolved = Arc::new(ClaimOverride {
            inner: FakeProvider::isolated("fake-a").expect("valid fake"),
            claimed_input: input.to_string(),
            invalid_input: String::new(),
            claimed_scheme: None,
        });
        let invalid = Arc::new(ClaimOverride {
            inner: FakeProvider::isolated("fake-b").expect("valid fake"),
            claimed_input: String::new(),
            invalid_input: input.to_string(),
            claimed_scheme: None,
        });
        let registry = ProviderRegistry::new(
            ProviderId::new("fake-a").expect("valid provider id"),
            [
                ProviderRuntime::with_transport(resolved).expect("resolved runtime is valid"),
                ProviderRuntime::with_transport(invalid).expect("invalid runtime is valid"),
            ],
        )
        .expect("registry is valid");

        let error = registry
            .resolve_target(input, None, None)
            .expect_err("conflicting claims must fail closed");
        assert!(matches!(
            error,
            ProviderError::InvalidInput { field, message }
                if field == "input"
                    && message.contains("`fake-a`")
                    && message.contains("`fake-b`")
                    && message.contains("conflict")
        ));
    }

    #[test]
    fn spotify_namespace_does_not_enable_embedded_recovery() {
        let runtime = fake_runtime_with_identity(
            ProviderId::new("custom-cloud").expect("valid provider id"),
            UriScheme::Spotify,
        );
        assert_eq!(runtime.transport_recovery(), TransportRecovery::RemoteOnly);

        let registry = ProviderRegistry::new(runtime.id().clone(), [runtime])
            .expect("custom provider is valid");
        assert_eq!(
            registry.default_provider().transport_recovery(),
            TransportRecovery::RemoteOnly
        );
    }

    #[test]
    fn embedded_recovery_requires_a_paired_player() {
        let provider = Arc::new(FakeProvider::with_identity(
            ProviderId::new("spotify-test").expect("valid provider id"),
            UriScheme::Spotify,
            FakeDataset::Standard,
        ));
        let (backend, events) =
            spotuify_player::backends::mock::MockPlayerBackend::new_for_provider(
                provider.id().clone(),
                MusicProvider::uri_scheme(provider.as_ref()).clone(),
            );
        let runtime = ProviderRuntime::with_player(
            provider,
            None,
            ProviderPlayer::new(Box::new(backend), events),
            TransportRecovery::EmbeddedPlayer,
        )
        .expect("paired embedded runtime is valid");
        assert_eq!(
            runtime.transport_recovery(),
            TransportRecovery::EmbeddedPlayer
        );
    }

    #[test]
    fn registry_rejects_multiple_player_facets() {
        let player_runtime = |id: &str| {
            let provider = Arc::new(FakeProvider::isolated(id).expect("valid fake"));
            let (backend, events) =
                spotuify_player::backends::mock::MockPlayerBackend::new_for_provider(
                    provider.id().clone(),
                    MusicProvider::uri_scheme(provider.as_ref()).clone(),
                );
            ProviderRuntime::with_player(
                provider,
                None,
                ProviderPlayer::new(Box::new(backend), events),
                TransportRecovery::RemoteOnly,
            )
            .expect("valid player runtime")
        };
        let error = ProviderRegistry::new(
            ProviderId::new("player-a").expect("valid provider"),
            [player_runtime("player-a"), player_runtime("player-b")],
        )
        .err()
        .expect("one daemon actor cannot own multiple player facets");
        assert!(matches!(
            error,
            ProviderError::InvalidInput { field, .. } if field == "player"
        ));
    }

    #[test]
    fn registry_rejects_player_on_non_default_provider() {
        let default = fake_runtime("default-a");
        let provider = Arc::new(FakeProvider::isolated("player-b").expect("valid fake"));
        let (backend, events) =
            spotuify_player::backends::mock::MockPlayerBackend::new_for_provider(
                provider.id().clone(),
                MusicProvider::uri_scheme(provider.as_ref()).clone(),
            );
        let player = ProviderRuntime::with_player(
            provider,
            None,
            ProviderPlayer::new(Box::new(backend), events),
            TransportRecovery::RemoteOnly,
        )
        .expect("valid player runtime");
        let error = ProviderRegistry::new(
            ProviderId::new("default-a").expect("valid provider"),
            [default, player],
        )
        .err()
        .expect("the installed player must belong to the default provider");
        assert!(matches!(
            error,
            ProviderError::InvalidInput { field, .. } if field == "player"
        ));
    }

    #[tokio::test]
    async fn transport_runtime_facets_share_one_adapter_instance() {
        let provider = Arc::new(FakeProvider::isolated("fake-a").expect("valid fake"));
        let runtime =
            ProviderRuntime::with_transport(provider.clone()).expect("fake runtime is valid");
        let music = runtime.music();
        let transport = runtime.transport().expect("fake has transport");
        assert_eq!(
            Arc::as_ptr(&music) as *const (),
            Arc::as_ptr(&transport) as *const (),
            "both facets must erase the same Arc allocation"
        );
        let saved_uri = ResourceUri::parse("fake-a:track:track-2").expect("valid resource URI");

        music
            .apply_mutation(
                RequestContext::FOREGROUND,
                Uuid::now_v7(),
                &Mutation::LibrarySave {
                    uris: vec![saved_uri],
                },
            )
            .await
            .expect("mutation succeeds");
        transport
            .execute(RequestContext::BACKGROUND_SYNC, TransportCommand::Pause)
            .await
            .expect("transport mutation succeeds");
        let library = music
            .library_items(
                RequestContext::BACKGROUND_SYNC,
                LibraryRequest {
                    kind: MediaKind::Track,
                    page: PageRequest::new(100, 0),
                },
            )
            .await
            .expect("library read succeeds");

        assert_eq!(library.items.len(), 2);
        let observed = provider.observed_requests().await;
        assert!(observed
            .iter()
            .any(|request| request.operation == "apply_mutation"));
        assert!(observed
            .iter()
            .any(|request| request.operation == "transport.execute"));
    }

    #[test]
    fn rejects_duplicate_provider_ids_and_uri_schemes() {
        let duplicate_id = ProviderRegistry::new(
            ProviderId::new("fake-a").expect("valid provider id"),
            [fake_runtime("fake-a"), fake_runtime("fake-a")],
        )
        .err()
        .expect("duplicate ID must fail");
        assert!(matches!(
            duplicate_id,
            ProviderError::InvalidInput { field, .. } if field == "provider_id"
        ));

        let shared_scheme = UriScheme::new("shared").expect("valid URI scheme");
        let duplicate_scheme = ProviderRegistry::new(
            ProviderId::new("fake-a").expect("valid provider id"),
            [
                fake_runtime_with_identity(
                    ProviderId::new("fake-a").expect("valid provider id"),
                    shared_scheme.clone(),
                ),
                fake_runtime_with_identity(
                    ProviderId::new("fake-b").expect("valid provider id"),
                    shared_scheme,
                ),
            ],
        )
        .err()
        .expect("duplicate scheme must fail");
        assert!(matches!(
            duplicate_scheme,
            ProviderError::InvalidInput { field, .. } if field == "uri_scheme"
        ));
    }

    #[test]
    fn rejects_unregistered_default_and_returns_typed_lookup_errors() {
        let missing_default = ProviderRegistry::new(
            ProviderId::new("missing").expect("valid provider id"),
            [fake_runtime("fake-a")],
        )
        .err()
        .expect("default must be registered");
        assert!(matches!(
            missing_default,
            ProviderError::InvalidInput { field, .. } if field == "default_provider"
        ));

        let registry = ProviderRegistry::new(
            ProviderId::new("fake-a").expect("valid provider id"),
            [fake_runtime("fake-a")],
        )
        .expect("registry is valid");
        assert!(matches!(
            registry.provider(&ProviderId::new("missing").expect("valid provider id")),
            Err(ProviderError::NotFound { .. })
        ));
        assert!(matches!(
            registry.provider_for_scheme(&UriScheme::new("missing").expect("valid URI scheme")),
            Err(ProviderError::NotFound { .. })
        ));
    }

    #[test]
    fn rejects_mismatched_transport_identity() {
        let mismatched_scheme = Arc::new(TransportIdentityOverride {
            inner: FakeProvider::isolated("fake-a").expect("valid fake"),
            transport_id: ProviderId::new("fake-a").expect("valid provider id"),
            transport_scheme: UriScheme::new("other").expect("valid URI scheme"),
        });
        let scheme_error = ProviderRuntime::with_transport(mismatched_scheme)
            .err()
            .expect("mismatched transport schemes must fail");
        assert!(matches!(
            scheme_error,
            ProviderError::InvalidInput { field, .. } if field == "transport.uri_scheme"
        ));

        let mismatched_id = Arc::new(TransportIdentityOverride {
            inner: FakeProvider::isolated("fake-a").expect("valid fake"),
            transport_id: ProviderId::new("fake-b").expect("valid provider id"),
            transport_scheme: UriScheme::new("fake-a").expect("valid URI scheme"),
        });
        let id_error = ProviderRuntime::with_transport(mismatched_id)
            .err()
            .expect("mismatched transport IDs must fail");
        assert!(matches!(
            id_error,
            ProviderError::InvalidInput { field, .. } if field == "transport.provider_id"
        ));
    }

    #[test]
    fn transport_presence_must_exactly_match_capabilities() {
        let music = Arc::new(FakeProvider::isolated("fake-a").expect("valid fake"));
        let missing = ProviderRuntime::music_only(music)
            .err()
            .expect("transport-capable providers require transport handles");
        assert!(matches!(
            missing,
            ProviderError::InvalidInput { field, .. } if field == "transport"
        ));

        let music = Arc::new(CapsOverride {
            inner: FakeProvider::isolated("fake-a").expect("valid fake"),
            capabilities: ProviderCaps::default(),
        });
        let unexpected = ProviderRuntime::with_transport(music)
            .err()
            .expect("transport-less capabilities reject transport handles");
        assert!(matches!(
            unexpected,
            ProviderError::InvalidInput { field, .. } if field == "transport"
        ));
    }

    #[test]
    fn transportless_runtime_returns_typed_unsupported_errors() {
        let music = Arc::new(CapsOverride {
            inner: FakeProvider::isolated("fake-a").expect("valid fake"),
            capabilities: ProviderCaps::default(),
        });
        let runtime = ProviderRuntime::music_only(music).expect("transport-less runtime is valid");

        assert!(matches!(
            runtime.transport(),
            Err(ProviderError::Unsupported { operation }) if operation == "remote_transport"
        ));
    }
}
