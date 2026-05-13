//! Player backend abstraction for spotuify.
//!
//! Phase 9 will land:
//! - `PlayerBackend` trait
//! - `EmbeddedBackend` (in-process librespot)
//! - `SpotifydBackend` (sibling subprocess)
//! - `ConnectOnlyBackend` (Web API transfer only)
//!
//! For now this crate is empty scaffolding so the workspace structure
//! matches the blueprint. The legacy spotifyd helper still lives at the
//! binary's `src/spotifyd.rs`.
