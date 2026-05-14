//! PlayerBackend implementations.
//!
//! - [`connect_only`] (Phase 9.0): Web API transfer only. No local audio
//!   output. Works for Free accounts.
//! - [`mock`] (Phase 9.0): in-memory test double behind the
//!   `test-support` feature flag. Other crates' tests use this to
//!   drive the daemon without touching real Spotify.
//! - `spotifyd` wrapping (Phase 9.1) and `embedded` librespot
//!   (Phase 9.2+) land in later sub-phases.

pub mod audio_counter_tap;
pub mod clock;
pub mod connect_only;
pub mod mercury_cache;
pub mod premium_gate;
pub mod recovering_sink;
pub mod spotifyd;
pub mod token_bridge;
pub mod worker;

#[cfg(any(test, feature = "test-support"))]
pub mod mock;

#[cfg(feature = "embedded-playback")]
pub mod embedded;
