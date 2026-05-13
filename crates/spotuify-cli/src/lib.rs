//! CLI: agent playlist scaffolds + selection helpers (Phase 7 leaf
//! extractions). The bulk of the CLI command surface still lives in
//! the binary because it threads through analytics + spotify_client.

pub mod actions;
pub mod agent_playlists;
pub mod cli_args;
pub mod commands;
pub mod output;
pub mod selection;

pub use cli_args::{LibraryCommand, PlaylistCommand, QueueCommand};
