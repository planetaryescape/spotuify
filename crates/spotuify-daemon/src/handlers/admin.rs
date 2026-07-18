//! `admin` request handlers (split out of the dispatch god-function).

use std::sync::Arc;

use spotuify_core::{now_ms, ClientPreferences, ProviderError};
use spotuify_protocol::{DaemonEvent, OperationSource, Request, ResponseData, SyncTargetData};

use crate::handler::*;
use crate::state::DaemonState;

pub(crate) async fn dispatch(
    state: Arc<DaemonState>,
    request: Request,
    _source: Option<OperationSource>,
) -> anyhow::Result<ResponseData> {
    match request {
        Request::Ping => Ok(ResponseData::Pong),
        Request::SubscribeEvents { .. } => Ok(ResponseData::Ack {
            message: "subscribed to daemon events".to_string(),
        }),
        Request::GetDaemonStatus => Ok(ResponseData::DaemonStatus {
            status: state.status(),
        }),
        Request::ProvidersList => {
            let catalog = state.providers().await?.catalog();
            Ok(ResponseData::ProviderList {
                default_provider: catalog.default_provider,
                providers: catalog.providers,
            })
        }
        Request::ResolveTarget {
            input,
            provider,
            expected_kinds,
        } => Ok(ResponseData::TargetResolved {
            target: state.providers().await?.resolve_target(
                &input,
                provider.as_ref(),
                expected_kinds.as_deref(),
            )?,
        }),
        Request::ListAudioOutputs => {
            let outputs = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                tokio::task::spawn_blocking(crate::server::list_audio_outputs),
            )
            .await
            .map_err(|_| anyhow::anyhow!("audio output enumeration timed out"))??;
            Ok(ResponseData::AudioOutputs {
                outputs,
                selected: state.accepted_player_settings().audio_output_device,
            })
        }
        Request::GetDoctorReport => Ok(ResponseData::DoctorReport {
            report: daemon_doctor_report(state.clone()).await?,
        }),
        Request::ClientSeed => {
            let catalog = state.providers().await?.catalog();
            let default_provider = catalog.default_provider.clone();
            let snapshot_provider = current_snapshot_provider_id(&state).await?;
            let playback = state.snapshot_playback();
            let queue = state.queue_snapshot_for_clients(
                state
                    .store()
                    .latest_provider_queue(500, &snapshot_provider)
                    .await?
                    .unwrap_or_default(),
            );
            let devices = cached_devices_with_own_device(&state, &snapshot_provider).await?;
            let recent = state
                .store()
                .list_provider_recent_items(20, default_provider.as_ref())
                .await?;
            let viz = state.viz_coordinator().diagnostics().await;
            let preferences = ClientPreferences {
                viz_color_scheme: Some(spotuify_config::load()?.config.viz.color_scheme),
            };
            Ok(ResponseData::ClientSeed {
                playback,
                queue,
                devices,
                recent,
                viz,
                provider_catalog: Some(catalog),
                preferences: Some(preferences),
                provider_policies: state.active_provider_policies(),
            })
        }
        Request::Reindex => {
            let store = state.store().clone();
            let search = state.search().clone();
            Ok(ResponseData::Reindex {
                stats: spotuify_search::reindex::reindex(&store, &search).await?,
            })
        }
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
        Request::Sync { target, provider } => {
            let providers = state.providers().await?;
            let runtime = providers.provider_or_default(provider.as_ref())?;
            let provider_id = runtime.id().clone();
            let available_transport = match runtime.transport() {
                Ok(transport) => Some(transport),
                Err(ProviderError::Unsupported { .. }) => None,
                Err(err) => return Err(err.into()),
            };
            let selected = state
                .active_transport_provider()
                .unwrap_or_else(|| providers.default_id().clone());
            let direct_transport_target = matches!(
                target,
                SyncTargetData::Playback | SyncTargetData::Queue | SyncTargetData::Devices
            );
            if direct_transport_target && available_transport.is_none() {
                return Err(ProviderError::unsupported(format!(
                    "provider `{provider_id}` transport sync"
                ))
                .into());
            }
            if direct_transport_target && provider_id != selected {
                return Err(ProviderError::InvalidInput {
                    field: "provider".to_string(),
                    message: format!(
                        "transport sync provider `{provider_id}` is inactive; active provider is `{selected}`"
                    ),
                }
                .into());
            }
            // `All` remains useful for an inactive secondary: sync its
            // playlists/library/recent catalog while leaving the daemon's one
            // global transport view untouched. Direct transport targets stay
            // strict and actionable above.
            let transport = if provider_id == selected {
                available_transport
            } else {
                None
            };
            let sync_provider = spotuify_sync::SyncProvider::new(runtime.music(), transport)?;
            state.emit_event(DaemonEvent::SyncStarted {
                target,
                provider: Some(provider_id.clone()),
            });
            let result =
                spotuify_sync::sync_provider_target_bounded(state.clone(), sync_provider, target)
                    .await;
            let summary = match result {
                Ok(summary) => summary,
                Err(err) => {
                    state.emit_event(DaemonEvent::SyncFinished {
                        summary: spotuify_protocol::CacheSyncSummary {
                            target,
                            provider: Some(provider_id),
                            playback_snapshots: 0,
                            queue_snapshots: 0,
                            queue_items: 0,
                            devices: 0,
                            playlists: 0,
                            playlist_items: 0,
                            recent_items: 0,
                            library_items: 0,
                            media_items: 0,
                            status: spotuify_protocol::SyncCompletionStatus::Failed,
                            error: Some(err.to_string()),
                            provider_outcomes: Vec::new(),
                        },
                    });
                    return Err(err);
                }
            };
            state.emit_event(DaemonEvent::SyncFinished {
                summary: summary.clone(),
            });
            Ok(ResponseData::Sync { summary })
        }
        Request::Shutdown => {
            state.request_shutdown();
            Ok(ResponseData::Shutdown)
        }

        // Phase 10 (P10.6) analytics dispatch.
        Request::Reload => match spotuify_config::load() {
            Ok(loaded) => {
                state.apply_runtime_config(&loaded.config).await?;
                state.emit_event(DaemonEvent::ConfigReloaded);
                Ok(ResponseData::Ack {
                    message: "config reloaded; runtime viz settings applied".to_string(),
                })
            }
            Err(err) => anyhow::bail!("reload failed: {err}"),
        },
        Request::ReloadAuth => {
            tracing::info!("daemon reload-auth requested");
            let target = state.configured_health_auth_target().await?;
            let _auth_operation = state.auth_sessions().operation_guard().await;
            state.reload_auth(Some(&target.provider_id)).await?;
            Ok(ResponseData::Ack {
                message: "auth reloaded".to_string(),
            })
        }
        Request::AuthStart { provider, method } => Ok(ResponseData::AuthSession {
            session: state
                .auth_sessions()
                .start(
                    state.clone(),
                    provider.map(|provider| provider.to_string()),
                    method,
                )
                .await?,
        }),
        Request::AuthPoll { session_id } => Ok(ResponseData::AuthSession {
            session: state.auth_sessions().poll(session_id).await?,
        }),
        Request::AuthCancel { session_id } => Ok(ResponseData::AuthSession {
            session: state.auth_sessions().cancel(session_id).await?,
        }),
        Request::AuthStatus { provider } => Ok(ResponseData::AuthStatus {
            status: state
                .auth_sessions()
                .status(state.clone(), provider)
                .await?,
        }),
        Request::AuthLogout { provider } => Ok(ResponseData::AuthLogout {
            result: state
                .auth_sessions()
                .logout(state.clone(), provider)
                .await?,
        }),
        Request::WebApiToken { force } => {
            let _auth_operation = state.auth_sessions().operation_guard().await;
            Ok(ResponseData::WebApiToken {
                token: state.web_api_bearer(force).await,
            })
        }
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

fn daemon_doctor_report(
    state: Arc<DaemonState>,
) -> futures::future::BoxFuture<'static, anyhow::Result<spotuify_protocol::DoctorReport>> {
    use futures::FutureExt;

    async move {
        // Pass the daemon's recent-event snapshot so the report includes
        // rate-limit, auth, and schema-compat findings.
        let mut report = crate::diagnostics::collect_report_with_events(
            state.status(),
            state.event_log_snapshot().await,
            Some(state.providers().await?),
            Some(state.store().clone()),
        )
        .await?;
        report.system = Some(state.system_integration.diagnostics());
        report.viz = Some(state.viz_coordinator().diagnostics().await);

        // Surface a zombie player session the health loop is recovering.
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
        Ok(report)
    }
    .boxed()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs::OpenOptions;
    use std::sync::Arc;
    use std::time::Duration;

    use fs2::FileExt;
    use spotuify_core::ProviderId;
    use spotuify_protocol::{AuthSessionState, AuthStrategyData, Request, ResponseData};
    use spotuify_provider_fake::FakeProvider;
    use spotuify_spotify::auth::StoredToken;

    use crate::provider_registry::{ProviderRegistry, ProviderRuntime};
    use crate::state::DaemonState;

    struct TestEnv {
        _temp: tempfile::TempDir,
        old_values: Vec<(&'static str, Option<OsString>)>,
    }

    impl TestEnv {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("temp directory");
            let old_values = [
                "SPOTUIFY_CACHE_DB",
                "SPOTUIFY_ANALYTICS_DB",
                "SPOTUIFY_SEARCH_INDEX",
                "SPOTUIFY_RUNTIME_DIR",
                "SPOTUIFY_DATA_DIR",
                "SPOTUIFY_CACHE_DIR",
                "SPOTUIFY_CONFIG_DIR",
                "SPOTUIFY_CONFIG",
                "SPOTUIFY_FAKE_SPOTIFY",
            ]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect();
            std::env::set_var("SPOTUIFY_CACHE_DB", temp.path().join("cache.sqlite3"));
            std::env::set_var(
                "SPOTUIFY_ANALYTICS_DB",
                temp.path().join("analytics.sqlite3"),
            );
            std::env::set_var("SPOTUIFY_SEARCH_INDEX", temp.path().join("search-index"));
            std::env::set_var("SPOTUIFY_RUNTIME_DIR", temp.path().join("runtime"));
            std::env::set_var("SPOTUIFY_DATA_DIR", temp.path().join("data"));
            std::env::set_var("SPOTUIFY_CACHE_DIR", temp.path().join("cache"));
            std::env::set_var("SPOTUIFY_CONFIG_DIR", temp.path().join("config"));
            std::env::set_var("SPOTUIFY_CONFIG", temp.path().join("spotuify.toml"));
            std::env::remove_var("SPOTUIFY_FAKE_SPOTIFY");
            Self {
                _temp: temp,
                old_values,
            }
        }

        fn use_spotify_auth_config(&self) {
            std::fs::write(
                self._temp.path().join("spotuify.toml"),
                r#"
[providers]
default = "spotify"

[providers.spotify]
type = "spotify"
client_id = "test-client"
redirect_uri = "http://127.0.0.1:8888/callback"
"#,
            )
            .expect("write isolated Spotify config");
        }
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            for (key, value) in &self.old_values {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[tokio::test]
    async fn typed_provider_ids_route_auth_status_and_logout() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(FakeProvider::isolated("custom-cloud").expect("valid fake"));
        let runtime = ProviderRuntime::with_transport(provider).expect("valid runtime");
        let registry = ProviderRegistry::new(
            ProviderId::new("custom-cloud").expect("valid provider id"),
            [runtime],
        )
        .expect("valid registry");
        let state = Arc::new(
            DaemonState::new_with_providers(registry)
                .await
                .expect("daemon state"),
        );
        let requested = ProviderId::new("custom-cloud").expect("valid provider id");

        let started = super::dispatch(
            state.clone(),
            Request::AuthStart {
                provider: Some(requested.clone()),
                method: None,
            },
            None,
        )
        .await
        .expect("auth start");
        assert!(matches!(
            started,
            ResponseData::AuthSession { session }
                if session.provider.as_str() == "custom-cloud"
                    && session.state == AuthSessionState::Authorized
        ));

        let status = super::dispatch(
            state.clone(),
            Request::AuthStatus {
                provider: Some(requested.clone()),
            },
            None,
        )
        .await
        .expect("auth status");
        assert!(matches!(
            status,
            ResponseData::AuthStatus { status }
                if status.provider.as_str() == "custom-cloud" && status.strategy == AuthStrategyData::None
        ));

        let logout = super::dispatch(
            state.clone(),
            Request::AuthLogout {
                provider: Some(requested),
            },
            None,
        )
        .await
        .expect("auth logout");
        assert!(matches!(
            logout,
            ResponseData::AuthLogout { result }
                if result.provider.as_str() == "custom-cloud" && !result.auth_required
        ));

        let reloaded = super::dispatch(state.clone(), Request::ReloadAuth, None)
            .await
            .expect("no-auth provider reload");
        assert!(matches!(reloaded, ResponseData::Ack { .. }));

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn reload_auth_dispatch_waits_for_auth_operation_guard() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(FakeProvider::isolated("custom-cloud").expect("valid fake"));
        let runtime = ProviderRuntime::with_transport(provider).expect("valid runtime");
        let registry = ProviderRegistry::new(
            ProviderId::new("custom-cloud").expect("valid provider id"),
            [runtime],
        )
        .expect("valid registry");
        let state = Arc::new(
            DaemonState::new_with_providers(registry)
                .await
                .expect("daemon state"),
        );

        let auth_operation = state.auth_sessions().operation_guard().await;
        let reload_state = state.clone();
        let mut reload =
            tokio::spawn(
                async move { super::dispatch(reload_state, Request::ReloadAuth, None).await },
            );
        assert!(
            tokio::time::timeout(Duration::from_millis(250), &mut reload)
                .await
                .is_err(),
            "reload dispatch must wait for the auth-operation mutex"
        );

        drop(auth_operation);
        let response = tokio::time::timeout(Duration::from_secs(2), reload)
            .await
            .expect("reload completion timeout")
            .expect("reload task")
            .expect("reload response");
        assert!(matches!(response, ResponseData::Ack { .. }));

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn reload_auth_waits_for_logout_purge_before_reading_credentials() {
        let _guard = crate::ENV_LOCK.lock().await;
        let env = TestEnv::new();
        env.use_spotify_auth_config();
        spotuify_spotify::auth::save_dev_app_token_for(
            "spotify",
            &StoredToken {
                access_token: "access".to_string(),
                refresh_token: "refresh".to_string(),
                expires_at: 4_000_000_000,
                scope: "user-read-playback-state".to_string(),
                token_type: "Bearer".to_string(),
            },
        )
        .expect("seed credential");
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        let provider = ProviderId::new("spotify").expect("valid provider id");

        // Hold the on-disk credential lock so the real logout request reaches
        // its purge while still owning the daemon auth-operation mutex.
        let token_lock_path = spotuify_protocol::paths::config_dir()
            .join("auth")
            .join("spotify")
            .join("token.lock");
        let token_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(token_lock_path)
            .expect("open token lock");
        token_lock.lock_exclusive().expect("hold token lock");

        let logout_state = state.clone();
        let logout_provider = provider.clone();
        let logout = tokio::spawn(async move {
            super::dispatch(
                logout_state,
                Request::AuthLogout {
                    provider: Some(logout_provider),
                },
                None,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !state.auth_sessions().operation_in_progress() {
                assert!(!logout.is_finished(), "logout finished before its purge");
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("logout did not acquire the auth-operation mutex");

        // Reload resolves its typed target first, then must wait behind the
        // in-flight logout until purge + finish_logout publish final state.
        let reload_state = state.clone();
        let mut reload =
            tokio::spawn(
                async move { super::dispatch(reload_state, Request::ReloadAuth, None).await },
            );
        assert!(
            tokio::time::timeout(Duration::from_millis(250), &mut reload)
                .await
                .is_err(),
            "reload must wait behind the in-flight logout operation"
        );

        FileExt::unlock(&token_lock).expect("release token lock");
        let logout_response = tokio::time::timeout(Duration::from_secs(8), logout)
            .await
            .expect("logout completion timeout")
            .expect("logout task")
            .expect("logout response");
        assert!(matches!(
            logout_response,
            ResponseData::AuthLogout { result }
                if result.provider == provider && result.removed_dev_app && result.auth_required
        ));

        let response = tokio::time::timeout(Duration::from_secs(8), reload)
            .await
            .expect("reload completion timeout")
            .expect("reload task")
            .expect("reload response");
        assert!(matches!(response, ResponseData::Ack { .. }));
        assert!(
            state.auth_required(),
            "post-logout reload must not publish stale authorized state"
        );
        let inventory = spotuify_spotify::auth::credential_inventory_for("spotify")
            .expect("credential inventory");
        assert!(inventory.dev_app.is_none() && inventory.first_party.is_none());

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn provider_catalog_remains_available_while_auth_is_required() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(FakeProvider::isolated("custom-cloud").expect("valid fake"));
        let runtime = ProviderRuntime::with_transport(provider).expect("valid runtime");
        let registry = ProviderRegistry::new(
            ProviderId::new("custom-cloud").expect("valid provider id"),
            [runtime],
        )
        .expect("valid registry");
        let state = Arc::new(
            DaemonState::new_with_providers(registry)
                .await
                .expect("daemon state"),
        );
        let auth_provider = ProviderId::new("spotify").expect("valid provider id");
        state.mark_auth_required(Some(&auth_provider)).await;

        let response = super::dispatch(state.clone(), Request::ProvidersList, None)
            .await
            .expect("provider discovery must not require provider auth");
        assert!(matches!(
            response,
            ResponseData::ProviderList { default_provider, providers }
                if default_provider.as_ref().map(ProviderId::as_str) == Some("custom-cloud")
                    && providers.iter().any(|provider| provider.id.as_str() == "custom-cloud")
        ));

        state.shutdown_search().await;
        state.shutdown_player().await;
    }
}
