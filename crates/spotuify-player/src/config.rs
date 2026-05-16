//! Resolved player settings consumed by every backend.
//!
//! Mirrors `spotuify_spotify::config::PlayerConfig` so the daemon can
//! pass a value-type into backend constructors without the player crate
//! reading config.toml directly. The TOML parse + validation lives in
//! `spotuify-spotify`; this struct is the daemon-facing shape.

use spotuify_core::BackendKind;

/// Player settings — fully defaulted, ready for the backend factory.
///
/// Kept distinct from `spotuify_spotify::config::PlayerConfig` so the
/// player crate doesn't pull the config-loading code into its
/// dependency graph; a small `From` impl in the daemon translates one
/// to the other.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerSettings {
    pub backend: BackendKind,
    pub bitrate: u32,
    pub device_name: Option<String>,
    pub normalization: bool,
    pub audio_cache_mib: u32,
    pub pulse_props: bool,
    pub event_hook: Option<String>,
}

impl Default for PlayerSettings {
    fn default() -> Self {
        Self {
            backend: BackendKind::default(),
            bitrate: 320,
            device_name: None,
            normalization: false,
            audio_cache_mib: 0,
            pulse_props: true,
            event_hook: None,
        }
    }
}

impl PlayerSettings {
    /// Resolve the effective device name, falling back to the system
    /// hostname when the user hasn't set one, and to "spotuify" when
    /// the hostname is unavailable or empty. Backends call this at
    /// `register_device` time.
    pub fn effective_device_name(&self) -> String {
        if let Some(name) = self.device_name.as_deref() {
            if !name.trim().is_empty() {
                return name.to_string();
            }
        }
        hostname::get()
            .ok()
            .and_then(|n| n.into_string().ok())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "spotuify".to_string())
    }
}

// We avoid pulling the `hostname` crate just for this one call —
// fall back through std env vars instead. Cheap, correct on all three
// targets (Linux/macOS/Windows).
mod hostname {
    pub fn get() -> std::io::Result<std::ffi::OsString> {
        if let Some(name) = std::env::var_os("HOSTNAME") {
            return Ok(name);
        }
        if let Some(name) = std::env::var_os("COMPUTERNAME") {
            return Ok(name);
        }
        // Best-effort: empty string -> falls back to "spotuify" in the
        // caller. Pulling libc/winapi just to read gethostname is not
        // worth the build-time cost for a default-name fallback.
        Ok(std::ffi::OsString::new())
    }
}

#[cfg(test)]
mod tests {
    use super::PlayerSettings;
    use spotuify_core::BackendKind;

    #[test]
    fn defaults_match_phase_9_doc() {
        // Phase 9 flipped the default backend from Spotifyd → Embedded.
        // The daemon's `player_factory` has auto-fallback to Spotifyd if
        // embedded fails to initialise, so this is safe on all platforms.
        let s = PlayerSettings::default();
        assert_eq!(s.backend, BackendKind::Embedded);
        assert_eq!(s.bitrate, 320);
        assert_eq!(s.audio_cache_mib, 0);
        assert!(!s.normalization);
        assert!(s.pulse_props);
    }

    #[test]
    fn effective_device_name_uses_explicit_override_when_set() {
        let s = PlayerSettings {
            device_name: Some("kitchen".to_string()),
            ..PlayerSettings::default()
        };
        assert_eq!(s.effective_device_name(), "kitchen");
    }

    #[test]
    fn effective_device_name_falls_back_to_spotuify_when_no_host() {
        // Force the empty-env path: the function returns "spotuify"
        // when no HOSTNAME / COMPUTERNAME env vars are set.
        // SAFETY: single-threaded test, only mutating the vars this
        // test owns.
        let orig_host = std::env::var_os("HOSTNAME");
        let orig_comp = std::env::var_os("COMPUTERNAME");
        std::env::remove_var("HOSTNAME");
        std::env::remove_var("COMPUTERNAME");

        let s = PlayerSettings::default();
        let name = s.effective_device_name();

        if let Some(v) = orig_host {
            std::env::set_var("HOSTNAME", v);
        }
        if let Some(v) = orig_comp {
            std::env::set_var("COMPUTERNAME", v);
        }

        assert_eq!(name, "spotuify");
    }
}
