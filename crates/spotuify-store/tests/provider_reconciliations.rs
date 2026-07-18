#![allow(clippy::unwrap_used)]

use spotuify_core::ProviderId;
use spotuify_protocol::{
    ApiErrorSummary, IpcErrorKind, Operation, OperationId, OperationKind, OperationSource,
    OperationStatus, PreState, Receipt, ReceiptId, ReceiptStatus, ReversalPlan, SyncTargetData,
};
use spotuify_store::{
    PartialOperationRecovery, PostWriteOperationGuard, ProviderReconciliation,
    ProviderReconciliationCompletion, ProviderReconciliationScope, Store,
};

fn receipt() -> Receipt {
    Receipt {
        receipt_id: ReceiptId::new_v7(),
        action: "partial-save".to_string(),
        status: ReceiptStatus::Pending,
        message: "queued".to_string(),
        started_at_ms: 10,
        finished_at_ms: None,
        error: None,
    }
}

fn operation(receipt_id: ReceiptId, kind: OperationKind) -> Operation {
    Operation {
        operation_id: OperationId::new_v7(),
        kind,
        occurred_at_ms: 10,
        finished_at_ms: None,
        source: OperationSource::DaemonInternal,
        requester: None,
        subject_uris: vec![],
        reversible: kind.is_reversible(),
        reversal_plan: None,
        pre_state: None,
        status: OperationStatus::Pending,
        receipt_id: Some(receipt_id),
        subject_op_id: None,
        undone_by_op_id: None,
        redone_by_op_id: None,
        error_message: None,
    }
}

fn partial_error() -> ApiErrorSummary {
    ApiErrorSummary {
        kind: IpcErrorKind::Provider,
        message: "provider partially applied library_save".to_string(),
        retry_after_secs: None,
        provider: Some(ProviderId::new("fake").unwrap()),
        detail: Some("{\"schema\":\"spotuify.provider-partial.v1\"}".to_string()),
    }
}

#[tokio::test]
async fn partial_finalization_and_reconciliation_claim_are_atomic_and_recoverable() {
    let store = Store::in_memory().await.unwrap();
    let receipt = receipt();
    let operation = operation(receipt.receipt_id, OperationKind::PlaylistCreate);
    store.insert_pending_receipt(&receipt, "{}").await.unwrap();
    store.insert_pending_operation(&operation).await.unwrap();
    let playlist_uri = "fake:playlist:created".to_string();
    let reconciliation = ProviderReconciliation::pending(
        receipt.receipt_id,
        operation.operation_id,
        ProviderId::new("fake").unwrap(),
        SyncTargetData::Playlists,
        vec![playlist_uri.clone()],
    );
    let recovery = PartialOperationRecovery {
        pre_state: PreState::PlaylistCreate {
            playlist_id: playlist_uri.clone(),
        },
        reversal_plan: ReversalPlan::PlaylistDelete {
            playlist_id: playlist_uri.clone(),
        },
        subject_uris: vec![playlist_uri],
    };

    store
        .finalize_partial_operation(
            receipt.receipt_id,
            "partially applied",
            &partial_error(),
            operation.operation_id,
            OperationStatus::Succeeded,
            "partially applied",
            std::slice::from_ref(&reconciliation),
            None,
            Some(&recovery),
            20,
        )
        .await
        .unwrap();

    assert_eq!(
        store.get_receipt(receipt.receipt_id).await.unwrap().status,
        ReceiptStatus::Failed
    );
    let operation = store.get_operation(operation.operation_id).await.unwrap();
    assert_eq!(operation.status, OperationStatus::Succeeded);
    assert!(operation.reversible);
    assert!(matches!(
        operation.reversal_plan,
        Some(ReversalPlan::PlaylistDelete { .. })
    ));

    let first_token = uuid::Uuid::now_v7();
    let first = store
        .claim_provider_reconciliation_if_attempts(reconciliation.reconciliation_id, 0, first_token)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.attempts, 1);
    assert!(store
        .claim_provider_reconciliation_if_attempts(
            reconciliation.reconciliation_id,
            0,
            uuid::Uuid::now_v7(),
        )
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .recover_running_provider_reconciliations()
            .await
            .unwrap(),
        1
    );
    let second_token = uuid::Uuid::now_v7();
    let second = store
        .claim_provider_reconciliation_if_attempts(
            reconciliation.reconciliation_id,
            first.attempts,
            second_token,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.attempts, 2);
    let secret = "Abc123".repeat(12);
    store
        .fail_provider_reconciliation_if_attempts(
            reconciliation.reconciliation_id,
            second.attempts,
            second_token,
            &format!("offline bearer {secret}"),
        )
        .await
        .unwrap();
    assert!(store
        .provider_reconciliation_pending(receipt.receipt_id)
        .await
        .unwrap());
    let pending = store.pending_provider_reconciliations().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].last_error.as_deref(),
        Some("offline bearer <redacted>")
    );
    let third_token = uuid::Uuid::now_v7();
    let third = store
        .claim_provider_reconciliation_if_attempts(
            reconciliation.reconciliation_id,
            second.attempts,
            third_token,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store
            .record_provider_reconciliation_success(
                reconciliation.reconciliation_id,
                third.attempts,
                third_token,
                30,
            )
            .await
            .unwrap(),
        ProviderReconciliationCompletion::Completed
    );
    assert!(!store
        .provider_reconciliation_pending(receipt.receipt_id)
        .await
        .unwrap());
}

#[tokio::test]
async fn one_receipt_fans_out_by_target_and_full_domain_dominates_duplicate_intents() {
    let store = Store::in_memory().await.unwrap();
    let receipt = receipt();
    let operation = operation(receipt.receipt_id, OperationKind::LibrarySave);
    let mutation_id = spotuify_protocol::MutationId::new_v7();
    store
        .claim_mutation(mutation_id, "fanout", "{}", &receipt, &operation, 10)
        .await
        .unwrap();
    let provider = ProviderId::new("fake").unwrap();
    let targeted_library = ProviderReconciliation::targeted(
        receipt.receipt_id,
        operation.operation_id,
        provider.clone(),
        SyncTargetData::Library,
        vec!["fake:track:one".to_string()],
    );
    let full_library = ProviderReconciliation::full_domain(
        receipt.receipt_id,
        operation.operation_id,
        provider.clone(),
        SyncTargetData::Library,
    );
    let targeted_playlists = ProviderReconciliation::targeted(
        receipt.receipt_id,
        operation.operation_id,
        ProviderId::new("other").unwrap(),
        SyncTargetData::Playlists,
        vec!["fake:playlist:one".to_string()],
    );
    let response = spotuify_protocol::Response::error_with_retryable(
        "indeterminate",
        IpcErrorKind::Internal,
        false,
    );

    store
        .finalize_claimed_mutation(
            mutation_id,
            receipt.receipt_id,
            ReceiptStatus::Failed,
            "indeterminate",
            None,
            operation.operation_id,
            OperationStatus::Failed,
            Some("indeterminate"),
            &serde_json::to_string(&response).unwrap(),
            false,
            &[targeted_library, full_library, targeted_playlists.clone()],
            None,
            None,
            20,
        )
        .await
        .unwrap();

    let pending = store
        .pending_provider_reconciliations_for_receipt(receipt.receipt_id)
        .await
        .unwrap();
    assert_eq!(pending.len(), 2);
    let library = pending
        .iter()
        .find(|item| item.target == SyncTargetData::Library)
        .unwrap();
    assert_eq!(library.scope, ProviderReconciliationScope::FullDomain);
    assert!(library.resource_uris.is_empty());
    let playlists = pending
        .iter()
        .find(|item| item.target == SyncTargetData::Playlists)
        .unwrap();
    assert_eq!(playlists.provider.as_str(), "other");
    assert_eq!(playlists.scope, ProviderReconciliationScope::Targeted);
    assert_eq!(playlists.resource_uris, targeted_playlists.resource_uris);
}

#[tokio::test]
async fn expected_attempt_guard_rejects_a_stale_retry_timer() {
    let store = Store::in_memory().await.unwrap();
    let receipt = receipt();
    let operation = operation(receipt.receipt_id, OperationKind::LibrarySave);
    store.insert_pending_receipt(&receipt, "{}").await.unwrap();
    store.insert_pending_operation(&operation).await.unwrap();
    let reconciliation = ProviderReconciliation::pending(
        receipt.receipt_id,
        operation.operation_id,
        ProviderId::new("fake").unwrap(),
        SyncTargetData::Library,
        vec!["fake:track:one".to_string()],
    );
    store
        .finalize_partial_operation(
            receipt.receipt_id,
            "partially applied",
            &partial_error(),
            operation.operation_id,
            OperationStatus::Failed,
            "partially applied",
            std::slice::from_ref(&reconciliation),
            None,
            None,
            20,
        )
        .await
        .unwrap();

    let first_token = uuid::Uuid::now_v7();
    let claimed = store
        .claim_provider_reconciliation_if_attempts(reconciliation.reconciliation_id, 0, first_token)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.attempts, 1);
    let resumed = store
        .recover_provider_reconciliation_claim_after_error(
            reconciliation.reconciliation_id,
            0,
            first_token,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resumed.attempts, claimed.attempts);
    assert!(store
        .fail_provider_reconciliation_if_attempts(
            reconciliation.reconciliation_id,
            claimed.attempts,
            first_token,
            "offline",
        )
        .await
        .unwrap());
    assert!(store
        .fail_provider_reconciliation_if_attempts(
            reconciliation.reconciliation_id,
            claimed.attempts,
            first_token,
            "offline",
        )
        .await
        .unwrap());
    assert!(store
        .claim_provider_reconciliation_if_attempts(
            reconciliation.reconciliation_id,
            0,
            uuid::Uuid::now_v7(),
        )
        .await
        .unwrap()
        .is_none());
    let second_token = uuid::Uuid::now_v7();
    let second = store
        .claim_provider_reconciliation_if_attempts(
            reconciliation.reconciliation_id,
            1,
            second_token,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.attempts, 2);
    assert!(store
        .recover_provider_reconciliation_claim_after_error(
            reconciliation.reconciliation_id,
            0,
            first_token,
        )
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .recover_provider_reconciliation_claim_after_error(
                reconciliation.reconciliation_id,
                1,
                second_token,
            )
            .await
            .unwrap()
            .unwrap()
            .attempts,
        second.attempts
    );
    assert!(!store
        .fail_provider_reconciliation_if_attempts(
            reconciliation.reconciliation_id,
            1,
            first_token,
            "stale failure",
        )
        .await
        .unwrap());
    assert!(store
        .recover_provider_reconciliation_claim_after_error(
            reconciliation.reconciliation_id,
            1,
            uuid::Uuid::now_v7(),
        )
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        store
            .record_provider_reconciliation_success(
                reconciliation.reconciliation_id,
                second.attempts,
                second_token,
                40,
            )
            .await
            .unwrap(),
        ProviderReconciliationCompletion::Completed
    );
}

#[tokio::test]
async fn indeterminate_reconciliation_requires_two_durable_successful_passes() {
    let store = Store::in_memory().await.unwrap();
    let receipt = receipt();
    let operation = operation(receipt.receipt_id, OperationKind::LibrarySave);
    store.insert_pending_receipt(&receipt, "{}").await.unwrap();
    store.insert_pending_operation(&operation).await.unwrap();
    let mut reconciliation = ProviderReconciliation::targeted(
        receipt.receipt_id,
        operation.operation_id,
        ProviderId::new("fake").unwrap(),
        SyncTargetData::Library,
        vec!["fake:track:one".to_string()],
    );
    reconciliation.require_stability_pass();
    store
        .finalize_partial_operation(
            receipt.receipt_id,
            "outcome indeterminate",
            &partial_error(),
            operation.operation_id,
            OperationStatus::Failed,
            "outcome indeterminate",
            std::slice::from_ref(&reconciliation),
            None,
            None,
            20,
        )
        .await
        .unwrap();

    let first_token = uuid::Uuid::now_v7();
    let first = store
        .claim_provider_reconciliation_if_attempts(reconciliation.reconciliation_id, 0, first_token)
        .await
        .unwrap()
        .unwrap();
    let first_pass_at = spotuify_core::now_ms();
    assert_eq!(
        store
            .record_provider_reconciliation_success(
                first.reconciliation_id,
                first.attempts,
                first_token,
                first_pass_at,
            )
            .await
            .unwrap(),
        ProviderReconciliationCompletion::NeedsAnotherPass
    );
    assert_eq!(
        store
            .record_provider_reconciliation_success(
                first.reconciliation_id,
                first.attempts,
                first_token,
                first_pass_at,
            )
            .await
            .unwrap(),
        ProviderReconciliationCompletion::NeedsAnotherPass
    );
    assert!(store
        .claim_provider_reconciliation_if_attempts(
            reconciliation.reconciliation_id,
            first.attempts,
            uuid::Uuid::now_v7(),
        )
        .await
        .unwrap()
        .is_none());
    assert!(store
        .provider_reconciliation_not_before_ms(reconciliation.reconciliation_id, first.attempts,)
        .await
        .unwrap()
        .is_some_and(|not_before| not_before > first_pass_at));
    assert!(store
        .provider_reconciliation_not_before_ms(reconciliation.reconciliation_id, 0)
        .await
        .unwrap()
        .is_none());
    sqlx::query(
        "UPDATE provider_reconciliation_stability SET next_pass_after_ms = 0
         WHERE reconciliation_id = ?",
    )
    .bind(reconciliation.reconciliation_id.to_string())
    .execute(store.writer_for_test())
    .await
    .unwrap();
    let second_token = uuid::Uuid::now_v7();
    let second = store
        .claim_provider_reconciliation_if_attempts(
            reconciliation.reconciliation_id,
            first.attempts,
            second_token,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        store
            .record_provider_reconciliation_success(
                second.reconciliation_id,
                second.attempts,
                second_token,
                40,
            )
            .await
            .unwrap(),
        ProviderReconciliationCompletion::Completed
    );
    assert!(!store
        .provider_reconciliation_pending(receipt.receipt_id)
        .await
        .unwrap());
}

#[tokio::test]
async fn partial_undo_atomically_disables_the_original_operation() {
    let store = Store::in_memory().await.unwrap();
    let original_receipt = receipt();
    let original = operation(original_receipt.receipt_id, OperationKind::PlaylistAdd);
    store
        .insert_pending_receipt(&original_receipt, "{}")
        .await
        .unwrap();
    store.insert_pending_operation(&original).await.unwrap();
    store
        .finalize_receipt(
            original_receipt.receipt_id,
            ReceiptStatus::Confirmed,
            "confirmed",
            15,
            None,
        )
        .await
        .unwrap();
    store
        .finalize_operation(original.operation_id, OperationStatus::Succeeded, 15, None)
        .await
        .unwrap();

    let undo_receipt = receipt();
    let undo = operation(undo_receipt.receipt_id, OperationKind::Undo);
    store
        .insert_pending_receipt(&undo_receipt, "{}")
        .await
        .unwrap();
    store.insert_pending_operation(&undo).await.unwrap();
    let reconciliation = ProviderReconciliation::pending(
        undo_receipt.receipt_id,
        undo.operation_id,
        ProviderId::new("fake").unwrap(),
        SyncTargetData::Playlists,
        vec!["fake:playlist:one".to_string()],
    );
    store
        .finalize_partial_operation(
            undo_receipt.receipt_id,
            "partial undo",
            &partial_error(),
            undo.operation_id,
            OperationStatus::Failed,
            "partial undo",
            std::slice::from_ref(&reconciliation),
            Some(PostWriteOperationGuard::DisableUndo(original.operation_id)),
            None,
            20,
        )
        .await
        .unwrap();

    let original = store.get_operation(original.operation_id).await.unwrap();
    assert_eq!(original.status, OperationStatus::Succeeded);
    assert!(!original.reversible);
    assert!(matches!(
        original.reversal_plan,
        Some(ReversalPlan::NotReversible { .. })
    ));
}

#[tokio::test]
async fn post_write_redo_guard_consumes_original_inside_finalization() {
    let store = Store::in_memory().await.unwrap();
    let original_receipt = receipt();
    let original = operation(original_receipt.receipt_id, OperationKind::PlaylistAdd);
    store
        .insert_pending_receipt(&original_receipt, "{}")
        .await
        .unwrap();
    store.insert_pending_operation(&original).await.unwrap();
    store
        .finalize_receipt(
            original_receipt.receipt_id,
            ReceiptStatus::Confirmed,
            "confirmed",
            15,
            None,
        )
        .await
        .unwrap();
    store
        .finalize_operation(original.operation_id, OperationStatus::Succeeded, 15, None)
        .await
        .unwrap();

    let undo_receipt = receipt();
    let undo = operation(undo_receipt.receipt_id, OperationKind::Undo);
    store
        .insert_pending_receipt(&undo_receipt, "{}")
        .await
        .unwrap();
    store.insert_pending_operation(&undo).await.unwrap();
    store
        .mark_operation_undone(original.operation_id, undo.operation_id)
        .await
        .unwrap();

    let redo_receipt = receipt();
    let redo = operation(redo_receipt.receipt_id, OperationKind::Redo);
    let mutation_id = spotuify_protocol::MutationId::new_v7();
    store
        .claim_mutation(mutation_id, "redo", "{}", &redo_receipt, &redo, 20)
        .await
        .unwrap();
    let response = spotuify_protocol::Response::error_with_retryable(
        "post-write bookkeeping failed",
        IpcErrorKind::Internal,
        false,
    );
    store
        .finalize_claimed_mutation(
            mutation_id,
            redo_receipt.receipt_id,
            ReceiptStatus::Failed,
            "post-write bookkeeping failed",
            None,
            redo.operation_id,
            OperationStatus::Failed,
            Some("post-write bookkeeping failed"),
            &serde_json::to_string(&response).unwrap(),
            false,
            &[],
            Some(PostWriteOperationGuard::MarkRedone(original.operation_id)),
            None,
            30,
        )
        .await
        .unwrap();

    let original = store.get_operation(original.operation_id).await.unwrap();
    assert_eq!(original.status, OperationStatus::Redone);
    assert_eq!(original.redone_by_op_id, Some(redo.operation_id));
}

#[tokio::test]
async fn retained_remote_artifact_keeps_cleanup_without_reconciliation() {
    let store = Store::in_memory().await.unwrap();
    let receipt = receipt();
    let operation = operation(receipt.receipt_id, OperationKind::PlaylistCreate);
    store.insert_pending_receipt(&receipt, "{}").await.unwrap();
    store.insert_pending_operation(&operation).await.unwrap();
    let playlist_uri = "fake:playlist:possibly-retained".to_string();
    let recovery = PartialOperationRecovery {
        pre_state: PreState::PlaylistCreate {
            playlist_id: playlist_uri.clone(),
        },
        reversal_plan: ReversalPlan::PlaylistDelete {
            playlist_id: playlist_uri.clone(),
        },
        subject_uris: vec![playlist_uri],
    };

    store
        .finalize_partial_operation(
            receipt.receipt_id,
            "rollback outcome unknown",
            &partial_error(),
            operation.operation_id,
            OperationStatus::Succeeded,
            "rollback outcome unknown",
            &[],
            None,
            Some(&recovery),
            20,
        )
        .await
        .unwrap();

    let operation = store.get_operation(operation.operation_id).await.unwrap();
    assert_eq!(operation.status, OperationStatus::Succeeded);
    assert!(operation.reversible);
    assert!(matches!(
        operation.reversal_plan,
        Some(ReversalPlan::PlaylistDelete { .. })
    ));
    assert!(store
        .pending_provider_reconciliations()
        .await
        .unwrap()
        .is_empty());
}
