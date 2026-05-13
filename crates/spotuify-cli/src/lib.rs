//! CLI: agent playlist scaffolds + selection helpers (Phase 7 leaf
//! extractions). The bulk of the CLI command surface still lives in
//! the binary because it threads through analytics + spotify_client.

pub mod agent_playlists;
pub mod cli_args;
pub mod commands;
pub mod output;

// actions and selection moved to spotuify-spotify so the daemon
// handler can consume them without pulling cli's clap dep tree in
// (which would create a cli↔daemon dep cycle).
pub mod actions {
    pub use spotuify_spotify::actions::*;
}
pub mod selection {
    pub use spotuify_spotify::selection::*;
}

pub use cli_args::{LibraryCommand, PlaylistCommand, QueueCommand};
