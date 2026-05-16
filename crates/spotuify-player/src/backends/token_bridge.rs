//! Phase 9.4b — two-token bridge.
//!
//! Wraps a token-fetching source with:
//! - A bounded 5s timeout. Per spotify-player's `token.rs:8-46`
//!   pattern: if the call hangs, we call `shutdown` on the source so
//!   the session can rebuild instead of the caller blocking.
//! - A small in-memory cache that's keyed on expiry — refresh when
//!   the token is within a refresh-headroom of expiry, NOT every
//!   call.
//! - Graceful degradation on refresh failure: if the refresh errors
//!   but the cached token is still inside its expiry window, return
//!   the cached value instead of bubbling the error.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use parking_lot::Mutex;
use thiserror::Error;

use crate::backends::clock::Clock;

const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const REFRESH_HEADROOM: Duration = Duration::from_secs(60);

/// Synchronous source of the Web API bearer token. The keyring-backed
/// implementation lives in the daemon wiring; tests use
/// `StaticTokenProvider`. Used by EmbeddedBackend to bridge librespot's
/// auth into spotuify's Web API client.
///
/// Distinct from `WebApiTokenSource` below: that one is async with
/// expiry-aware refresh; this one is a simple synchronous getter.
pub trait TokenProvider: Send + Sync {
    fn current_token(&self) -> Option<String>;
}

/// Test/utility provider that returns a fixed token (or none).
pub struct StaticTokenProvider {
    inner: Option<String>,
}

impl StaticTokenProvider {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            inner: Some(token.into()),
        }
    }

    pub fn missing() -> Self {
        Self { inner: None }
    }
}

impl TokenProvider for StaticTokenProvider {
    fn current_token(&self) -> Option<String> {
        self.inner.clone()
    }
}

#[derive(Debug, Error)]
pub enum TokenError {
    #[error("token fetch timed out after {0:?}")]
    Timeout(Duration),
    #[error("token source error: {0}")]
    Source(String),
}

#[derive(Debug, Clone)]
pub struct TokenWithExpiry {
    pub access_token: String,
    pub expires_at: Instant,
}

#[async_trait]
pub trait WebApiTokenSource: Send + Sync {
    async fn fetch_token(&self) -> Result<TokenWithExpiry, TokenError>;
    /// Called when fetch times out. Real impl tears down the
    /// librespot session; test fakes flip a flag.
    async fn shutdown(&self);
}

pub struct TokenBridge<S: WebApiTokenSource> {
    source: S,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
    timeout: Duration,
    headroom: Duration,
}

#[derive(Default)]
struct State {
    cached: Option<TokenWithExpiry>,
}

impl<S: WebApiTokenSource> TokenBridge<S> {
    pub fn new(source: S, clock: Arc<dyn Clock>) -> Self {
        Self {
            source,
            clock,
            state: Mutex::new(State::default()),
            timeout: FETCH_TIMEOUT,
            headroom: REFRESH_HEADROOM,
        }
    }

    pub fn with_timeout_and_headroom(
        source: S,
        clock: Arc<dyn Clock>,
        timeout: Duration,
        headroom: Duration,
    ) -> Self {
        Self {
            source,
            clock,
            state: Mutex::new(State::default()),
            timeout,
            headroom,
        }
    }

    /// Return the current token. Refreshes when within `headroom` of
    /// expiry. Falls back to the cached value if the refresh fails
    /// but the cache is still valid.
    pub async fn current(&self) -> Result<String, TokenError> {
        let cached = self.state.lock().cached.clone();
        let now = self.clock.now();

        // Use the cache when present and not near expiry.
        if let Some(token) = cached.as_ref() {
            if token.expires_at > now + self.headroom {
                return Ok(token.access_token.clone());
            }
        }

        // Attempt refresh, bounded by timeout.
        let refresh = tokio::time::timeout(self.timeout, self.source.fetch_token()).await;
        match refresh {
            Ok(Ok(fresh)) => {
                let access = fresh.access_token.clone();
                self.state.lock().cached = Some(fresh);
                Ok(access)
            }
            Ok(Err(err)) => {
                // Refresh errored. If we still have a not-yet-expired
                // cached token, hand it back rather than failing.
                if let Some(token) = cached {
                    if token.expires_at > now {
                        return Ok(token.access_token);
                    }
                }
                Err(err)
            }
            Err(_elapsed) => {
                // Hung. Tear down the session so it can rebuild.
                self.source.shutdown().await;
                // Same cache-fallback logic as the error path.
                if let Some(token) = cached {
                    if token.expires_at > now {
                        return Ok(token.access_token);
                    }
                }
                Err(TokenError::Timeout(self.timeout))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

    struct ScriptedSource {
        ttl: Duration,
        clock: Arc<FakeClock>,
        calls: AtomicUsize,
        shutdown_called: AtomicBool,
        delay: Mutex<Option<Duration>>,
        fail: AtomicBool,
        token_value: Mutex<String>,
    }

    impl ScriptedSource {
        fn new(ttl: Duration, clock: Arc<FakeClock>) -> Self {
            Self {
                ttl,
                clock,
                calls: AtomicUsize::new(0),
                shutdown_called: AtomicBool::new(false),
                delay: Mutex::new(None),
                fail: AtomicBool::new(false),
                token_value: Mutex::new("token-1".to_string()),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn shutdown_was_called(&self) -> bool {
            self.shutdown_called.load(Ordering::SeqCst)
        }

        fn set_delay(&self, d: Duration) {
            *self.delay.lock() = Some(d);
        }

        fn fail_next(&self) {
            self.fail.store(true, Ordering::SeqCst);
        }

        fn rotate_token(&self, new_value: &str) {
            *self.token_value.lock() = new_value.to_string();
        }
    }

    #[async_trait]
    impl WebApiTokenSource for ScriptedSource {
        async fn fetch_token(&self) -> Result<TokenWithExpiry, TokenError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let delay = *self.delay.lock();
            if let Some(d) = delay {
                tokio::time::sleep(d).await;
            }
            if self.fail.swap(false, Ordering::SeqCst) {
                return Err(TokenError::Source("scripted failure".to_string()));
            }
            let access_token = self.token_value.lock().clone();
            Ok(TokenWithExpiry {
                access_token,
                expires_at: self.clock.now() + self.ttl,
            })
        }

        async fn shutdown(&self) {
            self.shutdown_called.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn first_call_fetches_then_caches() {
        let clock = FakeClock::arc();
        let source = ScriptedSource::new(Duration::from_secs(3600), clock.clone());
        let bridge = TokenBridge::new(source, clock);

        let _ = bridge
            .current()
            .await
            .expect("first token fetch should succeed");
        let _ = bridge
            .current()
            .await
            .expect("cached token fetch should succeed");
        assert_eq!(bridge.source.calls(), 1, "cache hit must skip refetch");
    }

    #[tokio::test]
    async fn refresh_fires_within_headroom_of_expiry() {
        let clock = FakeClock::arc();
        let source = ScriptedSource::new(Duration::from_secs(120), clock.clone());
        let bridge = TokenBridge::with_timeout_and_headroom(
            source,
            clock.clone(),
            FETCH_TIMEOUT,
            Duration::from_secs(60),
        );

        let _ = bridge
            .current()
            .await
            .expect("initial token fetch should succeed");
        bridge.source.rotate_token("token-2");
        clock.advance(Duration::from_secs(70)); // within headroom

        let token = bridge
            .current()
            .await
            .expect("refresh within headroom should succeed");
        assert_eq!(token, "token-2");
        assert_eq!(bridge.source.calls(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn hang_triggers_shutdown_and_timeout_error() {
        // Adversarial: a hung source must NOT block the daemon. Per
        // spotify-player's pattern, we call shutdown() on timeout so
        // the session rebuilds.
        let clock = FakeClock::arc();
        let source = ScriptedSource::new(Duration::from_secs(3600), clock.clone());
        source.set_delay(Duration::from_secs(30));
        let bridge = TokenBridge::with_timeout_and_headroom(
            source,
            clock.clone(),
            Duration::from_secs(5),
            REFRESH_HEADROOM,
        );

        let err = bridge.current().await.expect_err("hang must time out");
        assert!(matches!(err, TokenError::Timeout(_)), "got {err:?}");
        assert!(
            bridge.source.shutdown_was_called(),
            "shutdown must fire on timeout"
        );
    }

    #[tokio::test]
    async fn refresh_failure_falls_back_to_still_valid_cached_token() {
        // Adversarial: a transient refresh blip mid-session must not
        // log the user out. As long as the cached token is still
        // unexpired we hand it back.
        let clock = FakeClock::arc();
        let source = ScriptedSource::new(Duration::from_secs(3600), clock.clone());
        let bridge = TokenBridge::with_timeout_and_headroom(
            source,
            clock.clone(),
            FETCH_TIMEOUT,
            Duration::from_secs(60),
        );

        let first = bridge
            .current()
            .await
            .expect("initial token fetch should succeed");
        // Push past headroom to force a refresh.
        clock.advance(Duration::from_secs(3540));
        bridge.source.fail_next();
        let second = bridge
            .current()
            .await
            .expect("refresh failure should fall back to cached token");
        assert_eq!(first, second, "expected cached fallback on refresh failure");
    }

    #[tokio::test]
    async fn refresh_failure_after_expiry_surfaces_error() {
        // Adversarial: after the cached token is actually expired,
        // the refresh failure must surface so callers re-auth instead
        // of using a known-dead token.
        let clock = FakeClock::arc();
        let source = ScriptedSource::new(Duration::from_secs(60), clock.clone());
        let bridge = TokenBridge::with_timeout_and_headroom(
            source,
            clock.clone(),
            FETCH_TIMEOUT,
            Duration::from_secs(10),
        );

        bridge
            .current()
            .await
            .expect("initial token fetch should succeed");
        clock.advance(Duration::from_secs(120)); // past expiry
        bridge.source.fail_next();
        let err = bridge
            .current()
            .await
            .expect_err("expired + fail must error");
        assert!(matches!(err, TokenError::Source(_)), "got {err:?}");
    }
}
