//! CLI: agent playlist scaffolds + selection helpers (Phase 7 leaf
//! extractions). The bulk of the CLI command surface still lives in
//! the binary while the workspace migration continues.

pub mod agent_playlists;
pub mod cli_args;
pub mod commands;
pub mod output;
pub mod selection;
mod style;

pub mod actions;

pub use cli_args::{
    AlbumCommand, AlbumGroup, ArtistCommand, LibraryCommand, LyricsCommand, MprisCommand,
    NotificationCommand, PlaylistCommand, QueueCommand, RadioCommand, ReminderCommand,
    SearchSourceArg, ShowCommand, VizCommand,
};
