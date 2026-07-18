use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{stream, StreamExt};
use spotuify_core::{MediaItem, MediaKind, Queue, RequestContext, ResourceUri};
use spotuify_search::SearchUpdateBatch;
use spotuify_store::IndexedMediaItem;
use tokio::sync::mpsc;

use crate::handler::{
    require_provider_capability, validate_provider_lookup_result, validate_provider_media_items,
};
use crate::state::DaemonState;

const QUEUE_WARM_BATCH_SIZE: usize = 5;
const QUEUE_WARM_CHANNEL_CAPACITY: usize = 8;
const QUEUE_WARM_TTL: Duration = Duration::from_secs(15 * 60);
const LYRICS_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const LYRICS_NEGATIVE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone)]
pub(crate) struct QueueWarmScheduler {
    tx: mpsc::Sender<QueueWarmRequest>,
    generation: Arc<AtomicU64>,
}

#[derive(Debug)]
pub(crate) struct QueueWarmRequest {
    generation: u64,
    uris: Vec<String>,
    audio_prewarm: bool,
}

impl QueueWarmScheduler {
    pub(crate) fn new() -> (Self, mpsc::Receiver<QueueWarmRequest>) {
        let (tx, rx) = mpsc::channel(QUEUE_WARM_CHANNEL_CAPACITY);
        (
            Self {
                tx,
                generation: Arc::new(AtomicU64::new(0)),
            },
            rx,
        )
    }

    pub(crate) fn enqueue_queue(&self, queue: &Queue) {
        self.enqueue(upcoming_queue_uris(queue), true);
    }

    pub(crate) fn enqueue_uris(&self, uris: Vec<String>) {
        self.enqueue(uris, true);
    }

    fn enqueue(&self, uris: Vec<String>, audio_prewarm: bool) {
        let uris = unique_warmable_uris(uris);
        if uris.is_empty() {
            return;
        }
        let request = QueueWarmRequest {
            generation: self.generation.fetch_add(1, Ordering::AcqRel) + 1,
            uris,
            audio_prewarm,
        };
        if let Err(err) = self.tx.try_send(request) {
            tracing::debug!(error = %err, "queue warm request dropped");
        }
    }
}

pub(crate) fn upcoming_queue_uris(queue: &Queue) -> Vec<String> {
    queue.items.iter().map(|item| item.uri.clone()).collect()
}

pub(crate) async fn run_queue_warm_worker(
    state: Arc<DaemonState>,
    mut rx: mpsc::Receiver<QueueWarmRequest>,
) {
    let mut shutdown_rx = state.shutdown_receiver();
    let mut recent = HashMap::new();

    loop {
        let request = tokio::select! {
            request = rx.recv() => request,
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow_and_update() {
                    break;
                }
                continue;
            }
        };

        let Some(request) = request else {
            break;
        };
        warm_generation(state.clone(), request, &mut rx, &mut recent).await;
    }
}

async fn warm_generation(
    state: Arc<DaemonState>,
    mut request: QueueWarmRequest,
    rx: &mut mpsc::Receiver<QueueWarmRequest>,
    recent: &mut HashMap<String, Instant>,
) {
    prune_recent(recent);

    'generation: loop {
        if let Some(newer) = drain_latest(rx) {
            request = newer;
            prune_recent(recent);
        }

        tracing::trace!(
            generation = request.generation,
            items = request.uris.len(),
            "queue warm generation started"
        );

        if request.audio_prewarm {
            if let Some(next_uri) = request.uris.first() {
                if let Ok(resource) = ResourceUri::parse(next_uri) {
                    if let Ok(provider) = state.provider_for_uri(&resource).await {
                        if state.provider_owns_embedded_player(provider.id()) {
                            state.prewarm_next_audio(next_uri);
                        }
                    }
                }
            }
        }

        for chunk in request.uris.chunks(QUEUE_WARM_BATCH_SIZE) {
            if let Some(newer) = drain_latest(rx) {
                request = newer;
                continue 'generation;
            }

            let uris = chunk
                .iter()
                .filter(|uri| !recently_warmed(recent, uri))
                .cloned()
                .collect::<Vec<_>>();
            if uris.is_empty() {
                continue;
            }

            let items = stream::iter(uris.iter().cloned())
                .map(|uri| warm_metadata_and_cover(state.clone(), uri))
                .buffer_unordered(QUEUE_WARM_BATCH_SIZE)
                .filter_map(|item| async move { item })
                .collect::<Vec<_>>()
                .await;

            if !items.is_empty() {
                index_items(&state, &items).await;
            }
            for item in &items {
                warm_lyrics(&state, item).await;
            }
            let now = Instant::now();
            for uri in uris {
                recent.insert(uri, now);
            }
        }

        tracing::trace!(
            generation = request.generation,
            "queue warm generation finished"
        );
        break;
    }
}

fn drain_latest(rx: &mut mpsc::Receiver<QueueWarmRequest>) -> Option<QueueWarmRequest> {
    let mut latest = None;
    while let Ok(request) = rx.try_recv() {
        latest = Some(request);
    }
    latest
}

fn unique_warmable_uris(uris: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    uris.into_iter()
        .filter(|uri| warmable_uri(uri))
        .filter(|uri| seen.insert(uri.clone()))
        .collect()
}

fn warmable_uri(uri: &str) -> bool {
    ResourceUri::parse(uri)
        .map(|resource| matches!(resource.kind(), MediaKind::Track | MediaKind::Episode))
        .unwrap_or(false)
}

fn prune_recent(recent: &mut HashMap<String, Instant>) {
    recent.retain(|_, warmed_at| warmed_at.elapsed() < QUEUE_WARM_TTL);
}

fn recently_warmed(recent: &HashMap<String, Instant>, uri: &str) -> bool {
    recent
        .get(uri)
        .is_some_and(|warmed_at| warmed_at.elapsed() < QUEUE_WARM_TTL)
}

async fn warm_metadata_and_cover(state: Arc<DaemonState>, uri: String) -> Option<MediaItem> {
    let item = match resolve_media_item(&state, &uri).await {
        Ok(Some(item)) => item,
        Ok(None) => return None,
        Err(err) => {
            tracing::debug!(error = %err, uri, "queue metadata warm failed");
            return None;
        }
    };

    if let Some(url) = item.image_url.as_deref() {
        if let Err(err) = state
            .system_integration
            .cover_cache
            .get_or_fetch_entry(url)
            .await
        {
            tracing::debug!(error = %err, uri = item.uri, "queue cover warm failed");
        }
    }

    Some(item)
}

async fn resolve_media_item(state: &DaemonState, uri: &str) -> anyhow::Result<Option<MediaItem>> {
    let lookup = [uri.to_string()];
    if let Some(item) = state.store().media_items_by_uris(&lookup).await?.pop() {
        return Ok(Some(item));
    }

    let resource = ResourceUri::parse(uri)?;
    let provider = state.provider_for_uri(&resource).await?;
    require_provider_capability(
        provider.as_ref(),
        &format!("{} catalog lookup", resource.kind()),
        provider
            .capabilities()
            .catalog
            .lookup_kinds
            .contains(&resource.kind()),
    )?;
    let provider_name = provider.id().as_str().to_string();
    let fetched = provider
        .media_item(RequestContext::BACKGROUND_SYNC, &resource)
        .await?;
    if let Some(item) = fetched.as_ref() {
        validate_provider_lookup_result(provider.as_ref(), &resource, item)?;
        state
            .store()
            .upsert_provider_media_items_bulk(
                provider.id(),
                std::slice::from_ref(item),
                &provider_name,
            )
            .await?;
    }
    Ok(fetched)
}

async fn index_items(state: &DaemonState, items: &[MediaItem]) {
    let mut entries = Vec::with_capacity(items.len());
    for item in items.iter().cloned() {
        let Ok(resource) = ResourceUri::parse(&item.uri) else {
            tracing::warn!(uri = %item.uri, "queue warm skipped search indexing for a non-canonical URI");
            continue;
        };
        let Ok(provider) = state.provider_for_uri(&resource).await else {
            tracing::warn!(uri = %item.uri, "queue warm skipped search indexing for an unroutable URI");
            continue;
        };
        if let Err(error) =
            validate_provider_media_items(provider.as_ref(), std::slice::from_ref(&item))
        {
            tracing::warn!(%error, uri = %item.uri, "queue warm skipped foreign provider output");
            continue;
        }
        let provider_id = provider.id().to_string();
        let search_origin = item
            .source
            .as_ref()
            .map_or(provider_id.as_str(), spotuify_core::ItemSource::as_str)
            .to_string();
        entries.push(IndexedMediaItem {
            item,
            provider: provider_id,
            liked: false,
            saved: false,
            added_at_ms: Some(spotuify_store::now_ms()),
            search_origin,
        });
    }
    if let Err(err) = state
        .search()
        .apply_batch(SearchUpdateBatch {
            entries,
            removed_uris: Vec::new(),
        })
        .await
    {
        tracing::debug!(error = %err, "queue search-index warm failed");
    }
}

async fn warm_lyrics(state: &DaemonState, item: &MediaItem) {
    if item.kind != MediaKind::Track {
        return;
    }
    match state.store().cached_lyrics(&item.uri, LYRICS_TTL).await {
        Ok(Some(_)) => return,
        Ok(None) => {}
        Err(err) => {
            tracing::debug!(error = %err, uri = item.uri, "queue lyrics cache lookup failed");
            return;
        }
    }
    match state.store().lyrics_lookup_blocked(&item.uri).await {
        Ok(true) => return,
        Ok(false) => {}
        Err(err) => {
            tracing::debug!(error = %err, uri = item.uri, "queue lyrics failure cache lookup failed");
            return;
        }
    }

    match spotuify_lyrics::LrclibProvider::new()
        .fetch(item, spotuify_store::now_ms())
        .await
    {
        Ok(Some(lyrics)) => {
            if let Err(err) = state.store().upsert_lyrics(&lyrics).await {
                tracing::debug!(error = %err, uri = item.uri, "queue lyrics cache write failed");
            }
        }
        Ok(None) => {
            if let Err(err) = state
                .store()
                .upsert_lyrics_lookup_failure(&item.uri, "not found", LYRICS_NEGATIVE_TTL)
                .await
            {
                tracing::debug!(error = %err, uri = item.uri, "queue lyrics failure cache write failed");
            }
        }
        Err(err) => {
            tracing::debug!(error = %err, uri = item.uri, "queue lyrics warm failed");
            if let Err(write_err) = state
                .store()
                .upsert_lyrics_lookup_failure(&item.uri, "provider error", LYRICS_NEGATIVE_TTL)
                .await
            {
                tracing::debug!(error = %write_err, uri = item.uri, "queue lyrics failure cache write failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{unique_warmable_uris, upcoming_queue_uris, QueueWarmScheduler};
    use spotuify_core::{MediaItem, MediaKind, Queue, ResourceUri};

    fn item(uri: &str, kind: MediaKind) -> MediaItem {
        MediaItem {
            id: ResourceUri::parse(uri)
                .ok()
                .map(|resource| resource.bare_id().to_string()),
            uri: uri.to_string(),
            name: "name".to_string(),
            subtitle: "artist".to_string(),
            context: "album".to_string(),
            duration_ms: 180_000,
            image_url: None,
            kind,
            source: None,
            freshness: None,
            explicit: None,
            is_playable: None,
            ..Default::default()
        }
    }

    #[test]
    fn upcoming_queue_uris_uses_only_upcoming_items() {
        let queue = Queue {
            currently_playing: Some(item("spotify:track:current", MediaKind::Track)),
            items: vec![
                item("spotify:track:next", MediaKind::Track),
                item("spotify:episode:pod", MediaKind::Episode),
            ],
            ..Default::default()
        };

        assert_eq!(
            upcoming_queue_uris(&queue),
            vec![
                "spotify:track:next".to_string(),
                "spotify:episode:pod".to_string()
            ]
        );
    }

    #[test]
    fn unique_warmable_uris_filters_contexts_and_dedupes() {
        assert_eq!(
            unique_warmable_uris(vec![
                "spotify:track:a".to_string(),
                "spotify:album:b".to_string(),
                "spotify:track:a".to_string(),
                "spotify:episode:c".to_string(),
            ]),
            vec![
                "spotify:track:a".to_string(),
                "spotify:episode:c".to_string()
            ]
        );
    }

    #[test]
    fn added_queue_uris_request_audio_prewarm() {
        let (scheduler, mut rx) = QueueWarmScheduler::new();

        scheduler.enqueue_uris(vec!["spotify:track:next".to_string()]);

        let request = rx.try_recv().expect("queue warm request");
        assert!(request.audio_prewarm);
    }
}
