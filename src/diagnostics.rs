use std::future::Future;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::auth::token_status;
use crate::config::{config_path, Config};
use crate::logging;
use crate::output::OutputFormat;
use crate::protocol::{
    DaemonStatus, DeviceDiagnostics, DeviceSummary, DoctorCheck, DoctorFinding,
    DoctorFindingCategory, DoctorFindingSeverity, DoctorReport, HealthClass,
};
use crate::spotify::{Device, SpotifyClient};
use crate::spotifyd;

const KEYCHAIN_CHECK_TIMEOUT: Duration = Duration::from_secs(20);
const LOCAL_CHECK_TIMEOUT: Duration = Duration::from_secs(3);
const API_CHECK_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn collect_report(daemon: DaemonStatus) -> Result<DoctorReport> {
    let config_path = config_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|err| format!("unresolved: {err}"));
    let logs_path = logging::log_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|err| format!("unresolved: {err}"));

    let config_result = Config::load();
    let (config_ok, config_error, config) = match config_result {
        Ok(config) => (true, None, Some(config)),
        Err(err) => (false, Some(err.to_string()), None),
    };
    let config_path = config
        .as_ref()
        .map(|config| config.config_path.display().to_string())
        .unwrap_or(config_path);

    let keychain_token = keychain_check();
    let (spotifyd_running_result, _) = timed_sync("spotifyd running", LOCAL_CHECK_TIMEOUT, || {
        Ok(spotifyd::is_running())
    });
    let mut spotifyd_running = spotifyd_running_result.and_then(Result::ok);
    if spotifyd_running == Some(false) {
        if let Some(config) = config.as_ref().filter(|config| config.spotifyd_autostart) {
            spotifyd_running = maybe_start_spotifyd(config).or(spotifyd_running);
        }
    }

    let mut api_checks = Vec::new();
    let mut device_diagnostics_report = None;

    if let Some(config) = config.as_ref().filter(|_| keychain_token.ok) {
        match SpotifyClient::new(config.clone()) {
            Ok(mut client) => {
                let (check, _) = timed_api("api playback", client.playback()).await;
                api_checks.push(check);

                let (check, devices) = timed_api("api devices", client.devices()).await;
                api_checks.push(check);
                if let Some(devices) = devices {
                    device_diagnostics_report = Some(device_diagnostics(config, &devices));
                }

                let (check, _) = timed_api("api queue", client.queue()).await;
                api_checks.push(check);

                let (check, _) = timed_api("api playlists", client.playlists()).await;
                api_checks.push(check);

                let (check, _) = timed_api("api recently played", client.recently_played()).await;
                api_checks.push(check);
            }
            Err(err) => api_checks.push(DoctorCheck {
                name: "spotify client".to_string(),
                ok: false,
                message: err.to_string(),
                elapsed_ms: 0,
            }),
        }
    }

    let mut report = DoctorReport {
        healthy: true,
        health_class: HealthClass::Healthy,
        config_path,
        config_ok,
        config_error,
        logs_path,
        spotifyd_config_path: config
            .as_ref()
            .map(|config| config.spotifyd_config_path.display().to_string()),
        spotifyd_autostart: config.as_ref().map(|config| config.spotifyd_autostart),
        spotifyd_running,
        client_id: config.as_ref().map(Config::redacted_client_id),
        client_secret_present: config.as_ref().map(|config| config.client_secret.is_some()),
        redirect_uri: config.as_ref().map(|config| config.redirect_uri.clone()),
        keychain_token,
        daemon,
        api_checks,
        device_diagnostics: device_diagnostics_report,
        recommended_next_steps: Vec::new(),
        findings: Vec::new(),
    };
    finalize_report(&mut report);
    Ok(report)
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

fn keychain_check() -> DoctorCheck {
    let started = Instant::now();
    let (result, elapsed_ms) = timed_sync("keychain token", KEYCHAIN_CHECK_TIMEOUT, token_status);
    match result {
        Some(Ok(Some(status))) => DoctorCheck {
            name: "keychain token".to_string(),
            ok: true,
            message: status,
            elapsed_ms,
        },
        Some(Ok(None)) => DoctorCheck {
            name: "keychain token".to_string(),
            ok: false,
            message: "missing; run `spotuify login`".to_string(),
            elapsed_ms,
        },
        Some(Err(err)) => DoctorCheck {
            name: "keychain token".to_string(),
            ok: false,
            message: err.to_string(),
            elapsed_ms,
        },
        None => DoctorCheck {
            name: "keychain token".to_string(),
            ok: false,
            message: format!("timed out after {}s", KEYCHAIN_CHECK_TIMEOUT.as_secs()),
            elapsed_ms: started.elapsed().as_millis(),
        },
    }
}

fn maybe_start_spotifyd(config: &Config) -> Option<bool> {
    let config = config.clone();
    let (result, _) = timed_sync("spotifyd start", LOCAL_CHECK_TIMEOUT, move || {
        let status = spotifyd::ensure_started(&config)?;
        if matches!(status, spotifyd::SpotifydStatus::Started) {
            std::thread::sleep(Duration::from_millis(750));
        }
        Ok(spotifyd::is_running())
    });
    result.and_then(Result::ok)
}

fn finalize_report(report: &mut DoctorReport) {
    report.findings = build_findings(report);
    report.recommended_next_steps = recommended_next_steps(report);
    report.healthy = report
        .findings
        .iter()
        .all(|finding| !matches!(finding.severity, DoctorFindingSeverity::Error));
    report.health_class = if report.healthy {
        HealthClass::Healthy
    } else {
        HealthClass::Degraded
    };
}

fn timed_sync<T, F>(
    _name: &str,
    timeout: Duration,
    operation: F,
) -> (Option<Result<T, anyhow::Error>>, u128)
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, anyhow::Error> + Send + 'static,
{
    let started = Instant::now();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(operation());
    });
    match rx.recv_timeout(timeout) {
        Ok(result) => (Some(result), started.elapsed().as_millis()),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => (None, started.elapsed().as_millis()),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => (
            Some(Err(anyhow::anyhow!(
                "worker exited before returning result"
            ))),
            started.elapsed().as_millis(),
        ),
    }
}

async fn timed_api<T>(
    name: &str,
    future: impl Future<Output = Result<T, anyhow::Error>>,
) -> (DoctorCheck, Option<T>) {
    let started = Instant::now();
    match tokio::time::timeout(API_CHECK_TIMEOUT, future).await {
        Ok(Ok(value)) => (
            DoctorCheck {
                name: name.to_string(),
                ok: true,
                message: "ok".to_string(),
                elapsed_ms: started.elapsed().as_millis(),
            },
            Some(value),
        ),
        Ok(Err(err)) => (
            DoctorCheck {
                name: name.to_string(),
                ok: false,
                message: err.to_string(),
                elapsed_ms: started.elapsed().as_millis(),
            },
            None,
        ),
        Err(_) => (
            DoctorCheck {
                name: name.to_string(),
                ok: false,
                message: format!("timed out after {}s", API_CHECK_TIMEOUT.as_secs()),
                elapsed_ms: started.elapsed().as_millis(),
            },
            None,
        ),
    }
}

fn device_diagnostics(config: &Config, devices: &[Device]) -> DeviceDiagnostics {
    let preferred_configured = config.spotifyd_device_name.clone();
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

fn build_findings(report: &DoctorReport) -> Vec<DoctorFinding> {
    let mut findings = Vec::new();
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
            message: format!("keychain token: {}", report.keychain_token.message),
            remediation: vec!["spotuify login".to_string()],
        });
    }
    if report.spotifyd_running == Some(false) {
        findings.push(DoctorFinding {
            category: DoctorFindingCategory::Spotifyd,
            severity: DoctorFindingSeverity::Warning,
            message: if report.spotifyd_autostart == Some(false) {
                "spotifyd is not running and autostart is disabled".to_string()
            } else {
                "spotifyd is not running".to_string()
            },
            remediation: if report.spotifyd_autostart == Some(false) {
                vec!["spotuify config set spotifyd.autostart true".to_string()]
            } else {
                vec!["spotuify daemon restart".to_string()]
            },
        });
    }
    if let Some(devices) = &report.device_diagnostics {
        if devices.preferred_configured.is_none() {
            findings.push(DoctorFinding {
                category: DoctorFindingCategory::Device,
                severity: DoctorFindingSeverity::Warning,
                message: "preferred Spotify device is not configured".to_string(),
                remediation: vec![
                    "spotuify config set spotifyd.device_name spotuify-hume".to_string()
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
        "spotifyd:     running={} autostart={} config={}",
        option_bool(report.spotifyd_running),
        option_bool(report.spotifyd_autostart),
        report.spotifyd_config_path.as_deref().unwrap_or("-")
    );
    println!(
        "Client ID:    {}",
        report.client_id.as_deref().unwrap_or("-")
    );
    println!(
        "Client secret: {}",
        report
            .client_secret_present
            .map(|present| if present { "present" } else { "missing" })
            .unwrap_or("-")
    );
    println!(
        "Redirect URI: {}",
        report.redirect_uri.as_deref().unwrap_or("-")
    );
    println!(
        "Keychain:     {} ({}ms)",
        report.keychain_token.message, report.keychain_token.elapsed_ms
    );

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
                .map(|device| device.name.as_str())
                .unwrap_or("none")
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
            "keychain token",
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
}

fn option_bool(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "-",
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

    fn config_with_preferred(name: &str) -> Config {
        Config {
            client_id: "client".to_string(),
            client_secret: None,
            redirect_uri: "http://127.0.0.1:8888/callback".to_string(),
            config_path: "spotuify.toml".into(),
            spotifyd_config_path: "spotifyd.conf".into(),
            spotifyd_device_name: Some(name.to_string()),
            spotifyd_autostart: true,
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
            spotifyd_config_path: None,
            spotifyd_autostart: Some(true),
            spotifyd_running: Some(true),
            client_id: Some("present".into()),
            client_secret_present: Some(false),
            redirect_uri: Some("http://127.0.0.1:8888/callback".into()),
            keychain_token: DoctorCheck {
                name: "keychain token".into(),
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
            },
            api_checks: Vec::new(),
            device_diagnostics: None,
            recommended_next_steps: Vec::new(),
            findings: Vec::new(),
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
        assert_eq!(diagnostics.active_device.unwrap().name, "phone");
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
        report.findings = build_findings(&report);

        assert_eq!(
            report.findings[0].remediation,
            vec!["spotuify config set spotifyd.device_name spotuify-hume".to_string()]
        );
    }

    #[test]
    fn rate_limited_optional_api_checks_do_not_make_doctor_unhealthy() {
        let mut report = healthy_report();
        report.api_checks.push(DoctorCheck {
            name: "api playlists".into(),
            ok: false,
            message: "Spotify GET /me/playlists was rate limited; retry after 60s".into(),
            elapsed_ms: 1,
        });

        finalize_report(&mut report);

        assert!(report.healthy);
        assert_eq!(report.health_class, HealthClass::Healthy);
        assert_eq!(report.findings[0].severity, DoctorFindingSeverity::Warning);
        assert_eq!(
            report.findings[0].remediation,
            vec!["wait for Spotify rate limit reset".to_string()]
        );
    }

    #[test]
    fn core_playback_api_failure_makes_doctor_unhealthy() {
        let mut report = healthy_report();
        report.api_checks.push(DoctorCheck {
            name: "api playback".into(),
            ok: false,
            message: "request failed".into(),
            elapsed_ms: 1,
        });

        finalize_report(&mut report);

        assert!(!report.healthy);
        assert_eq!(report.health_class, HealthClass::Degraded);
        assert_eq!(report.findings[0].severity, DoctorFindingSeverity::Error);
    }
}
