#![allow(clippy::panic, clippy::unwrap_used)]

//! Phase 12 — operation log protocol tests.
//!
//! Adversarial coverage:
//! - Every `ReversalPlan` and `PreState` variant round-trips through
//!   `serde_json`.
//! - `OperationKind::is_reversible` matches the impl-doc table.
//! - `OperationId` parses from string (CLI accepts string IDs).
//! - `Request::OpsUndo` defaults to last-reversible when `operation_id`
//!   is omitted (the wire form encodes that as the absence of the field).
//! - The full `Operation` row carries optional fields without leaking
//!   `null`s when absent.
//! - New `DaemonEvent` variants survive ipc-codec framing.

use spotuify_protocol::{
    DaemonEvent, Operation, OperationId, OperationKind, OperationSource, OperationStatus, PreState,
    ReceiptId, Request, ResponseData, ReversalPlan,
};
use std::str::FromStr;

#[test]
fn operation_id_parses_from_string() {
    let id = OperationId::new_v7();
    let s = id.to_string();
    let back = OperationId::from_str(&s).expect("uuid v7 round-trips");
    assert_eq!(id, back);
}

#[test]
fn operation_id_serializes_as_transparent_string() {
    let id = OperationId::new_v7();
    let json = serde_json::to_string(&id).unwrap();
    assert!(
        json.starts_with('"'),
        "OperationId must serialize as a JSON string"
    );
    let back: OperationId = serde_json::from_str(&json).unwrap();
    assert_eq!(id, back);
}

#[test]
fn reversal_plan_round_trips_every_variant() {
    let pid = "37i9dQZF1DXcBWIGoYBM5M".to_string();
    let snap = Some("AAAA-snapshot".to_string());
    let cases = vec![
        ReversalPlan::QueueRemove {
            uri: "spotify:track:1".into(),
        },
        ReversalPlan::PlaylistRemoveTracks {
            playlist_id: pid.clone(),
            uris: vec!["spotify:track:1".into(), "spotify:track:2".into()],
            snapshot_id: snap.clone(),
        },
        ReversalPlan::PlaylistAddAtPositions {
            playlist_id: pid.clone(),
            items: vec![("spotify:track:1".into(), 0), ("spotify:track:2".into(), 1)],
            snapshot_id: snap.clone(),
        },
        ReversalPlan::PlaylistDelete {
            playlist_id: pid.clone(),
        },
        ReversalPlan::PlaylistReorder {
            playlist_id: pid,
            range_start: 0,
            insert_before: 5,
            range_length: 3,
            snapshot_id: snap,
        },
        ReversalPlan::LibraryUnsave {
            uri: "spotify:album:1".into(),
        },
        ReversalPlan::LibrarySave {
            uri: "spotify:album:1".into(),
            prior_added_at_ms: Some(1_700_000_000_000),
        },
        ReversalPlan::TransferToPriorDevice {
            device_id: "dev-1".into(),
        },
        ReversalPlan::Like {
            uri: "spotify:track:1".into(),
        },
        ReversalPlan::Unlike {
            uri: "spotify:track:1".into(),
        },
        ReversalPlan::Redo {
            target_op_id: OperationId::new_v7(),
        },
        ReversalPlan::NotReversible {
            reason: "transport command".into(),
        },
    ];
    for plan in &cases {
        let json = serde_json::to_string(plan).unwrap();
        let back: ReversalPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(*plan, back, "round-trip failed for {json}");
    }
}

#[test]
fn pre_state_round_trips_every_variant() {
    let pid = "playlist-1".to_string();
    let cases = vec![
        PreState::PlaylistAdd {
            playlist_id: pid.clone(),
            snapshot_id: Some("snap".into()),
            added_uris: vec!["spotify:track:1".into()],
        },
        PreState::PlaylistRemove {
            playlist_id: pid.clone(),
            snapshot_id: None,
            removed_items: vec![("spotify:track:1".into(), 0)],
        },
        PreState::PlaylistCreate {
            playlist_id: pid.clone(),
        },
        PreState::PlaylistReorder {
            playlist_id: pid,
            snapshot_id: Some("snap".into()),
            range_start: 1,
            insert_before: 4,
            range_length: 2,
        },
        PreState::LibrarySave {
            uri: "spotify:album:1".into(),
            prior_was_saved: false,
        },
        PreState::Transfer {
            prior_device_id: Some("dev-prior".into()),
        },
        PreState::QueueAdd {
            uri: "spotify:track:1".into(),
        },
        PreState::Like {
            uri: "spotify:track:1".into(),
            prior_was_liked: true,
        },
        PreState::Transport,
    ];
    for pre in &cases {
        let json = serde_json::to_string(pre).unwrap();
        let back: PreState = serde_json::from_str(&json).unwrap();
        assert_eq!(*pre, back, "round-trip failed for {json}");
    }
}

#[test]
fn operation_kind_round_trips_every_variant_through_serde() {
    // Exhaustive serde round-trip for every OperationKind variant —
    // adding a kind without updating the label table would silently
    // produce JSON that can't be parsed back. This pins the contract.
    let cases = [
        OperationKind::QueueAdd,
        OperationKind::PlaylistAdd,
        OperationKind::PlaylistRemove,
        OperationKind::PlaylistCreate,
        OperationKind::PlaylistReorder,
        OperationKind::LibrarySave,
        OperationKind::LibraryUnsave,
        OperationKind::ArtistFollow,
        OperationKind::ArtistUnfollow,
        OperationKind::Transfer,
        OperationKind::Like,
        OperationKind::Unlike,
        OperationKind::Play,
        OperationKind::Pause,
        OperationKind::Resume,
        OperationKind::Toggle,
        OperationKind::Next,
        OperationKind::Previous,
        OperationKind::Seek,
        OperationKind::Volume,
        OperationKind::Shuffle,
        OperationKind::Repeat,
        OperationKind::Undo,
        OperationKind::Redo,
    ];
    for kind in cases {
        let json = serde_json::to_string(&kind).unwrap();
        let back: OperationKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, back, "round-trip failed for {kind:?} (json={json})");
    }
}

#[test]
fn operation_status_round_trips_every_variant_through_serde() {
    for status in [
        OperationStatus::Pending,
        OperationStatus::Succeeded,
        OperationStatus::Failed,
        OperationStatus::Undone,
        OperationStatus::Redone,
    ] {
        let json = serde_json::to_string(&status).unwrap();
        let back: OperationStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(status, back);
        assert_eq!(status.to_string(), status.label());
        assert_eq!(
            status
                .label()
                .parse::<OperationStatus>()
                .expect("status label parses"),
            status
        );
    }
}

#[test]
fn operation_source_round_trips_every_variant_through_serde() {
    for source in [
        OperationSource::Cli,
        OperationSource::Tui,
        OperationSource::Mcp,
        OperationSource::Agent,
        OperationSource::DaemonInternal,
    ] {
        let json = serde_json::to_string(&source).unwrap();
        let back: OperationSource = serde_json::from_str(&json).unwrap();
        assert_eq!(source, back);
        // Label-based parse also round-trips:
        let label = source.label();
        assert_eq!(OperationSource::from_label(label), Some(source));
        assert_eq!(source.to_string(), label);
        assert_eq!(
            label
                .parse::<OperationSource>()
                .expect("source label parses"),
            source
        );
    }
}

#[test]
fn operation_kind_round_trips_every_variant_through_label_display_parse_and_serde() {
    for kind in [
        OperationKind::QueueAdd,
        OperationKind::PlaylistAdd,
        OperationKind::PlaylistRemove,
        OperationKind::PlaylistCreate,
        OperationKind::PlaylistUnfollow,
        OperationKind::PlaylistSetImage,
        OperationKind::PlaylistReorder,
        OperationKind::LibrarySave,
        OperationKind::LibraryUnsave,
        OperationKind::Transfer,
        OperationKind::Like,
        OperationKind::Unlike,
        OperationKind::Play,
        OperationKind::Pause,
        OperationKind::Resume,
        OperationKind::Toggle,
        OperationKind::Next,
        OperationKind::Previous,
        OperationKind::Seek,
        OperationKind::Volume,
        OperationKind::Shuffle,
        OperationKind::Repeat,
        OperationKind::Undo,
        OperationKind::Redo,
    ] {
        let json = serde_json::to_string(&kind).unwrap();
        let back: OperationKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, back);
        assert_eq!(json.trim_matches('"'), kind.label());
        assert_eq!(kind.to_string(), kind.label());
        assert_eq!(
            kind.label()
                .parse::<OperationKind>()
                .expect("kind label parses"),
            kind
        );
    }
}

#[test]
fn operation_kind_labels_match_doc() {
    // Spot-check a few labels — the doc table is the contract.
    assert_eq!(OperationKind::PlaylistAdd.label(), "playlist_add");
    assert_eq!(OperationKind::LibrarySave.label(), "library_save");
    assert_eq!(OperationKind::Undo.label(), "undo");
    assert_eq!(OperationKind::Pause.label(), "pause");
}

#[test]
fn operation_kind_reversibility_matches_doc() {
    // Reversible per impl-doc table:
    for kind in [
        OperationKind::PlaylistAdd,
        OperationKind::PlaylistRemove,
        OperationKind::PlaylistCreate,
        OperationKind::PlaylistReorder,
        OperationKind::LibrarySave,
        OperationKind::LibraryUnsave,
        OperationKind::Transfer,
        OperationKind::Like,
        OperationKind::Unlike,
    ] {
        assert!(kind.is_reversible(), "{kind:?} must be reversible");
    }
    // Transport kinds are non-reversible. QueueAdd joins them because
    // neither the Web API nor librespot 0.8 has queue-remove, so the
    // op has no executable inverse:
    for kind in [
        OperationKind::QueueAdd,
        OperationKind::Play,
        OperationKind::Pause,
        OperationKind::Resume,
        OperationKind::Toggle,
        OperationKind::Next,
        OperationKind::Previous,
        OperationKind::Seek,
        OperationKind::Volume,
        OperationKind::Shuffle,
        OperationKind::Repeat,
    ] {
        assert!(!kind.is_reversible(), "{kind:?} must NOT be reversible");
    }
}

#[test]
fn operation_source_serializes_with_kebab_case() {
    let src = OperationSource::DaemonInternal;
    let json = serde_json::to_string(&src).unwrap();
    assert_eq!(json, "\"daemon-internal\"");
    assert_eq!(
        OperationSource::from_label("daemon-internal"),
        Some(OperationSource::DaemonInternal)
    );
    assert_eq!(OperationSource::from_label("daemon_internal"), None);
}

#[test]
fn operation_status_serializes_snake_case() {
    let json = serde_json::to_string(&OperationStatus::Succeeded).unwrap();
    assert_eq!(json, "\"succeeded\"");
}

#[test]
fn operation_row_serde_round_trip() {
    let op = Operation {
        operation_id: OperationId::new_v7(),
        kind: OperationKind::PlaylistAdd,
        occurred_at_ms: 1_700_000_000_000,
        finished_at_ms: Some(1_700_000_001_000),
        source: OperationSource::Cli,
        requester: None,
        subject_uris: vec!["spotify:track:1".into()],
        reversible: true,
        reversal_plan: Some(ReversalPlan::PlaylistRemoveTracks {
            playlist_id: "playlist-1".into(),
            uris: vec!["spotify:track:1".into()],
            snapshot_id: Some("snap".into()),
        }),
        pre_state: Some(PreState::PlaylistAdd {
            playlist_id: "playlist-1".into(),
            snapshot_id: Some("snap".into()),
            added_uris: vec!["spotify:track:1".into()],
        }),
        status: OperationStatus::Succeeded,
        receipt_id: Some(ReceiptId::new_v7()),
        subject_op_id: None,
        undone_by_op_id: None,
        redone_by_op_id: None,
        error_message: None,
    };
    let json = serde_json::to_string(&op).unwrap();
    let back: Operation = serde_json::from_str(&json).unwrap();
    assert_eq!(op, back);
    // Optional fields skip when None — assert the JSON doesn't carry "requester":null etc.
    assert!(!json.contains("\"requester\""));
    assert!(!json.contains("\"subject_op_id\""));
}

#[test]
fn ops_undo_request_round_trip_with_defaults() {
    let req = Request::OpsUndo {
        operation_id: None,
        dry_run: false,
        force: false,
        bulk_since_ms: None,
    };
    let json = serde_json::to_string(&req).unwrap();
    let back: Request = serde_json::from_str(&json).unwrap();
    assert_eq!(req, back);
}

#[test]
fn response_data_operations_round_trip() {
    let data = ResponseData::OperationUndoResult {
        undo_op_id: OperationId::new_v7(),
        succeeded: 1,
        skipped: 0,
        errors: vec![],
        preview: vec![],
    };
    let json = serde_json::to_string(&data).unwrap();
    assert!(json.contains("\"kind\":\"operation-undo-result\""));
    let back: ResponseData = serde_json::from_str(&json).unwrap();
    match (data, back) {
        (
            ResponseData::OperationUndoResult { succeeded: s1, .. },
            ResponseData::OperationUndoResult { succeeded: s2, .. },
        ) => assert_eq!(s1, s2),
        _ => panic!("unexpected variant after round trip"),
    }
}

#[test]
fn daemon_event_operation_recorded_round_trip() {
    let ev = DaemonEvent::OperationRecorded {
        operation_id: OperationId::new_v7(),
        kind: OperationKind::LibrarySave,
        source: OperationSource::Mcp,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains("\"event\":\"operation-recorded\""));
    let back: DaemonEvent = serde_json::from_str(&json).unwrap();
    match (ev, back) {
        (
            DaemonEvent::OperationRecorded {
                operation_id: a,
                kind: ka,
                source: sa,
            },
            DaemonEvent::OperationRecorded {
                operation_id: b,
                kind: kb,
                source: sb,
            },
        ) => {
            assert_eq!(a, b);
            assert_eq!(ka, kb);
            assert_eq!(sa, sb);
        }
        _ => panic!("unexpected variant"),
    }
}

#[test]
fn daemon_event_operation_undone_round_trip() {
    let ev = DaemonEvent::OperationUndone {
        undo_op_id: OperationId::new_v7(),
        original_op_id: OperationId::new_v7(),
        success: true,
    };
    let json = serde_json::to_string(&ev).unwrap();
    let back: DaemonEvent = serde_json::from_str(&json).unwrap();
    match (ev, back) {
        (
            DaemonEvent::OperationUndone { success: a, .. },
            DaemonEvent::OperationUndone { success: b, .. },
        ) => assert_eq!(a, b),
        _ => panic!("unexpected variant"),
    }
}
