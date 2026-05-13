//! Phase 6.2: Spotify response payload compat normalizer.
//!
//! Walks `serde_json::Value` before deserialization to backfill keys Spotify
//! has silently dropped. Returns the list of patched keys for telemetry
//! (`DaemonEvent::SchemaCompat`).
//!
//! Implementation pattern from spotatui `src/infra/network/requests.rs:129-240`.

// Body lands in Phase 6.2 implementation step.
