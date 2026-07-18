//! Durable idempotency claims for externally-visible Spotify writes.

use anyhow::Result;
use sqlx::Row;

use spotuify_protocol::{
    ApiErrorSummary, IpcErrorKind, MutationId, Operation, OperationId, OperationStatus, Receipt,
    Response,
};

use crate::provider_reconciliations::{
    apply_post_write_operation_guard_tx, insert_provider_reconciliation_tx,
    retain_partial_operation_recovery_tx,
};
use crate::{
    row_to_receipt, PartialOperationRecovery, PostWriteOperationGuard, ProviderReconciliation,
    Store,
};

pub const MUTATION_DEDUP_TTL_MS: i64 = 24 * 60 * 60 * 1_000;
const INDETERMINATE_MESSAGE: &str =
    "remote outcome indeterminate; inspect state before retrying with a new mutation id";

#[derive(Debug)]
pub enum MutationClaim {
    Claimed,
    Existing {
        receipt: Option<Box<Receipt>>,
        response_json: Option<String>,
    },
    FingerprintMismatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessingMutationClaim {
    pub mutation_id: MutationId,
    pub request_json: String,
    pub receipt_id: spotuify_protocol::ReceiptId,
    pub operation_id: OperationId,
}

impl Store {
    pub async fn terminal_mutation_response(
        &self,
        mutation_id: MutationId,
    ) -> Result<Option<Response>> {
        let raw = sqlx::query_scalar::<_, Option<String>>(
            "SELECT response_json FROM mutation_dedup
             WHERE mutation_id = ? AND state != 'processing'",
        )
        .bind(mutation_id.to_string())
        .fetch_optional(&self.reader)
        .await?
        .flatten();
        raw.map(|raw| serde_json::from_str(&raw).map_err(Into::into))
            .transpose()
    }

    /// Read an existing durable mutation claim without creating a new one.
    ///
    /// This lets handlers replay selector-style mutations (for example,
    /// "undo latest") before re-evaluating a selector whose result may have
    /// changed after the first successful request.
    pub async fn lookup_mutation_claim(
        &self,
        mutation_id: MutationId,
        fingerprint: &str,
    ) -> Result<Option<MutationClaim>> {
        let Some(row) = sqlx::query(
            "SELECT fingerprint, response_json, receipt_id
             FROM mutation_dedup WHERE mutation_id = ?",
        )
        .bind(mutation_id.to_string())
        .fetch_optional(&self.reader)
        .await?
        else {
            return Ok(None);
        };
        let existing_fingerprint: String = row.try_get("fingerprint")?;
        if existing_fingerprint != fingerprint {
            return Ok(Some(MutationClaim::FingerprintMismatch));
        }
        let response_json: Option<String> = row.try_get("response_json")?;
        let receipt_id: Option<String> = row.try_get("receipt_id")?;
        let receipt = if let Some(receipt_id) = receipt_id {
            let receipt_row = sqlx::query(
                "SELECT receipt_id, action, status, message, started_at_ms, finished_at_ms, error_json
                 FROM receipts WHERE receipt_id = ?",
            )
            .bind(receipt_id)
            .fetch_optional(&self.reader)
            .await?;
            receipt_row.as_ref().map(row_to_receipt).transpose()?
        } else {
            None
        };
        Ok(Some(MutationClaim::Existing {
            receipt: receipt.map(Box::new),
            response_json,
        }))
    }

    /// Atomically bind a mutation key to its fingerprint and create the linked
    /// pending receipt + operation. The loser of a concurrent claim observes
    /// the winner's current receipt and never executes the body.
    pub async fn claim_mutation(
        &self,
        mutation_id: MutationId,
        fingerprint: &str,
        request_json: &str,
        receipt: &Receipt,
        operation: &Operation,
        now_ms: i64,
    ) -> Result<MutationClaim> {
        let mut tx = self.writer.begin().await?;
        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO mutation_dedup (
                mutation_id, fingerprint, request_json, state, response_json,
                receipt_id, operation_id, created_at_ms, updated_at_ms, expires_at_ms
             ) VALUES (?, ?, ?, 'processing', NULL, ?, ?, ?, ?, ?)",
        )
        .bind(mutation_id.to_string())
        .bind(fingerprint)
        .bind(request_json)
        .bind(receipt.receipt_id.to_string())
        .bind(operation.operation_id.to_string())
        .bind(now_ms)
        .bind(now_ms)
        .bind(now_ms + MUTATION_DEDUP_TTL_MS)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;

        if inserted {
            sqlx::query(
                "INSERT INTO receipts
                 (receipt_id, action, status, request_json, message, started_at_ms, finished_at_ms, error_json)
                 VALUES (?, ?, 'pending', ?, ?, ?, NULL, NULL)",
            )
            .bind(receipt.receipt_id.to_string())
            .bind(&receipt.action)
            .bind(request_json)
            .bind(&receipt.message)
            .bind(receipt.started_at_ms)
            .execute(&mut *tx)
            .await?;

            let subject_uris_json = serde_json::to_string(&operation.subject_uris)?;
            let reversal_plan_json = operation
                .reversal_plan
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?;
            let pre_state_json = operation
                .pre_state
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?;
            sqlx::query(
                "INSERT INTO operations (
                    operation_id, kind, occurred_at_ms, finished_at_ms,
                    source, requester, subject_uris_json, reversible,
                    reversal_plan_json, pre_state_json, status, receipt_id,
                    subject_op_id, undone_by_op_id, redone_by_op_id, error_message
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(operation.operation_id.to_string())
            .bind(operation.kind.label())
            .bind(operation.occurred_at_ms)
            .bind(operation.finished_at_ms)
            .bind(operation.source.label())
            .bind(operation.requester.as_deref())
            .bind(subject_uris_json)
            .bind(operation.reversible as i64)
            .bind(reversal_plan_json)
            .bind(pre_state_json)
            .bind(operation.status.label())
            .bind(operation.receipt_id.map(|id| id.to_string()))
            .bind(operation.subject_op_id.map(|id| id.to_string()))
            .bind(operation.undone_by_op_id.map(|id| id.to_string()))
            .bind(operation.redone_by_op_id.map(|id| id.to_string()))
            .bind(operation.error_message.as_deref())
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(MutationClaim::Claimed);
        }

        let row = sqlx::query(
            "SELECT fingerprint, response_json, receipt_id
             FROM mutation_dedup WHERE mutation_id = ?",
        )
        .bind(mutation_id.to_string())
        .fetch_one(&mut *tx)
        .await?;
        let existing_fingerprint: String = row.try_get("fingerprint")?;
        if existing_fingerprint != fingerprint {
            tx.commit().await?;
            return Ok(MutationClaim::FingerprintMismatch);
        }
        let response_json: Option<String> = row.try_get("response_json")?;
        let receipt_id: Option<String> = row.try_get("receipt_id")?;
        let receipt = if let Some(receipt_id) = receipt_id {
            let receipt_row = sqlx::query(
                "SELECT receipt_id, action, status, message, started_at_ms, finished_at_ms, error_json
                 FROM receipts WHERE receipt_id = ?",
            )
            .bind(receipt_id)
            .fetch_optional(&mut *tx)
            .await?;
            receipt_row.as_ref().map(row_to_receipt).transpose()?
        } else {
            None
        };
        tx.commit().await?;
        Ok(MutationClaim::Existing {
            receipt: receipt.map(Box::new),
            response_json,
        })
    }

    /// Atomically finalize every durable row owned by a claimed mutation.
    /// A crash observes either all three pending rows or all three terminal
    /// rows, never a confirmed receipt paired with a processing dedup claim.
    #[allow(clippy::too_many_arguments)]
    pub async fn finalize_claimed_mutation(
        &self,
        mutation_id: MutationId,
        receipt_id: spotuify_protocol::ReceiptId,
        receipt_status: spotuify_protocol::ReceiptStatus,
        receipt_message: &str,
        receipt_error: Option<&ApiErrorSummary>,
        operation_id: spotuify_protocol::OperationId,
        operation_status: OperationStatus,
        operation_error: Option<&str>,
        response_json: &str,
        succeeded: bool,
        reconciliations: &[ProviderReconciliation],
        post_write_guard: Option<PostWriteOperationGuard>,
        operation_recovery: Option<&PartialOperationRecovery>,
        finished_at_ms: i64,
    ) -> Result<()> {
        let mut tx = self.writer.begin().await?;
        let error_json = receipt_error.map(serde_json::to_string).transpose()?;
        let receipt_rows = sqlx::query(
            "UPDATE receipts SET status = ?, message = ?, finished_at_ms = ?, error_json = ?
             WHERE receipt_id = ? AND status = 'pending'",
        )
        .bind(match receipt_status {
            spotuify_protocol::ReceiptStatus::Pending => "pending",
            spotuify_protocol::ReceiptStatus::Confirmed => "confirmed",
            spotuify_protocol::ReceiptStatus::Failed => "failed",
        })
        .bind(receipt_message)
        .bind(finished_at_ms)
        .bind(error_json)
        .bind(receipt_id.to_string())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        retain_partial_operation_recovery_tx(&mut tx, operation_id, operation_recovery).await?;
        let operation_rows = sqlx::query(
            "UPDATE operations SET status = ?, finished_at_ms = ?, error_message = ?
             WHERE operation_id = ? AND status = 'pending'",
        )
        .bind(operation_status.label())
        .bind(finished_at_ms)
        .bind(operation_error)
        .bind(operation_id.to_string())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        let claim_rows = sqlx::query(
            "UPDATE mutation_dedup SET state = ?, response_json = ?, updated_at_ms = ?, expires_at_ms = ?
             WHERE mutation_id = ? AND state = 'processing' AND receipt_id = ? AND operation_id = ?",
        )
        .bind(if succeeded { "completed" } else { "failed" })
        .bind(response_json)
        .bind(finished_at_ms)
        .bind(finished_at_ms + MUTATION_DEDUP_TTL_MS)
        .bind(mutation_id.to_string())
        .bind(receipt_id.to_string())
        .bind(operation_id.to_string())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if receipt_rows != 1 || operation_rows != 1 || claim_rows != 1 {
            let expected_receipt_status = match receipt_status {
                spotuify_protocol::ReceiptStatus::Pending => "pending",
                spotuify_protocol::ReceiptStatus::Confirmed => "confirmed",
                spotuify_protocol::ReceiptStatus::Failed => "failed",
            };
            let expected_claim_state = if succeeded { "completed" } else { "failed" };
            let terminal = sqlx::query(
                "SELECT mutation_dedup.state, mutation_dedup.response_json,
                        mutation_dedup.receipt_id, mutation_dedup.operation_id,
                        receipts.status AS receipt_status,
                        operations.status AS operation_status
                 FROM mutation_dedup
                 JOIN receipts ON receipts.receipt_id = mutation_dedup.receipt_id
                 JOIN operations ON operations.operation_id = mutation_dedup.operation_id
                 WHERE mutation_dedup.mutation_id = ?",
            )
            .bind(mutation_id.to_string())
            .fetch_optional(&mut *tx)
            .await?;
            let expected_receipt_id = receipt_id.to_string();
            let expected_operation_id = operation_id.to_string();
            let is_exact_replay = terminal.is_some_and(|row| {
                let Ok(state) = row.try_get::<String, _>("state") else {
                    return false;
                };
                let Ok(stored_response) = row.try_get::<Option<String>, _>("response_json") else {
                    return false;
                };
                let Ok(stored_receipt_id) = row.try_get::<Option<String>, _>("receipt_id") else {
                    return false;
                };
                let Ok(stored_operation_id) = row.try_get::<Option<String>, _>("operation_id")
                else {
                    return false;
                };
                let Ok(stored_receipt_status) = row.try_get::<String, _>("receipt_status") else {
                    return false;
                };
                let Ok(stored_operation_status) = row.try_get::<String, _>("operation_status")
                else {
                    return false;
                };
                state == expected_claim_state
                    && stored_response.as_deref() == Some(response_json)
                    && stored_receipt_id.as_deref() == Some(expected_receipt_id.as_str())
                    && stored_operation_id.as_deref() == Some(expected_operation_id.as_str())
                    && stored_receipt_status == expected_receipt_status
                    && stored_operation_status == operation_status.label()
            });
            if is_exact_replay {
                tx.commit().await?;
                return Ok(());
            }
            anyhow::bail!(
                "mutation finalization lost ownership (receipt={receipt_rows}, operation={operation_rows}, claim={claim_rows})"
            );
        }
        apply_post_write_operation_guard_tx(&mut tx, post_write_guard, operation_id).await?;
        for reconciliation in reconciliations {
            insert_provider_reconciliation_tx(&mut tx, reconciliation, finished_at_ms).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn mark_mutation_indeterminate(
        &self,
        mutation_id: MutationId,
        reconciliations: &[ProviderReconciliation],
        post_write_guard: Option<PostWriteOperationGuard>,
        now_ms: i64,
    ) -> Result<Response> {
        if let Some(row) =
            sqlx::query("SELECT state, response_json FROM mutation_dedup WHERE mutation_id = ?")
                .bind(mutation_id.to_string())
                .fetch_optional(&self.reader)
                .await?
        {
            let state: String = row.try_get("state")?;
            let response_json: Option<String> = row.try_get("response_json")?;
            if state != "processing" {
                if let Some(raw) = response_json {
                    return Ok(serde_json::from_str(&raw)?);
                }
            }
        }
        let response =
            Response::error_with_retryable(INDETERMINATE_MESSAGE, IpcErrorKind::Internal, false);
        let response_json = serde_json::to_string(&response)?;
        self.finalize_processing_claim(
            mutation_id,
            INDETERMINATE_MESSAGE,
            &response_json,
            reconciliations,
            post_write_guard,
            now_ms,
        )
        .await?;
        let durable_response = sqlx::query_scalar::<_, Option<String>>(
            "SELECT response_json FROM mutation_dedup WHERE mutation_id = ?",
        )
        .bind(mutation_id.to_string())
        .fetch_optional(&self.reader)
        .await?
        .flatten();
        match durable_response {
            Some(raw) => Ok(serde_json::from_str(&raw)?),
            None => Ok(response),
        }
    }

    async fn finalize_processing_claim(
        &self,
        mutation_id: MutationId,
        message: &str,
        response_json: &str,
        reconciliations: &[ProviderReconciliation],
        post_write_guard: Option<PostWriteOperationGuard>,
        now_ms: i64,
    ) -> Result<()> {
        let mut tx = self.writer.begin().await?;
        let row = sqlx::query(
            "SELECT receipt_id, operation_id FROM mutation_dedup
             WHERE mutation_id = ? AND state = 'processing'",
        )
        .bind(mutation_id.to_string())
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(());
        };
        let receipt_id: Option<String> = row.try_get("receipt_id")?;
        let operation_id: Option<String> = row.try_get("operation_id")?;
        let outer_operation_id = operation_id
            .as_deref()
            .map(str::parse::<OperationId>)
            .transpose()?;
        let error = ApiErrorSummary {
            kind: IpcErrorKind::Internal,
            message: message.to_string(),
            retry_after_secs: None,
            provider: None,
            detail: None,
        };
        let error_json = serde_json::to_string(&error)?;
        if let Some(receipt_id) = receipt_id {
            sqlx::query(
                "UPDATE receipts SET status = 'failed', message = ?, finished_at_ms = ?, error_json = ?
                 WHERE receipt_id = ? AND status = 'pending'",
            )
            .bind(message)
            .bind(now_ms)
            .bind(error_json)
            .bind(receipt_id)
            .execute(&mut *tx)
            .await?;
        }
        if let Some(operation_id) = operation_id {
            sqlx::query(
                "UPDATE operations SET status = ?, finished_at_ms = ?, error_message = ?
                 WHERE operation_id = ? AND status = 'pending'",
            )
            .bind(OperationStatus::Failed.label())
            .bind(now_ms)
            .bind(message)
            .bind(operation_id)
            .execute(&mut *tx)
            .await?;
        }
        if let Some(outer_operation_id) = outer_operation_id {
            apply_post_write_operation_guard_tx(&mut tx, post_write_guard, outer_operation_id)
                .await?;
        } else if post_write_guard.is_some() {
            anyhow::bail!("processing mutation has no operation for post-write guard");
        }
        sqlx::query(
            "UPDATE mutation_dedup SET state = 'failed', response_json = ?,
             updated_at_ms = ?, expires_at_ms = ? WHERE mutation_id = ?",
        )
        .bind(response_json)
        .bind(now_ms)
        .bind(now_ms + MUTATION_DEDUP_TTL_MS)
        .bind(mutation_id.to_string())
        .execute(&mut *tx)
        .await?;
        for reconciliation in reconciliations {
            insert_provider_reconciliation_tx(&mut tx, reconciliation, now_ms).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Return durable claims whose provider outcome may be unknown after a
    /// daemon restart. Recovery policy remains in the daemon because only it
    /// owns the live provider topology.
    pub async fn processing_mutation_claims(&self) -> Result<Vec<ProcessingMutationClaim>> {
        let rows = sqlx::query(
            "SELECT mutation_id, request_json, receipt_id, operation_id
             FROM mutation_dedup
             WHERE state = 'processing'
             ORDER BY created_at_ms ASC, mutation_id ASC",
        )
        .fetch_all(&self.reader)
        .await?;
        rows.into_iter()
            .map(|row| {
                let mutation_id = row.try_get::<String, _>("mutation_id")?.parse()?;
                let receipt_id = spotuify_protocol::ReceiptId(uuid::Uuid::parse_str(
                    &row.try_get::<String, _>("receipt_id")?,
                )?);
                let operation_id = row.try_get::<String, _>("operation_id")?.parse()?;
                Ok(ProcessingMutationClaim {
                    mutation_id,
                    request_json: row.try_get("request_json")?,
                    receipt_id,
                    operation_id,
                })
            })
            .collect()
    }

    pub async fn recover_processing_mutations(&self, now_ms: i64) -> Result<u64> {
        let rows = sqlx::query("SELECT mutation_id FROM mutation_dedup WHERE state = 'processing'")
            .fetch_all(&self.reader)
            .await?;
        for row in &rows {
            let raw: String = row.try_get("mutation_id")?;
            let id = raw.parse::<MutationId>()?;
            let _ = self
                .mark_mutation_indeterminate(id, &[], None, now_ms)
                .await?;
        }
        Ok(rows.len() as u64)
    }

    pub async fn prune_expired_mutations(&self, now_ms: i64) -> Result<u64> {
        Ok(sqlx::query(
            "DELETE FROM mutation_dedup WHERE state != 'processing' AND expires_at_ms <= ?",
        )
        .bind(now_ms)
        .execute(&self.bulk_writer)
        .await?
        .rows_affected())
    }
}
