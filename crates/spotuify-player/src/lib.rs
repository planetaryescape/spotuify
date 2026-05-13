//! Player backend abstraction for spotuify.
//!
//! Phase 9 will land:
//! - `PlayerBackend` trait
//! - `EmbeddedBackend` (in-process librespot)
//! - `ConnectOnlyBackend` (Web API transfer only)
//!
//! The legacy `spotifyd` helper now lives in spotuify-spotify since
//! every dep edge that needed it already reaches spotuify-spotify
//! (and we couldn't put it here without creating a cycle). When the
//! PlayerBackend trait lands, the spotifyd backend will move back
//! and re-export from there.

pub use spotuify_spotify::spotifyd;
