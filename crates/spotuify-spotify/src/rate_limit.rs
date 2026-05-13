//! Phase 6.3: rate-limit middleware for the Spotify Web API client.
//!
//! Wraps `reqwest::Client`, parses `Retry-After`, applies jittered
//! exponential backoff for 5xx, enforces per-priority concurrency caps,
//! persists the token-bucket budget across daemon restarts.
//!
//! Pattern from mxr `crates/daemon/src/loops.rs:435-441` and
//! `crates/provider-gmail/src/client.rs:102-120`.

// Body lands in Phase 6.3 implementation step.
