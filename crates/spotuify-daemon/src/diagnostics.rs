use std::future::Future;
use std::time::{Duration, Instant};

use anyhow::Result;
use futures::FutureExt;
use serde::Deserialize;

use crate::logging;
use crate::provider_registry::ProviderRegistry;
use spotuify_core::{Device, PageRequest, RequestContext};
use spotuify_protocol::OutputFormat;
use spotuify_protocol::{
    DaemonStatus, DeviceDiagnostics, DeviceSummary, DoctorCheck, DoctorFinding,
    DoctorFindingCategory, DoctorFindingSeverity, DoctorReport, HealthClass,
};
use spotuify_store::Store;

const AUTH_CHECK_TIMEOUT: Duration = Duration::from_secs(3);
const API_CHECK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default, Deserialize)]
#[serde(default)]
struct SpotifyDiagnosticsConfig {
    client_id: Option<String>,
    client_secret: Option<String>,
    redirect_uri: Option<String>,
    player: spotuify_config::PlayerSettings,
}

fn redacted_client_id(client_id: &str) -> String {
    let len = client_id.chars().count();
    if len <= 8 {
        return "present".to_string();
    }
    let start: String = client_id.chars().take(4).collect();
    let end = client_id
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("{start}...{end}")
}

pub async fn collect_report(daemon: DaemonStatus) -> Result<DoctorReport> {
    collect_report_with_events(daemon, Vec::new(), None, None).await
}

/// Phase 6.9 — collect the doctor report and merge in findings derived
/// from a snapshot of the daemon's recent-event log (RateLimited,
/// AuthError, SchemaCompat). When called from outside the daemon
/// process (CLI doctor invocation talking over IPC), pass an empty
/// vector; the daemon-side collect call supplies its event_log_snapshot.
pub fn collect_report_with_events(
    daemon: DaemonStatus,
    recent_events: Vec<spotuify_protocol::LoggedEvent>,
    providers: Option<std::sync::Arc<ProviderRegistry>>,
    store: Option<Store>,
) -> futures::future::BoxFuture<'static, Result<DoctorReport>> {
    async move {
    let config_path = spotuify_config::config_path().map_or_else(
        |err| format!("unresolved: {err}"),
        |path| path.display().to_string(),
    );
    let logs_path = logging::log_path().map_or_else(
        |err| format!("unresolved: {err}"),
        |path| path.display().to_string(),
    );

    let config_result = spotuify_config::load().and_then(|loaded| {
        let spotify = loaded
            .config
            .providers
            .iter()
            .find(|provider| provider.kind == "spotify")
            .map(|provider| provider.deserialize::<SpotifyDiagnosticsConfig>())
            .transpose()?;
        Ok((loaded.config, spotify))
    });
    let (config_ok, config_error, config) = match config_result {
        Ok(config) => (true, None, Some(config)),
        Err(err) => (false, Some(err.to_string()), None),
    };
    let config_path = config
        .as_ref()
        .map_or(config_path, |(config, _)| config.path.display().to_string());

    let fake_spotify = std::env::var_os("SPOTUIFY_FAKE_SPOTIFY").is_some();
    let auth_provider = crate::provider_factory::ProviderFactory::configured_health_auth_target()
        .ok()
        .filter(|target| {
            target.strategy == crate::provider_factory::ProviderAuthStrategy::SpotifyOauth
        })
        .map(|target| target.provider_id.to_string());
    let auth_required = !fake_spotify
        && config
            .as_ref()
            .is_none_or(|_| auth_provider.as_ref().is_some());
    let auth_provider = auth_provider.as_deref().unwrap_or("spotify");
    let auth_token = if !auth_required {
        skipped_auth_check("skipped: no configured provider requires Spotify OAuth")
    } else {
        auth_file_check(auth_provider)
    };
    let disk_token_cache = DoctorCheck {
        name: "auth file".to_string(),
        ok: true,
        message: if auth_required {
            spotuify_spotify::auth::disk_token_cache_status_for(auth_provider)
        } else {
            "skipped: no configured provider requires Spotify OAuth".to_string()
        },
        elapsed_ms: 0,
    };
    // Phase 0 cleanup: spotifyd subprocess health check removed
    // (librespot-only architecture). The embedded backend's readiness
    // is surfaced through DaemonEvent::PlayerReady / PlayerFailed.

    let mut api_checks = Vec::new();
    let mut device_diagnostics_report = None;
        let first_party = auth_required
        && spotuify_spotify::config::first_party_env_override()
            .unwrap_or_else(|| spotuify_spotify::auth::stored_first_party_only_for(auth_provider));
    // Genuinely-stuck first-party-only state (no dev-app token on disk),
    // computed independent of whether the config loaded: a first-party-only
    // user with no BYO client_id fails `Config::load()`, so we cannot key
    // the migration finding on the loaded config the way `first_party` above
    // does.
    let first_party_only = auth_required && resolved_first_party_only(auth_provider);
    if let Some(providers) = providers {
        let runtime = spotuify_core::ProviderId::new(auth_provider)
            .ok()
            .and_then(|provider| providers.provider(&provider).ok())
            .unwrap_or_else(|| providers.default_provider());
        let provider = runtime.music();
        let capabilities = provider.capabilities();
        if let Ok(transport) = runtime.transport() {
            if capabilities
                .transport
                .as_ref()
                .is_some_and(|caps| caps.playback_state)
            {
                let (check, _, _) = timed_api(
                    "api playback",
                    transport.playback(RequestContext::BACKGROUND_SYNC),
                )
                .await;
                api_checks.push(check);
            }

            if capabilities
                .transport
                .as_ref()
                .is_some_and(|caps| caps.devices)
            {
                let (check, devices, _) = timed_api(
                    "api devices",
                    transport.devices(RequestContext::BACKGROUND_SYNC),
                )
                .await;
                api_checks.push(check);
                if let (Some((_, Some(config))), Some(devices)) = (config.as_ref(), devices) {
                    device_diagnostics_report = Some(device_diagnostics(&config.player, &devices));
                }
            }

            if capabilities
                .transport
                .as_ref()
                .is_some_and(|caps| caps.queue_read)
            {
                let (check, _, _) = timed_api(
                    "api queue",
                    transport.queue(RequestContext::BACKGROUND_SYNC),
                )
                .await;
                api_checks.push(check);
            }
        }

        if !capabilities.playlists.list {
            api_checks.push(skipped_capability_check("api playlists"));
        } else if let Some(check) = skipped_rate_limit_check(
            store.clone(),
            provider.id().to_string(),
            "api playlists",
            "playlists",
        )
        .await
        {
            api_checks.push(check);
        } else {
            let limit = provider
                .capabilities()
                .playlists
                .list_max_page_size
                .unwrap_or(50) as u32;
            let (check, _, retry_after_secs) = timed_api(
                "api playlists",
                provider.playlists(
                    RequestContext::BACKGROUND_SYNC,
                    PageRequest::new(limit.max(1), 0),
                ),
            )
            .await;
            record_api_rate_limit(
                store.clone(),
                provider.id().to_string(),
                "playlists",
                check.clone(),
                retry_after_secs,
            )
            .await;
            api_checks.push(check);
        }

        if !capabilities.catalog.recently_played {
            api_checks.push(skipped_capability_check("api recently played"));
        } else if let Some(check) = skipped_rate_limit_check(
            store.clone(),
            provider.id().to_string(),
            "api recently played",
            "recent",
        )
        .await
        {
            api_checks.push(check);
        } else {
            let limit = provider
                .capabilities()
                .catalog
                .recently_played_max_page_size
                .unwrap_or(20) as u32;
            let (check, _, retry_after_secs) = timed_api(
                "api recently played",
                provider.recently_played(
                    RequestContext::BACKGROUND_SYNC,
                    PageRequest::new(limit.max(1), 0),
                ),
            )
            .await;
            record_api_rate_limit(
                store.clone(),
                provider.id().to_string(),
                "recent",
                check.clone(),
                retry_after_secs,
            )
            .await;
            api_checks.push(check);
        }
    } else if first_party && auth_token.ok {
        // A CLI-local doctor has no adapter runtime or librespot session.
        // Report informationally (ok=true) so a logged-in user is not
        // wrongly marked Unhealthy.
        api_checks.push(DoctorCheck {
            name: "provider api".to_string(),
            ok: true,
            message: "skipped: provider API checks run through the daemon (start it and rerun, or use `spotuify status`)"
                .to_string(),
            elapsed_ms: 0,
        });
    } else if auth_token.ok {
        api_checks.push(DoctorCheck {
            name: "provider api".to_string(),
            ok: true,
            message: "skipped: provider runtime unavailable outside the daemon".to_string(),
            elapsed_ms: 0,
        });
    }

    let mut report = DoctorReport {
        healthy: true,
        health_class: HealthClass::Healthy,
        config_path,
        config_ok,
        config_error,
        logs_path,
        client_id: config
            .as_ref()
            .and_then(|(_, config)| config.as_ref())
            .and_then(|config| config.client_id.as_deref())
            .map(redacted_client_id),
        client_secret_present: config
            .as_ref()
            .and_then(|(_, config)| config.as_ref())
            .map(|config| config.client_secret.is_some()),
        redirect_uri: config
            .as_ref()
            .and_then(|(_, config)| config.as_ref())
            .map(|config| {
                config
                    .redirect_uri
                    .clone()
                    .unwrap_or_else(|| "http://127.0.0.1:8888/callback".to_string())
            }),
        keychain_token: auth_token,
        daemon,
        api_checks,
        device_diagnostics: device_diagnostics_report,
        recommended_next_steps: Vec::new(),
        findings: Vec::new(),
        system: None,
        viz: None,
    };
    report.api_checks.push(disk_token_cache);
    finalize_report(&mut report, first_party_only);
    // Phase 6.9: append findings derived from the daemon's recent
    // event log (rate limits, auth errors, schema-compat patches).
    if !recent_events.is_empty() {
        let now_ms = crate::analytics::now_ms();
        let event_findings = spotuify_protocol::findings_from(&recent_events, now_ms);
        report.findings.extend(event_findings);
    }
        Ok(report)
    }
    .boxed()
}

async fn skipped_rate_limit_check(
    store: Option<Store>,
    provider: String,
    name: &'static str,
    domain: &'static str,
) -> Option<DoctorCheck> {
    let remaining_ms = store?
        .provider_rate_limit_cooldown_remaining_ms(&provider, domain)
        .await
        .ok()??;
    Some(DoctorCheck {
        name: name.to_string(),
        ok: false,
        message: format!(
            "skipped; provider rate limit cooldown has {}s remaining",
            (remaining_ms + 999) / 1000
        ),
        elapsed_ms: 0,
    })
}

fn skipped_capability_check(name: &str) -> DoctorCheck {
    DoctorCheck {
        name: name.to_string(),
        ok: true,
        message: "skipped: provider does not advertise this capability".to_string(),
        elapsed_ms: 0,
    }
}

async fn record_api_rate_limit(
    store: Option<Store>,
    provider: String,
    domain: &'static str,
    check: DoctorCheck,
    retry_after_secs: Option<u64>,
) {
    if check.ok || !is_rate_limited(&check.message) {
        return;
    }
    let Some(store) = store else {
        return;
    };
    if let Err(err) = store
        .record_provider_sync_event_with_retry_after(
            &provider,
            domain,
            spotuify_store::now_ms(),
            spotuify_store::ProviderSyncEventOutcome {
                status: "error",
                row_count: 0,
                error: Some(&check.message),
                retry_after_secs,
            },
        )
        .await
    {
        tracing::debug!(provider, domain, error = %err, "failed to record provider rate limit cooldown");
    }
}

pub fn print_report(report: &DoctorReport, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(report)?),
        OutputFormat::Jsonl => println!("{}", serde_json::to_string(report)?),
        OutputFormat::Csv => print_report_csv(report),
        OutputFormat::Ids => {
            for step in &report.recommended_next_steps {
                println!("{step}");
            }
        }
        OutputFormat::Table => print_report_table(report),
    }
    Ok(())
}

fn auth_file_check(provider: &str) -> DoctorCheck {
    let started = Instant::now();
    // First-party-aware: first-party auth uses a separate credential file,
    // so `credential_status` reports logged-in for either credential kind.
    let provider = provider.to_string();
    let (result, elapsed_ms) = timed_sync("auth token", AUTH_CHECK_TIMEOUT, move || {
        spotuify_spotify::auth::credential_status_for(&provider)
    });
    match result {
        Some(Ok(Some(status))) => DoctorCheck {
            name: "auth token".to_string(),
            ok: true,
            message: status,
            elapsed_ms,
        },
        Some(Ok(None)) => DoctorCheck {
            name: "auth token".to_string(),
            ok: false,
            message: "missing; run `spotuify login`".to_string(),
            elapsed_ms,
        },
        Some(Err(err)) => DoctorCheck {
            name: "auth token".to_string(),
            ok: false,
            message: err,
            elapsed_ms,
        },
        None => DoctorCheck {
            name: "auth token".to_string(),
            ok: false,
            message: format!("timed out after {}s", AUTH_CHECK_TIMEOUT.as_secs()),
            elapsed_ms: started.elapsed().as_millis() as u64,
        },
    }
}

fn skipped_auth_check(message: &str) -> DoctorCheck {
    DoctorCheck {
        name: "auth token".to_string(),
        ok: true,
        message: message.to_string(),
        elapsed_ms: 0,
    }
}

/// True when the daemon resolves to first-party-only Spotify auth (the
/// chronically rate-limited state): first-party credentials on disk, no
/// dev-app token, and the resolved mode is first-party. Mirrors
/// [`Config::is_first_party`] but does not require a loaded config, so it
/// still fires for a first-party-only user who has no BYO client_id (whose
/// `Config::load()` fails). Hybrid users (both credentials) and env-forced
/// dev-app users report `false`.
fn resolved_first_party_only(provider: &str) -> bool {
    let stored_first_party_only = spotuify_spotify::auth::stored_first_party_only_for(provider);
    let resolved_first_party = match spotuify_spotify::config::first_party_env_override() {
        Some(explicit) => explicit,
        None => stored_first_party_only,
    };
    resolved_first_party && stored_first_party_only
}

fn finalize_report(report: &mut DoctorReport, first_party_only: bool) {
    report.findings = build_findings(report, first_party_only);
    report.recommended_next_steps = recommended_next_steps(report);
    let has_error = report
        .findings
        .iter()
        .any(|f| matches!(f.severity, DoctorFindingSeverity::Error));
    let has_warning = report
        .findings
        .iter()
        .any(|f| matches!(f.severity, DoctorFindingSeverity::Warning));
    // Phase 13 (P13-K) — three-variant election. Any `Error` →
    // Unhealthy (can't reach Spotify, no auth, daemon down). Any
    // `Warning` with no errors → Degraded. All-info → Healthy.
    report.health_class = if has_error {
        HealthClass::Unhealthy
    } else if has_warning {
        HealthClass::Degraded
    } else {
        HealthClass::Healthy
    };
    report.healthy = matches!(report.health_class, HealthClass::Healthy);
}

fn timed_sync<T, E, F>(
    _name: &str,
    timeout: Duration,
    operation: F,
) -> (Option<Result<T, String>>, u64)
where
    T: Send + 'static,
    E: std::fmt::Display + Send + 'static,
    F: FnOnce() -> Result<T, E> + Send + 'static,
{
    let started = Instant::now();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(operation().map_err(|err| err.to_string()));
    });
    match rx.recv_timeout(timeout) {
        Ok(result) => (Some(result), started.elapsed().as_millis() as u64),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            (None, started.elapsed().as_millis() as u64)
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => (
            Some(Err("worker exited before returning result".to_string())),
            started.elapsed().as_millis() as u64,
        ),
    }
}

async fn timed_api<T>(
    name: &str,
    future: impl Future<Output = std::result::Result<T, spotuify_core::ProviderError>> + Send,
) -> (DoctorCheck, Option<T>, Option<u64>)
where
    T: Send,
{
    let started = Instant::now();
    match tokio::time::timeout(API_CHECK_TIMEOUT, future).await {
        Ok(Ok(value)) => (
            DoctorCheck {
                name: name.to_string(),
                ok: true,
                message: "ok".to_string(),
                elapsed_ms: started.elapsed().as_millis() as u64,
            },
            Some(value),
            None,
        ),
        Ok(Err(err)) => {
            let retry_after_secs = match &err {
                spotuify_core::ProviderError::RateLimited {
                    retry_after: Some(duration),
                    ..
                } => Some(duration.as_secs().max(1)),
                _ => None,
            };
            (
                DoctorCheck {
                    name: name.to_string(),
                    ok: false,
                    message: err.to_string(),
                    elapsed_ms: started.elapsed().as_millis() as u64,
                },
                None,
                retry_after_secs,
            )
        }
        Err(_) => (
            DoctorCheck {
                name: name.to_string(),
                ok: false,
                message: format!("timed out after {}s", API_CHECK_TIMEOUT.as_secs()),
                elapsed_ms: started.elapsed().as_millis() as u64,
            },
            None,
            None,
        ),
    }
}

fn device_diagnostics(
    config: &spotuify_config::PlayerSettings,
    devices: &[Device],
) -> DeviceDiagnostics {
    let preferred_configured = config.device_name.clone();
    let preferred_visible = preferred_configured.as_ref().is_some_and(|name| {
        devices
            .iter()
            .any(|device| !device.is_restricted && device.name.eq_ignore_ascii_case(name))
    });
    let active_device = devices
        .iter()
        .find(|device| device.is_active)
        .map(device_summary);
    let restricted_devices = devices
        .iter()
        .filter(|device| device.is_restricted)
        .map(device_summary)
        .collect();
    let visible_unrestricted_devices = devices
        .iter()
        .filter(|device| !device.is_restricted)
        .map(device_summary)
        .collect();

    DeviceDiagnostics {
        preferred_configured,
        preferred_visible,
        active_device,
        restricted_devices,
        visible_unrestricted_devices,
    }
}

fn device_summary(device: &Device) -> DeviceSummary {
    DeviceSummary {
        name: device.name.clone(),
        kind: device.kind.clone(),
        active: device.is_active,
        restricted: device.is_restricted,
        has_id: device.id.is_some(),
    }
}

/// Returns the host when a configured OAuth redirect URI uses a
/// loopback alias Spotify no longer accepts (`localhost`, `::1`);
/// only the literal `127.0.0.1` works since the Nov 2025 migration.
fn redirect_host_rejected_by_spotify(redirect_uri: &str) -> Option<String> {
    let host = url::Url::parse(redirect_uri).ok()?.host_str()?.to_string();
    let rejected = host.eq_ignore_ascii_case("localhost")
        || host.trim_start_matches('[').trim_end_matches(']') == "::1";
    rejected.then_some(host)
}

fn build_findings(report: &DoctorReport, first_party_only: bool) -> Vec<DoctorFinding> {
    let mut findings = Vec::new();
    if first_party_only {
        // A dev-app client_id is present when the config loaded (load errors
        // on an empty client_id), so `report.client_id` is our signal for
        // whether `spotuify login --dev-app` can even run.
        let remediation = if report.client_id.is_some() {
            vec!["Run `spotuify login --dev-app` to switch to dev-app auth".to_string()]
        } else {
            vec![
                "Run `spotuify onboard` to set up a dev app and switch off rate-limited \
                 first-party auth"
                    .to_string(),
            ]
        };
        findings.push(DoctorFinding {
            category: DoctorFindingCategory::Auth,
            severity: DoctorFindingSeverity::Warning,
            message: "First-party Spotify auth is heavily rate-limited by Spotify (chronic 429s)."
                .to_string(),
            remediation,
        });
    }
    if !report.config_ok {
        findings.push(DoctorFinding {
            category: DoctorFindingCategory::Config,
            severity: DoctorFindingSeverity::Error,
            message: report
                .config_error
                .clone()
                .unwrap_or_else(|| "config failed to load".to_string()),
            remediation: vec!["spotuify config init".to_string()],
        });
    }
    if !report.daemon.socket_reachable {
        findings.push(DoctorFinding {
            category: DoctorFindingCategory::Daemon,
            severity: DoctorFindingSeverity::Warning,
            message: if report.daemon.stale_socket {
                "daemon socket is stale".to_string()
            } else {
                "daemon is not running".to_string()
            },
            remediation: vec!["spotuify daemon start".to_string()],
        });
    }
    if !report.keychain_token.ok {
        findings.push(DoctorFinding {
            category: DoctorFindingCategory::Auth,
            severity: DoctorFindingSeverity::Error,
            message: format!("auth token: {}", report.keychain_token.message),
            remediation: vec!["spotuify login".to_string()],
        });
    }
    if let Some(host) = report
        .redirect_uri
        .as_deref()
        .and_then(redirect_host_rejected_by_spotify)
    {
        findings.push(DoctorFinding {
            category: DoctorFindingCategory::Auth,
            severity: DoctorFindingSeverity::Warning,
            message: format!(
                "OAuth redirect host `{host}` is rejected by Spotify since the \
                 Nov 2025 OAuth migration; only the literal 127.0.0.1 loopback works"
            ),
            remediation: vec![
                "spotuify config set redirect_uri http://127.0.0.1:8888/callback".to_string(),
            ],
        });
    }
    if let Some(devices) = &report.device_diagnostics {
        if devices.preferred_configured.is_none() {
            findings.push(DoctorFinding {
                category: DoctorFindingCategory::Device,
                severity: DoctorFindingSeverity::Warning,
                message: "preferred Spotify device is not configured".to_string(),
                remediation: vec![
                    "spotuify config set player.device_name spotuify-hume".to_string()
                ],
            });
        } else if !devices.preferred_visible {
            findings.push(DoctorFinding {
                category: DoctorFindingCategory::Device,
                severity: DoctorFindingSeverity::Error,
                message: format!(
                    "preferred Spotify device `{}` is not visible",
                    devices.preferred_configured.as_deref().unwrap_or("unknown")
                ),
                remediation: vec!["spotuify devices".to_string()],
            });
        }
        if !devices.restricted_devices.is_empty() {
            findings.push(DoctorFinding {
                category: DoctorFindingCategory::Device,
                severity: DoctorFindingSeverity::Warning,
                message: format!(
                    "{} restricted Spotify device(s) visible; Web API commands cannot target them",
                    devices.restricted_devices.len()
                ),
                remediation: vec!["spotuify devices".to_string()],
            });
        }
    }
    for check in &report.api_checks {
        if !check.ok {
            findings.push(DoctorFinding {
                category: DoctorFindingCategory::Network,
                severity: api_check_failure_severity(check),
                message: format!("{}: {}", check.name, check.message),
                remediation: api_check_remediation(check),
            });
        }
    }
    // Embedded-player audio-flow health (carried on DaemonStatus). Lets us tell
    // a network/session drop (connected=false) from an audio-route/keepalive
    // failure (connected=true, samples not advancing while playing).
    if let Some(audio) = &report.daemon.audio_health {
        findings.push(DoctorFinding {
            category: DoctorFindingCategory::Player,
            severity: DoctorFindingSeverity::Info,
            message: format!(
                "embedded player: connected={}, playing={}, samples_advancing={}, \
                 reconnect_attempts={}, backoff={}ms{}",
                audio.connected,
                audio.is_playing,
                audio.samples_advancing,
                audio.reconnect_attempts,
                audio.current_backoff_ms,
                if audio.last_stall_ms.is_some() {
                    ", last_stall=seen"
                } else {
                    ""
                }
            ),
            remediation: Vec::new(),
        });
        if audio.connected && audio.we_are_active && audio.is_playing && !audio.samples_advancing {
            findings.push(DoctorFinding {
                category: DoctorFindingCategory::Player,
                severity: DoctorFindingSeverity::Warning,
                message: "player connected but no audio is flowing (clock says playing); \
                          recovering. Likely an audio-route/keepalive failure (e.g. AirPods), \
                          not a network drop."
                    .to_string(),
                remediation: vec!["spotuify reconnect".to_string()],
            });
        }
    }
    findings
}

fn api_check_failure_severity(check: &DoctorCheck) -> DoctorFindingSeverity {
    if is_rate_limited(&check.message) {
        return DoctorFindingSeverity::Warning;
    }
    match check.name.as_str() {
        "api playback" | "api devices" => DoctorFindingSeverity::Error,
        _ => DoctorFindingSeverity::Warning,
    }
}

fn api_check_remediation(check: &DoctorCheck) -> Vec<String> {
    if is_rate_limited(&check.message) {
        vec!["wait for Spotify rate limit reset".to_string()]
    } else {
        vec!["spotuify doctor".to_string()]
    }
}

fn is_rate_limited(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("rate limit") || message.contains("rate limited")
}

fn recommended_next_steps(report: &DoctorReport) -> Vec<String> {
    let mut steps = Vec::new();
    for finding in &report.findings {
        for step in &finding.remediation {
            if !steps.contains(step) {
                steps.push(step.clone());
            }
        }
    }
    if steps.is_empty() {
        steps.push("spotuify status".to_string());
        steps.push("spotuify search \"luther vandross\" --type track".to_string());
    }
    steps
}

fn print_report_table(report: &DoctorReport) {
    println!("Health:       {}", report.health_class.as_str());
    println!("Healthy:      {}", report.healthy);
    println!(
        "Daemon:       {}{}",
        if report.daemon.running {
            "running"
        } else {
            "down"
        },
        report
            .daemon
            .daemon_pid
            .map(|pid| format!(" (pid {pid})"))
            .unwrap_or_default()
    );
    println!(
        "Socket:       {} (exists: {}, stale: {})",
        if report.daemon.socket_reachable {
            "reachable"
        } else {
            "unreachable"
        },
        report.daemon.socket_exists,
        report.daemon.stale_socket
    );
    println!(
        "Config:       {} (ok: {})",
        report.config_path, report.config_ok
    );
    if let Some(error) = &report.config_error {
        println!("Config error: {error}");
    }
    println!("Logs:         {}", report.logs_path);
    println!(
        "Client ID:    {}",
        report.client_id.as_deref().unwrap_or("-")
    );
    println!(
        "Client secret: {}",
        report
            .client_secret_present
            .map_or("-", |present| if present { "present" } else { "missing" })
    );
    println!(
        "Redirect URI: {}",
        report.redirect_uri.as_deref().unwrap_or("-")
    );
    println!(
        "Auth token:   {} ({}ms)",
        report.keychain_token.message, report.keychain_token.elapsed_ms
    );
    if let Some(system) = &report.system {
        println!(
            "Media keys:   {}{}",
            yes_no(system.media_controls_enabled),
            system
                .media_controls_bus_name
                .as_ref()
                .map(|name| format!(" (bus {name})"))
                .unwrap_or_default()
        );
        println!(
            "Hook:         {}{}",
            yes_no(system.hooks_enabled),
            system
                .hook_command
                .as_ref()
                .map(|command| format!(" ({command}, {}ms)", system.hook_timeout_ms.unwrap_or(0)))
                .unwrap_or_default()
        );
        println!("Notifications: {}", yes_no(system.notifications_enabled));
        println!(
            "Discord RPC:  {}{}",
            yes_no(system.discord_enabled),
            system
                .discord_application_id
                .as_ref()
                .map(|app_id| format!(" (app {app_id})"))
                .unwrap_or_default()
        );
    }

    if let Some(devices) = &report.device_diagnostics {
        println!(
            "Preferred device configured: {}",
            devices
                .preferred_configured
                .as_deref()
                .unwrap_or("not configured")
        );
        println!(
            "Preferred device visible:    {}",
            yes_no(devices.preferred_visible)
        );
        println!(
            "Active device:               {}",
            devices
                .active_device
                .as_ref()
                .map_or("none", |device| device.name.as_str())
        );
        println!(
            "Restricted devices:          {}",
            devices.restricted_devices.len()
        );
        println!("Visible devices:");
        for device in &devices.visible_unrestricted_devices {
            println!(
                "  - {} ({}, {})",
                device.name,
                device.kind,
                if device.active { "active" } else { "idle" }
            );
        }
        for device in &devices.restricted_devices {
            println!("  - {} ({}, restricted)", device.name, device.kind);
        }
    }

    println!("\nAPI checks:");
    if report.api_checks.is_empty() {
        println!("  skipped");
    } else {
        for check in &report.api_checks {
            println!(
                "  {}: {} ({}ms) {}",
                check.name,
                if check.ok { "ok" } else { "failed" },
                check.elapsed_ms,
                check.message
            );
        }
    }

    if !report.findings.is_empty() {
        println!("\nFindings:");
        for finding in &report.findings {
            println!("  [{:?}] {}", finding.category, finding.message);
            for step in &finding.remediation {
                println!("      -> {step}");
            }
        }
    }

    println!("\nNext:");
    for step in &report.recommended_next_steps {
        println!("  {step}");
    }
}

fn print_report_csv(report: &DoctorReport) {
    println!("name,ok,elapsed_ms,message");
    println!(
        "{}",
        csv_row(&[
            "auth token",
            bool_str(report.keychain_token.ok),
            &report.keychain_token.elapsed_ms.to_string(),
            &report.keychain_token.message,
        ])
    );
    for check in &report.api_checks {
        println!(
            "{}",
            csv_row(&[
                &check.name,
                bool_str(check.ok),
                &check.elapsed_ms.to_string(),
                &check.message,
            ])
        );
    }
    if let Some(system) = &report.system {
        println!(
            "{}",
            csv_row(&[
                "media controls",
                bool_str(system.media_controls_enabled),
                "0",
                system.media_controls_bus_name.as_deref().unwrap_or("-"),
            ])
        );
        println!(
            "{}",
            csv_row(&[
                "shell hook",
                bool_str(system.hooks_enabled),
                &system.hook_timeout_ms.unwrap_or(0).to_string(),
                system.hook_command.as_deref().unwrap_or("-"),
            ])
        );
        println!(
            "{}",
            csv_row(&[
                "notifications",
                bool_str(system.notifications_enabled),
                "0",
                "-",
            ])
        );
        println!(
            "{}",
            csv_row(&[
                "discord rpc",
                bool_str(system.discord_enabled),
                "0",
                system.discord_application_id.as_deref().unwrap_or("-"),
            ])
        );
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn bool_str(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

fn csv_row(values: &[&str]) -> String {
    values
        .iter()
        .map(|value| csv_value(value))
        .collect::<Vec<_>>()
        .join(",")
}

fn csv_value(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str, active: bool, restricted: bool) -> Device {
        Device {
            id: Some(format!("id-{name}")),
            name: name.to_string(),
            kind: "Computer".to_string(),
            is_active: active,
            is_restricted: restricted,
            volume_percent: Some(50),
            supports_volume: true,
        }
    }

    fn config_with_preferred(name: &str) -> spotuify_config::PlayerSettings {
        spotuify_config::PlayerSettings {
            device_name: Some(name.to_string()),
            ..spotuify_config::PlayerSettings::default()
        }
    }

    fn healthy_report() -> DoctorReport {
        DoctorReport {
            healthy: true,
            health_class: HealthClass::Healthy,
            config_path: "spotuify.toml".into(),
            config_ok: true,
            config_error: None,
            logs_path: "spotuify.log".into(),
            client_id: Some("present".into()),
            client_secret_present: Some(false),
            redirect_uri: Some("http://127.0.0.1:8888/callback".into()),
            keychain_token: DoctorCheck {
                name: "auth token".into(),
                ok: true,
                message: "present".into(),
                elapsed_ms: 1,
            },
            daemon: DaemonStatus {
                running: true,
                socket_path: "sock".into(),
                socket_exists: true,
                socket_reachable: true,
                stale_socket: false,
                daemon_pid: Some(1),
                uptime_secs: Some(1),
                protocol_version: 1,
                daemon_version: Some("0.1.0".into()),
                daemon_build_id: Some("build".into()),
                audio_health: None,
            },
            api_checks: Vec::new(),
            device_diagnostics: None,
            recommended_next_steps: Vec::new(),
            findings: Vec::new(),
            system: None,
            viz: None,
        }
    }

    #[test]
    fn device_diagnostics_reports_preferred_active_and_restricted_devices() {
        let diagnostics = device_diagnostics(
            &config_with_preferred("spotuify-hume"),
            &[
                device("spotuify-hume", false, false),
                device("phone", true, false),
                device("tv", false, true),
            ],
        );

        assert!(diagnostics.preferred_visible);
        assert_eq!(
            diagnostics
                .active_device
                .expect("active device should be reported")
                .name,
            "phone"
        );
        assert_eq!(diagnostics.restricted_devices[0].name, "tv");
        assert_eq!(diagnostics.visible_unrestricted_devices.len(), 2);
    }

    #[test]
    fn findings_include_exact_preferred_device_remediation() {
        let mut report = healthy_report();
        report.device_diagnostics = Some(DeviceDiagnostics {
            preferred_configured: None,
            preferred_visible: false,
            active_device: None,
            restricted_devices: Vec::new(),
            visible_unrestricted_devices: Vec::new(),
        });
        report.findings = build_findings(&report, false);

        assert_eq!(
            report.findings[0].remediation,
            vec!["spotuify config set player.device_name spotuify-hume".to_string()]
        );
    }

    #[test]
    fn first_party_only_with_client_id_recommends_dev_app_login() {
        let report = healthy_report(); // client_id: Some("present")
        let findings = build_findings(&report, true);
        let auth = findings
            .iter()
            .find(|f| f.category == DoctorFindingCategory::Auth)
            .expect("first-party-only finding");
        assert_eq!(auth.severity, DoctorFindingSeverity::Warning);
        assert!(auth.message.contains("rate-limited"), "{}", auth.message);
        assert_eq!(
            auth.remediation,
            vec!["Run `spotuify login --dev-app` to switch to dev-app auth".to_string()]
        );
    }

    #[test]
    fn first_party_only_without_client_id_recommends_onboard() {
        let mut report = healthy_report();
        report.client_id = None; // no BYO app → `login --dev-app` can't run
        let findings = build_findings(&report, true);
        let auth = findings
            .iter()
            .find(|f| f.category == DoctorFindingCategory::Auth)
            .expect("first-party-only finding");
        assert_eq!(
            auth.remediation,
            vec![
                "Run `spotuify onboard` to set up a dev app and switch off rate-limited \
                 first-party auth"
                    .to_string()
            ]
        );
    }

    #[test]
    fn non_first_party_report_has_no_migration_finding() {
        let report = healthy_report();
        let findings = build_findings(&report, false);
        assert!(!findings
            .iter()
            .any(|f| f.message.contains("First-party Spotify auth")));
    }

    #[test]
    fn rate_limited_optional_api_checks_make_doctor_degraded() {
        let mut report = healthy_report();
        report.api_checks.push(DoctorCheck {
            name: "api playlists".into(),
            ok: false,
            message: "Spotify GET /me/playlists was rate limited; retry after 60s".into(),
            elapsed_ms: 1,
        });

        finalize_report(&mut report, false);

        assert!(!report.healthy);
        assert_eq!(report.health_class, HealthClass::Degraded);
        assert_eq!(report.findings[0].severity, DoctorFindingSeverity::Warning);
        assert_eq!(
            report.findings[0].remediation,
            vec!["wait for Spotify rate limit reset".to_string()]
        );
    }

    #[tokio::test]
    async fn doctor_persists_typed_provider_retry_after() {
        let store = Store::in_memory().await.expect("in-memory store");
        let (check, value, retry_after_secs) = timed_api::<()>("api playlists", async {
            Err(spotuify_core::ProviderError::RateLimited {
                scope: Some("playlists".to_string()),
                retry_after: Some(Duration::from_secs(30)),
            })
        })
        .await;

        assert!(value.is_none());
        assert_eq!(retry_after_secs, Some(30));
        record_api_rate_limit(
            Some(store.clone()),
            "secondary".to_string(),
            "playlists",
            check,
            retry_after_secs,
        )
        .await;

        let remaining = store
            .provider_rate_limit_cooldown_remaining_ms("secondary", "playlists")
            .await
            .expect("cooldown query")
            .expect("persisted cooldown");
        assert!(remaining > 25_000, "remaining cooldown was {remaining}ms");
    }

    #[test]
    fn daemon_unreachable_makes_doctor_degraded() {
        let mut report = healthy_report();
        report.daemon.running = false;
        report.daemon.socket_exists = false;
        report.daemon.socket_reachable = false;
        report.daemon.daemon_pid = None;

        finalize_report(&mut report, false);

        assert!(!report.healthy);
        assert_eq!(report.health_class, HealthClass::Degraded);
        assert_eq!(report.findings[0].category, DoctorFindingCategory::Daemon);
        assert_eq!(report.findings[0].message, "daemon is not running");
    }

    #[test]
    fn core_playback_api_failure_makes_doctor_unhealthy() {
        // Phase 13 (P13-K) — `Unhealthy` is the new third health
        // class. Errors (vs Warnings) now upgrade the rollup from
        // Degraded to Unhealthy so monitoring scripts can act on the
        // hard-failure case differently.
        let mut report = healthy_report();
        report.api_checks.push(DoctorCheck {
            name: "api playback".into(),
            ok: false,
            message: "request failed".into(),
            elapsed_ms: 1,
        });

        finalize_report(&mut report, false);

        assert!(!report.healthy);
        assert_eq!(report.health_class, HealthClass::Unhealthy);
        assert_eq!(report.findings[0].severity, DoctorFindingSeverity::Error);
    }

    #[tokio::test]
    async fn doctor_checks_secondary_spotify_auth_when_default_is_fake() {
        let _guard = crate::ENV_LOCK.lock().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let config_path = temp.path().join("spotuify.toml");
        std::fs::write(
            &config_path,
            r#"
[providers]
default = "local"

[providers.local]
type = "fake"

[providers.custom-cloud]
type = "spotify"
client_id = "secondary-client-id"
redirect_uri = "http://127.0.0.1:8888/callback"
"#,
        )
        .expect("config");
        let old_config = std::env::var_os("SPOTUIFY_CONFIG");
        let old_fake = std::env::var_os("SPOTUIFY_FAKE_SPOTIFY");
        std::env::set_var("SPOTUIFY_CONFIG", &config_path);
        std::env::remove_var("SPOTUIFY_FAKE_SPOTIFY");

        let report = collect_report_with_events(healthy_report().daemon, Vec::new(), None, None)
            .await
            .expect("doctor report");

        assert!(
            !report.keychain_token.message.contains("skipped"),
            "secondary Spotify auth must not be hidden by the fake default: {}",
            report.keychain_token.message
        );
        assert!(
            !report.keychain_token.ok,
            "fixture has no custom-cloud token"
        );
        assert_eq!(report.client_id.as_deref(), Some("seco...t-id"));

        match old_config {
            Some(value) => std::env::set_var("SPOTUIFY_CONFIG", value),
            None => std::env::remove_var("SPOTUIFY_CONFIG"),
        }
        match old_fake {
            Some(value) => std::env::set_var("SPOTUIFY_FAKE_SPOTIFY", value),
            None => std::env::remove_var("SPOTUIFY_FAKE_SPOTIFY"),
        }
    }
}
