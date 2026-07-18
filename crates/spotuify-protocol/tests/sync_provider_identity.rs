#![allow(clippy::panic, clippy::unwrap_used)]

use spotuify_core::ProviderId;
use spotuify_protocol::{CacheSyncSummary, DaemonEvent, SyncTargetData};

#[test]
fn legacy_sync_events_decode_without_provider_identity() {
    let started: DaemonEvent =
        serde_json::from_str(r#"{"event":"sync-started","target":"library"}"#).unwrap();
    assert!(matches!(
        started,
        DaemonEvent::SyncStarted {
            target: SyncTargetData::Library,
            provider: None
        }
    ));

    let finished: DaemonEvent = serde_json::from_str(
        r#"{
            "event":"sync-finished",
            "summary":{
                "target":"library",
                "playback_snapshots":0,
                "devices":0,
                "playlists":0,
                "playlist_items":0,
                "recent_items":0,
                "library_items":1,
                "media_items":1
            }
        }"#,
    )
    .unwrap();
    assert!(matches!(
        finished,
        DaemonEvent::SyncFinished { summary } if summary.provider.is_none()
    ));
}

#[test]
fn provider_identity_round_trips_on_sync_events() {
    let event = DaemonEvent::SyncFinished {
        summary: CacheSyncSummary {
            target: SyncTargetData::Library,
            provider: Some(ProviderId::new("apple").unwrap()),
            playback_snapshots: 0,
            queue_snapshots: 0,
            queue_items: 0,
            devices: 0,
            playlists: 0,
            playlist_items: 0,
            recent_items: 0,
            library_items: 1,
            media_items: 1,
            status: Default::default(),
            error: None,
            provider_outcomes: Vec::new(),
        },
    };
    let json = serde_json::to_string(&event).unwrap();
    let decoded: DaemonEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        decoded,
        DaemonEvent::SyncFinished { summary }
            if summary.provider.as_ref().map(ProviderId::as_str) == Some("apple")
    ));
}

#[test]
fn failed_sync_outcome_is_additive_to_legacy_event_shape() {
    let failed: DaemonEvent = serde_json::from_str(
        r#"{
            "event":"sync-finished",
            "summary":{
                "target":"library",
                "provider":"apple",
                "playback_snapshots":0,
                "devices":0,
                "playlists":0,
                "playlist_items":0,
                "recent_items":0,
                "library_items":0,
                "media_items":0,
                "status":"failed",
                "error":"timed out"
            }
        }"#,
    )
    .unwrap();
    assert!(matches!(
        failed,
        DaemonEvent::SyncFinished { summary }
            if summary.status == spotuify_protocol::SyncCompletionStatus::Failed
                && summary.error.as_deref() == Some("timed out")
    ));
}
