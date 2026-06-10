//! `analytics` request handlers (split out of the dispatch god-function).

use std::sync::Arc;

use spotuify_core::now_ms;
use spotuify_protocol::{OperationSource, Request, ResponseData};
use spotuify_spotify::config::Config;

use crate::analytics::AnalyticsStore;
use crate::handler::*;
use crate::retention::retention_cutoffs;
use crate::state::DaemonState;

pub(crate) async fn dispatch(
    state: Arc<DaemonState>,
    request: Request,
    _source: Option<OperationSource>,
) -> anyhow::Result<ResponseData> {
    match request {
        Request::ListenSessions { limit } => {
            // Cache-backed; refresh recently-played in the background so the
            // merged history picks up other-device plays next time.
            let sessions = state.store().list_listen_sessions(limit).await?;
            spawn_recent_refresh(state.clone());
            Ok(ResponseData::ListenSessions { sessions })
        }
        Request::AnalyticsRebuild { since_ms } => Ok(ResponseData::AnalyticsRebuildReport {
            report: state
                .store()
                .rebuild_derivations_from_events(since_ms)
                .await?,
        }),
        Request::AnalyticsTop {
            kind,
            since_window,
            limit,
        } => Ok(ResponseData::AnalyticsTop {
            entries: state.store().top_entries(kind, since_window, limit).await?,
        }),
        Request::AnalyticsHabits { window, since_ms } => Ok(ResponseData::AnalyticsHabits {
            buckets: state.store().habit_buckets(window, since_ms).await?,
        }),
        Request::AnalyticsSearch { mode, limit } => Ok(ResponseData::AnalyticsSearch {
            entries: state
                .store()
                .search_history(
                    matches!(mode, spotuify_protocol::SearchMode::Normalized),
                    limit,
                )
                .await?,
        }),
        Request::AnalyticsRediscovery { gap_days } => Ok(ResponseData::AnalyticsRediscovery {
            candidates: state.store().rediscovery_candidates(gap_days, 50).await?,
        }),
        Request::AnalyticsPrune { apply } => {
            // Prune raw playback_progress (90d) + analytics_events (365d)
            // + operations (90d) older than the configured retention
            // windows. Dry-run by default. Read the windows from config
            // when available; fall back to blueprint defaults.
            let now = now_ms();
            let analytics = Config::load().ok().map(|config| config.analytics);
            let cutoffs = retention_cutoffs(now, analytics.as_ref());

            if !apply {
                // Dry-run: count rows that *would* be deleted via
                // COUNT() rather than DELETE. Best-effort: errors here
                // fall back to zero so the daemon never panics from a
                // diagnostic query.
                let count_progress: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM playback_progress WHERE sampled_at_ms < ?",
                )
                .bind(cutoffs.progress_ms)
                .fetch_one(state.store().reader())
                .await
                .unwrap_or(0);
                let count_events = match AnalyticsStore::open_default().await {
                    Ok(store) => store
                        .count_events_older_than(cutoffs.events_ms)
                        .await
                        .unwrap_or(0),
                    Err(_) => 0,
                };
                let count_ops: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM operations WHERE occurred_at_ms < ?")
                        .bind(cutoffs.operations_ms)
                        .fetch_one(state.store().reader())
                        .await
                        .unwrap_or(0);
                return Ok(ResponseData::AnalyticsPruneReport {
                    rows_pruned: count_progress.max(0) as u64
                        + count_events
                        + count_ops.max(0) as u64,
                    dry_run: true,
                });
            }

            let pruned_progress = state
                .store()
                .prune_playback_progress(cutoffs.progress_ms)
                .await
                .unwrap_or(0);
            let pruned_events = match AnalyticsStore::open_default().await {
                Ok(store) => store
                    .prune_events_older_than(cutoffs.events_ms)
                    .await
                    .unwrap_or(0),
                Err(_) => 0,
            };
            let pruned_ops = state
                .store()
                .prune_operations_older_than(cutoffs.operations_ms)
                .await
                .unwrap_or(0);
            Ok(ResponseData::AnalyticsPruneReport {
                rows_pruned: pruned_progress + pruned_events + pruned_ops,
                dry_run: false,
            })
        }
        _ => unreachable!("non-analytics request routed to analytics dispatcher"),
    }
}
