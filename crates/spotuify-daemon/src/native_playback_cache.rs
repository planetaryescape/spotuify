//! Coalesced persistence for native playback observations.

use std::sync::Arc;

use parking_lot::RwLock;
use spotuify_core::ProviderId;
use spotuify_store::Store;
use tokio::sync::{watch, Mutex};

use crate::clock::PlaybackClock;

pub(crate) struct NativePlaybackCache {
    pub store: Store,
    pub clock: Arc<PlaybackClock>,
    pub commit_lane: Arc<Mutex<()>>,
    pub embedded_provider: Arc<RwLock<Option<ProviderId>>>,
    pub active_provider: Arc<RwLock<Option<ProviderId>>>,
}

impl NativePlaybackCache {
    pub async fn run(self, mut updates: watch::Receiver<Option<ProviderId>>) {
        while updates.changed().await.is_ok() {
            let Some(provider) = updates.borrow_and_update().clone() else {
                continue;
            };
            let _guard = self.commit_lane.lock().await;
            if self.embedded_provider.read().as_ref() != Some(&provider)
                || !self
                    .active_provider
                    .read()
                    .as_ref()
                    .is_none_or(|active| active == &provider)
            {
                continue;
            }
            // Commands and remote reconciliation can win while this worker
            // waits. Persist canonical truth, never a queued event snapshot.
            let mut current = self.clock.snapshot();
            if let Some(item) = current.item.as_mut().filter(|item| item.name.is_empty()) {
                // A newer event may still be enriching its URI-only clock
                // stub. Preserve metadata already present in the store.
                match self
                    .store
                    .media_items_by_uris(std::slice::from_ref(&item.uri))
                    .await
                {
                    Ok(items) => {
                        if let Some(cached) = items.into_iter().next() {
                            *item = cached;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "failed to prepare native playback cache metadata");
                        continue;
                    }
                }
            }
            if let Err(error) = self
                .store
                .persist_provider_playback(&provider, &current)
                .await
            {
                tracing::warn!(%error, "failed to persist native playback transition");
            }
        }
    }
}
