#![allow(clippy::panic, clippy::unwrap_used)]

use spotuify_core::ProviderId;
use spotuify_protocol::{
    ApiErrorSummary, IpcErrorKind, MutationId, Operation, OperationId, OperationKind,
    OperationSource, OperationStatus, Receipt, ReceiptId, ReceiptStatus, Response, SyncTargetData,
};
use spotuify_store::{
    MutationClaim, PostWriteOperationGuard, ProviderReconciliation, Store, MUTATION_DEDUP_TTL_MS,
};

fn linked_rows(now: i64) -> (Receipt, Operation) {
    let receipt_id = ReceiptId::new_v7();
    (
        Receipt {
            receipt_id,
            action: "save".into(),
            status: ReceiptStatus::Pending,
            message: "queued".into(),
            started_at_ms: now,
            finished_at_ms: None,
            error: None,
        },
        Operation {
            operation_id: OperationId::new_v7(),
            kind: OperationKind::LibrarySave,
            occurred_at_ms: now,
            finished_at_ms: None,
            source: OperationSource::Cli,
            requester: None,
            subject_uris: vec!["spotify:track:1".into()],
            reversible: true,
            reversal_plan: None,
            pre_state: None,
            status: OperationStatus::Pending,
            receipt_id: Some(receipt_id),
            subject_op_id: None,
            undone_by_op_id: None,
            redone_by_op_id: None,
            error_message: None,
        },
    )
}

#[tokio::test]
async fn claim_is_atomic_and_replays_current_linked_receipt() {
    let store = Store::in_memory().await.unwrap();
    let id = MutationId::new_v7();
    let (receipt, operation) = linked_rows(100);
    assert!(matches!(
        store
            .claim_mutation(id, "same", "{}", &receipt, &operation, 100)
            .await
            .unwrap(),
        MutationClaim::Claimed
    ));

    let (unused_receipt, unused_operation) = linked_rows(101);
    let replay = store
        .claim_mutation(id, "same", "{}", &unused_receipt, &unused_operation, 101)
        .await
        .unwrap();
    match replay {
        MutationClaim::Existing {
            receipt: Some(found),
            response_json: None,
        } => {
            assert_eq!(found.receipt_id, receipt.receipt_id);
            assert_eq!(found.message, receipt.message);
        }
        other => panic!("expected linked receipt replay, got {other:?}"),
    }
    assert!(store
        .get_operation(unused_operation.operation_id)
        .await
        .is_err());
}

#[tokio::test]
async fn reused_key_with_different_request_is_rejected() {
    let store = Store::in_memory().await.unwrap();
    let id = MutationId::new_v7();
    let (receipt, operation) = linked_rows(100);
    store
        .claim_mutation(id, "first", "{}", &receipt, &operation, 100)
        .await
        .unwrap();
    let (receipt2, operation2) = linked_rows(101);
    assert!(matches!(
        store
            .claim_mutation(id, "different", "{}", &receipt2, &operation2, 101)
            .await
            .unwrap(),
        MutationClaim::FingerprintMismatch
    ));
}

#[tokio::test]
async fn restart_recovery_marks_processing_claim_non_retryable_and_keeps_binding() {
    let store = Store::in_memory().await.unwrap();
    let id = MutationId::new_v7();
    let (receipt, operation) = linked_rows(100);
    store
        .claim_mutation(id, "same", "{}", &receipt, &operation, 100)
        .await
        .unwrap();

    assert_eq!(store.recover_processing_mutations(200).await.unwrap(), 1);
    let found = store.get_receipt(receipt.receipt_id).await.unwrap();
    assert_eq!(found.status, ReceiptStatus::Failed);
    assert!(found.message.contains("outcome indeterminate"));
    assert_eq!(
        store
            .get_operation(operation.operation_id)
            .await
            .unwrap()
            .status,
        OperationStatus::Failed
    );

    let (receipt2, operation2) = linked_rows(201);
    match store
        .claim_mutation(id, "same", "{}", &receipt2, &operation2, 201)
        .await
        .unwrap()
    {
        MutationClaim::Existing {
            response_json: Some(raw),
            ..
        } => match serde_json::from_str::<Response>(&raw).unwrap() {
            Response::Error { retryable, .. } => assert!(!retryable),
            other => panic!("expected cached error, got {other:?}"),
        },
        other => panic!("expected cached replay, got {other:?}"),
    }
}

#[tokio::test]
async fn topology_recovery_can_inspect_claim_and_atomically_attach_reconciliation() {
    let store = Store::in_memory().await.unwrap();
    let id = MutationId::new_v7();
    let (receipt, operation) = linked_rows(100);
    let request_json = r#"{"type":"library_save","provider":"fake"}"#;
    store
        .claim_mutation(id, "same", request_json, &receipt, &operation, 100)
        .await
        .unwrap();

    let processing = store.processing_mutation_claims().await.unwrap();
    assert_eq!(processing.len(), 1);
    assert_eq!(processing[0].mutation_id, id);
    assert_eq!(processing[0].request_json, request_json);
    assert_eq!(processing[0].receipt_id, receipt.receipt_id);
    assert_eq!(processing[0].operation_id, operation.operation_id);

    let reconciliation = ProviderReconciliation::full_domain(
        receipt.receipt_id,
        operation.operation_id,
        ProviderId::new("fake").unwrap(),
        SyncTargetData::Library,
    );
    let response = store
        .mark_mutation_indeterminate(id, &[reconciliation], None, 200)
        .await
        .unwrap();
    assert!(matches!(
        response,
        Response::Error {
            kind: IpcErrorKind::Internal,
            retryable: false,
            ..
        }
    ));
    assert!(store.processing_mutation_claims().await.unwrap().is_empty());
    assert_eq!(
        store
            .pending_provider_reconciliations_for_receipt(receipt.receipt_id)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn retention_never_prunes_processing_claims() {
    let store = Store::in_memory().await.unwrap();
    let id = MutationId::new_v7();
    let (receipt, operation) = linked_rows(100);
    store
        .claim_mutation(id, "same", "{}", &receipt, &operation, 100)
        .await
        .unwrap();
    assert_eq!(
        store
            .prune_expired_mutations(100 + MUTATION_DEDUP_TTL_MS + 1)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn bulk_undo_recovery_preserves_exact_selected_originals() {
    let store = Store::in_memory().await.unwrap();
    let outer_undo_id = OperationId::new_v7();
    let (_, mut outer_undo) = linked_rows(130);
    outer_undo.operation_id = outer_undo_id;
    outer_undo.kind = OperationKind::Undo;
    outer_undo.reversible = false;
    outer_undo.receipt_id = None;
    store.insert_pending_operation(&outer_undo).await.unwrap();
    let (_, mut already_linked) = linked_rows(80);
    let (_, still_eligible) = linked_rows(120);
    let (_, too_old) = linked_rows(70);
    already_linked.receipt_id = None;
    let mut still_eligible = still_eligible;
    still_eligible.receipt_id = None;
    let mut too_old = too_old;
    too_old.receipt_id = None;
    for operation in [&already_linked, &still_eligible, &too_old] {
        store.insert_pending_operation(operation).await.unwrap();
        store
            .finalize_operation(
                operation.operation_id,
                OperationStatus::Succeeded,
                operation.occurred_at_ms + 1,
                None,
            )
            .await
            .unwrap();
    }
    store
        .record_bulk_undo_candidates(
            outer_undo_id,
            &[still_eligible.clone(), already_linked.clone()],
        )
        .await
        .unwrap();
    store
        .mark_operation_undone(already_linked.operation_id, outer_undo_id)
        .await
        .unwrap();

    let recovered = store
        .operations_for_bulk_undo_recovery(100, outer_undo_id)
        .await
        .unwrap();
    assert_eq!(
        recovered
            .iter()
            .map(|operation| operation.operation_id)
            .collect::<Vec<_>>(),
        vec![still_eligible.operation_id, already_linked.operation_id]
    );
}

#[tokio::test]
async fn claimed_success_finalizes_receipt_operation_and_dedup_atomically() {
    let store = Store::in_memory().await.unwrap();
    let id = MutationId::new_v7();
    let (receipt, operation) = linked_rows(100);
    store
        .claim_mutation(id, "same", "{}", &receipt, &operation, 100)
        .await
        .unwrap();
    let response = Response::Ok {
        data: spotuify_protocol::ResponseData::Ack {
            message: "done".into(),
        },
    };
    store
        .finalize_claimed_mutation(
            id,
            receipt.receipt_id,
            ReceiptStatus::Confirmed,
            "save confirmed",
            None,
            operation.operation_id,
            OperationStatus::Succeeded,
            None,
            &serde_json::to_string(&response).unwrap(),
            true,
            &[],
            None,
            None,
            200,
        )
        .await
        .unwrap();

    // A caller that lost the COMMIT acknowledgement may retry the exact
    // finalization intent. The already-terminal rows are success, not a
    // false ownership loss that could strand the request.
    store
        .finalize_claimed_mutation(
            id,
            receipt.receipt_id,
            ReceiptStatus::Confirmed,
            "save confirmed",
            None,
            operation.operation_id,
            OperationStatus::Succeeded,
            None,
            &serde_json::to_string(&response).unwrap(),
            true,
            &[],
            None,
            None,
            200,
        )
        .await
        .unwrap();
    let conflicting_response = Response::Ok {
        data: spotuify_protocol::ResponseData::Ack {
            message: "different".into(),
        },
    };
    assert!(store
        .finalize_claimed_mutation(
            id,
            receipt.receipt_id,
            ReceiptStatus::Confirmed,
            "save confirmed",
            None,
            operation.operation_id,
            OperationStatus::Succeeded,
            None,
            &serde_json::to_string(&conflicting_response).unwrap(),
            true,
            &[],
            None,
            None,
            200,
        )
        .await
        .is_err());

    assert_eq!(store.recover_processing_mutations(300).await.unwrap(), 0);
    let finalized_receipt = store.get_receipt(receipt.receipt_id).await.unwrap();
    assert_eq!(finalized_receipt.status, ReceiptStatus::Confirmed);
    assert_eq!(finalized_receipt.message, "save confirmed");
    assert_eq!(
        store
            .get_operation(operation.operation_id)
            .await
            .unwrap()
            .status,
        OperationStatus::Succeeded
    );
}

#[tokio::test]
async fn ambiguous_finalization_retry_preserves_guard_and_reconciliation_fanout() {
    let store = Store::in_memory().await.unwrap();
    let (_, mut original) = linked_rows(90);
    original.receipt_id = None;
    store.insert_pending_operation(&original).await.unwrap();
    store
        .finalize_operation(original.operation_id, OperationStatus::Succeeded, 95, None)
        .await
        .unwrap();

    let mutation_id = MutationId::new_v7();
    let (receipt, mut outer) = linked_rows(100);
    outer.kind = OperationKind::Undo;
    outer.reversible = false;
    store
        .claim_mutation(mutation_id, "undo", "{}", &receipt, &outer, 100)
        .await
        .unwrap();
    let provider = ProviderId::new("fake").unwrap();
    let reconciliations = vec![
        ProviderReconciliation::targeted(
            receipt.receipt_id,
            outer.operation_id,
            provider.clone(),
            SyncTargetData::Library,
            vec!["fake:track:one".to_string()],
        ),
        ProviderReconciliation::full_domain(
            receipt.receipt_id,
            outer.operation_id,
            provider,
            SyncTargetData::Playlists,
        ),
    ];
    let response = Response::error_with_retryable(
        "provider outcome indeterminate",
        IpcErrorKind::Internal,
        false,
    );
    let response_json = serde_json::to_string(&response).unwrap();
    for _ in 0..2 {
        store
            .finalize_claimed_mutation(
                mutation_id,
                receipt.receipt_id,
                ReceiptStatus::Failed,
                "provider outcome indeterminate",
                None,
                outer.operation_id,
                OperationStatus::Failed,
                Some("provider outcome indeterminate"),
                &response_json,
                false,
                &reconciliations,
                Some(PostWriteOperationGuard::DisableUndo(original.operation_id)),
                None,
                200,
            )
            .await
            .unwrap();
    }
    assert!(
        !store
            .get_operation(original.operation_id)
            .await
            .unwrap()
            .reversible
    );
    assert_eq!(
        store
            .pending_provider_reconciliations_for_receipt(receipt.receipt_id)
            .await
            .unwrap()
            .len(),
        2
    );
    let stored_response = store
        .terminal_mutation_response(mutation_id)
        .await
        .unwrap()
        .expect("terminal response");
    assert_eq!(
        serde_json::to_value(stored_response).unwrap(),
        serde_json::to_value(response).unwrap()
    );
}

#[tokio::test]
async fn auth_failure_after_claim_stays_terminal_and_cannot_repeat_partial_work() {
    let store = Store::in_memory().await.unwrap();
    let id = MutationId::new_v7();
    let (receipt, operation) = linked_rows(100);
    store
        .claim_mutation(id, "batch", "{}", &receipt, &operation, 100)
        .await
        .unwrap();
    let auth_error = ApiErrorSummary {
        kind: IpcErrorKind::AuthRevoked,
        message: "authorization failed after part of the batch may have applied".into(),
        retry_after_secs: None,
        provider: None,
        detail: None,
    };
    let response = Response::error_with_retryable(
        auth_error.message.clone(),
        IpcErrorKind::AuthRevoked,
        false,
    );
    store
        .finalize_claimed_mutation(
            id,
            receipt.receipt_id,
            ReceiptStatus::Failed,
            &auth_error.message,
            Some(&auth_error),
            operation.operation_id,
            OperationStatus::Failed,
            Some(&auth_error.message),
            &serde_json::to_string(&response).unwrap(),
            false,
            &[],
            None,
            None,
            200,
        )
        .await
        .unwrap();

    let (retry_receipt, retry_operation) = linked_rows(300);
    match store
        .claim_mutation(id, "batch", "{}", &retry_receipt, &retry_operation, 300)
        .await
        .unwrap()
    {
        MutationClaim::Existing {
            receipt: Some(found),
            response_json: Some(raw),
        } => {
            assert_eq!(found.status, ReceiptStatus::Failed);
            assert_eq!(found.error, Some(auth_error));
            assert!(matches!(
                serde_json::from_str::<Response>(&raw).unwrap(),
                Response::Error {
                    kind: IpcErrorKind::AuthRevoked,
                    retryable: false,
                    ..
                }
            ));
        }
        other => panic!("expected terminal auth replay, got {other:?}"),
    }
    assert!(store
        .get_operation(retry_operation.operation_id)
        .await
        .is_err());
}
