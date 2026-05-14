//! Phase 10 — analytics IPC types.
//!
//! Adversarial coverage:
//! - `SinceWindow` JSON shape (`"all"` vs `{"days": 30}`).
//! - Every `Request::Analytics*` round-trips.
//! - `ResponseData::AnalyticsTop` decodes back to the same rows.
//! - `DaemonEvent::ListenQualified` carries optional artist/album.

use spotuify_protocol::{
    DaemonEvent, ExportTarget, HabitWindow, Request, ResponseData, SearchMode, SinceWindow,
    TopEntry, TopKind,
};

#[test]
fn since_window_json_shape() {
    let all = SinceWindow::All;
    let days = SinceWindow::Days(30);
    let all_json = serde_json::to_string(&all).unwrap();
    let days_json = serde_json::to_string(&days).unwrap();
    assert_eq!(all_json, "\"all\"");
    assert_eq!(days_json, "{\"days\":30}");

    let all_back: SinceWindow = serde_json::from_str("\"all\"").unwrap();
    let days_back: SinceWindow = serde_json::from_str("{\"days\": 90}").unwrap();
    assert_eq!(all_back, SinceWindow::All);
    assert_eq!(days_back, SinceWindow::Days(90));
}

#[test]
fn habit_window_serializes_snake_case() {
    assert_eq!(
        serde_json::to_string(&HabitWindow::Week).unwrap(),
        "\"week\""
    );
}

#[test]
fn top_kind_serializes_snake_case() {
    assert_eq!(
        serde_json::to_string(&TopKind::Tracks).unwrap(),
        "\"tracks\""
    );
}

#[test]
fn analytics_top_request_round_trip() {
    let req = Request::AnalyticsTop {
        kind: TopKind::Artists,
        since_window: SinceWindow::Days(30),
        limit: 25,
    };
    let json = serde_json::to_string(&req).unwrap();
    let back: Request = serde_json::from_str(&json).unwrap();
    assert_eq!(req, back);
}

#[test]
fn analytics_habits_request_round_trip() {
    let req = Request::AnalyticsHabits {
        window: HabitWindow::Month,
        since_ms: Some(1_700_000_000_000),
    };
    let json = serde_json::to_string(&req).unwrap();
    let back: Request = serde_json::from_str(&json).unwrap();
    assert_eq!(req, back);
}

#[test]
fn analytics_search_request_round_trip() {
    let req = Request::AnalyticsSearch {
        mode: SearchMode::Normalized,
        limit: 100,
    };
    let json = serde_json::to_string(&req).unwrap();
    let back: Request = serde_json::from_str(&json).unwrap();
    assert_eq!(req, back);
}

#[test]
fn analytics_export_round_trip() {
    let req = Request::AnalyticsExport {
        target: ExportTarget::ListenBrainz,
        since_ms: Some(1_700_000_000_000),
    };
    let json = serde_json::to_string(&req).unwrap();
    assert!(json.contains("\"listen_brainz\""));
    let back: Request = serde_json::from_str(&json).unwrap();
    assert_eq!(req, back);
}

#[test]
fn analytics_prune_request_round_trip() {
    let req = Request::AnalyticsPrune { apply: false };
    let json = serde_json::to_string(&req).unwrap();
    let back: Request = serde_json::from_str(&json).unwrap();
    assert_eq!(req, back);
}

#[test]
fn analytics_top_response_round_trip() {
    let data = ResponseData::AnalyticsTop {
        entries: vec![TopEntry {
            uri: "spotify:track:1".into(),
            name: "Never Too Much".into(),
            subtitle: "Luther Vandross".into(),
            qualified_count: 42,
            skip_count: 1,
            total_audible_ms: 3_600_000,
            last_listened_at_ms: Some(1_700_000_000_000),
        }],
    };
    let json = serde_json::to_string(&data).unwrap();
    assert!(json.contains("\"kind\":\"analytics-top\""));
    let back: ResponseData = serde_json::from_str(&json).unwrap();
    match back {
        ResponseData::AnalyticsTop { entries } => {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].uri, "spotify:track:1");
            assert_eq!(entries[0].qualified_count, 42);
        }
        other => panic!("expected AnalyticsTop, got {other:?}"),
    }
}

#[test]
fn listen_qualified_event_round_trip() {
    let ev = DaemonEvent::ListenQualified {
        track_uri: "spotify:track:1".into(),
        duration_ms: 180_000,
        audible_ms: 95_000,
        artist_uri: Some("spotify:artist:1".into()),
        album_uri: None,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains("\"event\":\"listen-qualified\""));
    assert!(
        !json.contains("\"album_uri\""),
        "None must skip: got {json}"
    );
    let back: DaemonEvent = serde_json::from_str(&json).unwrap();
    match back {
        DaemonEvent::ListenQualified {
            track_uri,
            duration_ms,
            audible_ms,
            artist_uri,
            album_uri,
        } => {
            assert_eq!(track_uri, "spotify:track:1");
            assert_eq!(duration_ms, 180_000);
            assert_eq!(audible_ms, 95_000);
            assert_eq!(artist_uri.as_deref(), Some("spotify:artist:1"));
            assert!(album_uri.is_none());
        }
        other => panic!("expected ListenQualified, got {other:?}"),
    }
}
