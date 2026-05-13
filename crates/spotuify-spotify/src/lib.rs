//! Spotify Web API support crate for spotuify.
//!
//! Phase 6 lands typed errors, a compat normalizer, and a rate-limit
//! middleware in this crate. The legacy `SpotifyClient` implementation
//! still lives in the root binary's `src/spotify.rs` during Phase 7's
//! incremental extraction; it consumes types from this crate via re-export
//! or direct import.
//!
//! See `docs/implementation/09-phase-6-sync-hardening.md` and
//! `docs/implementation/10-phase-7-workspace-split.md`.

pub mod compat;
pub mod error;
pub mod rate_limit;

pub use error::{AuthErrorKind, SpotifyError};
