#![allow(clippy::panic, clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use spotuify_core::{
    FreshnessProbe, LibraryRequest, MediaItem, MediaKind, MusicProvider, PageContinuation,
    PageRequest, Playback, Playlist, ProviderCaps, ProviderError, ProviderId, ProviderPage,
    ProviderResult, RemoteTransport, RequestContext, TransportCaps, UriScheme,
};
use spotuify_protocol::{DaemonEvent, SyncCompletionStatus, SyncTargetData};
use spotuify_provider_fake::FakeProvider;
use spotuify_store::Store;
use spotuify_sync::{
    spawn_background_scheduler, sync_provider_target_bounded_with_timeout, sync_target,
    sync_target_isolated_with_timeout, SyncContext, SyncProvider,
};
use tokio::sync::watch;

#[derive(Clone, Copy)]
enum LibraryBehavior {
    Healthy,
    ExpireOnce,
    Hang,
    Panic,
    ForeignOutput,
    Empty,
    RepeatedCursor,
    WrongOffsetSecond,
}

struct ScriptedLibraryProvider {
    id: ProviderId,
    scheme: UriScheme,
    behavior: LibraryBehavior,
    freshness_probe: bool,
    expired: AtomicBool,
    library_calls: AtomicUsize,
    probe_calls: AtomicUsize,
    call_thread: Mutex<Option<tokio::sync::oneshot::Sender<String>>>,
}

impl ScriptedLibraryProvider {
    fn new(id: &str, behavior: LibraryBehavior, freshness_probe: bool) -> Self {
        Self {
            id: ProviderId::new(id).unwrap(),
            scheme: UriScheme::new(id).unwrap(),
            behavior,
            freshness_probe,
            expired: AtomicBool::new(false),
            library_calls: AtomicUsize::new(0),
            probe_calls: AtomicUsize::new(0),
            call_thread: Mutex::new(None),
        }
    }

    fn item(&self) -> MediaItem {
        MediaItem {
            id: Some("one".to_string()),
            uri: format!("{}:track:one", self.scheme.label()),
            name: "One".to_string(),
            kind: MediaKind::Track,
            ..Default::default()
        }
    }
}

#[async_trait::async_trait]
impl MusicProvider for ScriptedLibraryProvider {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn uri_scheme(&self) -> &UriScheme {
        &self.scheme
    }

    fn display_name(&self) -> &str {
        "Scripted library"
    }

    fn capabilities(&self) -> ProviderCaps {
        let mut caps = ProviderCaps::default();
        caps.library.read_kinds = vec![MediaKind::Track];
        caps.library.max_page_size = Some(50);
        caps.library.freshness_probe = self.freshness_probe;
        caps
    }

    async fn library_freshness_probe(
        &self,
        _context: RequestContext,
        _kind: MediaKind,
    ) -> ProviderResult<FreshnessProbe> {
        self.probe_calls.fetch_add(1, Ordering::SeqCst);
        Ok(FreshnessProbe(b"opaque-v1".to_vec()))
    }

    async fn library_items(
        &self,
        _context: RequestContext,
        request: LibraryRequest,
    ) -> ProviderResult<ProviderPage<MediaItem>> {
        if let Some(observed) = self.call_thread.lock().unwrap().take() {
            let _ = observed.send(
                std::thread::current()
                    .name()
                    .unwrap_or("unnamed")
                    .to_string(),
            );
        }
        let call = self.library_calls.fetch_add(1, Ordering::SeqCst);
        match self.behavior {
            LibraryBehavior::Hang => std::future::pending().await,
            LibraryBehavior::Panic => panic!("scripted provider panic"),
            LibraryBehavior::ForeignOutput => Ok(ProviderPage {
                items: vec![MediaItem {
                    id: Some("poison".to_string()),
                    uri: "foreign:track:poison".to_string(),
                    name: "Poison".to_string(),
                    kind: MediaKind::Track,
                    ..Default::default()
                }],
                total: Some(1),
                requested_offset: request.page.offset,
                next: None,
            }),
            LibraryBehavior::ExpireOnce if !self.expired.swap(true, Ordering::SeqCst) => {
                Err(ProviderError::SyncTokenExpired {
                    reason: "fixture expired".to_string(),
                })
            }
            LibraryBehavior::RepeatedCursor => Ok(ProviderPage {
                items: vec![self.item()],
                total: None,
                requested_offset: request.page.offset,
                next: Some(PageContinuation::Cursor("same".to_string())),
            }),
            LibraryBehavior::WrongOffsetSecond if call == 0 => Ok(ProviderPage {
                items: vec![self.item()],
                total: Some(2),
                requested_offset: request.page.offset,
                next: Some(PageContinuation::Offset(1)),
            }),
            LibraryBehavior::WrongOffsetSecond => Ok(ProviderPage {
                items: vec![self.item()],
                total: Some(2),
                requested_offset: request.page.offset.saturating_add(1),
                next: None,
            }),
            LibraryBehavior::Empty => Ok(ProviderPage {
                items: Vec::new(),
                total: Some(0),
                requested_offset: request.page.offset,
                next: None,
            }),
            LibraryBehavior::Healthy | LibraryBehavior::ExpireOnce => Ok(ProviderPage {
                items: vec![self.item()],
                total: Some(1),
                requested_offset: request.page.offset,
                next: None,
            }),
        }
    }
}

struct TestContext {
    store: Store,
    shutdown_rx: watch::Receiver<bool>,
    providers: Vec<SyncProvider>,
    events: Mutex<Vec<DaemonEvent>>,
    removed_index_uris: Mutex<Vec<String>>,
    sync_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    runtime: Option<tokio::runtime::Handle>,
    panic_on_background_runtime: AtomicBool,
}

#[async_trait::async_trait]
impl SyncContext for TestContext {
    fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown_rx.clone()
    }

    fn store(&self) -> &Store {
        &self.store
    }

    fn emit_event(&self, event: DaemonEvent) {
        self.events.lock().unwrap().push(event);
    }

    async fn remove_indexed_media_items(&self, uris: &[String]) -> anyhow::Result<()> {
        self.removed_index_uris
            .lock()
            .unwrap()
            .extend_from_slice(uris);
        Ok(())
    }

    fn sync_locks_for(
        &self,
        provider_id: &str,
        _target: SyncTargetData,
    ) -> Vec<Arc<tokio::sync::Mutex<()>>> {
        let mut locks = self.sync_locks.lock().unwrap();
        vec![locks
            .entry(provider_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()]
    }

    async fn sync_providers(&self) -> anyhow::Result<Vec<SyncProvider>> {
        Ok(self.providers.clone())
    }

    fn background_runtime(&self) -> Option<tokio::runtime::Handle> {
        if self
            .panic_on_background_runtime
            .swap(false, Ordering::SeqCst)
        {
            panic!("scripted background runtime panic");
        }
        self.runtime.clone()
    }
}

async fn context(providers: Vec<Arc<ScriptedLibraryProvider>>) -> TestContext {
    let (_, shutdown_rx) = watch::channel(false);
    let providers = providers
        .into_iter()
        .map(|provider| {
            let music: Arc<dyn MusicProvider> = provider;
            SyncProvider::new(music, None).unwrap()
        })
        .collect();
    TestContext {
        store: Store::in_memory().await.unwrap(),
        shutdown_rx,
        providers,
        events: Mutex::new(Vec::new()),
        removed_index_uris: Mutex::new(Vec::new()),
        sync_locks: Mutex::new(HashMap::new()),
        runtime: None,
        panic_on_background_runtime: AtomicBool::new(false),
    }
}

#[tokio::test]
async fn expired_token_is_cleared_and_full_sync_retried_once() {
    let provider = Arc::new(ScriptedLibraryProvider::new(
        "expiry",
        LibraryBehavior::ExpireOnce,
        true,
    ));
    let ctx = context(vec![provider.clone()]).await;

    let summary = sync_target(&ctx, SyncTargetData::Library).await.unwrap();

    assert_eq!(
        summary.provider.as_ref().map(ProviderId::as_str),
        Some("expiry")
    );
    assert_eq!(summary.library_items, 1);
    assert_eq!(provider.library_calls.load(Ordering::SeqCst), 2);
    assert_eq!(provider.probe_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        ctx.store
            .sync_cursor("expiry", "library/track")
            .await
            .unwrap(),
        Some(b"opaque-v1".to_vec())
    );
}

#[tokio::test]
async fn foreign_library_output_is_rejected_before_store_write() {
    let provider = Arc::new(ScriptedLibraryProvider::new(
        "library-owner",
        LibraryBehavior::ForeignOutput,
        false,
    ));
    let ctx = context(vec![provider]).await;

    let error = sync_target(&ctx, SyncTargetData::Library)
        .await
        .expect_err("foreign provider output must fail closed");
    assert!(matches!(
        error.downcast_ref::<ProviderError>(),
        Some(ProviderError::InvalidInput { field, .. }) if field == "media_item.uri"
    ));
    assert!(ctx
        .store
        .list_library_items(10, Some("library-owner"))
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn wrong_second_page_offset_is_rejected_before_store_write() {
    let provider = Arc::new(ScriptedLibraryProvider::new(
        "offset-owner",
        LibraryBehavior::WrongOffsetSecond,
        false,
    ));
    let ctx = context(vec![provider.clone()]).await;

    let error = sync_target(&ctx, SyncTargetData::Library)
        .await
        .expect_err("a provider must echo the requested page offset");
    assert!(matches!(
        error.downcast_ref::<ProviderError>(),
        Some(ProviderError::InvalidInput { field, .. }) if field == "library_items.requested_offset"
    ));
    assert_eq!(provider.library_calls.load(Ordering::SeqCst), 2);
    assert!(ctx
        .store
        .list_library_items(10, Some("offset-owner"))
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn explicit_unsupported_sync_target_returns_typed_error() {
    let provider = Arc::new(ScriptedLibraryProvider::new(
        "transportless",
        LibraryBehavior::Healthy,
        false,
    ));
    let music: Arc<dyn MusicProvider> = provider.clone();
    let sync_provider = SyncProvider::new(music, None).unwrap();
    let ctx = Arc::new(context(vec![provider]).await);

    let error = sync_provider_target_bounded_with_timeout(
        ctx,
        sync_provider,
        SyncTargetData::Playback,
        Duration::from_secs(1),
    )
    .await
    .expect_err("explicit unsupported targets must fail");
    assert!(matches!(
        error.downcast_ref::<ProviderError>(),
        Some(ProviderError::Unsupported { operation })
            if operation.contains("transportless playback sync")
    ));
}

#[tokio::test]
async fn unsupported_targets_are_skipped_without_provider_calls() {
    let provider = Arc::new(ScriptedLibraryProvider::new(
        "disabled",
        LibraryBehavior::Healthy,
        false,
    ));
    let music: Arc<dyn MusicProvider> = provider.clone();
    let mut sync_provider = SyncProvider::new(music, None).unwrap();
    // Exercise a provider with no advertised capabilities while keeping
    // call-counting methods available to catch accidental probing.
    sync_provider.music = Arc::new(NoCapabilitiesProvider(provider.clone()));
    let (_, shutdown_rx) = watch::channel(false);
    let ctx = TestContext {
        store: Store::in_memory().await.unwrap(),
        shutdown_rx,
        providers: vec![sync_provider],
        events: Mutex::new(Vec::new()),
        removed_index_uris: Mutex::new(Vec::new()),
        sync_locks: Mutex::new(HashMap::new()),
        runtime: None,
        panic_on_background_runtime: AtomicBool::new(false),
    };

    let summary = sync_target(&ctx, SyncTargetData::All).await.unwrap();

    assert_eq!(summary.media_items, 0);
    assert_eq!(provider.library_calls.load(Ordering::SeqCst), 0);
    assert_eq!(provider.probe_calls.load(Ordering::SeqCst), 0);
}

struct NoCapabilitiesProvider(Arc<ScriptedLibraryProvider>);

#[async_trait::async_trait]
impl MusicProvider for NoCapabilitiesProvider {
    fn id(&self) -> &ProviderId {
        self.0.id()
    }
    fn uri_scheme(&self) -> &UriScheme {
        self.0.uri_scheme()
    }
    fn display_name(&self) -> &str {
        "Disabled"
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps::default()
    }
    async fn library_freshness_probe(
        &self,
        context: RequestContext,
        kind: MediaKind,
    ) -> ProviderResult<FreshnessProbe> {
        self.0.library_freshness_probe(context, kind).await
    }
    async fn library_items(
        &self,
        context: RequestContext,
        request: LibraryRequest,
    ) -> ProviderResult<ProviderPage<MediaItem>> {
        self.0.library_items(context, request).await
    }
}

struct TransportLibraryProvider {
    inner: ScriptedLibraryProvider,
    playback_calls: AtomicUsize,
}

impl TransportLibraryProvider {
    fn new(id: &str) -> Self {
        Self {
            inner: ScriptedLibraryProvider::new(id, LibraryBehavior::Healthy, false),
            playback_calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl MusicProvider for TransportLibraryProvider {
    fn id(&self) -> &ProviderId {
        self.inner.id()
    }

    fn uri_scheme(&self) -> &UriScheme {
        self.inner.uri_scheme()
    }

    fn display_name(&self) -> &str {
        "Transport library"
    }

    fn capabilities(&self) -> ProviderCaps {
        let mut caps = self.inner.capabilities();
        caps.transport = Some(TransportCaps {
            playback_state: true,
            ..Default::default()
        });
        caps
    }

    async fn library_items(
        &self,
        context: RequestContext,
        request: LibraryRequest,
    ) -> ProviderResult<ProviderPage<MediaItem>> {
        self.inner.library_items(context, request).await
    }
}

#[async_trait::async_trait]
impl RemoteTransport for TransportLibraryProvider {
    fn provider_id(&self) -> &ProviderId {
        MusicProvider::id(self)
    }

    fn uri_scheme(&self) -> &UriScheme {
        MusicProvider::uri_scheme(self)
    }

    async fn playback(&self, _context: RequestContext) -> ProviderResult<Playback> {
        self.playback_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Playback::default())
    }
}

#[tokio::test]
async fn only_selected_transport_provider_updates_global_transport_while_all_sync_library() {
    let selected = Arc::new(TransportLibraryProvider::new("selected"));
    let secondary = Arc::new(TransportLibraryProvider::new("secondary"));
    let selected_music: Arc<dyn MusicProvider> = selected.clone();
    let selected_transport: Arc<dyn RemoteTransport> = selected.clone();
    let secondary_music: Arc<dyn MusicProvider> = secondary.clone();
    let (_, shutdown_rx) = watch::channel(false);
    let ctx = TestContext {
        store: Store::in_memory().await.unwrap(),
        shutdown_rx,
        providers: vec![
            SyncProvider::new(selected_music, Some(selected_transport)).unwrap(),
            SyncProvider::new(secondary_music, None).unwrap(),
        ],
        events: Mutex::new(Vec::new()),
        removed_index_uris: Mutex::new(Vec::new()),
        sync_locks: Mutex::new(HashMap::new()),
        runtime: None,
        panic_on_background_runtime: AtomicBool::new(false),
    };

    let summary = sync_target(&ctx, SyncTargetData::All).await.unwrap();

    assert_eq!(selected.playback_calls.load(Ordering::SeqCst), 1);
    assert_eq!(secondary.playback_calls.load(Ordering::SeqCst), 0);
    assert_eq!(selected.inner.library_calls.load(Ordering::SeqCst), 1);
    assert_eq!(secondary.inner.library_calls.load(Ordering::SeqCst), 1);
    assert_eq!(summary.library_items, 2);
}

#[tokio::test]
async fn dual_real_fake_sync_persists_each_provider_namespace_independently() {
    let first = Arc::new(FakeProvider::isolated("fake-a").unwrap());
    let second = Arc::new(FakeProvider::isolated("fake-b").unwrap());
    let first_music: Arc<dyn MusicProvider> = first.clone();
    let second_music: Arc<dyn MusicProvider> = second.clone();
    let (_, shutdown_rx) = watch::channel(false);
    let ctx = TestContext {
        store: Store::in_memory().await.unwrap(),
        shutdown_rx,
        providers: vec![
            SyncProvider::new(first_music, None).unwrap(),
            SyncProvider::new(second_music, None).unwrap(),
        ],
        events: Mutex::new(Vec::new()),
        removed_index_uris: Mutex::new(Vec::new()),
        sync_locks: Mutex::new(HashMap::new()),
        runtime: None,
        panic_on_background_runtime: AtomicBool::new(false),
    };

    let summary = sync_target(&ctx, SyncTargetData::Library).await.unwrap();

    assert_eq!(summary.library_items, 4);
    assert_eq!(summary.provider_outcomes.len(), 2);
    for namespace in ["fake-a", "fake-b"] {
        let items = ctx
            .store
            .list_library_items(10, Some(namespace))
            .await
            .unwrap();
        assert_eq!(items.len(), 2);
        assert!(items
            .iter()
            .all(|item| item.uri.starts_with(&format!("{namespace}:"))));
    }
    let first_cursor = ctx
        .store
        .sync_cursor("fake-a", "library/track")
        .await
        .unwrap()
        .expect("first provider cursor");
    let second_cursor = ctx
        .store
        .sync_cursor("fake-b", "library/track")
        .await
        .unwrap()
        .expect("second provider cursor");
    assert_ne!(first_cursor, second_cursor);
    assert!(first
        .observed_requests()
        .await
        .iter()
        .any(|request| request.operation == "library_items"));
    assert!(second
        .observed_requests()
        .await
        .iter()
        .any(|request| request.operation == "library_items"));
}

struct EmptyPlaylistProvider {
    id: ProviderId,
    scheme: UriScheme,
}

#[async_trait::async_trait]
impl MusicProvider for EmptyPlaylistProvider {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn uri_scheme(&self) -> &UriScheme {
        &self.scheme
    }

    fn display_name(&self) -> &str {
        "Empty playlists"
    }

    fn capabilities(&self) -> ProviderCaps {
        let mut caps = ProviderCaps::default();
        caps.playlists.list = true;
        caps
    }

    async fn playlists(
        &self,
        _context: RequestContext,
        page: PageRequest,
    ) -> ProviderResult<ProviderPage<Playlist>> {
        Ok(ProviderPage {
            items: Vec::new(),
            total: Some(0),
            requested_offset: page.offset,
            next: None,
        })
    }
}

#[tokio::test]
async fn empty_playlist_sync_removes_canonical_index_uri_and_preserves_other_provider() {
    let provider = Arc::new(EmptyPlaylistProvider {
        id: ProviderId::new("spotify").unwrap(),
        scheme: UriScheme::new("spotify").unwrap(),
    });
    let music: Arc<dyn MusicProvider> = provider;
    let (_, shutdown_rx) = watch::channel(false);
    let ctx = TestContext {
        store: Store::in_memory().await.unwrap(),
        shutdown_rx,
        providers: vec![SyncProvider::new(music, None).unwrap()],
        events: Mutex::new(Vec::new()),
        removed_index_uris: Mutex::new(Vec::new()),
        sync_locks: Mutex::new(HashMap::new()),
        runtime: None,
        panic_on_background_runtime: AtomicBool::new(false),
    };
    let playlist = |id: &str| Playlist {
        id: id.to_string(),
        name: id.to_string(),
        owner: "owner".to_string(),
        tracks_total: 0,
        image_url: None,
        version_token: Some("v1".to_string()),
    };
    ctx.store
        .replace_provider_playlists_bulk("spotify", "spotify", &[playlist("bare-id")])
        .await
        .unwrap();
    ctx.store
        .replace_provider_playlists_bulk("other", "other", &[playlist("other:playlist:kept")])
        .await
        .unwrap();

    sync_target(&ctx, SyncTargetData::Playlists).await.unwrap();

    assert_eq!(
        *ctx.removed_index_uris.lock().unwrap(),
        vec!["spotify:playlist:bare-id"]
    );
    assert_eq!(
        ctx.store
            .list_provider_playlists(
                10,
                Some(&ProviderId::new("other").expect("valid provider id")),
            )
            .await
            .unwrap()
            .into_iter()
            .map(|playlist| playlist.id)
            .collect::<Vec<_>>(),
        vec!["other:playlist:kept"]
    );
}

#[tokio::test]
async fn one_hung_provider_does_not_block_a_healthy_provider() {
    let hung = Arc::new(ScriptedLibraryProvider::new(
        "hung",
        LibraryBehavior::Hang,
        false,
    ));
    let healthy = Arc::new(ScriptedLibraryProvider::new(
        "healthy",
        LibraryBehavior::Healthy,
        false,
    ));
    let ctx = Arc::new(context(vec![hung.clone(), healthy.clone()]).await);

    let summary = sync_target_isolated_with_timeout(
        ctx.clone(),
        SyncTargetData::Library,
        Duration::from_millis(25),
    )
    .await
    .expect("healthy provider makes the aggregate partial, not failed");

    assert_eq!(summary.status, SyncCompletionStatus::Partial);
    assert_eq!(summary.provider_outcomes.len(), 2);
    assert_eq!(healthy.library_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        ctx.store
            .media_items_by_uris(&["healthy:track:one".to_string()])
            .await
            .unwrap()
            .len(),
        1
    );
    let events = ctx.events.lock().unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        DaemonEvent::SyncStarted {
            provider: Some(provider),
            ..
        } if provider.as_str() == "healthy"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        DaemonEvent::SyncFinished { summary }
            if summary.provider.as_ref().map(ProviderId::as_str) == Some("healthy")
                && summary.status == SyncCompletionStatus::Succeeded
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        DaemonEvent::SyncFinished { summary }
            if summary.provider.as_ref().map(ProviderId::as_str) == Some("hung")
                && summary.status == SyncCompletionStatus::Failed
                && summary.error.as_deref().is_some_and(|error| error.contains("timed out"))
    )));
    drop(events);
    let lock = ctx.sync_locks_for("hung", SyncTargetData::Library)[0].clone();
    assert!(
        lock.try_lock().is_ok(),
        "timed-out task must release its lane lock"
    );
}

#[tokio::test]
async fn bounded_single_provider_surface_aborts_hanging_work() {
    let hung = Arc::new(ScriptedLibraryProvider::new(
        "warm-hang",
        LibraryBehavior::Hang,
        false,
    ));
    let provider = {
        let music: Arc<dyn MusicProvider> = hung.clone();
        SyncProvider::new(music, None).unwrap()
    };
    let ctx = Arc::new(context(vec![hung.clone()]).await);

    let err = tokio::time::timeout(
        Duration::from_secs(1),
        sync_provider_target_bounded_with_timeout(
            ctx.clone(),
            provider,
            SyncTargetData::Library,
            Duration::from_millis(25),
        ),
    )
    .await
    .expect("bounded provider surface must return")
    .expect_err("bounded provider work must time out");

    assert!(err.to_string().contains("timed out"), "{err}");
    assert_eq!(hung.library_calls.load(Ordering::SeqCst), 1);
    let lock = ctx.sync_locks_for("warm-hang", SyncTargetData::Library)[0].clone();
    assert!(
        lock.try_lock().is_ok(),
        "timed-out provider work must release its lane lock"
    );
}

#[tokio::test]
async fn auxiliary_runtime_shutdown_does_not_cancel_provider_work() {
    let provider = Arc::new(ScriptedLibraryProvider::new(
        "main-runtime",
        LibraryBehavior::Healthy,
        false,
    ));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let mut ctx = context(vec![provider.clone()]).await;
    ctx.runtime = Some(runtime.handle().clone());
    let ctx = Arc::new(ctx);
    runtime.shutdown_background();

    let summary = sync_target_isolated_with_timeout(
        ctx.clone(),
        SyncTargetData::Library,
        Duration::from_secs(1),
    )
    .await
    .expect("provider work must remain on the caller runtime");
    assert_eq!(
        summary.provider.as_ref().map(ProviderId::as_str),
        Some("main-runtime")
    );
    assert_eq!(provider.library_calls.load(Ordering::SeqCst), 1);

    let events = ctx.events.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, DaemonEvent::SyncStarted { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                DaemonEvent::SyncFinished { summary }
                    if summary.provider.as_ref().map(ProviderId::as_str) == Some("main-runtime")
                        && summary.status == SyncCompletionStatus::Succeeded
            ))
            .count(),
        1
    );
}

#[tokio::test]
async fn isolated_child_panic_has_one_failed_outcome_and_one_terminal_event() {
    let first = Arc::new(ScriptedLibraryProvider::new(
        "panic-a",
        LibraryBehavior::Panic,
        false,
    ));
    let second = Arc::new(ScriptedLibraryProvider::new(
        "panic-b",
        LibraryBehavior::Healthy,
        false,
    ));
    let ctx = Arc::new(context(vec![first, second]).await);
    let summary = sync_target_isolated_with_timeout(
        ctx.clone(),
        SyncTargetData::Library,
        Duration::from_secs(1),
    )
    .await
    .expect("one healthy provider keeps the aggregate observable");

    assert_eq!(summary.status, SyncCompletionStatus::Partial);
    assert_eq!(summary.provider_outcomes.len(), 2);
    assert_eq!(
        summary
            .provider_outcomes
            .iter()
            .filter(|outcome| outcome.status == SyncCompletionStatus::Failed)
            .count(),
        1
    );
    assert_eq!(
        summary
            .provider_outcomes
            .iter()
            .filter(|outcome| outcome.status == SyncCompletionStatus::Succeeded)
            .count(),
        1
    );

    let events = ctx.events.lock().unwrap();
    for provider in ["panic-a", "panic-b"] {
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    DaemonEvent::SyncStarted {
                        provider: Some(event_provider),
                        ..
                    } if event_provider.as_str() == provider
                ))
                .count(),
            1,
            "every provider must start exactly once"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    DaemonEvent::SyncFinished { summary }
                        if summary.provider.as_ref().map(ProviderId::as_str) == Some(provider)
                ))
                .count(),
            1,
            "every started provider must finish exactly once"
        );
    }
}

#[tokio::test]
async fn provider_scoped_locks_block_only_the_matching_provider() {
    let locked = Arc::new(ScriptedLibraryProvider::new(
        "locked",
        LibraryBehavior::Healthy,
        false,
    ));
    let independent = Arc::new(ScriptedLibraryProvider::new(
        "independent",
        LibraryBehavior::Healthy,
        false,
    ));
    let ctx = Arc::new(context(vec![locked.clone(), independent.clone()]).await);
    let lock = ctx.sync_locks_for("locked", SyncTargetData::Library)[0].clone();
    let guard = lock.lock_owned().await;

    let task = tokio::spawn(sync_target_isolated_with_timeout(
        ctx.clone(),
        SyncTargetData::Library,
        Duration::from_secs(2),
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        while independent.library_calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("independent provider should pass its own lock");
    assert_eq!(locked.library_calls.load(Ordering::SeqCst), 0);

    drop(guard);
    task.await.unwrap().unwrap();
    assert_eq!(locked.library_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancelling_isolated_sync_aborts_provider_work_and_releases_lane_lock() {
    let hung = Arc::new(ScriptedLibraryProvider::new(
        "cancelled",
        LibraryBehavior::Hang,
        false,
    ));
    let ctx = Arc::new(context(vec![hung.clone()]).await);
    let task = tokio::spawn(sync_target_isolated_with_timeout(
        ctx.clone(),
        SyncTargetData::Library,
        Duration::from_secs(5),
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        while hung.library_calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("provider call should start");

    task.abort();
    let _ = task.await;
    let lock = ctx.sync_locks_for("cancelled", SyncTargetData::Library)[0].clone();
    tokio::time::timeout(Duration::from_millis(250), lock.lock_owned())
        .await
        .expect("cancellation must abort detached provider task and release lock");
}

#[tokio::test]
async fn isolated_provider_work_stays_on_the_calling_runtime() {
    let provider = Arc::new(ScriptedLibraryProvider::new(
        "background",
        LibraryBehavior::Healthy,
        false,
    ));
    let (observed_tx, observed_rx) = tokio::sync::oneshot::channel();
    *provider.call_thread.lock().unwrap() = Some(observed_tx);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("spotuify-sync-provider-test")
        .enable_all()
        .build()
        .unwrap();
    let mut ctx = context(vec![provider]).await;
    ctx.runtime = Some(runtime.handle().clone());
    let ctx = Arc::new(ctx);
    let caller_thread = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_string();

    let summary = sync_target_isolated_with_timeout(
        ctx.clone(),
        SyncTargetData::Library,
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    assert_eq!(
        summary.provider.as_ref().map(ProviderId::as_str),
        Some("background")
    );
    assert_eq!(observed_rx.await.unwrap(), caller_thread);

    drop(ctx);
    runtime.shutdown_background();
}

struct RuntimeProbeContext {
    store: Store,
    shutdown_rx: watch::Receiver<bool>,
    runtime: tokio::runtime::Handle,
    observed: Mutex<Option<tokio::sync::oneshot::Sender<std::thread::ThreadId>>>,
}

struct RecoveringSchedulerContext {
    store: Store,
    shutdown_rx: watch::Receiver<bool>,
    provider_revision_rx: watch::Receiver<u64>,
    attempts: AtomicUsize,
}

#[async_trait::async_trait]
impl SyncContext for RecoveringSchedulerContext {
    fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown_rx.clone()
    }

    fn sync_provider_revision_receiver(&self) -> Option<watch::Receiver<u64>> {
        Some(self.provider_revision_rx.clone())
    }

    fn store(&self) -> &Store {
        &self.store
    }

    fn emit_event(&self, _event: DaemonEvent) {}

    async fn sync_providers(&self) -> anyhow::Result<Vec<SyncProvider>> {
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(ProviderError::AuthRequired.into());
        }
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn scheduler_reconciles_after_initial_auth_failure() {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (provider_revision_tx, provider_revision_rx) = watch::channel(0_u64);
    let ctx = Arc::new(RecoveringSchedulerContext {
        store: Store::in_memory().await.unwrap(),
        shutdown_rx,
        provider_revision_rx,
        attempts: AtomicUsize::new(0),
    });
    let handles = spawn_background_scheduler(ctx.clone());

    tokio::time::timeout(Duration::from_secs(1), async {
        while ctx.attempts.load(Ordering::SeqCst) < 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("scheduler must attempt provider discovery");

    provider_revision_tx.send(1).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while ctx.attempts.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("auth/config revision must trigger provider reconciliation");

    shutdown_tx.send(true).unwrap();
    for handle in handles {
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("scheduler must honor shutdown")
            .unwrap();
    }
}

#[tokio::test]
async fn scheduler_retries_initial_provider_failure_without_a_revision() {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (_provider_revision_tx, provider_revision_rx) = watch::channel(0_u64);
    let ctx = Arc::new(RecoveringSchedulerContext {
        store: Store::in_memory().await.unwrap(),
        shutdown_rx,
        provider_revision_rx,
        attempts: AtomicUsize::new(0),
    });
    let handles = spawn_background_scheduler(ctx.clone());

    tokio::time::timeout(Duration::from_secs(1), async {
        while ctx.attempts.load(Ordering::SeqCst) < 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("scheduler must attempt provider discovery");
    tokio::time::sleep(Duration::from_secs(15)).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while ctx.attempts.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("provider discovery must retry after its bounded delay");

    shutdown_tx.send(true).unwrap();
    for handle in handles {
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("scheduler must honor shutdown")
            .unwrap();
    }
}

struct RevisableSchedulerContext {
    store: Store,
    shutdown_rx: watch::Receiver<bool>,
    provider_revision_rx: watch::Receiver<u64>,
    provider: Mutex<SyncProvider>,
    attempts: AtomicUsize,
    subscriber_panics_remaining: AtomicUsize,
}

#[async_trait::async_trait]
impl SyncContext for RevisableSchedulerContext {
    fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown_rx.clone()
    }

    fn sync_provider_revision_receiver(&self) -> Option<watch::Receiver<u64>> {
        Some(self.provider_revision_rx.clone())
    }

    fn store(&self) -> &Store {
        &self.store
    }

    fn emit_event(&self, _event: DaemonEvent) {}

    fn event_subscriber_count(&self) -> usize {
        if self
            .subscriber_panics_remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            panic!("scripted scheduler lane panic");
        }
        1
    }

    async fn sync_providers(&self) -> anyhow::Result<Vec<SyncProvider>> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Ok(vec![self.provider.lock().unwrap().clone()])
    }
}

fn transport_sync_provider(provider: Arc<TransportLibraryProvider>) -> SyncProvider {
    let music: Arc<dyn MusicProvider> = provider.clone();
    let transport: Arc<dyn RemoteTransport> = provider;
    SyncProvider::new(music, Some(transport)).unwrap()
}

#[tokio::test]
async fn scheduler_revision_replaces_same_id_adapter_instance() {
    let initial = Arc::new(TransportLibraryProvider::new("replaceable"));
    let replacement = Arc::new(TransportLibraryProvider::new("replaceable"));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (provider_revision_tx, provider_revision_rx) = watch::channel(0_u64);
    let ctx = Arc::new(RevisableSchedulerContext {
        store: Store::in_memory().await.unwrap(),
        shutdown_rx,
        provider_revision_rx,
        provider: Mutex::new(transport_sync_provider(initial.clone())),
        attempts: AtomicUsize::new(0),
        subscriber_panics_remaining: AtomicUsize::new(0),
    });
    let handles = spawn_background_scheduler(ctx.clone());

    tokio::time::timeout(Duration::from_secs(1), async {
        while ctx.attempts.load(Ordering::SeqCst) < 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial provider generation must start");
    tokio::time::sleep(Duration::from_secs(3)).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while initial.playback_calls.load(Ordering::SeqCst) < 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial adapter must own the first lane generation");
    let initial_calls = initial.playback_calls.load(Ordering::SeqCst);

    *ctx.provider.lock().unwrap() = transport_sync_provider(replacement.clone());
    provider_revision_tx.send(1).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while ctx.attempts.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("provider revision must rebuild scheduler lanes");
    tokio::time::sleep(Duration::from_secs(3)).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while replacement.playback_calls.load(Ordering::SeqCst) < 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("replacement adapter must receive the next lane tick");

    assert_eq!(initial.playback_calls.load(Ordering::SeqCst), initial_calls);
    assert_eq!(replacement.playback_calls.load(Ordering::SeqCst), 1);

    shutdown_tx.send(true).unwrap();
    for handle in handles {
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("scheduler must honor shutdown")
            .unwrap();
    }
}

#[tokio::test]
async fn scheduler_restarts_only_the_lane_that_panics() {
    let provider = Arc::new(TransportLibraryProvider::new("lane-restart"));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (_provider_revision_tx, provider_revision_rx) = watch::channel(0_u64);
    let ctx = Arc::new(RevisableSchedulerContext {
        store: Store::in_memory().await.unwrap(),
        shutdown_rx,
        provider_revision_rx,
        provider: Mutex::new(transport_sync_provider(provider.clone())),
        attempts: AtomicUsize::new(0),
        subscriber_panics_remaining: AtomicUsize::new(1),
    });
    let handles = spawn_background_scheduler(ctx.clone());

    tokio::time::timeout(Duration::from_secs(1), async {
        while ctx.attempts.load(Ordering::SeqCst) < 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("provider scheduler generation must start");
    tokio::time::sleep(Duration::from_secs(3)).await;
    tokio::task::yield_now().await;
    assert_eq!(provider.playback_calls.load(Ordering::SeqCst), 0);

    tokio::time::sleep(Duration::from_secs(4)).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while provider.playback_calls.load(Ordering::SeqCst) < 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("panicked fast lane must restart for the same provider");

    assert_eq!(ctx.attempts.load(Ordering::SeqCst), 1);
    assert_eq!(provider.playback_calls.load(Ordering::SeqCst), 1);

    shutdown_tx.send(true).unwrap();
    for handle in handles {
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("scheduler must honor shutdown")
            .unwrap();
    }
}

#[tokio::test]
async fn persistently_panicking_scheduler_lane_uses_restart_backoff() {
    let provider = Arc::new(TransportLibraryProvider::new("lane-backoff"));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (_provider_revision_tx, provider_revision_rx) = watch::channel(0_u64);
    let ctx = Arc::new(RevisableSchedulerContext {
        store: Store::in_memory().await.unwrap(),
        shutdown_rx,
        provider_revision_rx,
        provider: Mutex::new(transport_sync_provider(provider)),
        attempts: AtomicUsize::new(0),
        subscriber_panics_remaining: AtomicUsize::new(3),
    });
    let handles = spawn_background_scheduler(ctx.clone());

    tokio::time::timeout(Duration::from_secs(1), async {
        while ctx.attempts.load(Ordering::SeqCst) < 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("provider scheduler generation must start");

    tokio::time::sleep(Duration::from_secs(3)).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while ctx.subscriber_panics_remaining.load(Ordering::SeqCst) != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first lane generation must panic");
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }

    tokio::time::sleep(Duration::from_secs(3)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        ctx.subscriber_panics_remaining.load(Ordering::SeqCst),
        2,
        "first restart must wait before beginning a new scheduler cadence"
    );

    tokio::time::sleep(Duration::from_secs(1)).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while ctx.subscriber_panics_remaining.load(Ordering::SeqCst) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first delayed restart must eventually run");

    tokio::time::sleep(Duration::from_secs(4)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        ctx.subscriber_panics_remaining.load(Ordering::SeqCst),
        1,
        "consecutive failure must increase the restart delay"
    );

    shutdown_tx.send(true).unwrap();
    for handle in handles {
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("scheduler must honor shutdown")
            .unwrap();
    }
}

#[tokio::test]
async fn provider_revision_cancels_a_pending_lane_restart() {
    let initial = Arc::new(TransportLibraryProvider::new("revision-during-restart"));
    let replacement = Arc::new(TransportLibraryProvider::new("revision-during-restart"));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (provider_revision_tx, provider_revision_rx) = watch::channel(0_u64);
    let ctx = Arc::new(RevisableSchedulerContext {
        store: Store::in_memory().await.unwrap(),
        shutdown_rx,
        provider_revision_rx,
        provider: Mutex::new(transport_sync_provider(initial.clone())),
        attempts: AtomicUsize::new(0),
        subscriber_panics_remaining: AtomicUsize::new(1),
    });
    let handles = spawn_background_scheduler(ctx.clone());

    tokio::time::timeout(Duration::from_secs(1), async {
        while ctx.attempts.load(Ordering::SeqCst) < 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("provider scheduler generation must start");
    tokio::time::sleep(Duration::from_secs(3)).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while ctx.subscriber_panics_remaining.load(Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial fast lane must panic");
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }

    *ctx.provider.lock().unwrap() = transport_sync_provider(replacement.clone());
    provider_revision_tx.send(1).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while ctx.attempts.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("revision must cancel the pending restart and rebuild lanes");

    tokio::time::sleep(Duration::from_secs(3)).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while replacement.playback_calls.load(Ordering::SeqCst) < 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("replacement generation must run on its normal cadence");
    assert_eq!(initial.playback_calls.load(Ordering::SeqCst), 0);
    assert_eq!(replacement.playback_calls.load(Ordering::SeqCst), 1);

    shutdown_tx.send(true).unwrap();
    for handle in handles {
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("scheduler must honor shutdown")
            .unwrap();
    }
}

#[async_trait::async_trait]
impl SyncContext for RuntimeProbeContext {
    fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown_rx.clone()
    }

    fn store(&self) -> &Store {
        &self.store
    }

    fn emit_event(&self, _event: DaemonEvent) {}

    async fn sync_providers(&self) -> anyhow::Result<Vec<SyncProvider>> {
        if let Some(observed) = self.observed.lock().unwrap().take() {
            let _ = observed.send(std::thread::current().id());
        }
        Ok(Vec::new())
    }

    fn background_runtime(&self) -> Option<tokio::runtime::Handle> {
        Some(self.runtime.clone())
    }
}

#[tokio::test]
async fn scheduler_supervisor_stays_on_the_calling_runtime() {
    let caller_thread = std::thread::current().id();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let (observed_tx, observed_rx) = tokio::sync::oneshot::channel();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let ctx = Arc::new(RuntimeProbeContext {
        store: Store::in_memory().await.unwrap(),
        shutdown_rx,
        runtime: runtime.handle().clone(),
        observed: Mutex::new(Some(observed_tx)),
    });

    let handles = spawn_background_scheduler(ctx);
    let observed_thread = tokio::time::timeout(Duration::from_secs(1), observed_rx)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(observed_thread, caller_thread);
    shutdown_tx.send(true).unwrap();
    for handle in handles {
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("scheduler must honor shutdown")
            .unwrap();
    }
    runtime.shutdown_background();
}

struct BlockingSchedulerContext {
    store: Store,
    shutdown_rx: watch::Receiver<bool>,
    started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

#[async_trait::async_trait]
impl SyncContext for BlockingSchedulerContext {
    fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown_rx.clone()
    }

    fn store(&self) -> &Store {
        &self.store
    }

    fn emit_event(&self, _event: DaemonEvent) {}

    async fn sync_providers(&self) -> anyhow::Result<Vec<SyncProvider>> {
        if let Some(started) = self.started.lock().unwrap().take() {
            let _ = started.send(());
        }
        std::future::pending().await
    }
}

#[tokio::test]
async fn dropping_scheduler_handle_aborts_supervisor_instead_of_detaching_it() {
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let ctx = Arc::new(BlockingSchedulerContext {
        store: Store::in_memory().await.unwrap(),
        shutdown_rx,
        started: Mutex::new(Some(started_tx)),
    });
    let weak = Arc::downgrade(&ctx);
    let handles = spawn_background_scheduler(ctx.clone());
    started_rx.await.unwrap();

    drop(handles);
    drop(ctx);
    tokio::time::timeout(Duration::from_millis(250), async {
        while weak.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("abort-on-drop scheduler must release its context");
}

#[tokio::test]
async fn repeated_cursor_fails_after_second_page_instead_of_spinning() {
    let provider = Arc::new(ScriptedLibraryProvider::new(
        "cursor",
        LibraryBehavior::RepeatedCursor,
        false,
    ));
    let ctx = context(vec![provider.clone()]).await;

    let err = sync_target(&ctx, SyncTargetData::Library)
        .await
        .expect_err("repeated cursor must fail the provider pass");

    assert!(err.to_string().contains("cursor"), "{err}");
    assert_eq!(provider.library_calls.load(Ordering::SeqCst), 2);
    let (status, event_provider): (String, String) = sqlx::query_as(
        "SELECT status, provider FROM sync_events
         WHERE domain = 'library' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(ctx.store.reader())
    .await
    .unwrap();
    assert_eq!(status, "error");
    assert_eq!(event_provider, "cursor");
}

#[tokio::test]
async fn persisted_cooldown_from_other_domain_gates_first_sync_after_restart() {
    let provider = Arc::new(ScriptedLibraryProvider::new(
        "cooled",
        LibraryBehavior::Healthy,
        false,
    ));
    let ctx = context(vec![provider.clone()]).await;
    ctx.store
        .record_provider_sync_event_with_retry_after(
            "cooled",
            "playback",
            spotuify_store::now_ms(),
            spotuify_store::ProviderSyncEventOutcome {
                status: "error",
                row_count: 0,
                error: Some("rate limited"),
                retry_after_secs: Some(60),
            },
        )
        .await
        .unwrap();

    let err = sync_target(&ctx, SyncTargetData::Library)
        .await
        .expect_err("provider-wide persisted cooldown must gate library");
    assert!(err
        .downcast_ref::<ProviderError>()
        .is_some_and(|err| { matches!(err, ProviderError::RateLimited { .. }) }));
    assert_eq!(provider.library_calls.load(Ordering::SeqCst), 0);
    assert!(ctx.events.lock().unwrap().iter().any(|event| matches!(
        event,
        DaemonEvent::SyncFinished { summary }
            if summary.provider.as_ref().map(ProviderId::as_str) == Some("cooled")
                && summary.status == SyncCompletionStatus::Failed
    )));
}

#[tokio::test]
async fn empty_full_library_sync_reconciles_only_that_provider() {
    let provider = Arc::new(ScriptedLibraryProvider::new(
        "empty",
        LibraryBehavior::Empty,
        false,
    ));
    let ctx = context(vec![provider]).await;
    let old = MediaItem {
        id: Some("old".to_string()),
        uri: "empty:track:old".to_string(),
        name: "Old".to_string(),
        kind: MediaKind::Track,
        ..Default::default()
    };
    let other = MediaItem {
        id: Some("kept".to_string()),
        uri: "other:track:kept".to_string(),
        name: "Kept".to_string(),
        kind: MediaKind::Track,
        ..Default::default()
    };
    ctx.store
        .replace_provider_library_kind_bulk("empty", &MediaKind::Track, &[old])
        .await
        .unwrap();
    ctx.store
        .replace_provider_library_kind_bulk("other", &MediaKind::Track, &[other])
        .await
        .unwrap();

    sync_target(&ctx, SyncTargetData::Library).await.unwrap();

    assert!(ctx
        .store
        .list_library_items(10, Some("empty"))
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        ctx.store
            .list_library_items(10, Some("other"))
            .await
            .unwrap()
            .len(),
        1
    );
}
