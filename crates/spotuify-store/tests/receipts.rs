//! Phase 6.6 — receipts SQLite lifecycle.
//!
//! Adversarial coverage:
//! - Pending receipts persist immediately.
//! - Finalizing transitions Pending → Confirmed/Failed exactly once.
//! - Recovering pending receipts after a simulated daemon kill returns
//!   the right set; re-finalizing them is idempotent.
//! - Status filter on `list_receipts` returns the right rows.

use spotuify_protocol::{ApiErrorSummary, IpcErrorKind, Receipt, ReceiptId, ReceiptStatus};
use spotuify_store::Store;

async fn fresh() -> Store {
    Store::in_memory().await.expect("in_memory store")
}

fn pending(action: &str) -> Receipt {
    Receipt {
        receipt_id: ReceiptId::new_v7(),
        action: action.to_string(),
        status: ReceiptStatus::Pending,
        message: "queued".to_string(),
        started_at_ms: 1_700_000_000_000,
        finished_at_ms: None,
        error: None,
    }
}

#[tokio::test]
async fn insert_pending_receipt_persists_and_is_visible_via_get() {
    let store = fresh().await;
    let r = pending("playlist_add");
    store.insert_pending_receipt(&r, "{}").await.unwrap();

    let got = store.get_receipt(r.receipt_id).await.unwrap();
    assert_eq!(got.receipt_id, r.receipt_id);
    assert_eq!(got.status, ReceiptStatus::Pending);
    assert_eq!(got.action, "playlist_add");
    assert!(got.finished_at_ms.is_none());
}

#[tokio::test]
async fn finalize_receipt_with_confirmed_sets_finished_at_and_status() {
    let store = fresh().await;
    let r = pending("library_save");
    store.insert_pending_receipt(&r, "{}").await.unwrap();

    store
        .finalize_receipt(
            r.receipt_id,
            ReceiptStatus::Confirmed,
            "saved",
            1_700_000_000_500,
            None,
        )
        .await
        .unwrap();

    let got = store.get_receipt(r.receipt_id).await.unwrap();
    assert_eq!(got.status, ReceiptStatus::Confirmed);
    assert_eq!(got.finished_at_ms, Some(1_700_000_000_500));
    assert_eq!(got.message, "saved");
    assert!(got.error.is_none());
}

#[tokio::test]
async fn finalize_receipt_with_failed_records_typed_error() {
    let store = fresh().await;
    let r = pending("playlist_add");
    store.insert_pending_receipt(&r, "{}").await.unwrap();

    let err = ApiErrorSummary {
        kind: IpcErrorKind::RateLimited,
        message: "retry in 60s".to_string(),
        retry_after_secs: Some(60),
    };
    store
        .finalize_receipt(
            r.receipt_id,
            ReceiptStatus::Failed,
            "rate limited",
            1_700_000_000_900,
            Some(&err),
        )
        .await
        .unwrap();

    let got = store.get_receipt(r.receipt_id).await.unwrap();
    assert_eq!(got.status, ReceiptStatus::Failed);
    let got_err = got.error.expect("typed error preserved");
    assert_eq!(got_err.kind, IpcErrorKind::RateLimited);
    assert_eq!(got_err.retry_after_secs, Some(60));
}

#[tokio::test]
async fn finalize_receipt_twice_is_idempotent_with_first_winning() {
    // Real-world: daemon restart races with delayed Spotify response.
    // The store should not double-fire MutationFinalized events.
    let store = fresh().await;
    let r = pending("queue_add");
    store.insert_pending_receipt(&r, "{}").await.unwrap();

    store
        .finalize_receipt(
            r.receipt_id,
            ReceiptStatus::Confirmed,
            "queued",
            1_700_000_001_000,
            None,
        )
        .await
        .unwrap();

    // Second attempt with conflicting status; current contract is "first
    // wins" -- the second finalize is a silent no-op.
    let result = store
        .finalize_receipt(
            r.receipt_id,
            ReceiptStatus::Failed,
            "would-be conflict",
            1_700_000_002_000,
            None,
        )
        .await;
    assert!(
        result.is_ok(),
        "second finalize must not error; got {result:?}"
    );

    let got = store.get_receipt(r.receipt_id).await.unwrap();
    assert_eq!(got.status, ReceiptStatus::Confirmed, "first finalize wins");
    assert_eq!(got.finished_at_ms, Some(1_700_000_001_000));
}

#[tokio::test]
async fn recover_pending_receipts_after_daemon_kill_returns_them_all() {
    // Mid-mutation daemon kill: pending receipts written, never finalized.
    let store = fresh().await;
    for action in ["playlist_add", "library_save", "queue_add"] {
        store
            .insert_pending_receipt(&pending(action), "{}")
            .await
            .unwrap();
    }

    let pending_set = store.list_pending_receipts().await.unwrap();
    assert_eq!(pending_set.len(), 3);
    for r in pending_set {
        assert_eq!(r.status, ReceiptStatus::Pending);
        assert!(r.finished_at_ms.is_none());
    }
}

#[tokio::test]
async fn confirmed_and_failed_receipts_are_not_returned_by_list_pending() {
    let store = fresh().await;
    let p1 = pending("a");
    let p2 = pending("b");
    let p3 = pending("c");
    store.insert_pending_receipt(&p1, "{}").await.unwrap();
    store.insert_pending_receipt(&p2, "{}").await.unwrap();
    store.insert_pending_receipt(&p3, "{}").await.unwrap();

    store
        .finalize_receipt(p1.receipt_id, ReceiptStatus::Confirmed, "ok", 100, None)
        .await
        .unwrap();
    store
        .finalize_receipt(p2.receipt_id, ReceiptStatus::Failed, "no", 200, None)
        .await
        .unwrap();

    let pending_set = store.list_pending_receipts().await.unwrap();
    assert_eq!(pending_set.len(), 1);
    assert_eq!(pending_set[0].receipt_id, p3.receipt_id);
}

#[tokio::test]
async fn get_receipt_returns_anyhow_error_when_missing() {
    let store = fresh().await;
    let missing = ReceiptId::new_v7();
    let result = store.get_receipt(missing).await;
    assert!(
        result.is_err(),
        "missing receipt must error rather than return a default"
    );
}

#[tokio::test]
async fn insert_pending_receipt_preserves_request_payload_for_recovery() {
    // The store keeps the original Request JSON so recovery can re-issue
    // the mutation or render the diff to the user. Phase 12 ops_log
    // uses this field.
    let store = fresh().await;
    let r = pending("playlist_add");
    let request_json = r#"{"cmd":"playlist-add-items","playlist":"x","uris":["spotify:track:y"]}"#;
    store
        .insert_pending_receipt(&r, request_json)
        .await
        .unwrap();

    let raw = store.receipt_request_json(r.receipt_id).await.unwrap();
    assert_eq!(raw, request_json);
}
