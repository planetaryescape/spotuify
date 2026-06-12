//! Ratatui frontend for spotuify.
//!
//! `app` owns the App state struct and the input/event loop;
//! `ui` renders frames against ratatui; `tui_actions` is the
//! action registry consumed by both the keymap and the command
//! palette.

pub mod app;
pub(crate) mod hit;
pub mod now_playing;
pub mod tui_actions;
pub mod ui;
pub mod widgets;

pub use app::run_tui;
