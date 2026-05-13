//! Background sync engine for spotuify.
//!
//! Currently empty scaffolding. The legacy sync implementation lives in the
//! binary at `src/sync.rs`. It depends on `daemon::state::DaemonState`, so
//! moving it here requires decoupling that seam first (extract a
//! `SyncContext` trait, or move sync into spotuify-daemon).
