use anyhow::Result;
use spotuify_core::ProviderId;
use spotuify_protocol::{
    ApiErrorSummary, OperationId, OperationStatus, PreState, ReceiptId, ReversalPlan,
    SyncTargetData,
};
use sqlx::{Row, Sqlite, Transaction};

use crate::Store;

/// Durable follow-up required after a provider acknowledges only part of a
/// batch mutation. The row is keyed to the mutation's existing receipt and
/// operation so dedup replay and daemon restart converge on one reconciliation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderReconciliation {
    pub reconciliation_id: uuid::Uuid,
    pub receipt_id: ReceiptId,
    pub operation_id: OperationId,
    pub provider: ProviderId,
    pub target: SyncTargetData,
    pub scope: ProviderReconciliationScope,
    pub resource_uris: Vec<String>,
    pub attempts: u32,
    pub claim_token: Option<uuid::Uuid>,
    pub last_error: Option<String>,
    minimum_successful_passes: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderReconciliationCompletion {
    Completed,
    NeedsAnotherPass,
    Stale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderReconciliationScope {
    Targeted,
    FullDomain,
}

impl ProviderReconciliationScope {
    fn label(self) -> &'static str {
        match self {
            Self::Targeted => "targeted",
            Self::FullDomain => "full_domain",
        }
    }

    fn parse(label: &str) -> Result<Self> {
        match label {
            "targeted" => Ok(Self::Targeted),
            "full_domain" => Ok(Self::FullDomain),
            other => anyhow::bail!("unknown provider reconciliation scope `{other}`"),
        }
    }
}

fn normalized_resources(
    scope: ProviderReconciliationScope,
    resources: impl IntoIterator<Item = String>,
) -> Vec<String> {
    if scope == ProviderReconciliationScope::FullDomain {
        return Vec::new();
    }
    let mut resources = resources.into_iter().collect::<Vec<_>>();
    resources.sort();
    resources.dedup();
    resources
}

#[derive(Clone, Debug)]
pub struct PartialOperationRecovery {
    pub pre_state: PreState,
    pub reversal_plan: ReversalPlan,
    pub subject_uris: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostWriteOperationGuard {
    /// A partially acknowledged undo must never be offered again until the
    /// remote state has been reconciled.
    DisableUndo(OperationId),
    /// A remotely applied redo consumes the original undone operation even
    /// if later receipt bookkeeping fails.
    MarkRedone(OperationId),
}

impl ProviderReconciliation {
    /// Compatibility constructor for the original targeted-only API.
    pub fn pending(
        receipt_id: ReceiptId,
        operation_id: OperationId,
        provider: ProviderId,
        target: SyncTargetData,
        resource_uris: Vec<String>,
    ) -> Self {
        Self::targeted(receipt_id, operation_id, provider, target, resource_uris)
    }

    pub fn targeted(
        receipt_id: ReceiptId,
        operation_id: OperationId,
        provider: ProviderId,
        target: SyncTargetData,
        resource_uris: Vec<String>,
    ) -> Self {
        Self {
            reconciliation_id: uuid::Uuid::now_v7(),
            receipt_id,
            operation_id,
            provider,
            target,
            scope: ProviderReconciliationScope::Targeted,
            resource_uris,
            attempts: 0,
            claim_token: None,
            last_error: None,
            minimum_successful_passes: 1,
        }
    }

    pub fn full_domain(
        receipt_id: ReceiptId,
        operation_id: OperationId,
        provider: ProviderId,
        target: SyncTargetData,
    ) -> Self {
        Self {
            reconciliation_id: uuid::Uuid::now_v7(),
            receipt_id,
            operation_id,
            provider,
            target,
            scope: ProviderReconciliationScope::FullDomain,
            resource_uris: Vec::new(),
            attempts: 0,
            claim_token: None,
            last_error: None,
            minimum_successful_passes: 1,
        }
    }

    pub fn require_stability_pass(&mut self) {
        self.minimum_successful_passes = 2;
    }
}

async fn upsert_reconciliation_stability_tx(
    tx: &mut Transaction<'_, Sqlite>,
    reconciliation_id: uuid::Uuid,
    minimum_successful_passes: u8,
) -> Result<()> {
    if minimum_successful_passes <= 1 {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO provider_reconciliation_stability (
             reconciliation_id, required_passes, successful_passes
         ) VALUES (?, ?, 0)
         ON CONFLICT(reconciliation_id) DO UPDATE SET
             required_passes = MAX(required_passes, excluded.required_passes)",
    )
    .bind(reconciliation_id.to_string())
    .bind(i64::from(minimum_successful_passes))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub(crate) async fn insert_provider_reconciliation_tx(
    tx: &mut Transaction<'_, Sqlite>,
    reconciliation: &ProviderReconciliation,
    created_at_ms: i64,
) -> Result<()> {
    let existing = sqlx::query(
        "SELECT reconciliation_id, operation_id, scope, resource_uris_json
         FROM provider_reconciliations
         WHERE receipt_id = ? AND provider = ? AND target = ?",
    )
    .bind(reconciliation.receipt_id.to_string())
    .bind(reconciliation.provider.as_str())
    .bind(reconciliation.target.label())
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(existing) = existing {
        let existing_operation: String = existing.try_get("operation_id")?;
        if existing_operation != reconciliation.operation_id.to_string() {
            anyhow::bail!("reconciliation key is already bound to operation {existing_operation}");
        }
        let existing_scope =
            ProviderReconciliationScope::parse(&existing.try_get::<String, _>("scope")?)?;
        let merged_scope = if existing_scope == ProviderReconciliationScope::FullDomain
            || reconciliation.scope == ProviderReconciliationScope::FullDomain
        {
            ProviderReconciliationScope::FullDomain
        } else {
            ProviderReconciliationScope::Targeted
        };
        let existing_resources = serde_json::from_str::<Vec<String>>(
            &existing.try_get::<String, _>("resource_uris_json")?,
        )?;
        let merged_resources = normalized_resources(
            merged_scope,
            existing_resources
                .into_iter()
                .chain(reconciliation.resource_uris.iter().cloned()),
        );
        sqlx::query(
            "UPDATE provider_reconciliations
             SET scope = ?, resource_uris_json = ?
             WHERE reconciliation_id = ?",
        )
        .bind(merged_scope.label())
        .bind(serde_json::to_string(&merged_resources)?)
        .bind(existing.try_get::<String, _>("reconciliation_id")?)
        .execute(&mut **tx)
        .await?;
        upsert_reconciliation_stability_tx(
            tx,
            uuid::Uuid::parse_str(&existing.try_get::<String, _>("reconciliation_id")?)?,
            reconciliation.minimum_successful_passes,
        )
        .await?;
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO provider_reconciliations
         (reconciliation_id, receipt_id, operation_id, provider, target, scope, resource_uris_json,
          status, attempts, last_error, created_at_ms, finished_at_ms)
         VALUES (?, ?, ?, ?, ?, ?, ?, 'pending', 0, NULL, ?, NULL)",
    )
    .bind(reconciliation.reconciliation_id.to_string())
    .bind(reconciliation.receipt_id.to_string())
    .bind(reconciliation.operation_id.to_string())
    .bind(reconciliation.provider.as_str())
    .bind(reconciliation.target.label())
    .bind(reconciliation.scope.label())
    .bind(serde_json::to_string(&normalized_resources(
        reconciliation.scope,
        reconciliation.resource_uris.iter().cloned(),
    ))?)
    .bind(created_at_ms)
    .execute(&mut **tx)
    .await?;
    upsert_reconciliation_stability_tx(
        tx,
        reconciliation.reconciliation_id,
        reconciliation.minimum_successful_passes,
    )
    .await?;
    Ok(())
}

pub(crate) async fn disable_partial_undo_tx(
    tx: &mut Transaction<'_, Sqlite>,
    original_operation_id: Option<OperationId>,
) -> Result<()> {
    let Some(original_operation_id) = original_operation_id else {
        return Ok(());
    };
    let plan = spotuify_protocol::ReversalPlan::NotReversible {
        reason: "a partial reversal was acknowledged; reconcile remote state before a new mutation"
            .to_string(),
    };
    let rows = sqlx::query(
        "UPDATE operations
         SET reversible = 0, reversal_plan_json = ?
         WHERE operation_id = ?",
    )
    .bind(serde_json::to_string(&plan)?)
    .bind(original_operation_id.to_string())
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if rows != 1 {
        anyhow::bail!(
            "partial undo original operation {} was not found",
            original_operation_id
        );
    }
    Ok(())
}

pub(crate) async fn apply_post_write_operation_guard_tx(
    tx: &mut Transaction<'_, Sqlite>,
    guard: Option<PostWriteOperationGuard>,
    outer_operation_id: OperationId,
) -> Result<()> {
    match guard {
        None => Ok(()),
        Some(PostWriteOperationGuard::DisableUndo(original)) => {
            disable_partial_undo_tx(tx, Some(original)).await
        }
        Some(PostWriteOperationGuard::MarkRedone(original)) => {
            let rows = sqlx::query(
                "UPDATE operations
                 SET status = 'redone', redone_by_op_id = ?
                 WHERE operation_id = ? AND status = 'undone'",
            )
            .bind(outer_operation_id.to_string())
            .bind(original.to_string())
            .execute(&mut **tx)
            .await?
            .rows_affected();
            if rows == 1 {
                return Ok(());
            }
            let existing = sqlx::query(
                "SELECT status, redone_by_op_id FROM operations WHERE operation_id = ?",
            )
            .bind(original.to_string())
            .fetch_optional(&mut **tx)
            .await?;
            let outer_operation_id = outer_operation_id.to_string();
            let already_applied = existing.is_some_and(|row| {
                let Ok(status) = row.try_get::<String, _>("status") else {
                    return false;
                };
                let Ok(redone_by) = row.try_get::<Option<String>, _>("redone_by_op_id") else {
                    return false;
                };
                status == "redone" && redone_by.as_deref() == Some(outer_operation_id.as_str())
            });
            if already_applied {
                Ok(())
            } else {
                anyhow::bail!("redo original operation {original} is not undone")
            }
        }
    }
}

pub(crate) async fn retain_partial_operation_recovery_tx(
    tx: &mut Transaction<'_, Sqlite>,
    operation_id: OperationId,
    recovery: Option<&PartialOperationRecovery>,
) -> Result<()> {
    let Some(recovery) = recovery else {
        return Ok(());
    };
    sqlx::query(
        "UPDATE operations
         SET reversible = 1, pre_state_json = ?, reversal_plan_json = ?, subject_uris_json = ?
         WHERE operation_id = ? AND status = 'pending'",
    )
    .bind(serde_json::to_string(&recovery.pre_state)?)
    .bind(serde_json::to_string(&recovery.reversal_plan)?)
    .bind(serde_json::to_string(&recovery.subject_uris)?)
    .bind(operation_id.to_string())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

impl Store {
    /// Force the next playlist sync to refetch items even when a provider
    /// incorrectly reuses its opaque version token after a partial mutation.
    pub async fn clear_playlist_version_token(
        &self,
        provider: &str,
        playlist_uri: &str,
    ) -> Result<()> {
        let result = sqlx::query(
            "UPDATE playlists SET snapshot_id = NULL
             WHERE (id = ? OR uri = ?)
               AND EXISTS (
                   SELECT 1 FROM media_items
                   WHERE media_items.uri = playlists.uri
                     AND media_items.provider = ?
               )",
        )
        .bind(playlist_uri)
        .bind(playlist_uri)
        .bind(provider)
        .execute(&self.bulk_writer)
        .await?;
        if result.rows_affected() == 0 {
            // No matching row means the forced refetch never gets armed — the
            // exact failure this method exists to prevent. Surface it instead
            // of silently succeeding.
            tracing::warn!(
                provider,
                playlist_uri,
                "clear_playlist_version_token matched no playlist row; version-token \
                 refetch was not forced (missing playlist or provider mismatch)"
            );
        }
        Ok(())
    }

    /// Atomically terminalize an unclaimed failed mutation, retain any cleanup
    /// plan, make a partially reversed original non-undoable, and optionally
    /// persist an authoritative reconciliation intent.
    #[allow(clippy::too_many_arguments)]
    pub async fn finalize_partial_operation(
        &self,
        receipt_id: ReceiptId,
        receipt_message: &str,
        receipt_error: &ApiErrorSummary,
        operation_id: OperationId,
        operation_status: OperationStatus,
        operation_error: &str,
        reconciliations: &[ProviderReconciliation],
        post_write_guard: Option<PostWriteOperationGuard>,
        operation_recovery: Option<&PartialOperationRecovery>,
        finished_at_ms: i64,
    ) -> Result<()> {
        let mut tx = self.writer.begin().await?;
        let receipt_rows = sqlx::query(
            "UPDATE receipts
             SET status = 'failed', message = ?, finished_at_ms = ?, error_json = ?
             WHERE receipt_id = ? AND status = 'pending'",
        )
        .bind(receipt_message)
        .bind(finished_at_ms)
        .bind(serde_json::to_string(receipt_error)?)
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
        if receipt_rows != 1 || operation_rows != 1 {
            anyhow::bail!(
                "partial mutation finalization lost ownership (receipt={receipt_rows}, operation={operation_rows})"
            );
        }
        apply_post_write_operation_guard_tx(&mut tx, post_write_guard, operation_id).await?;
        for reconciliation in reconciliations {
            insert_provider_reconciliation_tx(&mut tx, reconciliation, finished_at_ms).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn pending_provider_reconciliations(&self) -> Result<Vec<ProviderReconciliation>> {
        let rows = sqlx::query(
            "SELECT reconciliation_id, receipt_id, operation_id, provider, target,
                    scope, resource_uris_json, attempts, claim_token, last_error
             FROM provider_reconciliations
             WHERE status = 'pending'
             ORDER BY created_at_ms ASC",
        )
        .fetch_all(&self.reader)
        .await?;
        rows.iter().map(row_to_reconciliation).collect()
    }

    pub async fn pending_provider_reconciliations_for_receipt(
        &self,
        receipt_id: ReceiptId,
    ) -> Result<Vec<ProviderReconciliation>> {
        let rows = sqlx::query(
            "SELECT reconciliation_id, receipt_id, operation_id, provider, target,
                    scope, resource_uris_json, attempts, claim_token, last_error
             FROM provider_reconciliations
             WHERE receipt_id = ? AND status = 'pending'
             ORDER BY created_at_ms ASC, reconciliation_id ASC",
        )
        .bind(receipt_id.to_string())
        .fetch_all(&self.reader)
        .await?;
        rows.iter().map(row_to_reconciliation).collect()
    }

    pub async fn claim_provider_reconciliation_if_attempts(
        &self,
        reconciliation_id: uuid::Uuid,
        expected_attempts: u32,
        claim_token: uuid::Uuid,
    ) -> Result<Option<ProviderReconciliation>> {
        self.claim_provider_reconciliation_inner(reconciliation_id, expected_attempts, claim_token)
            .await
    }

    pub async fn recover_provider_reconciliation_claim_after_error(
        &self,
        reconciliation_id: uuid::Uuid,
        expected_attempts: u32,
        claim_token: uuid::Uuid,
    ) -> Result<Option<ProviderReconciliation>> {
        let row = sqlx::query(
            "SELECT reconciliation_id, receipt_id, operation_id, provider, target,
                    scope, resource_uris_json, attempts, claim_token, last_error
             FROM provider_reconciliations
             WHERE reconciliation_id = ? AND status = 'running'",
        )
        .bind(reconciliation_id.to_string())
        .fetch_optional(&self.reader)
        .await?;
        if let Some(reconciliation) = row.as_ref().map(row_to_reconciliation).transpose()? {
            if reconciliation.attempts == expected_attempts.saturating_add(1)
                && reconciliation.claim_token == Some(claim_token)
            {
                return Ok(Some(reconciliation));
            }
        }
        self.claim_provider_reconciliation_inner(reconciliation_id, expected_attempts, claim_token)
            .await
    }

    async fn claim_provider_reconciliation_inner(
        &self,
        reconciliation_id: uuid::Uuid,
        expected_attempts: u32,
        claim_token: uuid::Uuid,
    ) -> Result<Option<ProviderReconciliation>> {
        let claim_result: Result<Option<ProviderReconciliation>> = async {
            let mut tx = self.writer.begin().await?;
            let now_ms = spotuify_core::now_ms();
            let claimed = sqlx::query(
                "UPDATE provider_reconciliations
                 SET status = 'running', attempts = attempts + 1,
                     claim_token = ?, last_claim_token = NULL
                 WHERE reconciliation_id = ? AND status = 'pending' AND attempts = ?
                   AND NOT EXISTS (
                       SELECT 1 FROM provider_reconciliation_stability stability
                       WHERE stability.reconciliation_id = provider_reconciliations.reconciliation_id
                         AND stability.next_pass_after_ms > ?
                   )",
            )
            .bind(claim_token.to_string())
            .bind(reconciliation_id.to_string())
            .bind(i64::from(expected_attempts))
            .bind(now_ms)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if claimed == 0 {
                tx.commit().await?;
                return Ok(None);
            }
            let row = sqlx::query(
                "SELECT reconciliation_id, receipt_id, operation_id, provider, target,
                    scope, resource_uris_json, attempts, claim_token, last_error
             FROM provider_reconciliations
             WHERE reconciliation_id = ? AND status = 'running'",
            )
            .bind(reconciliation_id.to_string())
            .fetch_optional(&mut *tx)
            .await?;
            tx.commit().await?;
            row.as_ref().map(row_to_reconciliation).transpose()
        }
        .await;
        match claim_result {
            Ok(result) => Ok(result),
            Err(error) => {
                let row = sqlx::query(
                    "SELECT reconciliation_id, receipt_id, operation_id, provider, target,
                            scope, resource_uris_json, attempts, claim_token, last_error
                     FROM provider_reconciliations
                     WHERE reconciliation_id = ? AND status = 'running'",
                )
                .bind(reconciliation_id.to_string())
                .fetch_optional(&self.reader)
                .await?;
                match row.as_ref().map(row_to_reconciliation).transpose()? {
                    Some(reconciliation)
                        if reconciliation.attempts == expected_attempts.saturating_add(1)
                            && reconciliation.claim_token == Some(claim_token) =>
                    {
                        Ok(Some(reconciliation))
                    }
                    _ => Err(error),
                }
            }
        }
    }

    pub async fn recover_running_provider_reconciliations(&self) -> Result<u64> {
        Ok(sqlx::query(
            "UPDATE provider_reconciliations
             SET status = 'pending',
                 last_error = 'daemon stopped during reconciliation',
                 claim_token = NULL, last_claim_token = NULL
             WHERE status = 'running'",
        )
        .execute(&self.writer)
        .await?
        .rows_affected())
    }

    pub async fn provider_reconciliation_not_before_ms(
        &self,
        reconciliation_id: uuid::Uuid,
        expected_attempts: u32,
    ) -> Result<Option<i64>> {
        let now_ms = spotuify_core::now_ms();
        Ok(sqlx::query_scalar::<_, Option<i64>>(
            "SELECT stability.next_pass_after_ms
             FROM provider_reconciliations reconciliation
             JOIN provider_reconciliation_stability stability
               ON stability.reconciliation_id = reconciliation.reconciliation_id
             WHERE reconciliation.reconciliation_id = ?
               AND reconciliation.status = 'pending'
               AND reconciliation.attempts = ?
               AND stability.next_pass_after_ms > ?",
        )
        .bind(reconciliation_id.to_string())
        .bind(i64::from(expected_attempts))
        .bind(now_ms)
        .fetch_optional(&self.reader)
        .await?
        .flatten())
    }

    pub async fn provider_reconciliation_pending(&self, receipt_id: ReceiptId) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM provider_reconciliations
             WHERE receipt_id = ? AND status IN ('pending', 'running')",
        )
        .bind(receipt_id.to_string())
        .fetch_one(&self.reader)
        .await?
            > 0)
    }

    /// Whether a durable reconciliation row exists, regardless of whether a
    /// worker has already claimed or completed it.
    pub async fn provider_reconciliation_exists(&self, receipt_id: ReceiptId) -> Result<bool> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM provider_reconciliations WHERE receipt_id = ?",
        )
        .bind(receipt_id.to_string())
        .fetch_one(&self.reader)
        .await?;
        Ok(count > 0)
    }

    pub async fn record_provider_reconciliation_success(
        &self,
        reconciliation_id: uuid::Uuid,
        expected_attempts: u32,
        claim_token: uuid::Uuid,
        finished_at_ms: i64,
    ) -> Result<ProviderReconciliationCompletion> {
        let update: Result<ProviderReconciliationCompletion> = async {
            let mut tx = self.writer.begin().await?;
            let row = sqlx::query(
                "SELECT reconciliation_id,
                        COALESCE((SELECT required_passes
                                  FROM provider_reconciliation_stability stability
                                  WHERE stability.reconciliation_id = provider_reconciliations.reconciliation_id), 1)
                            AS required_passes,
                        COALESCE((SELECT successful_passes
                                  FROM provider_reconciliation_stability stability
                                  WHERE stability.reconciliation_id = provider_reconciliations.reconciliation_id), 0)
                            AS successful_passes
                 FROM provider_reconciliations
                 WHERE reconciliation_id = ? AND status = 'running' AND attempts = ?
                   AND claim_token = ?",
            )
            .bind(reconciliation_id.to_string())
            .bind(i64::from(expected_attempts))
            .bind(claim_token.to_string())
            .fetch_optional(&mut *tx)
            .await?;
            let Some(row) = row else {
                tx.commit().await?;
                return Ok(ProviderReconciliationCompletion::Stale);
            };
            let required = row.try_get::<i64, _>("required_passes")?;
            let successful = row.try_get::<i64, _>("successful_passes")?;
            if successful.saturating_add(1) < required {
                sqlx::query(
                    "UPDATE provider_reconciliation_stability
                     SET successful_passes = successful_passes + 1,
                         next_pass_after_ms = ?
                     WHERE reconciliation_id = ?",
                )
                .bind(finished_at_ms.saturating_add(2_000))
                .bind(reconciliation_id.to_string())
                .execute(&mut *tx)
                .await?;
                let rows = sqlx::query(
                    "UPDATE provider_reconciliations
                     SET status = 'pending', last_error = NULL,
                         claim_token = NULL, last_claim_token = ?
                     WHERE reconciliation_id = ? AND status = 'running' AND attempts = ?
                       AND claim_token = ?",
                )
                .bind(claim_token.to_string())
                .bind(reconciliation_id.to_string())
                .bind(i64::from(expected_attempts))
                .bind(claim_token.to_string())
                .execute(&mut *tx)
                .await?
                .rows_affected();
                if rows != 1 {
                    anyhow::bail!("provider reconciliation stability pass lost ownership");
                }
                tx.commit().await?;
                return Ok(ProviderReconciliationCompletion::NeedsAnotherPass);
            }
            let rows = sqlx::query(
                "UPDATE provider_reconciliations
                 SET status = 'completed', finished_at_ms = ?, last_error = NULL,
                     claim_token = NULL, last_claim_token = ?
                 WHERE reconciliation_id = ? AND status = 'running' AND attempts = ?
                   AND claim_token = ?",
            )
            .bind(finished_at_ms)
            .bind(claim_token.to_string())
            .bind(reconciliation_id.to_string())
            .bind(i64::from(expected_attempts))
            .bind(claim_token.to_string())
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if rows != 1 {
                anyhow::bail!("provider reconciliation completion lost ownership");
            }
            sqlx::query(
                "UPDATE provider_reconciliation_stability
                 SET successful_passes = successful_passes + 1,
                     next_pass_after_ms = NULL
                 WHERE reconciliation_id = ?",
            )
            .bind(reconciliation_id.to_string())
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(ProviderReconciliationCompletion::Completed)
        }
        .await;
        let update_error = match update {
            Ok(ProviderReconciliationCompletion::Completed) => {
                return Ok(ProviderReconciliationCompletion::Completed);
            }
            Ok(ProviderReconciliationCompletion::NeedsAnotherPass) => {
                return Ok(ProviderReconciliationCompletion::NeedsAnotherPass);
            }
            Ok(ProviderReconciliationCompletion::Stale) => None,
            Err(error) => Some(error),
        };
        let row = sqlx::query(
            "SELECT status, attempts, claim_token, last_claim_token
             FROM provider_reconciliations
             WHERE reconciliation_id = ?",
        )
        .bind(reconciliation_id.to_string())
        .fetch_optional(&self.reader)
        .await?;
        match row {
            Some(row) => {
                let attempts = row.try_get::<i64, _>("attempts")?;
                let status = row.try_get::<String, _>("status")?;
                let current_token =
                    parse_optional_uuid(row.try_get::<Option<String>, _>("claim_token")?)?;
                let last_token =
                    parse_optional_uuid(row.try_get::<Option<String>, _>("last_claim_token")?)?;
                if attempts != i64::from(expected_attempts) {
                    return Ok(ProviderReconciliationCompletion::Stale);
                }
                match status.as_str() {
                    "completed" if last_token == Some(claim_token) => {
                        Ok(ProviderReconciliationCompletion::Completed)
                    }
                    "pending" if last_token == Some(claim_token) => {
                        Ok(ProviderReconciliationCompletion::NeedsAnotherPass)
                    }
                    "running" if current_token == Some(claim_token) => Err(update_error
                        .unwrap_or_else(|| {
                            anyhow::anyhow!("provider reconciliation success lost ownership")
                        })),
                    _ => Ok(ProviderReconciliationCompletion::Stale),
                }
            }
            None => update_error.map_or_else(|| Ok(ProviderReconciliationCompletion::Stale), Err),
        }
    }

    pub async fn fail_provider_reconciliation_if_attempts(
        &self,
        reconciliation_id: uuid::Uuid,
        expected_attempts: u32,
        claim_token: uuid::Uuid,
        error: &str,
    ) -> Result<bool> {
        let redacted = spotuify_protocol::redact_sensitive_text(error);
        let mut error = redacted.chars().take(512).collect::<String>();
        if redacted.chars().count() > 512 {
            error.push('…');
        }
        let update = sqlx::query(
            "UPDATE provider_reconciliations
             SET status = 'pending', last_error = ?,
                 claim_token = NULL, last_claim_token = ?
             WHERE reconciliation_id = ? AND status = 'running' AND attempts = ?
               AND claim_token = ?",
        )
        .bind(error)
        .bind(claim_token.to_string())
        .bind(reconciliation_id.to_string())
        .bind(i64::from(expected_attempts))
        .bind(claim_token.to_string())
        .execute(&self.writer)
        .await;
        let update_error = match update {
            Ok(result) if result.rows_affected() == 1 => return Ok(true),
            Ok(_) => None,
            Err(error) => Some(error),
        };
        let row = sqlx::query(
            "SELECT status, attempts, claim_token, last_claim_token
             FROM provider_reconciliations
             WHERE reconciliation_id = ?",
        )
        .bind(reconciliation_id.to_string())
        .fetch_optional(&self.reader)
        .await?;
        match row {
            Some(row) => {
                let status = row.try_get::<String, _>("status")?;
                let attempts = row.try_get::<i64, _>("attempts")?;
                let current_token =
                    parse_optional_uuid(row.try_get::<Option<String>, _>("claim_token")?)?;
                let last_token =
                    parse_optional_uuid(row.try_get::<Option<String>, _>("last_claim_token")?)?;
                match classify_guarded_write_after_error(
                    &status,
                    attempts,
                    expected_attempts,
                    current_token,
                    last_token,
                    claim_token,
                    "pending",
                ) {
                    GuardedWriteAfterError::Applied => Ok(true),
                    GuardedWriteAfterError::Stale => Ok(false),
                    GuardedWriteAfterError::Retry => Err(update_error.map_or_else(
                        || anyhow::anyhow!("provider reconciliation reset lost ownership"),
                        Into::into,
                    )),
                }
            }
            None => update_error.map_or_else(|| Ok(false), |error| Err(error.into())),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GuardedWriteAfterError {
    Applied,
    Stale,
    Retry,
}

fn classify_guarded_write_after_error(
    status: &str,
    attempts: i64,
    expected_attempts: u32,
    current_token: Option<uuid::Uuid>,
    last_token: Option<uuid::Uuid>,
    claim_token: uuid::Uuid,
    applied_status: &str,
) -> GuardedWriteAfterError {
    if attempts != i64::from(expected_attempts) {
        return GuardedWriteAfterError::Stale;
    }
    if status == applied_status && last_token == Some(claim_token) {
        return GuardedWriteAfterError::Applied;
    }
    if status == "running" && current_token == Some(claim_token) {
        return GuardedWriteAfterError::Retry;
    }
    GuardedWriteAfterError::Stale
}

fn row_to_reconciliation(row: &sqlx::sqlite::SqliteRow) -> Result<ProviderReconciliation> {
    let reconciliation_id = uuid::Uuid::parse_str(&row.try_get::<String, _>("reconciliation_id")?)?;
    let receipt_id = ReceiptId(uuid::Uuid::parse_str(
        &row.try_get::<String, _>("receipt_id")?,
    )?);
    let operation_id = row.try_get::<String, _>("operation_id")?.parse()?;
    let provider = ProviderId::new(row.try_get::<String, _>("provider")?)?;
    let target = match row.try_get::<String, _>("target")?.as_str() {
        "library" => SyncTargetData::Library,
        "playlists" => SyncTargetData::Playlists,
        other => anyhow::bail!("unknown provider reconciliation target `{other}`"),
    };
    Ok(ProviderReconciliation {
        reconciliation_id,
        receipt_id,
        operation_id,
        provider,
        target,
        scope: ProviderReconciliationScope::parse(&row.try_get::<String, _>("scope")?)?,
        resource_uris: serde_json::from_str(&row.try_get::<String, _>("resource_uris_json")?)?,
        attempts: row.try_get::<i64, _>("attempts")?.try_into()?,
        claim_token: parse_optional_uuid(row.try_get("claim_token")?)?,
        last_error: row.try_get("last_error")?,
        minimum_successful_passes: 1,
    })
}

fn parse_optional_uuid(value: Option<String>) -> Result<Option<uuid::Uuid>> {
    value
        .map(|value| uuid::Uuid::parse_str(&value).map_err(Into::into))
        .transpose()
}

#[cfg(test)]
mod guarded_write_recovery_tests {
    use super::{
        classify_guarded_write_after_error,
        GuardedWriteAfterError::{Applied, Retry, Stale},
    };

    #[test]
    fn same_generation_running_write_error_must_retry() {
        let token = uuid::Uuid::now_v7();
        let other = uuid::Uuid::now_v7();
        assert_eq!(
            classify_guarded_write_after_error(
                "running",
                2,
                2,
                Some(token),
                None,
                token,
                "pending",
            ),
            Retry
        );
        assert_eq!(
            classify_guarded_write_after_error(
                "running",
                2,
                2,
                Some(other),
                None,
                token,
                "pending",
            ),
            Stale
        );
        assert_eq!(
            classify_guarded_write_after_error(
                "pending",
                2,
                2,
                None,
                Some(token),
                token,
                "pending",
            ),
            Applied
        );
        assert_eq!(
            classify_guarded_write_after_error(
                "pending",
                2,
                2,
                None,
                Some(other),
                token,
                "pending",
            ),
            Stale
        );
        assert_eq!(
            classify_guarded_write_after_error(
                "running",
                3,
                2,
                Some(token),
                None,
                token,
                "pending",
            ),
            Stale
        );
    }
}
