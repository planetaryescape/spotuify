//! Phase 12 — operations log CRUD.
//!
//! Every mutating Spotify request becomes an `operations` row. The
//! daemon writes a pending row before calling Spotify (so a crash mid-
//! mutation leaves a forensic trail), finalises the row on completion,
//! and later reads + mutates rows for `ops log` / `ops undo` / `ops redo`.
//!
//! Two invariants are enforced at the SQL layer:
//! 1. `finalize_operation` only updates rows currently in `pending`.
//!    Idempotency: daemon restarts can re-emit Phase 6 lifecycle events
//!    without double-flipping status.
//! 2. `mark_operation_undone` only updates rows in `succeeded`. Trying
//!    to undo an already-undone op is a silent no-op at the SQL layer;
//!    the caller surfaces the "already undone" message.

use anyhow::Result;
use sqlx::Row;

use spotuify_protocol::{
    Operation, OperationId, OperationKind, OperationSource, OperationStatus, PreState, ReceiptId,
    ReversalPlan,
};

use crate::Store;

impl Store {
    /// Persist a new operation row at issue time. The caller is
    /// expected to have minted `op.operation_id` and set
    /// `op.status = Pending`; the row is rejected at the SQL layer if
    /// the id collides.
    pub async fn insert_pending_operation(&self, op: &Operation) -> Result<()> {
        let subject_uris_json = serde_json::to_string(&op.subject_uris)?;
        let reversal_plan_json = match &op.reversal_plan {
            Some(plan) => Some(serde_json::to_string(plan)?),
            None => None,
        };
        let pre_state_json = match &op.pre_state {
            Some(pre) => Some(serde_json::to_string(pre)?),
            None => None,
        };
        sqlx::query(
            "INSERT INTO operations (
                operation_id, kind, occurred_at_ms, finished_at_ms,
                source, requester, subject_uris_json, reversible,
                reversal_plan_json, pre_state_json, status, receipt_id,
                subject_op_id, undone_by_op_id, redone_by_op_id, error_message
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(op.operation_id.0.to_string())
        .bind(op.kind.label())
        .bind(op.occurred_at_ms)
        .bind(op.finished_at_ms)
        .bind(op.source.label())
        .bind(op.requester.as_deref())
        .bind(subject_uris_json)
        .bind(op.reversible as i64)
        .bind(reversal_plan_json)
        .bind(pre_state_json)
        .bind(op.status.label())
        .bind(op.receipt_id.map(|r| r.0.to_string()))
        .bind(op.subject_op_id.map(|s| s.0.to_string()))
        .bind(op.undone_by_op_id.map(|u| u.0.to_string()))
        .bind(op.redone_by_op_id.map(|r| r.0.to_string()))
        .bind(op.error_message.as_deref())
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    /// Transition a pending operation to a terminal status. First-write
    /// wins via the `status = 'pending'` guard: subsequent calls are
    /// silent no-ops so daemon restarts can't double-fire lifecycle
    /// events.
    pub async fn finalize_operation(
        &self,
        operation_id: OperationId,
        status: OperationStatus,
        finished_at_ms: i64,
        error: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE operations
             SET status = ?, finished_at_ms = ?, error_message = ?
             WHERE operation_id = ? AND status = 'pending'",
        )
        .bind(status.label())
        .bind(finished_at_ms)
        .bind(error)
        .bind(operation_id.0.to_string())
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    /// Phase 12 (P12-B) — fill in `pre_state_json` and
    /// `reversal_plan_json` after the daemon has captured pre-mutation
    /// state inside the body. Only updates pending rows so that a
    /// crash mid-mutation doesn't retroactively edit a finalised op.
    pub async fn update_operation_plan(
        &self,
        operation_id: OperationId,
        pre_state: Option<&PreState>,
        plan: Option<&ReversalPlan>,
    ) -> Result<()> {
        let pre_state_json = match pre_state {
            Some(p) => Some(serde_json::to_string(p)?),
            None => None,
        };
        let reversal_plan_json = match plan {
            Some(plan) => Some(serde_json::to_string(plan)?),
            None => None,
        };
        sqlx::query(
            "UPDATE operations
             SET pre_state_json = ?, reversal_plan_json = ?
             WHERE operation_id = ? AND status = 'pending'",
        )
        .bind(pre_state_json)
        .bind(reversal_plan_json)
        .bind(operation_id.0.to_string())
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    /// Atomically publish a late-bound reversal plan after the remote write
    /// has confirmed the version token the reversal must guard against.
    /// Until this succeeds, the pending operation remains non-reversible.
    pub async fn activate_operation_reversal_plan(
        &self,
        operation_id: OperationId,
        pre_state: &PreState,
        plan: &ReversalPlan,
    ) -> Result<()> {
        let pre_state_json = serde_json::to_string(pre_state)?;
        let reversal_plan_json = serde_json::to_string(plan)?;
        let result = sqlx::query(
            "UPDATE operations
             SET pre_state_json = ?, reversal_plan_json = ?, reversible = 1
             WHERE operation_id = ? AND status = 'pending' AND reversible = 0",
        )
        .bind(pre_state_json)
        .bind(reversal_plan_json)
        .bind(operation_id.0.to_string())
        .execute(&self.writer)
        .await?;
        if result.rows_affected() != 1 {
            anyhow::bail!("operation {operation_id} is not a pending non-reversible operation");
        }
        Ok(())
    }

    /// Phase 12 (P12-B) — late-bound `subject_uris` update used by
    /// `playlist_create` whose result URIs are unknown until Spotify
    /// returns the new playlist id.
    pub async fn update_operation_subject_uris(
        &self,
        operation_id: OperationId,
        subject_uris: &[String],
    ) -> Result<()> {
        let subject_uris_json = serde_json::to_string(subject_uris)?;
        sqlx::query(
            "UPDATE operations
             SET subject_uris_json = ?
             WHERE operation_id = ? AND status = 'pending'",
        )
        .bind(subject_uris_json)
        .bind(operation_id.0.to_string())
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    /// Link a pending undo/redo row to the operation it acts on.
    pub async fn update_operation_subject(
        &self,
        operation_id: OperationId,
        subject_op_id: OperationId,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE operations
             SET subject_op_id = ?
             WHERE operation_id = ? AND status = 'pending'",
        )
        .bind(subject_op_id.0.to_string())
        .bind(operation_id.0.to_string())
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    /// Mark a succeeded operation as undone, recording the new undo op
    /// that performed the reversal. Silent no-op for rows not currently
    /// in `succeeded` — the caller surfaces "already undone" /
    /// "transport ops aren't undoable" through the request layer.
    pub async fn mark_operation_undone(
        &self,
        original_id: OperationId,
        undo_op_id: OperationId,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE operations
             SET status = 'undone', undone_by_op_id = ?
             WHERE operation_id = ? AND status = 'succeeded'",
        )
        .bind(undo_op_id.0.to_string())
        .bind(original_id.0.to_string())
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    /// Mark an undone operation as redone (a redo cycle re-executed the
    /// original forward action).
    pub async fn mark_operation_redone(
        &self,
        original_id: OperationId,
        redo_op_id: OperationId,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE operations
             SET status = 'redone', redone_by_op_id = ?
             WHERE operation_id = ? AND status = 'undone'",
        )
        .bind(redo_op_id.0.to_string())
        .bind(original_id.0.to_string())
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    /// Fetch one operation row by id. Errors when missing rather than
    /// returning a synthetic default — the daemon must not treat "not
    /// found" as "already succeeded".
    pub async fn get_operation(&self, operation_id: OperationId) -> Result<Operation> {
        let row = sqlx::query("SELECT * FROM operations WHERE operation_id = ?")
            .bind(operation_id.0.to_string())
            .fetch_optional(&self.reader)
            .await?
            .ok_or_else(|| anyhow::anyhow!("operation {operation_id} not found"))?;
        row_to_operation(&row)
    }

    /// Resolve the operation referenced by an undo/redo relation. Recovery
    /// callers use this to recover the original mutation domain without
    /// embedding operation-table joins in daemon policy.
    pub async fn get_subject_operation(
        &self,
        operation_id: OperationId,
    ) -> Result<Option<Operation>> {
        let row = sqlx::query(
            "SELECT subject.*
             FROM operations outer_op
             JOIN operations subject ON subject.operation_id = outer_op.subject_op_id
             WHERE outer_op.operation_id = ?",
        )
        .bind(operation_id.to_string())
        .fetch_optional(&self.reader)
        .await?;
        row.as_ref().map(row_to_operation).transpose()
    }

    /// List operations, newest first. `since_ms` filters by
    /// `occurred_at_ms >= since`; `source` filters by source label.
    pub async fn list_operations(
        &self,
        limit: u32,
        since_ms: Option<i64>,
        source: Option<OperationSource>,
    ) -> Result<Vec<Operation>> {
        let rows = match (since_ms, source) {
            (Some(since), Some(src)) => {
                sqlx::query(
                    "SELECT * FROM operations
                     WHERE occurred_at_ms >= ? AND source = ?
                     ORDER BY occurred_at_ms DESC
                     LIMIT ?",
                )
                .bind(since)
                .bind(src.label())
                .bind(limit as i64)
                .fetch_all(&self.reader)
                .await?
            }
            (Some(since), None) => {
                sqlx::query(
                    "SELECT * FROM operations
                     WHERE occurred_at_ms >= ?
                     ORDER BY occurred_at_ms DESC
                     LIMIT ?",
                )
                .bind(since)
                .bind(limit as i64)
                .fetch_all(&self.reader)
                .await?
            }
            (None, Some(src)) => {
                sqlx::query(
                    "SELECT * FROM operations
                     WHERE source = ?
                     ORDER BY occurred_at_ms DESC
                     LIMIT ?",
                )
                .bind(src.label())
                .bind(limit as i64)
                .fetch_all(&self.reader)
                .await?
            }
            (None, None) => {
                sqlx::query(
                    "SELECT * FROM operations
                     ORDER BY occurred_at_ms DESC
                     LIMIT ?",
                )
                .bind(limit as i64)
                .fetch_all(&self.reader)
                .await?
            }
        };
        rows.iter().map(row_to_operation).collect()
    }

    /// Find the most recent reversible + succeeded operation. Powers
    /// `spotuify ops undo` (with no id argument) and MCP `undo_last`.
    pub async fn find_last_reversible_operation(&self) -> Result<Option<Operation>> {
        let row = sqlx::query(
            "SELECT * FROM operations
             WHERE reversible = 1 AND status = 'succeeded'
             ORDER BY occurred_at_ms DESC
             LIMIT 1",
        )
        .fetch_optional(&self.reader)
        .await?;
        match row {
            Some(r) => Ok(Some(row_to_operation(&r)?)),
            None => Ok(None),
        }
    }

    /// Find reversible succeeded ops newer than `since_ms`. Drives
    /// `ops undo --since 1h`.
    pub async fn find_reversible_operations_since(
        &self,
        since_ms: i64,
        source: Option<OperationSource>,
    ) -> Result<Vec<Operation>> {
        let rows = match source {
            Some(src) => {
                sqlx::query(
                    "SELECT * FROM operations
                     WHERE reversible = 1 AND status = 'succeeded'
                       AND occurred_at_ms >= ? AND source = ?
                     ORDER BY occurred_at_ms DESC",
                )
                .bind(since_ms)
                .bind(src.label())
                .fetch_all(&self.reader)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT * FROM operations
                     WHERE reversible = 1 AND status = 'succeeded'
                       AND occurred_at_ms >= ?
                     ORDER BY occurred_at_ms DESC",
                )
                .bind(since_ms)
                .fetch_all(&self.reader)
                .await?
            }
        };
        rows.iter().map(row_to_operation).collect()
    }

    /// Recover the complete candidate set for a bulk undo whose provider
    /// write may have completed before local relation bookkeeping. Originals
    /// already linked to the outer undo are retained even after their status
    /// changed; still-succeeded candidates cover the unrecorded suffix.
    pub async fn operations_for_bulk_undo_recovery(
        &self,
        _since_ms: i64,
        outer_undo_id: OperationId,
    ) -> Result<Vec<Operation>> {
        let rows = sqlx::query(
            "SELECT operations.*
             FROM bulk_undo_candidates
             JOIN operations
               ON operations.operation_id = bulk_undo_candidates.member_operation_id
             WHERE bulk_undo_candidates.outer_operation_id = ?
             ORDER BY bulk_undo_candidates.position ASC",
        )
        .bind(outer_undo_id.to_string())
        .fetch_all(&self.reader)
        .await?;
        rows.iter().map(row_to_operation).collect()
    }

    pub async fn record_bulk_undo_candidates(
        &self,
        outer_undo_id: OperationId,
        operations: &[Operation],
    ) -> Result<()> {
        let mut tx = self.writer.begin().await?;
        for (position, operation) in operations.iter().enumerate() {
            sqlx::query(
                "INSERT INTO bulk_undo_candidates (
                     outer_operation_id, member_operation_id, position
                 ) VALUES (?, ?, ?)
                 ON CONFLICT(outer_operation_id, member_operation_id) DO NOTHING",
            )
            .bind(outer_undo_id.to_string())
            .bind(operation.operation_id.to_string())
            .bind(position as i64)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Delete operations older than `cutoff_ms`. Returns rows affected.
    /// Called by the daemon's daily retention job (default 90d).
    /// Routes through the bulk writer so retention pruning doesn't
    /// compete with hot-path mutations.
    pub async fn prune_operations_older_than(&self, cutoff_ms: i64) -> Result<u64> {
        let result = sqlx::query("DELETE FROM operations WHERE occurred_at_ms < ?")
            .bind(cutoff_ms)
            .execute(self.bulk_writer())
            .await?;
        Ok(result.rows_affected())
    }
}

fn row_to_operation(row: &sqlx::sqlite::SqliteRow) -> Result<Operation> {
    let id_str: String = row.try_get("operation_id")?;
    let operation_id = uuid::Uuid::parse_str(&id_str)
        .map(OperationId)
        .map_err(|err| anyhow::anyhow!("malformed operation_id `{id_str}`: {err}"))?;

    let kind_label: String = row.try_get("kind")?;
    let kind = kind_label.parse::<OperationKind>()?;
    let source_label: String = row.try_get("source")?;
    let source = source_label.parse::<OperationSource>()?;
    let status_label: String = row.try_get("status")?;
    let status = status_label.parse::<OperationStatus>()?;

    let subject_uris_json: String = row.try_get("subject_uris_json")?;
    let subject_uris: Vec<String> = serde_json::from_str(&subject_uris_json)
        .map_err(|err| anyhow::anyhow!("malformed subject_uris_json: {err}"))?;

    let reversal_plan_json: Option<String> = row.try_get("reversal_plan_json")?;
    let reversal_plan: Option<ReversalPlan> = match reversal_plan_json {
        Some(raw) if !raw.is_empty() => Some(
            serde_json::from_str(&raw)
                .map_err(|err| anyhow::anyhow!("malformed reversal_plan_json: {err}"))?,
        ),
        _ => None,
    };
    let pre_state_json: Option<String> = row.try_get("pre_state_json")?;
    let pre_state: Option<PreState> = match pre_state_json {
        Some(raw) if !raw.is_empty() => Some(
            serde_json::from_str(&raw)
                .map_err(|err| anyhow::anyhow!("malformed pre_state_json: {err}"))?,
        ),
        _ => None,
    };

    let reversible: i64 = row.try_get("reversible")?;
    let receipt_id_str: Option<String> = row.try_get("receipt_id")?;
    let receipt_id = match receipt_id_str {
        Some(raw) => Some(
            uuid::Uuid::parse_str(&raw)
                .map(ReceiptId)
                .map_err(|err| anyhow::anyhow!("malformed receipt_id `{raw}`: {err}"))?,
        ),
        None => None,
    };
    let subject_op_id = parse_optional_op_id(row, "subject_op_id")?;
    let undone_by_op_id = parse_optional_op_id(row, "undone_by_op_id")?;
    let redone_by_op_id = parse_optional_op_id(row, "redone_by_op_id")?;

    Ok(Operation {
        operation_id,
        kind,
        occurred_at_ms: row.try_get("occurred_at_ms")?,
        finished_at_ms: row.try_get("finished_at_ms")?,
        source,
        requester: row.try_get("requester")?,
        subject_uris,
        reversible: reversible != 0,
        reversal_plan,
        pre_state,
        status,
        receipt_id,
        subject_op_id,
        undone_by_op_id,
        redone_by_op_id,
        error_message: row.try_get("error_message")?,
    })
}

fn parse_optional_op_id(
    row: &sqlx::sqlite::SqliteRow,
    column: &str,
) -> Result<Option<OperationId>> {
    let raw: Option<String> = row.try_get(column)?;
    match raw {
        Some(raw) => {
            Ok(Some(uuid::Uuid::parse_str(&raw).map(OperationId).map_err(
                |err| anyhow::anyhow!("malformed {column} `{raw}`: {err}"),
            )?))
        }
        None => Ok(None),
    }
}
