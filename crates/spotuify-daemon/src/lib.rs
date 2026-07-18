//! Daemon for spotuify.
//!
//! The Unix-socket server, request handler, in-memory event log
//! ring buffer (Phase 6.9), per-mutation receipt lifecycle wrapping
//! (Phase 6.6), and the doctor report assembly live here. The binary's
//! `src/main.rs` calls `server::ensure_daemon_running()` to autostart
//! the daemon and `server::run_daemon_with_pid_handle()` to host it
//! in-process when run with `daemon --foreground`.

pub mod analytics;
pub(crate) mod auth_sessions;
pub mod clock;
pub mod diagnostics;
pub mod handler;
mod handlers;
pub mod hook_executor;
pub(crate) mod lastfm_import;
pub mod logging;
pub mod player_factory;
pub(crate) mod provider_factory;
pub mod provider_registry;
pub(crate) mod queue_warm;
pub mod reminders;
pub mod retention;
pub mod server;
pub mod session_tracker;
pub mod state;
pub mod status;
pub mod undo;
pub mod update;
pub mod viz_coordinator;

pub use session_tracker::SessionTracker;
pub use viz_coordinator::VizCoordinator;

#[cfg(test)]
pub(crate) static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
