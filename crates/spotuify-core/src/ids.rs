//! Compatibility re-exports for the typed Spotify ID newtypes.
//!
//! URI parsing and formatting live in [`crate::uri`]. This module remains so
//! existing `spotuify_core::ids::*` imports do not break.

pub use crate::uri::{AlbumId, ArtistId, PlaylistId, TrackId};
