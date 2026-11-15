# SQLx SQLite security backport

Source: the crates.io `sqlx-sqlite` 0.8.6 archive, SHA-256
`c2d12fe70b2c1b4401038055f90f151b78208de1f9f89a7dbfd41587a10c3eea`,
from upstream commit `bab1b022bd56a64f9a08b46b36b97c5cff19d77e`.
The Rust source and upstream MIT/Apache-2.0 licenses are unchanged.

The normalized `Cargo.toml` changes only `libsqlite3-sys` from 0.30.1 to
0.39.1. That selects patched bundled SQLite 3.54.1 for RUSTSEC-2026-0459,
without changing SQLx's API or the application's Rust 1.90 minimum.
SQLx 0.9 requires Rust 1.94 and its published SQLite driver still excludes
the patched bindings. Upstream subsequently allowed 0.39.x in
[PR #4502](https://github.com/launchbadge/sqlx/pull/4502).

This patch changes no application schema or migration. The store tests exercise
the actual native SQLite version and existing persistence behavior. Keep the
advisory check enabled. Remove this directory and its root Cargo patch when an
upstream driver compatible with the application's Rust minimum permits patched
SQLite bindings.
