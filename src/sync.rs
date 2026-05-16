//! Re-export bridge for spotuify-sync.
//!
//! The sync engine implementation lives in
//! `crates/spotuify-sync/src/sync_loop.rs` and is generic over the
//! `SyncContext` trait, with `DaemonState` providing the impl
//! (`src/daemon/state.rs`).

#![allow(unused_imports)]

pub use spotuify_sync::{spawn_background_scheduler, sync_target};
