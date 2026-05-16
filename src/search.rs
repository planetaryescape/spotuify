//! Re-export bridge for spotuify-search. The implementation lives in
//! `crates/spotuify-search/`. Migrating callers off this shim is incremental.

#![allow(unused_imports)]

pub use spotuify_search::*;
