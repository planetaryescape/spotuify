//! PlayerBackend implementations.
//!
//! - [`embedded`]: in-process player supplied by the built-in provider adapter.
//! - [`mock`] (test/`test-support` feature only): in-memory test double
//!   for other crates' tests.

pub mod audio_counter_tap;
pub mod clock;
pub mod recovering_sink;
pub mod token_bridge;
pub mod visualization_tap;
pub mod worker;

#[cfg(feature = "embedded-playback")]
pub mod first_party_auth;

#[cfg(feature = "embedded-playback")]
pub mod librespot_sink_chain;

// Mock backend is exposed unconditionally for integration tests and headless
// smoke runs. Production code paths never construct it.
pub mod mock;

#[cfg(feature = "embedded-playback")]
pub mod embedded;
