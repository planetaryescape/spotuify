//! Spotify Web API support crate for spotuify.
//!
//! Phase 6 lands typed errors, a compat normalizer, and a rate-limit
//! middleware in this crate.
//!
//! See `docs/implementation/09-phase-6-sync-hardening.md` and
//! `docs/implementation/10-phase-7-workspace-split.md`.

pub mod auth;
pub mod client;
pub mod compat;
pub mod config;
pub mod endpoints;
pub mod error;
pub mod first_party;
pub mod mercury;
pub mod provider;
pub mod rate_limit;
pub mod refresh_planner;
pub mod selection;

pub use client::{SpotifyClient, WebApiBearerProvider};

pub use error::{AuthErrorKind, SpotifyError, SpotifyResult};
