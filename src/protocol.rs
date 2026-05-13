//! Re-export bridge for spotuify-protocol.
//!
//! The actual IPC types live in `crates/spotuify-protocol/`. This module
//! re-exports them so existing `crate::protocol::*` call sites keep
//! compiling during Phase 7's incremental extraction. Future PRs will
//! migrate callers to `use spotuify_protocol::*` and remove this file.

pub use spotuify_protocol::*;
