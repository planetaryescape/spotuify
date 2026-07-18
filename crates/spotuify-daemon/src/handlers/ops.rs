//! `ops` request handlers (split out of the dispatch god-function).

use std::sync::Arc;

use spotuify_protocol::{MutationId, OperationSource, Request, ResponseData};

use crate::handler::*;
use crate::state::DaemonState;

pub(crate) async fn dispatch(
    state: Arc<DaemonState>,
    request: Request,
    source: Option<OperationSource>,
    mutation_id: Option<MutationId>,
) -> anyhow::Result<ResponseData> {
    let operation_source = source.unwrap_or(OperationSource::DaemonInternal);
    let mutation_lane = state.mutation_lane(&request).await;
    match request {
        Request::OpsLog {
            limit,
            since_ms,
            source,
        } => Ok(ResponseData::Operations {
            ops: state
                .store()
                .list_operations(limit, since_ms, source)
                .await?,
        }),
        Request::OpsShow {
            operation_id,
            with_diff,
        } => {
            let op = state.store().get_operation(operation_id).await?;
            let diff = if with_diff {
                op.reversal_plan
                    .as_ref()
                    .zip(op.pre_state.as_ref())
                    .map(|(plan, pre)| crate::undo::render_plan_summary(plan, pre))
            } else {
                None
            };
            Ok(ResponseData::OperationDetail { op, diff })
        }
        Request::OpsUndo {
            operation_id,
            dry_run,
            force,
            bulk_since_ms,
        } => {
            let _mutation_guard = match mutation_lane {
                Some(lane) => Some(lane.lock_owned().await),
                None => None,
            };
            handle_ops_undo(
                &state,
                operation_id,
                operation_source,
                dry_run,
                force,
                bulk_since_ms,
                mutation_id,
            )
            .await
        }
        Request::OpsRedo { operation_id } => {
            let _mutation_guard = match mutation_lane {
                Some(lane) => Some(lane.lock_owned().await),
                None => None,
            };
            handle_ops_redo(&state, operation_id, operation_source, mutation_id).await
        }

        // --- Phase 13 — QoL / spec-compliance handlers ---
        _ => unreachable!("non-ops request routed to ops dispatcher"),
    }
}
