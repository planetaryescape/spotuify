//! Re-export bridge for spotuify-store. The implementation lives in
//! `crates/spotuify-store/`. Migrating callers off this shim is incremental.

#![allow(unused_imports)]

pub use spotuify_store::*;
