//! Phase 10 (P10.1) — SessionTracker finalize integration tests.
//!
//! Verifies the three behaviour scenarios called out in
//! `docs/implementation/13-phase-10-analytics-derivations.md`'s
//! "Verification" section:
//!
//! 1. A track played to ~60% qualifies → listen_facts row with
//!    `qualified = 1`.
//! 2. A track skipped in <5 seconds → listen_facts row with
//!    `qualified = 0`, skip_reason recorded.
//! 3. A `SessionDisconnected` event mid-track → `qualified = 0`
//!    regardless of accumulated audible time (blueprint rule:
//!    AirPods unpaired never counts as a qualified listen).
//!
//! These tests construct `SessionState` snapshots directly and call
//! `SessionTracker::finalize`. That is the exact path
//! `observe()` takes when an `EndOfTrack` / `TrackChanged` /
//! `SessionDisconnected` event arrives; production callers reach it
//! through the player-event stream.
//!
//! The tests assert via observable side-effects on the store:
//! - `listen_facts` row exists with the expected columns.
//! - `track_metrics` upsert applied the right increment.
//!
//! Anti-implementation-coupling guarantees:
//! - We don't assert on internal call counts or method orderings.
//! - We don't poke private fields; the only test-only surface is
//!   `pub fn finalize` and `pub enum SessionState`, both of which are
//!   part of the daemon's deliberate test interface.

use std::sync::Arc;

use spotuify_core::{
    qualify_listen, BackendLabel, MediaItem, MediaKind, PlaybackSource, SkipReason,
    QUALIFICATION_RULE_VERSION,
};
use spotuify_daemon::session_tracker::{SessionState, SessionTracker};
use spotuify_protocol::IpcPayload;
use spotuify_store::Store;
use tokio::sync::broadcast;

async fn in_memory_store() -> Arc<Store> {
    Arc::new(
        Store::in_memory()
            .await
            .expect("in-memory store should open"),
    )
}

fn listen_qualified_uri(payload: IpcPayload) -> Option<String> {
    match payload {
        IpcPayload::Event(spotuify_protocol::DaemonEvent::ListenQualified {
            track_uri, ..
        }) => Some(track_uri),
        _ => None,
    }
}

fn playing_snapshot(
    session_id: &str,
    uri: &str,
    started_at_ms: i64,
    last_position_ms: u32,
    accumulated_paused_ms: i64,
    private_session: bool,
) -> SessionState {
    SessionState::Playing {
        session_id: session_id.to_string(),
        uri: uri.to_string(),
        started_at_ms,
        last_position_ms,
        accumulated_paused_ms,
        source: PlaybackSource::Unknown,
        backend: BackendLabel::Embedded,
        private_session,
    }
}

async fn fact_count(store: &Store) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM listen_facts")
        .fetch_one(store.reader())
        .await
        .expect("listen_facts count should load")
}

async fn latest_fact(store: &Store) -> (String, i64, i64, i64, Option<String>) {
    let row: (String, i64, i64, i64, Option<String>) = sqlx::query_as(
        "SELECT track_uri, qualified, audible_ms, qualification_rule_version, skip_reason
         FROM listen_facts
         ORDER BY id DESC
         LIMIT 1",
    )
    .fetch_one(store.reader())
    .await
    .expect("latest listen_fact should load");
    row
}

async fn track_metric_for(store: &Store, uri: &str) -> Option<(i64, i64, i64)> {
    sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT qualified_count, skip_count, total_audible_ms
         FROM track_metrics
         WHERE track_uri = ?",
    )
    .bind(uri)
    .fetch_optional(store.reader())
    .await
    .expect("track metric query should load")
}

async fn cache_track_duration(store: &Store, uri: &str, duration_ms: u64) {
    store
        .upsert_media_items(
            &[MediaItem {
                id: Some(uri.rsplit(':').next().unwrap_or(uri).to_string()),
                uri: uri.to_string(),
                name: "Cached Track".to_string(),
                subtitle: "Cached Artist".to_string(),
                context: "Cached Album".to_string(),
                duration_ms,
                image_url: None,
                kind: MediaKind::Track,
                source: Some("test".to_string()),
                freshness: None,
                explicit: None,
                is_playable: Some(true),
            }],
            "test",
        )
        .await
        .expect("track metadata should cache");
}

/// Cycle A — verification scenario 1: 60% audible play qualifies.
#[tokio::test]
async fn qualified_when_audible_reaches_threshold() {
    let store = in_memory_store().await;
    let (event_tx, mut event_rx) = broadcast::channel(8);
    let tracker = SessionTracker::with_store(store.clone(), event_tx);

    // A 3-minute track started ~108s in the past with no pauses gives
    // audible_ms ≈ 108_000. Threshold for a 180s track is min(90s, 240s)
    // floored at 30s = 90s. 108s > 90s → must qualify.
    let now = spotuify_core::now_ms();
    let snapshot = playing_snapshot(
        "session-A",
        "spotify:track:1",
        now - 108_000,
        180_000,
        0,
        false,
    );
    tracker.finalize(snapshot, SkipReason::TrackEnd).await;

    // Assert: one listen_fact row with qualified=1, correct rule version.
    assert_eq!(fact_count(&store).await, 1);
    let (uri, qualified, audible_ms, rule_version, skip_reason) = latest_fact(&store).await;
    assert_eq!(uri, "spotify:track:1");
    assert_eq!(qualified, 1, "60% play of a 180s track must qualify");
    assert!(
        audible_ms >= 90_000,
        "audible_ms must reach the 50% threshold; got {audible_ms}"
    );
    assert_eq!(rule_version as u32, QUALIFICATION_RULE_VERSION);
    assert_eq!(skip_reason.as_deref(), Some("track_end"));

    // Assert: track_metrics row incremented by 1 qualified, 0 skips.
    let metric = track_metric_for(&store, "spotify:track:1")
        .await
        .expect("track_metrics row must exist after finalize");
    assert_eq!(metric.0, 1, "qualified_count must be 1");
    assert_eq!(metric.1, 0, "skip_count must be 0 for a qualified listen");
    assert!(
        metric.2 >= 90_000,
        "total_audible_ms must include this listen"
    );

    // Assert: ListenQualified event emitted on the broadcast.
    let envelope = event_rx
        .try_recv()
        .expect("a ListenQualified event must be broadcast for a qualified, non-private listen");
    let track_uri = listen_qualified_uri(envelope.payload).expect("expected ListenQualified event");
    assert_eq!(track_uri, "spotify:track:1");
}

/// Cycle B — verification scenario 2: <5s skip is NOT qualified.
#[tokio::test]
async fn sub_five_second_skip_is_not_qualified() {
    let store = in_memory_store().await;
    let (event_tx, mut event_rx) = broadcast::channel(8);
    let tracker = SessionTracker::with_store(store.clone(), event_tx);

    // 4 seconds of audible time on a 180s track. Threshold is 90s.
    let now = spotuify_core::now_ms();
    let snapshot = playing_snapshot(
        "session-B",
        "spotify:track:2",
        now - 4_000,
        180_000,
        0,
        false,
    );
    tracker.finalize(snapshot, SkipReason::UserNext).await;

    assert_eq!(fact_count(&store).await, 1);
    let (_, qualified, _audible, _, skip_reason) = latest_fact(&store).await;
    assert_eq!(qualified, 0, "4 second skip must NOT qualify");
    assert_eq!(skip_reason.as_deref(), Some("user_next"));

    let metric = track_metric_for(&store, "spotify:track:2")
        .await
        .expect("track_metrics row must exist even for skipped listens");
    assert_eq!(metric.0, 0, "qualified_count must be 0 for a sub-5s skip");
    assert_eq!(metric.1, 1, "skip_count must be 1");

    // No ListenQualified event for an unqualified listen.
    assert!(
        event_rx.try_recv().is_err(),
        "no ListenQualified event should fire for a sub-threshold listen"
    );
}

/// Regression guard: duration must come from track metadata when we
/// have it, not from the last position. A 31s skip on a 4min track
/// crosses the absolute 30s floor but is nowhere near the real 50%
/// threshold, so it must NOT qualify.
#[tokio::test]
async fn long_track_skip_uses_cached_duration_not_last_position() {
    let store = in_memory_store().await;
    cache_track_duration(&store, "spotify:track:long", 240_000).await;
    let (event_tx, mut event_rx) = broadcast::channel(8);
    let tracker = SessionTracker::with_store(store.clone(), event_tx);

    let now = spotuify_core::now_ms();
    let snapshot = playing_snapshot(
        "session-long-skip",
        "spotify:track:long",
        now - 31_000,
        31_000,
        0,
        false,
    );
    tracker.finalize(snapshot, SkipReason::UserNext).await;

    let row: (i64, i64, i64) = sqlx::query_as(
        "SELECT duration_ms, audible_ms, qualified
         FROM listen_facts
         WHERE track_uri = 'spotify:track:long'",
    )
    .fetch_one(store.reader())
    .await
    .expect("listen fact should load");

    assert_eq!(row.0, 240_000, "duration must use cached track metadata");
    assert!(
        row.1 >= 30_000,
        "precondition: audible time should cross the absolute floor"
    );
    assert_eq!(row.2, 0, "31s audible on a 240s track must not qualify");
    assert!(
        event_rx.try_recv().is_err(),
        "false qualification must not emit ListenQualified"
    );
}

/// Cycle C — verification scenario 3: SessionDisconnected mid-play
/// never qualifies, regardless of audible_ms accrued.
#[tokio::test]
async fn session_died_never_qualifies_even_at_threshold() {
    let store = in_memory_store().await;
    let (event_tx, mut event_rx) = broadcast::channel(8);
    let tracker = SessionTracker::with_store(store.clone(), event_tx);

    // Build a snapshot that WOULD qualify under the threshold rule
    // (180s audible on a 180s track) but is being finalised as
    // session_died — the gate must override.
    let now = spotuify_core::now_ms();
    let snapshot = playing_snapshot(
        "session-C",
        "spotify:track:3",
        now - 180_000,
        180_000,
        0,
        false,
    );

    // Sanity: the threshold rule alone would qualify this listen.
    let q = qualify_listen(180_000, 180_000);
    assert!(
        q.qualified,
        "precondition: full-duration listen must qualify under the rule alone"
    );

    tracker.finalize(snapshot, SkipReason::SessionDied).await;

    let (_, qualified, _audible, _, skip_reason) = latest_fact(&store).await;
    assert_eq!(
        qualified, 0,
        "session_died must force qualified=false even when the threshold rule would otherwise pass"
    );
    assert_eq!(skip_reason.as_deref(), Some("session_died"));

    let metric = track_metric_for(&store, "spotify:track:3")
        .await
        .expect("track_metrics row must exist for session_died");
    assert_eq!(metric.0, 0, "qualified_count must be 0 after session_died");
    assert_eq!(metric.1, 1, "session_died counts as a skip");

    assert!(
        event_rx.try_recv().is_err(),
        "no ListenQualified event should fire for a session_died finalisation"
    );
}

/// Private session must suppress the `ListenQualified` event even when
/// the rule passes, while still recording the listen_fact row with
/// `private_session = 1` for local-only analytics.
#[tokio::test]
async fn private_session_suppresses_listen_qualified_event() {
    let store = in_memory_store().await;
    let (event_tx, mut event_rx) = broadcast::channel(8);
    let tracker = SessionTracker::with_store(store.clone(), event_tx);

    let now = spotuify_core::now_ms();
    let snapshot = playing_snapshot(
        "session-private",
        "spotify:track:4",
        now - 120_000,
        180_000,
        0,
        true, // private_session = true
    );
    tracker.finalize(snapshot, SkipReason::TrackEnd).await;

    // The listen_fact row still lands (Wrapped-style local analytics
    // is the point of private mode):
    let (uri, qualified, _audible, _, _) = latest_fact(&store).await;
    assert_eq!(uri, "spotify:track:4");
    assert_eq!(qualified, 1, "rule passes on a 120s/180s listen");

    let private: i64 = sqlx::query_scalar(
        "SELECT private_session FROM listen_facts WHERE track_uri = 'spotify:track:4'",
    )
    .fetch_one(store.reader())
    .await
    .expect("private_session flag should load");
    assert_eq!(private, 1, "private_session flag must be persisted");

    // But the public-facing event MUST NOT fire — that's what private
    // mode is for.
    assert!(
        event_rx.try_recv().is_err(),
        "private session must suppress ListenQualified emission"
    );
}

/// Pauses accumulated during playback reduce `audible_ms` so a track
/// where the user paused most of it does NOT qualify even when the
/// wall-clock window covers the threshold.
#[tokio::test]
async fn pauses_reduce_audible_time_below_threshold() {
    let store = in_memory_store().await;
    let (event_tx, _event_rx) = broadcast::channel(8);
    let tracker = SessionTracker::with_store(store.clone(), event_tx);

    // 3 minutes of wall clock elapsed, but 100s of it was paused.
    // Net audible = 80s on a 180s track → threshold is 90s → not qualified.
    let now = spotuify_core::now_ms();
    let snapshot = playing_snapshot(
        "session-paused",
        "spotify:track:5",
        now - 180_000,
        180_000,
        100_000, // accumulated_paused_ms
        false,
    );
    tracker.finalize(snapshot, SkipReason::TrackEnd).await;

    let (_, qualified, audible_ms, _, _) = latest_fact(&store).await;
    assert!(
        audible_ms <= 80_001,
        "audible_ms must subtract paused time; got {audible_ms}"
    );
    assert_eq!(
        qualified, 0,
        "80s audible on a 180s track must not cross the 90s threshold"
    );
}

/// Calling `finalize` on `SessionState::Idle` is a no-op — the
/// tracker only records actual listen sessions.
#[tokio::test]
async fn finalize_on_idle_state_is_a_noop() {
    let store = in_memory_store().await;
    let (event_tx, _event_rx) = broadcast::channel(8);
    let tracker = SessionTracker::with_store(store.clone(), event_tx);

    tracker
        .finalize(SessionState::Idle, SkipReason::TrackEnd)
        .await;
    assert_eq!(
        fact_count(&store).await,
        0,
        "finalize from Idle must NOT insert a listen_fact row"
    );
}

/// Second qualifying listen of the SAME track must accumulate the
/// per-entity metric counters — they are append-only rollups, not
/// per-session values.
#[tokio::test]
async fn track_metrics_accumulate_across_listens() {
    let store = in_memory_store().await;
    let (event_tx, _event_rx) = broadcast::channel(8);
    let tracker = SessionTracker::with_store(store.clone(), event_tx);

    let now = spotuify_core::now_ms();
    for session in ["a", "b", "c"] {
        let snapshot = playing_snapshot(
            session,
            "spotify:track:loop",
            now - 120_000,
            180_000,
            0,
            false,
        );
        tracker.finalize(snapshot, SkipReason::TrackEnd).await;
    }
    let metric = track_metric_for(&store, "spotify:track:loop")
        .await
        .expect("track_metrics row must exist for repeated listens");
    assert_eq!(
        metric.0, 3,
        "three qualified listens must increment qualified_count to 3"
    );
    assert_eq!(metric.1, 0, "no skips recorded");
    assert!(
        metric.2 >= 3 * 90_000,
        "total_audible_ms must accumulate (got {})",
        metric.2
    );
}
