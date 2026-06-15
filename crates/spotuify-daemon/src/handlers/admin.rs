//! `admin` request handlers (split out of the dispatch god-function).

use std::sync::Arc;

use spotuify_core::now_ms;
use spotuify_protocol::{DaemonEvent, OperationSource, Request, ResponseData};

use crate::handler::*;
use crate::state::DaemonState;

pub(crate) async fn dispatch(
    state: Arc<DaemonState>,
    request: Request,
    _source: Option<OperationSource>,
) -> anyhow::Result<ResponseData> {
    match request {
        Request::Ping => Ok(ResponseData::Pong),
        Request::SubscribeEvents => Ok(ResponseData::Ack {
            message: "subscribed to daemon events".to_string(),
        }),
        Request::GetDaemonStatus => Ok(ResponseData::DaemonStatus {
            status: state.status(),
        }),
        Request::GetDoctorReport => Ok(ResponseData::DoctorReport {
            // Phase 6.9: pass the daemon's recent-event snapshot so the
            // report includes RateLimited / AuthError / SchemaCompat
            // findings.
            report: {
                // Mint the first-party bearer directly (no IPC) so the
                // live API checks can run; the daemon holds the session.
                let bearer = state.web_api_bearer(false).await;
                let mut report = crate::diagnostics::collect_report_with_events(
                    state.status(),
                    state.event_log_snapshot().await,
                    bearer,
                )
                .await?;
                report.system = Some(state.system_integration.diagnostics());
                report.viz = Some(state.viz_coordinator().diagnostics().await);
                // Phase: surface a zombie player session the health loop is
                // working to recover. Warning severity (a reconnecting
                // session is degraded, not fatal) so `healthy` stays as
                // build_findings computed it.
                let health = state.player_health_snapshot();
                if !health.connected && state.is_we_are_active() {
                    let remediation = if health.gave_up {
                        vec!["spotuify reconnect".to_string()]
                    } else {
                        Vec::new()
                    };
                    report.findings.push(spotuify_protocol::DoctorFinding {
                        category: spotuify_protocol::DoctorFindingCategory::Device,
                        severity: spotuify_protocol::DoctorFindingSeverity::Warning,
                        message: if health.gave_up {
                            format!(
                                "player session is down after {} reconnect attempts; \
                                 run `spotuify reconnect` or play something to re-register",
                                health.consecutive_failures
                            )
                        } else {
                            "player session is down; the daemon is auto-reconnecting".to_string()
                        },
                        remediation,
                    });
                }
                // (Audio-flow findings now live in diagnostics::build_findings,
                // sourced from DaemonStatus.audio_health, so they show on the
                // local `doctor` path too — not just this daemon handler.)
                report
            },
        }),
        Request::ClientSeed => {
            let playback = state.snapshot_playback();
            let queue = state.store().latest_queue(500).await?.unwrap_or_default();
            let devices = cached_devices_with_own_device(&state).await?;
            let recent = state.store().list_recent_items(20).await?;
            let viz = state.viz_coordinator().diagnostics().await;
            Ok(ResponseData::ClientSeed {
                playback,
                queue,
                devices,
                recent,
                viz,
            })
        }
        Request::Reindex => Ok(ResponseData::Reindex {
            stats: spotuify_search::reindex::reindex(state.store(), state.search()).await?,
        }),
        Request::CacheStatus => {
            let index_documents = state.search().num_docs().await.unwrap_or(0);
            let mut status = state.store().cache_status(index_documents).await?;
            match state.system_integration.cover_cache.stats() {
                Ok(stats) => {
                    status.cover_cache_path = stats.root.display().to_string();
                    status.cover_cache_files = stats.files;
                    status.cover_cache_bytes = stats.bytes;
                    status.cover_cache_oldest_entry_ms = stats.oldest_entry_ms;
                    status.cover_cache_ttl_secs = stats.ttl_secs;
                    status.cover_cache_max_bytes = stats.max_bytes;
                }
                Err(err) => tracing::warn!(error = %err, "cover cache stats unavailable"),
            }
            Ok(ResponseData::CacheStatus { status })
        }
        Request::LogsTail { lines } => Ok(ResponseData::Logs {
            lines: crate::logging::read_tail(lines)?
                .lines()
                .map(ToString::to_string)
                .collect(),
        }),
        Request::Sync { target } => Ok(ResponseData::Sync {
            summary: spotuify_sync::sync_target(state.as_ref(), target).await?,
        }),
        Request::Shutdown => {
            state.request_shutdown();
            Ok(ResponseData::Shutdown)
        }

        // Phase 10 (P10.6) analytics dispatch.
        Request::Reload => match spotuify_spotify::config::Config::load() {
            Ok(config) => {
                state.apply_runtime_config(&config).await;
                state.emit_event(DaemonEvent::ConfigReloaded);
                Ok(ResponseData::Ack {
                    message: "config reloaded; runtime viz settings applied".to_string(),
                })
            }
            Err(err) => anyhow::bail!("reload failed: {err}"),
        },
        Request::ReloadAuth => {
            tracing::info!("daemon reload-auth requested");
            state.reload_auth().await;
            Ok(ResponseData::Ack {
                message: "auth reloaded".to_string(),
            })
        }
        Request::WebApiToken { force } => Ok(ResponseData::WebApiToken {
            token: state.web_api_bearer(force).await,
        }),
        Request::CheckUpdate { force } => {
            let current = crate::update::current_version().to_string();
            let now = now_ms();
            // Six-hour freshness window mirrors the background loop's cadence.
            let stale = state
                .cached_release()
                .map(|r| now - r.checked_at_ms > 6 * 3_600_000)
                .unwrap_or(true);
            if force {
                // Block on a fresh check so `update --force` reflects reality.
                crate::server::run_update_check_once(&state).await;
            } else if stale {
                // Warm the cache in the background; return whatever we have now.
                let bg = state.clone();
                state.spawn_background("update-check", async move {
                    crate::server::run_update_check_once(&bg).await;
                });
            }
            let cached = state.cached_release();
            let latest_version = cached.as_ref().map(|r| r.latest_version.clone());
            let release_url = cached.as_ref().and_then(|r| r.release_url.clone());
            let checked_at_ms = cached.as_ref().map(|r| r.checked_at_ms);
            let update_available = latest_version
                .as_deref()
                .map(|latest| crate::update::is_newer(&current, latest))
                .unwrap_or(false);
            let method = crate::update::detect_upgrade_method(&crate::update::current_exe_path());
            let upgrade = crate::update::upgrade_hint(
                method,
                latest_version.as_deref().unwrap_or(&current),
                release_url.as_deref(),
            );
            Ok(ResponseData::UpdateStatus {
                update_available,
                current_version: current,
                latest_version,
                release_url,
                upgrade,
                checked_at_ms,
            })
        }
        Request::SearchCachePrune { older_than_ms } => {
            let cutoff = older_than_ms.unwrap_or_else(|| now_ms() - 30 * 86_400_000);
            let pruned_runs = state
                .store()
                .prune_search_runs_older_than(cutoff)
                .await
                .unwrap_or(0);
            Ok(ResponseData::SearchCachePruned {
                pruned_runs,
                pruned_results: 0,
            })
        }
        _ => unreachable!("non-admin request routed to admin dispatcher"),
    }
}
