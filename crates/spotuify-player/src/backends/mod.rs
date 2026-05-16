//! PlayerBackend implementations.
//!
//! - [`embedded`] (Phase 9.2+, Phase 0 cleanup): in-process librespot
//!   Player + Spirc. Sole supported runtime backend.
//! - [`mock`] (test/`test-support` feature only): in-memory test double
//!   for other crates' tests.
//!
//! Pre-Phase-0 backends (spotifyd subprocess, ConnectOnly Web-API)
//! removed 2026-05-16 — spotuify is librespot-only.

pub mod audio_counter_tap;
pub mod clock;
pub mod mercury_cache;
pub mod premium_gate;
pub mod recovering_sink;
pub mod token_bridge;
pub mod visualization_tap;
pub mod worker;

#[cfg(feature = "embedded-playback")]
pub mod librespot_sink_chain;

#[cfg(any(test, feature = "test-support"))]
pub mod mock;

#[cfg(feature = "embedded-playback")]
pub mod embedded;
