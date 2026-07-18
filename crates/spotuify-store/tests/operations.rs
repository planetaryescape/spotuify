#![allow(clippy::panic, clippy::unwrap_used)]

//! Phase 12 — operations log CRUD tests.
//!
//! Adversarial coverage:
//! - `insert_pending_operation` round-trips every field through SQLite,
//!   including JSON-serialised `ReversalPlan` / `PreState`.
//! - `finalize_operation` is idempotent: a second call on the same id
//!   is a silent no-op (mirrors `finalize_receipt`).
//! - `mark_operation_undone` flips status + writes `undone_by_op_id`.
//! - `list_operations` orders by `occurred_at_ms DESC`.
//! - `list_operations(source=mcp)` filters correctly.
//! - `find_last_reversible_operation` skips failed/undone/non-reversible rows.
//! - `prune_operations_older_than` deletes only rows past the cutoff.

use spotuify_core::now_ms;
use spotuify_protocol::{
    Operation, OperationId, OperationKind, OperationSource, OperationStatus, PreState, Receipt,
    ReceiptId, ReceiptStatus, ReversalPlan,
};
use spotuify_store::Store;

async fn store() -> Store {
    Store::in_memory().await.expect("in-memory store")
}

fn op(
    id: OperationId,
    kind: OperationKind,
    occurred_at_ms: i64,
    source: OperationSource,
    status: OperationStatus,
    reversible: bool,
) -> Operation {
    Operation {
        operation_id: id,
        kind,
        occurred_at_ms,
        finished_at_ms: None,
        source,
        requester: None,
        subject_uris: vec!["spotify:track:1".into()],
        reversible,
        reversal_plan: reversible.then(|| ReversalPlan::QueueRemove {
            uri: "spotify:track:1".into(),
        }),
        pre_state: reversible.then(|| PreState::QueueAdd {
            uri: "spotify:track:1".into(),
        }),
        status,
        receipt_id: None,
        subject_op_id: None,
        undone_by_op_id: None,
        redone_by_op_id: None,
        error_message: None,
    }
}

#[tokio::test]
async fn insert_pending_operation_round_trips() {
    let s = store().await;
    let id = OperationId::new_v7();
    let original = op(
        id,
        OperationKind::PlaylistAdd,
        now_ms(),
        OperationSource::Cli,
        OperationStatus::Pending,
        true,
    );
    s.insert_pending_operation(&original).await.unwrap();
    let back = s.get_operation(id).await.unwrap();
    assert_eq!(back.operation_id, id);
    assert_eq!(back.kind, OperationKind::PlaylistAdd);
    assert_eq!(back.status, OperationStatus::Pending);
    assert_eq!(back.source, OperationSource::Cli);
    assert!(back.reversible);
    assert_eq!(back.subject_uris, vec!["spotify:track:1"]);
    assert!(back.reversal_plan.is_some());
}

#[tokio::test]
async fn bulk_undo_recovery_uses_the_exact_persisted_candidate_snapshot() {
    let s = store().await;
    let first = op(
        OperationId::new_v7(),
        OperationKind::PlaylistAdd,
        10,
        OperationSource::Cli,
        OperationStatus::Succeeded,
        true,
    );
    let second = op(
        OperationId::new_v7(),
        OperationKind::LibrarySave,
        20,
        OperationSource::Cli,
        OperationStatus::Succeeded,
        true,
    );
    let later = op(
        OperationId::new_v7(),
        OperationKind::PlaylistRemove,
        30,
        OperationSource::Cli,
        OperationStatus::Succeeded,
        true,
    );
    let outer = op(
        OperationId::new_v7(),
        OperationKind::Undo,
        25,
        OperationSource::Cli,
        OperationStatus::Pending,
        false,
    );
    for operation in [&first, &second, &outer] {
        s.insert_pending_operation(operation).await.unwrap();
    }
    s.record_bulk_undo_candidates(outer.operation_id, &[second.clone(), first.clone()])
        .await
        .unwrap();
    s.insert_pending_operation(&later).await.unwrap();

    let recovered = s
        .operations_for_bulk_undo_recovery(0, outer.operation_id)
        .await
        .unwrap();
    assert_eq!(
        recovered
            .iter()
            .map(|operation| operation.operation_id)
            .collect::<Vec<_>>(),
        vec![second.operation_id, first.operation_id]
    );
}

#[tokio::test]
async fn record_bulk_undo_candidates_is_convergent_under_a_shifted_replay() {
    // A replay with a reordered candidate list used to trip the
    // UNIQUE(outer, position) constraint (the DO NOTHING only covered the PK)
    // and abort mid-transaction. The write must instead converge on the latest
    // list and order.
    let s = store().await;
    let a = op(
        OperationId::new_v7(),
        OperationKind::PlaylistAdd,
        10,
        OperationSource::Cli,
        OperationStatus::Succeeded,
        true,
    );
    let b = op(
        OperationId::new_v7(),
        OperationKind::LibrarySave,
        20,
        OperationSource::Cli,
        OperationStatus::Succeeded,
        true,
    );
    let c = op(
        OperationId::new_v7(),
        OperationKind::PlaylistRemove,
        30,
        OperationSource::Cli,
        OperationStatus::Succeeded,
        true,
    );
    let outer = op(
        OperationId::new_v7(),
        OperationKind::Undo,
        40,
        OperationSource::Cli,
        OperationStatus::Pending,
        false,
    );
    for operation in [&a, &b, &c, &outer] {
        s.insert_pending_operation(operation).await.unwrap();
    }

    s.record_bulk_undo_candidates(outer.operation_id, &[a.clone(), b.clone(), c.clone()])
        .await
        .unwrap();
    // Same members, shifted positions — this is the case that used to error.
    s.record_bulk_undo_candidates(outer.operation_id, &[c.clone(), a.clone(), b.clone()])
        .await
        .expect("shifted replay must converge, not error");

    let recovered = s
        .operations_for_bulk_undo_recovery(0, outer.operation_id)
        .await
        .unwrap();
    assert_eq!(
        recovered
            .iter()
            .map(|operation| operation.operation_id)
            .collect::<Vec<_>>(),
        vec![c.operation_id, a.operation_id, b.operation_id]
    );
}

#[tokio::test]
async fn insert_pending_operation_round_trips_all_operation_kind_labels() {
    let s = store().await;
    let kinds = [
        OperationKind::PlaylistUnfollow,
        OperationKind::PlaylistSetImage,
    ];
    for kind in kinds {
        let id = OperationId::new_v7();
        s.insert_pending_operation(&op(
            id,
            kind,
            now_ms(),
            OperationSource::Cli,
            OperationStatus::Pending,
            false,
        ))
        .await
        .unwrap();
        let back = s.get_operation(id).await.unwrap();
        assert_eq!(back.kind, kind);
    }
}

#[tokio::test]
async fn finalize_operation_is_idempotent() {
    let s = store().await;
    let id = OperationId::new_v7();
    let row = op(
        id,
        OperationKind::QueueAdd,
        now_ms(),
        OperationSource::Tui,
        OperationStatus::Pending,
        true,
    );
    s.insert_pending_operation(&row).await.unwrap();
    s.finalize_operation(id, OperationStatus::Succeeded, now_ms(), None)
        .await
        .unwrap();
    // Second call must NOT flip status away from Succeeded.
    s.finalize_operation(id, OperationStatus::Failed, now_ms(), Some("oops"))
        .await
        .unwrap();
    let back = s.get_operation(id).await.unwrap();
    assert_eq!(
        back.status,
        OperationStatus::Succeeded,
        "second finalize must be a silent no-op"
    );
    assert!(back.error_message.is_none());
}

#[tokio::test]
async fn reversal_plan_activation_is_atomic_and_only_allowed_while_pending() {
    let s = store().await;
    let id = OperationId::new_v7();
    let pre_state = PreState::PlaylistRemove {
        playlist_id: "spotify:playlist:focus".to_string(),
        version_token: Some("v1".to_string()),
        removed_items: vec![("spotify:track:one".to_string(), 2)],
    };
    let mut row = op(
        id,
        OperationKind::PlaylistRemove,
        now_ms(),
        OperationSource::Cli,
        OperationStatus::Pending,
        false,
    );
    row.pre_state = Some(pre_state.clone());
    row.reversal_plan = Some(ReversalPlan::NotReversible {
        reason: "awaiting post-mutation version".to_string(),
    });
    s.insert_pending_operation(&row).await.unwrap();

    let plan = ReversalPlan::PlaylistAddAtPositions {
        playlist_id: "spotify:playlist:focus".to_string(),
        items: vec![("spotify:track:one".to_string(), 2)],
        version_token: Some("v2".to_string()),
    };
    s.activate_operation_reversal_plan(id, &pre_state, &plan)
        .await
        .unwrap();
    let active = s.get_operation(id).await.unwrap();
    assert!(active.reversible);
    assert_eq!(active.pre_state, Some(pre_state.clone()));
    assert_eq!(active.reversal_plan, Some(plan.clone()));

    s.finalize_operation(id, OperationStatus::Succeeded, now_ms(), None)
        .await
        .unwrap();
    assert!(s
        .activate_operation_reversal_plan(id, &pre_state, &plan)
        .await
        .is_err());
}

#[tokio::test]
async fn mark_operation_undone_flips_status() {
    let s = store().await;
    let id = OperationId::new_v7();
    let row = op(
        id,
        OperationKind::PlaylistAdd,
        now_ms(),
        OperationSource::Cli,
        OperationStatus::Pending,
        true,
    );
    s.insert_pending_operation(&row).await.unwrap();
    s.finalize_operation(id, OperationStatus::Succeeded, now_ms(), None)
        .await
        .unwrap();

    // The undo op itself must exist before marking the original undone
    // (FK constraint on undone_by_op_id). Real callers insert the undo
    // row immediately before flipping the original's status.
    let undo_id = OperationId::new_v7();
    let mut undo_row = op(
        undo_id,
        OperationKind::Undo,
        now_ms(),
        OperationSource::Cli,
        OperationStatus::Succeeded,
        true,
    );
    undo_row.subject_op_id = Some(id);
    s.insert_pending_operation(&undo_row).await.unwrap();
    s.mark_operation_undone(id, undo_id).await.unwrap();
    let back = s.get_operation(id).await.unwrap();
    assert_eq!(back.status, OperationStatus::Undone);
    assert_eq!(back.undone_by_op_id, Some(undo_id));
}

#[tokio::test]
async fn list_operations_orders_descending_by_time() {
    let s = store().await;
    let now = now_ms();
    let ids: Vec<_> = (0..3).map(|_| OperationId::new_v7()).collect();
    for (i, id) in ids.iter().enumerate() {
        let row = op(
            *id,
            OperationKind::QueueAdd,
            now - (i as i64) * 1_000,
            OperationSource::Cli,
            OperationStatus::Succeeded,
            true,
        );
        s.insert_pending_operation(&row).await.unwrap();
        s.finalize_operation(*id, OperationStatus::Succeeded, now, None)
            .await
            .unwrap();
    }
    let rows = s.list_operations(10, None, None).await.unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows[0].occurred_at_ms >= rows[1].occurred_at_ms);
    assert!(rows[1].occurred_at_ms >= rows[2].occurred_at_ms);
}

#[tokio::test]
async fn list_operations_filters_by_source() {
    let s = store().await;
    let cli_id = OperationId::new_v7();
    let mcp_id = OperationId::new_v7();
    s.insert_pending_operation(&op(
        cli_id,
        OperationKind::QueueAdd,
        now_ms(),
        OperationSource::Cli,
        OperationStatus::Pending,
        true,
    ))
    .await
    .unwrap();
    s.insert_pending_operation(&op(
        mcp_id,
        OperationKind::QueueAdd,
        now_ms(),
        OperationSource::Mcp,
        OperationStatus::Pending,
        true,
    ))
    .await
    .unwrap();

    let only_mcp = s
        .list_operations(10, None, Some(OperationSource::Mcp))
        .await
        .unwrap();
    assert_eq!(only_mcp.len(), 1);
    assert_eq!(only_mcp[0].operation_id, mcp_id);
}

#[tokio::test]
async fn list_operations_filters_by_since() {
    let s = store().await;
    let old_id = OperationId::new_v7();
    let new_id = OperationId::new_v7();
    let now = now_ms();
    s.insert_pending_operation(&op(
        old_id,
        OperationKind::QueueAdd,
        now - 10_000,
        OperationSource::Cli,
        OperationStatus::Pending,
        true,
    ))
    .await
    .unwrap();
    s.insert_pending_operation(&op(
        new_id,
        OperationKind::QueueAdd,
        now,
        OperationSource::Cli,
        OperationStatus::Pending,
        true,
    ))
    .await
    .unwrap();

    let recent = s
        .list_operations(10, Some(now - 5_000), None)
        .await
        .unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].operation_id, new_id);
}

#[tokio::test]
async fn find_last_reversible_operation_skips_unsuccessful_rows() {
    let s = store().await;
    let now = now_ms();

    // Successful but NOT reversible (transport):
    let transport_id = OperationId::new_v7();
    let mut transport = op(
        transport_id,
        OperationKind::Pause,
        now - 3_000,
        OperationSource::Cli,
        OperationStatus::Pending,
        false,
    );
    transport.reversal_plan = None;
    transport.pre_state = Some(PreState::Transport);
    s.insert_pending_operation(&transport).await.unwrap();
    s.finalize_operation(transport_id, OperationStatus::Succeeded, now, None)
        .await
        .unwrap();

    // Reversible, succeeded — should be the answer.
    let target_id = OperationId::new_v7();
    s.insert_pending_operation(&op(
        target_id,
        OperationKind::PlaylistAdd,
        now - 2_000,
        OperationSource::Cli,
        OperationStatus::Pending,
        true,
    ))
    .await
    .unwrap();
    s.finalize_operation(target_id, OperationStatus::Succeeded, now, None)
        .await
        .unwrap();

    // Reversible but failed — should be skipped.
    let failed_id = OperationId::new_v7();
    s.insert_pending_operation(&op(
        failed_id,
        OperationKind::PlaylistAdd,
        now - 1_000,
        OperationSource::Cli,
        OperationStatus::Pending,
        true,
    ))
    .await
    .unwrap();
    s.finalize_operation(failed_id, OperationStatus::Failed, now, Some("nope"))
        .await
        .unwrap();

    let last = s.find_last_reversible_operation().await.unwrap();
    assert_eq!(
        last.map(|o| o.operation_id),
        Some(target_id),
        "must pick the most-recent reversible succeeded op"
    );
}

#[tokio::test]
async fn find_reversible_operations_since_excludes_failed_and_pending() {
    let s = store().await;
    let now = now_ms();

    // Recent + succeeded + reversible — should be included.
    let good = OperationId::new_v7();
    s.insert_pending_operation(&op(
        good,
        OperationKind::PlaylistAdd,
        now - 1_000,
        OperationSource::Cli,
        OperationStatus::Pending,
        true,
    ))
    .await
    .unwrap();
    s.finalize_operation(good, OperationStatus::Succeeded, now, None)
        .await
        .unwrap();

    // Recent + reversible + still pending — NOT included.
    let pending = OperationId::new_v7();
    s.insert_pending_operation(&op(
        pending,
        OperationKind::PlaylistAdd,
        now - 1_500,
        OperationSource::Cli,
        OperationStatus::Pending,
        true,
    ))
    .await
    .unwrap();

    // Recent + reversible + failed — NOT included.
    let failed = OperationId::new_v7();
    s.insert_pending_operation(&op(
        failed,
        OperationKind::PlaylistAdd,
        now - 2_000,
        OperationSource::Cli,
        OperationStatus::Pending,
        true,
    ))
    .await
    .unwrap();
    s.finalize_operation(failed, OperationStatus::Failed, now, Some("nope"))
        .await
        .unwrap();

    // Old + succeeded + reversible — NOT included (before cutoff).
    let old = OperationId::new_v7();
    s.insert_pending_operation(&op(
        old,
        OperationKind::PlaylistAdd,
        now - 10_000,
        OperationSource::Cli,
        OperationStatus::Pending,
        true,
    ))
    .await
    .unwrap();
    s.finalize_operation(old, OperationStatus::Succeeded, now, None)
        .await
        .unwrap();

    let bulk = s
        .find_reversible_operations_since(now - 5_000, None)
        .await
        .unwrap();
    let ids: Vec<_> = bulk.iter().map(|o| o.operation_id).collect();
    assert_eq!(
        ids,
        vec![good],
        "only the recent succeeded reversible op qualifies"
    );
}

#[tokio::test]
async fn find_reversible_operations_since_filters_by_source() {
    let s = store().await;
    let now = now_ms();
    let cli_id = OperationId::new_v7();
    let mcp_id = OperationId::new_v7();
    for (id, source) in [
        (cli_id, OperationSource::Cli),
        (mcp_id, OperationSource::Mcp),
    ] {
        s.insert_pending_operation(&op(
            id,
            OperationKind::PlaylistAdd,
            now - 1_000,
            source,
            OperationStatus::Pending,
            true,
        ))
        .await
        .unwrap();
        s.finalize_operation(id, OperationStatus::Succeeded, now, None)
            .await
            .unwrap();
    }
    let only_mcp = s
        .find_reversible_operations_since(now - 5_000, Some(OperationSource::Mcp))
        .await
        .unwrap();
    assert_eq!(only_mcp.len(), 1);
    assert_eq!(only_mcp[0].operation_id, mcp_id);
}

#[tokio::test]
async fn mark_operation_undone_is_silent_noop_when_already_undone() {
    let s = store().await;
    let original_id = OperationId::new_v7();
    s.insert_pending_operation(&op(
        original_id,
        OperationKind::PlaylistAdd,
        now_ms(),
        OperationSource::Cli,
        OperationStatus::Pending,
        true,
    ))
    .await
    .unwrap();
    s.finalize_operation(original_id, OperationStatus::Succeeded, now_ms(), None)
        .await
        .unwrap();

    // First undo op + apply.
    let undo1 = OperationId::new_v7();
    let mut u1 = op(
        undo1,
        OperationKind::Undo,
        now_ms(),
        OperationSource::Cli,
        OperationStatus::Succeeded,
        true,
    );
    u1.subject_op_id = Some(original_id);
    s.insert_pending_operation(&u1).await.unwrap();
    s.mark_operation_undone(original_id, undo1).await.unwrap();

    // Second undo attempt — must not overwrite undone_by_op_id.
    let undo2 = OperationId::new_v7();
    let mut u2 = op(
        undo2,
        OperationKind::Undo,
        now_ms(),
        OperationSource::Cli,
        OperationStatus::Succeeded,
        true,
    );
    u2.subject_op_id = Some(original_id);
    s.insert_pending_operation(&u2).await.unwrap();
    s.mark_operation_undone(original_id, undo2).await.unwrap();

    let back = s.get_operation(original_id).await.unwrap();
    assert_eq!(back.status, OperationStatus::Undone);
    assert_eq!(
        back.undone_by_op_id,
        Some(undo1),
        "first undo wins; second mark_operation_undone must be a silent no-op"
    );
}

#[tokio::test]
async fn mark_operation_redone_only_flips_undone_rows() {
    let s = store().await;
    let original = OperationId::new_v7();
    s.insert_pending_operation(&op(
        original,
        OperationKind::PlaylistAdd,
        now_ms(),
        OperationSource::Cli,
        OperationStatus::Pending,
        true,
    ))
    .await
    .unwrap();
    s.finalize_operation(original, OperationStatus::Succeeded, now_ms(), None)
        .await
        .unwrap();

    // Try redoing a succeeded (NOT undone) op — must be a silent no-op.
    let redo_id = OperationId::new_v7();
    let mut redo = op(
        redo_id,
        OperationKind::Redo,
        now_ms(),
        OperationSource::Cli,
        OperationStatus::Succeeded,
        false,
    );
    redo.subject_op_id = Some(original);
    s.insert_pending_operation(&redo).await.unwrap();
    s.mark_operation_redone(original, redo_id).await.unwrap();

    let back = s.get_operation(original).await.unwrap();
    assert_eq!(
        back.status,
        OperationStatus::Succeeded,
        "redo of a non-undone op must not transition status"
    );
    assert!(back.redone_by_op_id.is_none());
}

#[tokio::test]
async fn prune_operations_older_than_deletes_only_old_rows() {
    let s = store().await;
    let now = now_ms();
    let old_id = OperationId::new_v7();
    let new_id = OperationId::new_v7();
    s.insert_pending_operation(&op(
        old_id,
        OperationKind::QueueAdd,
        now - 100_000,
        OperationSource::Cli,
        OperationStatus::Pending,
        true,
    ))
    .await
    .unwrap();
    s.insert_pending_operation(&op(
        new_id,
        OperationKind::QueueAdd,
        now,
        OperationSource::Cli,
        OperationStatus::Pending,
        true,
    ))
    .await
    .unwrap();

    let pruned = s.prune_operations_older_than(now - 50_000).await.unwrap();
    assert_eq!(pruned, 1);
    let remaining = s.list_operations(10, None, None).await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].operation_id, new_id);
}

// Regression: every operation row written by `record_operation` carries a
// `receipt_id` pointing at a receipt that hasn't been inserted yet, so
// with PRAGMA foreign_keys=ON the insert fails the FK constraint. The
// daemon's `let _ = ...` swallowed the error and the operations table
// stayed empty in production. Lock the contract so any future caller
// that touches insert ordering gets a real error instead of silence.
#[tokio::test]
async fn insert_pending_operation_rejects_unknown_receipt_id() {
    let s = store().await;
    let id = OperationId::new_v7();
    let mut row = op(
        id,
        OperationKind::PlaylistCreate,
        now_ms(),
        OperationSource::Cli,
        OperationStatus::Pending,
        true,
    );
    // Receipt id minted but the matching row was never inserted.
    row.receipt_id = Some(ReceiptId::new_v7());
    let result = s.insert_pending_operation(&row).await;
    assert!(
        result.is_err(),
        "insert must surface the FK violation rather than silently succeed"
    );
}

#[tokio::test]
async fn insert_pending_operation_succeeds_when_receipt_exists_first() {
    let s = store().await;
    let receipt_id = ReceiptId::new_v7();
    let receipt = Receipt {
        receipt_id,
        action: "playlist-create".to_string(),
        status: ReceiptStatus::Pending,
        message: "queued".to_string(),
        started_at_ms: now_ms(),
        finished_at_ms: None,
        error: None,
    };
    s.insert_pending_receipt(&receipt, "{}").await.unwrap();

    let id = OperationId::new_v7();
    let mut row = op(
        id,
        OperationKind::PlaylistCreate,
        now_ms(),
        OperationSource::Cli,
        OperationStatus::Pending,
        true,
    );
    row.receipt_id = Some(receipt_id);
    s.insert_pending_operation(&row)
        .await
        .expect("operation with valid receipt FK must persist");

    let back = s.get_operation(id).await.unwrap();
    assert_eq!(back.receipt_id, Some(receipt_id));
}
