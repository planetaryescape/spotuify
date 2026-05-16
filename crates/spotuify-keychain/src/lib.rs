//! Phase 11 — cross-platform credential storage.
//!
//! Wraps the `keyring` crate with a stable, target-agnostic API and a
//! typed `KeychainError` so call sites can pattern-match on `NoEntry`
//! without leaking `keyring::Error` into the daemon layer.
//!
//! Behind the scenes:
//! - **macOS**: Security framework via `keyring/apple-native`.
//! - **Linux**: Secret Service via DBus (`linux-native-sync-persistent`)
//!   — needs GNOME Keyring or KWallet running. Headless encrypted-file
//!   fallback is planned, but not shipped as a stable credential path.
//! - **Windows**: Credential Manager via `windows-native`.
//!
//! The fallback file path is deliberately omitted until there is a
//! validated headless-server workflow; call sites should surface
//! `Unavailable` with a clear setup message instead of silently writing
//! credentials to disk.

use thiserror::Error;

/// Typed errors callers may want to pattern-match on. `NoEntry` is
/// the common "credential doesn't exist" case used by login / logout
/// to print the right user-facing message.
#[derive(Debug, Error)]
pub enum KeychainError {
    /// No credential stored under this `(service, account)` pair.
    #[error("no credential stored for {service}/{account}")]
    NoEntry { service: String, account: String },
    /// Platform keystore is unreachable. On Linux this typically means
    /// GNOME Keyring / KWallet isn't running. On Windows it usually
    /// means Credential Manager is disabled by policy.
    #[error("keychain backend unavailable: {0}")]
    Unavailable(String),
    /// Any other backend failure (transport error, malformed entry,
    /// permission denied). Always returned by the wrapper rather than
    /// `keyring::Error` so the public API stays stable across versions.
    #[error("keychain error: {0}")]
    Other(String),
}

impl KeychainError {
    pub fn is_no_entry(&self) -> bool {
        matches!(self, Self::NoEntry { .. })
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn map_keyring_error(service: &str, account: &str, err: keyring::Error) -> KeychainError {
    match err {
        keyring::Error::NoEntry => KeychainError::NoEntry {
            service: service.to_string(),
            account: account.to_string(),
        },
        keyring::Error::PlatformFailure(msg) => KeychainError::Unavailable(msg.to_string()),
        other => KeychainError::Other(other.to_string()),
    }
}

/// Read a credential from the platform keystore.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub fn get_password(service: &str, account: &str) -> Result<String, KeychainError> {
    keyring::Entry::new(service, account)
        .map_err(|err| map_keyring_error(service, account, err))?
        .get_password()
        .map_err(|err| map_keyring_error(service, account, err))
}

/// Persist a credential into the platform keystore.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub fn set_password(service: &str, account: &str, password: &str) -> Result<(), KeychainError> {
    keyring::Entry::new(service, account)
        .map_err(|err| map_keyring_error(service, account, err))?
        .set_password(password)
        .map_err(|err| map_keyring_error(service, account, err))
}

/// Remove a credential from the platform keystore. Idempotent: a
/// missing entry returns `NoEntry` rather than panicking.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub fn delete_password(service: &str, account: &str) -> Result<(), KeychainError> {
    keyring::Entry::new(service, account)
        .map_err(|err| map_keyring_error(service, account, err))?
        .delete_credential()
        .map_err(|err| map_keyring_error(service, account, err))
}

/// Whether the platform keystore is currently reachable. Lightweight
/// probe used by `spotuify doctor` and the Phase 11 fallback gate.
pub fn is_available() -> bool {
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    {
        // Roundtrip a known-absent entry. NoEntry == reachable;
        // Unavailable == backend down.
        match get_password("__spotuify_probe__", "__spotuify_probe__") {
            Ok(_) => true,
            Err(err) => !matches!(err, KeychainError::Unavailable(_)),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        false
    }
}

// Stubs for other unix-likes (BSDs etc.) so the crate compiles on
// every target. Real BSD / illumos support would route through the
// planned encrypted-file fallback.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub fn get_password(_service: &str, _account: &str) -> Result<String, KeychainError> {
    Err(KeychainError::Unavailable(
        "no native keychain on this platform; build with file-fallback".into(),
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub fn set_password(_service: &str, _account: &str, _password: &str) -> Result<(), KeychainError> {
    Err(KeychainError::Unavailable(
        "no native keychain on this platform; build with file-fallback".into(),
    ))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub fn delete_password(_service: &str, _account: &str) -> Result<(), KeychainError> {
    Err(KeychainError::Unavailable(
        "no native keychain on this platform; build with file-fallback".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_entry_error_classifies_correctly() {
        let err = KeychainError::NoEntry {
            service: "spotuify".into(),
            account: "spotify".into(),
        };
        assert!(err.is_no_entry());
        assert!(err.to_string().contains("spotuify"));
        assert!(err.to_string().contains("spotify"));
    }

    #[test]
    fn unavailable_does_not_classify_as_no_entry() {
        assert!(!KeychainError::Unavailable("dbus down".into()).is_no_entry());
    }
}
