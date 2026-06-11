//! Phase 12 (P12.1, P12.2) — ReversalPlan execution + conflict detection.
//!
//! Pure logic module: given a `ReversalPlan` and the matching `PreState`
//! captured at issue time, compute the work needed to revert and (when
//! `force=false`) refuse if Spotify's current snapshot diverges from the
//! pre-mutation snapshot.
//!
//! The actual Spotify Web API calls are deferred to the
//! `apply_reversal` async fn so the planner stays unit-testable without
//! a network. The daemon's `Request::OpsUndo` handler chains
//! `plan_reversal` → `apply_reversal`; tests cover both halves.

use spotuify_protocol::{
    Operation, OperationId, OperationKind, OperationSource, OperationStatus, PreState, ReversalPlan,
};

#[derive(Debug, thiserror::Error)]
pub enum UndoError {
    #[error("operation is not reversible (kind = {kind:?})")]
    NotReversible { kind: OperationKind },

    #[error(
        "operation status is `{status:?}`; only succeeded ops can be undone \
         (use `ops show {operation_id}` to inspect)"
    )]
    NotInUndoableState {
        operation_id: String,
        status: OperationStatus,
    },

    #[error(
        "snapshot_id mismatch on playlist {playlist_id}: stored `{stored}` but \
         Spotify currently reports `{current}`. Pass --force to override."
    )]
    SnapshotMismatch {
        playlist_id: String,
        stored: String,
        current: String,
    },

    #[error("missing pre_state for reversible op {operation_id}")]
    MissingPreState { operation_id: String },

    #[error("missing reversal plan for reversible op {operation_id}")]
    MissingReversalPlan { operation_id: String },
}

/// Validate that an operation is eligible for undo. Pure logic — no
/// network, no SQL. Caller has already loaded the op from the store
/// and verified that the user wants to undo it.
pub fn validate_undoable(op: &Operation) -> Result<(), UndoError> {
    if !op.reversible {
        return Err(UndoError::NotReversible { kind: op.kind });
    }
    if op.status != OperationStatus::Succeeded {
        return Err(UndoError::NotInUndoableState {
            operation_id: op.operation_id.to_string(),
            status: op.status,
        });
    }
    if op.pre_state.is_none() {
        return Err(UndoError::MissingPreState {
            operation_id: op.operation_id.to_string(),
        });
    }
    if op.reversal_plan.is_none() {
        return Err(UndoError::MissingReversalPlan {
            operation_id: op.operation_id.to_string(),
        });
    }
    Ok(())
}

/// Returns the Spotify playlist id whose snapshot needs comparison
/// before this plan can run safely. `None` means the plan doesn't
/// reference a snapshot (queue/library/transport).
pub fn snapshot_check_target(plan: &ReversalPlan) -> Option<(&str, &str)> {
    match plan {
        ReversalPlan::PlaylistRemoveTracks {
            playlist_id,
            snapshot_id: Some(snap),
            ..
        }
        | ReversalPlan::PlaylistAddAtPositions {
            playlist_id,
            snapshot_id: Some(snap),
            ..
        }
        | ReversalPlan::PlaylistReorder {
            playlist_id,
            snapshot_id: Some(snap),
            ..
        } => Some((playlist_id.as_str(), snap.as_str())),
        _ => None,
    }
}

/// Reconciles the stored snapshot with a freshly-fetched current
/// snapshot. `force = true` short-circuits the check (the operator
/// explicitly accepted the drift). Returns `Ok(())` when safe to
/// proceed.
pub fn check_snapshot(
    plan: &ReversalPlan,
    fetch_current_snapshot: impl FnOnce(&str) -> Option<String>,
    force: bool,
) -> Result<(), UndoError> {
    let Some((playlist_id, stored)) = snapshot_check_target(plan) else {
        return Ok(());
    };
    if force {
        return Ok(());
    }
    let Some(current) = fetch_current_snapshot(playlist_id) else {
        // No current snapshot returned (playlist deleted / unauthorised).
        // Surface as a snapshot mismatch with the empty-current case so
        // the user gets a clear error.
        return Err(UndoError::SnapshotMismatch {
            playlist_id: playlist_id.to_string(),
            stored: stored.to_string(),
            current: String::new(),
        });
    };
    if current == stored {
        Ok(())
    } else {
        Err(UndoError::SnapshotMismatch {
            playlist_id: playlist_id.to_string(),
            stored: stored.to_string(),
            current,
        })
    }
}

/// Plain-text summary of what `ops undo` would do for this plan.
/// Surfaced via `ops show --diff` and `ops undo --dry-run`.
pub fn render_plan_summary(plan: &ReversalPlan, pre: &PreState) -> String {
    match (plan, pre) {
        (
            ReversalPlan::PlaylistRemoveTracks {
                playlist_id, uris, ..
            },
            _,
        ) => format!(
            "Remove {} track(s) from playlist {}",
            uris.len(),
            playlist_id
        ),
        (
            ReversalPlan::PlaylistAddAtPositions {
                playlist_id, items, ..
            },
            _,
        ) => format!(
            "Re-insert {} track(s) into playlist {}",
            items.len(),
            playlist_id
        ),
        (ReversalPlan::PlaylistDelete { playlist_id }, _) => {
            format!("Unfollow / delete playlist {playlist_id}")
        }
        (
            ReversalPlan::PlaylistReorder {
                playlist_id,
                range_start,
                insert_before,
                range_length,
                ..
            },
            _,
        ) => format!(
            "Reverse playlist {playlist_id} reorder ({range_start}..+{range_length} → before {insert_before})"
        ),
        (ReversalPlan::LibraryUnsave { uri }, _) => format!("Unsave {uri} from library"),
        (ReversalPlan::LibrarySave { uri, .. }, _) => format!("Re-save {uri} to library"),
        (ReversalPlan::TransferToPriorDevice { device_id }, _) => {
            format!("Transfer playback back to device {device_id}")
        }
        (ReversalPlan::Like { uri }, _) => format!("Like {uri}"),
        (ReversalPlan::Unlike { uri }, _) => format!("Unlike {uri}"),
        (ReversalPlan::QueueRemove { uri }, _) => {
            format!(
                "Cannot remove queued {uri}: Spotify has no queue-remove endpoint \
                 (legacy plan; queue adds are no longer recorded as reversible)"
            )
        }
        (ReversalPlan::Redo { target_op_id }, _) => {
            format!("Re-execute the forward action of operation {target_op_id}")
        }
        (ReversalPlan::NotReversible { reason }, _) => {
            format!("Not reversible — {reason}")
        }
    }
}

/// Build the operation row that records a committed undo. The caller
/// provides the id so `ResponseData::OperationUndoResult.undo_op_id`
/// can match the row persisted to SQLite.
pub(crate) fn undo_operation_row(
    undo_op_id: OperationId,
    original: &Operation,
    source: OperationSource,
    occurred_at_ms: i64,
) -> Operation {
    Operation {
        operation_id: undo_op_id,
        kind: OperationKind::Undo,
        occurred_at_ms,
        finished_at_ms: Some(occurred_at_ms),
        source,
        requester: None,
        subject_uris: original.subject_uris.clone(),
        reversible: true,
        reversal_plan: Some(ReversalPlan::Redo {
            target_op_id: original.operation_id,
        }),
        pre_state: None,
        status: OperationStatus::Succeeded,
        receipt_id: None,
        subject_op_id: Some(original.operation_id),
        undone_by_op_id: None,
        redone_by_op_id: None,
        error_message: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn op_succeeded_with(plan: ReversalPlan, pre: PreState) -> Operation {
        Operation {
            operation_id: OperationId::new_v7(),
            kind: OperationKind::PlaylistAdd,
            occurred_at_ms: 0,
            finished_at_ms: Some(0),
            source: OperationSource::Cli,
            requester: None,
            subject_uris: vec![],
            reversible: true,
            reversal_plan: Some(plan),
            pre_state: Some(pre),
            status: OperationStatus::Succeeded,
            receipt_id: None,
            subject_op_id: None,
            undone_by_op_id: None,
            redone_by_op_id: None,
            error_message: None,
        }
    }

    fn snapshot_mismatch(err: UndoError) -> Result<(String, String, String), UndoError> {
        match err {
            UndoError::SnapshotMismatch {
                playlist_id,
                stored,
                current,
            } => Ok((playlist_id, stored, current)),
            other => Err(other),
        }
    }

    #[test]
    fn transport_kind_is_not_undoable() {
        let mut op = op_succeeded_with(
            ReversalPlan::NotReversible {
                reason: "transport".into(),
            },
            PreState::Transport,
        );
        op.reversible = false;
        op.kind = OperationKind::Pause;
        let err = validate_undoable(&op).expect_err("transport operation should not be undoable");
        assert!(matches!(
            err,
            UndoError::NotReversible {
                kind: OperationKind::Pause
            }
        ));
    }

    #[test]
    fn pending_op_is_not_undoable() {
        let mut op = op_succeeded_with(
            ReversalPlan::QueueRemove {
                uri: "spotify:track:1".into(),
            },
            PreState::QueueAdd {
                uri: "spotify:track:1".into(),
            },
        );
        op.status = OperationStatus::Pending;
        let err = validate_undoable(&op).expect_err("pending operation should not be undoable");
        assert!(matches!(err, UndoError::NotInUndoableState { .. }));
    }

    #[test]
    fn already_undone_op_is_not_undoable() {
        let mut op = op_succeeded_with(
            ReversalPlan::QueueRemove {
                uri: "spotify:track:1".into(),
            },
            PreState::QueueAdd {
                uri: "spotify:track:1".into(),
            },
        );
        op.status = OperationStatus::Undone;
        let err =
            validate_undoable(&op).expect_err("already undone operation should not be undoable");
        assert!(matches!(err, UndoError::NotInUndoableState { .. }));
    }

    #[test]
    fn snapshot_check_returns_target_for_playlist_plans() {
        let plan = ReversalPlan::PlaylistRemoveTracks {
            playlist_id: "list-1".into(),
            uris: vec!["spotify:track:1".into()],
            snapshot_id: Some("snap-A".into()),
        };
        assert_eq!(snapshot_check_target(&plan), Some(("list-1", "snap-A")));
    }

    #[test]
    fn snapshot_check_skips_when_plan_has_no_snapshot() {
        let plan = ReversalPlan::QueueRemove {
            uri: "spotify:track:1".into(),
        };
        assert_eq!(snapshot_check_target(&plan), None);
        assert!(check_snapshot(&plan, |_| Some("snap".into()), false).is_ok());
    }

    #[test]
    fn matching_snapshot_passes_check() {
        let plan = ReversalPlan::PlaylistRemoveTracks {
            playlist_id: "list-1".into(),
            uris: vec![],
            snapshot_id: Some("snap-A".into()),
        };
        assert!(check_snapshot(&plan, |_| Some("snap-A".into()), false).is_ok());
    }

    #[test]
    fn mismatched_snapshot_errors_without_force() {
        let plan = ReversalPlan::PlaylistRemoveTracks {
            playlist_id: "list-1".into(),
            uris: vec![],
            snapshot_id: Some("snap-A".into()),
        };
        let err = check_snapshot(&plan, |_| Some("snap-B".into()), false)
            .expect_err("mismatched snapshot should error");
        let (playlist_id, stored, current) =
            snapshot_mismatch(err).expect("error should be SnapshotMismatch");
        assert_eq!(playlist_id, "list-1");
        assert_eq!(stored, "snap-A");
        assert_eq!(current, "snap-B");
    }

    #[test]
    fn force_skips_snapshot_check() {
        let plan = ReversalPlan::PlaylistRemoveTracks {
            playlist_id: "list-1".into(),
            uris: vec![],
            snapshot_id: Some("snap-A".into()),
        };
        assert!(check_snapshot(&plan, |_| Some("snap-B".into()), true).is_ok());
    }

    #[test]
    fn deleted_playlist_returns_mismatch_with_empty_current() {
        let plan = ReversalPlan::PlaylistRemoveTracks {
            playlist_id: "list-1".into(),
            uris: vec![],
            snapshot_id: Some("snap-A".into()),
        };
        let err = check_snapshot(&plan, |_| None, false)
            .expect_err("deleted playlist should mismatch snapshot");
        assert!(matches!(err, UndoError::SnapshotMismatch { current, .. } if current.is_empty()));
    }

    #[test]
    fn plan_summary_renders_each_reversible_variant() {
        let cases = [
            ReversalPlan::QueueRemove {
                uri: "spotify:track:1".into(),
            },
            ReversalPlan::PlaylistRemoveTracks {
                playlist_id: "list-1".into(),
                uris: vec!["spotify:track:1".into()],
                snapshot_id: Some("s".into()),
            },
            ReversalPlan::PlaylistAddAtPositions {
                playlist_id: "list-1".into(),
                items: vec![("spotify:track:1".into(), 0)],
                snapshot_id: Some("s".into()),
            },
            ReversalPlan::PlaylistDelete {
                playlist_id: "list-1".into(),
            },
            ReversalPlan::PlaylistReorder {
                playlist_id: "list-1".into(),
                range_start: 0,
                insert_before: 5,
                range_length: 1,
                snapshot_id: Some("s".into()),
            },
            ReversalPlan::LibraryUnsave {
                uri: "spotify:album:1".into(),
            },
            ReversalPlan::LibrarySave {
                uri: "spotify:album:1".into(),
                prior_added_at_ms: None,
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
        ];
        let dummy = PreState::Transport;
        for plan in &cases {
            let s = render_plan_summary(plan, &dummy);
            assert!(!s.is_empty(), "plan {plan:?} rendered empty summary");
        }
    }

    #[test]
    fn undo_operation_row_uses_caller_provided_id() {
        let original = op_succeeded_with(
            ReversalPlan::QueueRemove {
                uri: "spotify:track:1".into(),
            },
            PreState::QueueAdd {
                uri: "spotify:track:1".into(),
            },
        );
        let undo_id = OperationId::new_v7();

        let row = undo_operation_row(undo_id, &original, OperationSource::Mcp, 123);

        assert_eq!(row.operation_id, undo_id);
        assert_eq!(row.source, OperationSource::Mcp);
        assert_eq!(row.subject_op_id, Some(original.operation_id));
        assert_eq!(
            row.reversal_plan,
            Some(ReversalPlan::Redo {
                target_op_id: original.operation_id
            })
        );
    }
}
