//! Daemon for spotuify.
//!
//! The Unix-socket server, request handler, in-memory event log
//! ring buffer (Phase 6.9), per-mutation receipt lifecycle wrapping
//! (Phase 6.6), and the doctor report assembly live here. The binary's
//! `src/main.rs` calls `server::ensure_daemon_running()` to autostart
//! the daemon and `server::run_daemon_with_pid_handle()` to host it
//! in-process when run with `daemon --foreground`.

pub mod analytics;
pub mod diagnostics;
pub mod handler;
pub mod hook_executor;
pub mod logging;
pub mod player_factory;
pub mod server;
pub mod session_tracker;
pub mod state;
pub mod status;
pub mod undo;

pub use session_tracker::SessionTracker;
