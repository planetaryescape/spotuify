//! Phase 9.4a — Mercury cache.
//!
//! In-memory TTL-bounded cache wrapping a `MercuryFetcher`. Used by
//! the embedded backend's `mercury_get` (lyrics, autoplay flag, radio
//! recommendations) so repeated reads don't hammer Spotify's mercury
//! bus.
//!
//! Why in-memory and not SQLite: the player crate intentionally
//! doesn't depend on `spotuify-store` (dependency rule). The daemon
//! can layer persistence on top in a future phase if cache-warmth
//! across restarts becomes a need.
//!
//! Adversarial guarantees pinned by the tests:
//! - Hit doesn't re-invoke the fetcher (counter-based assertion).
//! - TTL drives expiry through an injected `Clock`.
//! - Different URIs stay isolated.
//! - Fetcher errors don't poison the cache slot.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use parking_lot::Mutex;
use thiserror::Error;

use crate::backends::clock::Clock;

const DEFAULT_TTL: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Error)]
pub enum MercuryError {
    #[error("mercury fetch failed: {0}")]
    Fetch(String),
}

#[async_trait]
pub trait MercuryFetcher: Send + Sync {
    async fn fetch(&self, uri: &str) -> Result<Bytes, MercuryError>;
}

pub struct CachedMercury<F: MercuryFetcher> {
    fetcher: F,
    clock: Arc<dyn Clock>,
    ttl: Duration,
    entries: Mutex<HashMap<String, Entry>>,
}

struct Entry {
    value: Bytes,
    stored_at: Instant,
}

impl<F: MercuryFetcher> CachedMercury<F> {
    pub fn new(fetcher: F, clock: Arc<dyn Clock>) -> Self {
        Self::with_ttl(fetcher, clock, DEFAULT_TTL)
    }

    pub fn with_ttl(fetcher: F, clock: Arc<dyn Clock>, ttl: Duration) -> Self {
        Self {
            fetcher,
            clock,
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub async fn get(&self, uri: &str) -> Result<Bytes, MercuryError> {
        if let Some(cached) = self.lookup(uri) {
            return Ok(cached);
        }
        let value = self.fetcher.fetch(uri).await?;
        self.entries.lock().insert(
            uri.to_string(),
            Entry {
                value: value.clone(),
                stored_at: self.clock.now(),
            },
        );
        Ok(value)
    }

    fn lookup(&self, uri: &str) -> Option<Bytes> {
        let entries = self.entries.lock();
        let entry = entries.get(uri)?;
        let now = self.clock.now();
        // Guard against backward clock steps (NTP) by clamping.
        let age = if now >= entry.stored_at {
            now.duration_since(entry.stored_at)
        } else {
            Duration::ZERO
        };
        if age > self.ttl {
            None
        } else {
            Some(entry.value.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    struct FakeClock {
        now: Mutex<Instant>,
    }

    impl FakeClock {
        fn arc() -> Arc<Self> {
            Arc::new(Self {
                now: Mutex::new(Instant::now()),
            })
        }

        fn advance(&self, by: Duration) {
            let mut now = self.now.lock();
            *now = now.checked_add(by).unwrap_or(*now);
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            *self.now.lock()
        }
    }

    struct CountingFetcher {
        calls: AtomicUsize,
        body: Vec<u8>,
        fail_next: AtomicUsize,
    }

    impl CountingFetcher {
        fn new(body: &[u8]) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                body: body.to_vec(),
                fail_next: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn fail_next_calls(&self, n: usize) {
            self.fail_next.store(n, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl MercuryFetcher for CountingFetcher {
        async fn fetch(&self, _uri: &str) -> Result<Bytes, MercuryError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let remaining = self.fail_next.load(Ordering::SeqCst);
            if remaining > 0 {
                self.fail_next.store(remaining - 1, Ordering::SeqCst);
                return Err(MercuryError::Fetch("scripted failure".to_string()));
            }
            Ok(Bytes::from(self.body.clone()))
        }
    }

    #[tokio::test]
    async fn miss_calls_fetcher_and_caches_value() {
        let fetcher = CountingFetcher::new(b"lyrics");
        let cache = CachedMercury::new(fetcher, FakeClock::arc());
        let body = cache
            .get("hm://lyrics/v1/track/A")
            .await
            .expect("mercury fetch should succeed");
        assert_eq!(&body[..], b"lyrics");
    }

    #[tokio::test]
    async fn hit_within_ttl_skips_fetcher() {
        let fetcher = CountingFetcher::new(b"lyrics");
        let clock = FakeClock::arc();
        let cache = CachedMercury::new(fetcher, clock.clone());

        let _ = cache
            .get("hm://lyrics/v1/track/A")
            .await
            .expect("initial mercury fetch should succeed");
        // Adversarial: assert the fetcher counter — not "got back the
        // right value" — so a regression where get() bypasses the
        // cache surfaces here.
        let counter_after_miss = cache.fetcher.calls();
        let _ = cache
            .get("hm://lyrics/v1/track/A")
            .await
            .expect("cached mercury fetch should succeed");
        assert_eq!(cache.fetcher.calls(), counter_after_miss);
    }

    #[tokio::test]
    async fn ttl_expiry_drives_refetch() {
        let fetcher = CountingFetcher::new(b"lyrics");
        let clock = FakeClock::arc();
        let cache = CachedMercury::with_ttl(fetcher, clock.clone(), Duration::from_secs(60));

        cache
            .get("hm://lyrics/v1/track/A")
            .await
            .expect("initial mercury fetch should succeed");
        clock.advance(Duration::from_secs(120)); // 2x TTL
        cache
            .get("hm://lyrics/v1/track/A")
            .await
            .expect("expired mercury fetch should refetch successfully");
        assert_eq!(cache.fetcher.calls(), 2, "expected refetch after TTL");
    }

    #[tokio::test]
    async fn different_uris_do_not_collide() {
        let fetcher = CountingFetcher::new(b"lyrics");
        let cache = CachedMercury::new(fetcher, FakeClock::arc());
        cache
            .get("hm://lyrics/v1/track/A")
            .await
            .expect("first mercury URI should fetch");
        cache
            .get("hm://lyrics/v1/track/B")
            .await
            .expect("second mercury URI should fetch");
        assert_eq!(cache.fetcher.calls(), 2);
    }

    #[tokio::test]
    async fn fetcher_error_does_not_poison_cache_slot() {
        // Adversarial: a transient mercury error must not block
        // subsequent reads. Catches the bug where the cache stores
        // the error or marks the slot "permanently failed".
        let fetcher = CountingFetcher::new(b"lyrics");
        fetcher.fail_next_calls(1);
        let cache = CachedMercury::new(fetcher, FakeClock::arc());

        let first = cache.get("hm://lyrics/v1/track/A").await;
        assert!(matches!(first, Err(MercuryError::Fetch(_))));

        let second = cache
            .get("hm://lyrics/v1/track/A")
            .await
            .expect("retry after fetch failure should succeed");
        assert_eq!(&second[..], b"lyrics");
    }
}
