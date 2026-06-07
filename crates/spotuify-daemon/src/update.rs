//! Update-awareness: periodically ask GitHub whether a newer release exists,
//! cache the result on `DaemonState`, and tell clients exactly how to upgrade
//! for the way this binary was installed (Homebrew / cargo / DMG).
//!
//! Privacy: this is the only outbound non-Spotify call spotuify makes. It hits
//! the public, unauthenticated GitHub releases API with a plain User-Agent and
//! sends no identifying data. It is disabled entirely when
//! `SPOTUIFY_NO_UPDATE_CHECK` is set (see [`update_check_disabled`]).

use std::path::Path;
use std::time::Duration;

use spotuify_protocol::{UpgradeHint, UpgradeMethod};

const RELEASES_LATEST_API: &str =
    "https://api.github.com/repos/planetaryescape/spotuify/releases/latest";
const RELEASES_LATEST_PAGE: &str = "https://github.com/planetaryescape/spotuify/releases/latest";
const REPO_GIT_URL: &str = "https://github.com/planetaryescape/spotuify";
const HOMEBREW_UPGRADE: &str = "brew upgrade planetaryescape/spotuify/spotuify";

/// A cached observation of the latest GitHub release.
#[derive(Debug, Clone)]
pub(crate) struct CachedRelease {
    /// Normalized version (no leading `v`), e.g. `0.1.48`.
    pub latest_version: String,
    /// The release's `html_url`, when GitHub returned one.
    pub release_url: Option<String>,
    /// When this check ran (epoch ms).
    pub checked_at_ms: i64,
}

/// Whether `latest` is a strictly newer semver than `current`. Tolerates a
/// leading `v` on either side and returns `false` on any parse failure — we
/// never nag the user over malformed version strings.
pub(crate) fn is_newer(current: &str, latest: &str) -> bool {
    let parse = |s: &str| semver::Version::parse(s.trim().trim_start_matches('v'));
    match (parse(current), parse(latest)) {
        (Ok(cur), Ok(new)) => new > cur,
        _ => false,
    }
}

/// Classify how this install upgrades, from the running executable's path.
pub(crate) fn detect_upgrade_method(exe: &Path) -> UpgradeMethod {
    let p = exe.to_string_lossy();
    // A dev build run straight out of a workspace target dir (`target/` or the
    // CLI-isolated `target-cli/`) — nothing to "upgrade"; rebuild from source.
    let in_target_dir = p.contains("/target/") || p.contains("/target-cli/");
    if in_target_dir && (p.contains("/debug/") || p.contains("/release/")) {
        return UpgradeMethod::Dev;
    }
    if p.contains("/Cellar/") || p.contains("/homebrew/") {
        UpgradeMethod::Homebrew
    } else if p.contains("/.cargo/") {
        UpgradeMethod::Cargo
    } else if p.contains(".app/Contents/") || p.contains("/.local/bin/") {
        // The macOS app bundles the CLI at Contents/Resources and installs a
        // copy to ~/.local/bin — both paths mean "upgrade via the DMG".
        UpgradeMethod::MacApp
    } else {
        UpgradeMethod::Manual
    }
}

/// Build the actionable upgrade guidance a client renders verbatim.
pub(crate) fn upgrade_hint(
    method: UpgradeMethod,
    latest_version: &str,
    release_url: Option<&str>,
) -> UpgradeHint {
    let release_page = || {
        release_url
            .map(str::to_string)
            .unwrap_or_else(|| RELEASES_LATEST_PAGE.to_string())
    };
    let (command, url) = match method {
        UpgradeMethod::Homebrew => (Some(HOMEBREW_UPGRADE.to_string()), None),
        UpgradeMethod::Cargo => (
            Some(format!(
                "cargo install --git {REPO_GIT_URL} --tag v{latest_version} --locked spotuify"
            )),
            None,
        ),
        UpgradeMethod::MacApp | UpgradeMethod::Manual => (None, Some(release_page())),
        UpgradeMethod::Dev => (None, None),
    };
    UpgradeHint {
        method,
        command,
        url,
    }
}

/// The current daemon/CLI version (compile-time).
pub(crate) fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The running executable's path (canonicalized when possible) for install
/// detection. Falls back to a bare `spotuify` so detection degrades to
/// `Manual` rather than panicking.
pub(crate) fn current_exe_path() -> std::path::PathBuf {
    std::env::current_exe()
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
        .unwrap_or_else(|_| std::path::PathBuf::from("spotuify"))
}

/// True when the periodic update check is disabled by the user. Honors
/// `SPOTUIFY_NO_UPDATE_CHECK` (any non-empty, non-`0`/`false` value).
pub(crate) fn update_check_disabled() -> bool {
    match std::env::var("SPOTUIFY_NO_UPDATE_CHECK") {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            !(v.is_empty() || v == "0" || v == "false")
        }
        Err(_) => false,
    }
}

#[derive(serde::Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    html_url: Option<String>,
}

/// Fetch the latest release `(version, html_url)` from the public GitHub API.
/// Bounded: 4s connect / 8s total. Returns the normalized version (no `v`).
pub(crate) async fn fetch_latest_release() -> anyhow::Result<(String, Option<String>)> {
    let user_agent = format!("spotuify/{} (+{REPO_GIT_URL})", current_version());
    let client = reqwest::Client::builder()
        .user_agent(user_agent)
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(8))
        .build()?;
    let raw: RawRelease = client
        .get(RELEASES_LATEST_API)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let version = raw.tag_name.trim().trim_start_matches('v').to_string();
    Ok((version, raw.html_url))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn is_newer_handles_v_prefix_and_ordering() {
        assert!(is_newer("0.1.47", "0.1.48"));
        assert!(is_newer("0.1.47", "v0.1.48"));
        assert!(is_newer("v0.1.47", "0.2.0"));
        assert!(!is_newer("0.1.48", "0.1.48"));
        assert!(!is_newer("0.1.48", "0.1.47")); // older release never nags
        assert!(!is_newer("0.1.48", "")); // junk never nags
        assert!(!is_newer("0.1.48", "not-a-version"));
    }

    #[test]
    fn detect_upgrade_method_maps_paths() {
        assert_eq!(
            detect_upgrade_method(&PathBuf::from(
                "/opt/homebrew/Cellar/spotuify/0.1.47/bin/spotuify"
            )),
            UpgradeMethod::Homebrew
        );
        assert_eq!(
            detect_upgrade_method(&PathBuf::from("/Users/me/.cargo/bin/spotuify")),
            UpgradeMethod::Cargo
        );
        assert_eq!(
            detect_upgrade_method(&PathBuf::from(
                "/Applications/Spotuify.app/Contents/Resources/spotuify"
            )),
            UpgradeMethod::MacApp
        );
        assert_eq!(
            detect_upgrade_method(&PathBuf::from("/Users/me/.local/bin/spotuify")),
            UpgradeMethod::MacApp
        );
        assert_eq!(
            detect_upgrade_method(&PathBuf::from(
                "/Users/me/code/spotuify/target/release/spotuify"
            )),
            UpgradeMethod::Dev
        );
        assert_eq!(
            detect_upgrade_method(&PathBuf::from(
                "/Users/me/code/spotuify/target-cli/debug/spotuify"
            )),
            UpgradeMethod::Dev
        );
        assert_eq!(
            detect_upgrade_method(&PathBuf::from("/usr/bin/spotuify")),
            UpgradeMethod::Manual
        );
    }

    #[test]
    fn upgrade_hint_renders_per_method() {
        let brew = upgrade_hint(UpgradeMethod::Homebrew, "0.1.48", None);
        assert_eq!(brew.command.as_deref(), Some(HOMEBREW_UPGRADE));
        assert!(brew.url.is_none());

        let cargo = upgrade_hint(UpgradeMethod::Cargo, "0.1.48", None);
        assert!(cargo.command.as_deref().unwrap().contains("--tag v0.1.48"));

        let app = upgrade_hint(
            UpgradeMethod::MacApp,
            "0.1.48",
            Some("https://example.com/r"),
        );
        assert_eq!(app.url.as_deref(), Some("https://example.com/r"));
        assert!(app.command.is_none());

        let manual = upgrade_hint(UpgradeMethod::Manual, "0.1.48", None);
        assert_eq!(manual.url.as_deref(), Some(RELEASES_LATEST_PAGE));

        let dev = upgrade_hint(UpgradeMethod::Dev, "0.1.48", None);
        assert!(dev.command.is_none() && dev.url.is_none());
    }
}
