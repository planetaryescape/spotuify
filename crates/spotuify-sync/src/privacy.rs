//! Phase 10 (P10.2) — privacy gate.
//!
//! Suppresses `listen_qualified` emission + tags `listen_facts` rows
//! when the active Spotify session is private. Two signals drive the
//! decision:
//!
//! 1. `me().product == "open"` — Spotify Free / unauthenticated state;
//!    treated as "we have no permission to scrobble".
//! 2. `is_private_session` — Spotify's first-party private-listening
//!    flag (the user toggled "Start a private session" in the official
//!    app). Spotify doesn't surface this on every endpoint; when absent
//!    we conservatively assume the session is NOT private.
//!
//! The gate is a small struct so callers can cache the result for the
//! lifetime of a player session — the daemon refreshes it on
//! `SessionDisconnected → resumed` transitions.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Cached private-session signal. Two atomics so reads from the
/// SessionTracker hot path stay lock-free.
#[derive(Debug, Default)]
pub struct PrivacyGate {
    /// `true` when Spotify's product is `open` (Free / unauthenticated)
    /// — we treat this as a private session for the purpose of
    /// scrobble suppression.
    product_open: AtomicBool,
    /// `true` when Spotify's `is_private_session` flag is set.
    is_private: AtomicBool,
}

impl PrivacyGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn set_product(&self, product: &str) {
        self.product_open
            .store(product == "open", Ordering::Relaxed);
    }

    pub fn set_is_private(&self, is_private: bool) {
        self.is_private.store(is_private, Ordering::Relaxed);
    }

    /// Treat as private when EITHER flag is set. The conservative
    /// default (both false → public) only triggers suppression when
    /// Spotify explicitly told us so.
    pub fn is_private(&self) -> bool {
        self.product_open.load(Ordering::Relaxed) || self.is_private.load(Ordering::Relaxed)
    }
}

/// Redact a raw search query when the user has opted out of raw-query
/// storage via `[analytics] store_raw_queries = false`. The normalised
/// query hash is always preserved so derived analytics stay meaningful.
pub fn redact_search_query_if_disabled(store_raw_queries: bool, query: &str) -> Option<String> {
    if store_raw_queries {
        Some(query.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_gate_is_public() {
        let g = PrivacyGate::default();
        assert!(!g.is_private());
    }

    #[test]
    fn product_open_marks_private() {
        let g = PrivacyGate::default();
        g.set_product("open");
        assert!(g.is_private());
    }

    #[test]
    fn premium_product_stays_public() {
        let g = PrivacyGate::default();
        g.set_product("premium");
        assert!(!g.is_private());
    }

    #[test]
    fn explicit_private_session_marks_private() {
        let g = PrivacyGate::default();
        g.set_is_private(true);
        assert!(g.is_private());
    }

    #[test]
    fn clearing_the_flag_returns_to_public() {
        let g = PrivacyGate::default();
        g.set_is_private(true);
        assert!(g.is_private());
        g.set_is_private(false);
        assert!(!g.is_private());
    }

    #[test]
    fn redact_search_query_drops_raw_when_disabled() {
        assert_eq!(
            redact_search_query_if_disabled(false, "luther vandross"),
            None
        );
    }

    #[test]
    fn redact_search_query_keeps_raw_when_enabled() {
        assert_eq!(
            redact_search_query_if_disabled(true, "luther vandross"),
            Some("luther vandross".to_string())
        );
    }
}
