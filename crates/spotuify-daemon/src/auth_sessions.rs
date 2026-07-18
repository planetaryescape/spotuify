use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use spotuify_core::{now_ms, ProviderError, ProviderId};
use spotuify_protocol::{
    AuthCredentialKind, AuthCredentialStatus, AuthLogoutData, AuthMethodData, AuthSessionData,
    AuthSessionId, AuthSessionState, AuthStatusData, AuthStrategyData,
};
use tokio::sync::{watch, Mutex};

use crate::state::DaemonState;

pub(crate) const AUTH_SESSION_TTL: Duration = Duration::from_secs(5 * 60);

struct AuthSessionEntry {
    data: Arc<RwLock<AuthSessionData>>,
    cancel: watch::Sender<bool>,
    lifecycle: Arc<AtomicU8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum AuthLifecycle {
    Active = 0,
    Committing = 1,
    Terminal = 2,
}

impl AuthLifecycle {
    fn load(value: &AtomicU8) -> Self {
        match value.load(Ordering::Acquire) {
            0 => Self::Active,
            1 => Self::Committing,
            _ => Self::Terminal,
        }
    }
}

#[derive(Default)]
pub(crate) struct AuthSessions {
    entries: Mutex<HashMap<AuthSessionId, AuthSessionEntry>>,
    mutation: Arc<Mutex<()>>,
}

impl AuthSessions {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Serialize daemon auth operations which can mint, commit, or revoke
    /// credentials. If logout wins this guard, later bearer requests observe
    /// `auth_required`; if minting wins, logout waits and purges afterward.
    pub(crate) async fn operation_guard(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.mutation.lock().await
    }

    /// Block new auth commits while runtime config is installed and cancel
    /// sessions whose OAuth settings came from the superseded snapshot.
    pub(crate) async fn config_reload_guard(&self) -> tokio::sync::MutexGuard<'_, ()> {
        let guard = self.mutation.lock().await;
        let mut entries = self.entries.lock().await;
        prune_expired_terminals(&mut entries);
        for entry in entries.values() {
            if transition_active_to_terminal(
                &entry.data,
                &entry.lifecycle,
                AuthSessionState::Cancelled,
            ) {
                let _ = entry.cancel.send(true);
            }
        }
        drop(entries);
        guard
    }

    #[cfg(test)]
    pub(crate) fn operation_in_progress(&self) -> bool {
        self.mutation.try_lock().is_err()
    }

    pub(crate) async fn start(
        &self,
        daemon: Arc<DaemonState>,
        provider: Option<String>,
        method: Option<String>,
    ) -> anyhow::Result<AuthSessionData> {
        let target = daemon.configured_auth_target(provider.as_deref()).await?;
        let reload_provider = target.provider_id.clone();
        let provider_id = target.provider_id.clone();
        let provider = provider_id.to_string();
        let _mutation = self.mutation.lock().await;
        if target.strategy == crate::provider_factory::ProviderAuthStrategy::None {
            validate_no_auth_method(&provider, method.as_deref())?;
            let data = new_session_data(
                provider_id,
                "none".to_string(),
                AuthSessionState::Authorized,
            );
            let (cancel, _cancel_rx) = watch::channel(false);
            let mut entries = self.entries.lock().await;
            prune_expired_terminals(&mut entries);
            entries.insert(
                data.session_id,
                AuthSessionEntry {
                    data: Arc::new(RwLock::new(data.clone())),
                    cancel,
                    lifecycle: Arc::new(AtomicU8::new(AuthLifecycle::Terminal as u8)),
                },
            );
            return Ok(data);
        }

        let method = resolve_method(&provider, method.as_deref())?;
        let accepted = daemon.accepted_provider_config().await?;
        let mut config = match method {
            AuthMethod::DevApp => {
                let provider_id = spotuify_core::ProviderId::new(provider.clone())
                    .map_err(anyhow::Error::from)?;
                let entry = accepted.provider(&provider_id).ok_or_else(|| {
                    invalid_input(
                        "provider",
                        format!("provider `{provider}` is not configured"),
                    )
                })?;
                spotuify_spotify::config::provider_config_from_table(
                    entry.raw_table(),
                    accepted.path.clone(),
                )
                .map_err(|error| anyhow::anyhow!("failed to load Spotify config: {error}"))?
            }
            AuthMethod::FirstParty => {
                spotuify_spotify::config::first_party_auth_config(accepted.path.clone())
            }
        };
        let method_label = method.label().to_string();
        let mut entries = self.entries.lock().await;
        prune_expired_terminals(&mut entries);
        ensure_no_active_session(&entries, &provider_id)?;

        let credential_provider = provider.clone();
        let data = Arc::new(RwLock::new(new_session_data(
            provider_id,
            method_label,
            AuthSessionState::Starting,
        )));
        let session_id = data.read().session_id;
        let lifecycle = Arc::new(AtomicU8::new(AuthLifecycle::Active as u8));
        let (cancel, cancel_rx) = watch::channel(false);
        entries.insert(
            session_id,
            AuthSessionEntry {
                data: data.clone(),
                cancel,
                lifecycle: lifecycle.clone(),
            },
        );
        drop(entries);

        let mutation = self.mutation.clone();
        let task_data = data.clone();
        tokio::spawn(async move {
            let data = task_data;
            let progress_data = data.clone();
            let progress_lifecycle = lifecycle.clone();
            let progress = move |event| apply_progress(&progress_data, &progress_lifecycle, event);
            let result = match method {
                AuthMethod::DevApp => spotuify_spotify::auth::authorize_token(
                    &config,
                    spotuify_spotify::auth::SCOPES,
                    cancel_rx,
                    progress,
                )
                .await
                .map(AuthCredential::DevApp),
                AuthMethod::FirstParty => {
                    config.client_id = spotuify_spotify::config::KEYMASTER_CLIENT_ID.to_string();
                    config.redirect_uri = "http://127.0.0.1:8898/login".to_string();
                    spotuify_spotify::auth::authorize_token(
                        &config,
                        spotuify_spotify::auth::FIRST_PARTY_SCOPES,
                        cancel_rx,
                        progress,
                    )
                    .await
                    .map(AuthCredential::FirstParty)
                }
            };

            match result {
                Ok(credential) => {
                    let _mutation = mutation.lock().await;
                    if !claim_commit(&lifecycle) {
                        return;
                    }
                    let save =
                        tokio::task::spawn_blocking(move || credential.save(&credential_provider))
                            .await;
                    match save {
                        Ok(Ok(())) => match daemon.reload_auth(Some(&reload_provider)).await {
                            Ok(()) => {
                                finish_commit(&data, &lifecycle, AuthSessionState::Authorized)
                            }
                            Err(error) => finish_commit(
                                &data,
                                &lifecycle,
                                AuthSessionState::Failed {
                                    message: error.to_string(),
                                },
                            ),
                        },
                        Ok(Err(error)) => finish_commit(
                            &data,
                            &lifecycle,
                            AuthSessionState::Failed {
                                message: error.to_string(),
                            },
                        ),
                        Err(error) => finish_commit(
                            &data,
                            &lifecycle,
                            AuthSessionState::Failed {
                                message: format!("auth persistence task failed: {error}"),
                            },
                        ),
                    }
                }
                Err(error) => {
                    transition_active_to_terminal(
                        &data,
                        &lifecycle,
                        AuthSessionState::Failed {
                            message: error.to_string(),
                        },
                    );
                }
            }
        });

        let snapshot = data.read().clone();
        Ok(snapshot)
    }

    pub(crate) async fn status(
        &self,
        daemon: Arc<DaemonState>,
        provider: Option<ProviderId>,
    ) -> anyhow::Result<AuthStatusData> {
        let target = daemon
            .configured_auth_target(provider.as_ref().map(ProviderId::as_str))
            .await?;
        let _mutation = self.mutation.lock().await;
        let strategy = match target.strategy {
            crate::provider_factory::ProviderAuthStrategy::None => AuthStrategyData::None,
            crate::provider_factory::ProviderAuthStrategy::SpotifyOauth => {
                AuthStrategyData::SpotifyOauth
            }
        };
        let provider = target.provider_id.to_string();
        let preferred_method =
            if target.strategy == crate::provider_factory::ProviderAuthStrategy::SpotifyOauth {
                Some(preferred_method(&provider).as_data())
            } else {
                None
            };
        let credentials =
            if target.strategy == crate::provider_factory::ProviderAuthStrategy::SpotifyOauth {
                let provider = provider.clone();
                let inventory = tokio::task::spawn_blocking(move || {
                    spotuify_spotify::auth::credential_inventory_for(&provider)
                })
                .await
                .map_err(|error| anyhow::anyhow!("auth status task failed: {error}"))??;
                credential_statuses(inventory)
            } else {
                Vec::new()
            };
        Ok(AuthStatusData {
            provider: target.provider_id,
            strategy,
            preferred_method,
            auth_required: target.strategy
                == crate::provider_factory::ProviderAuthStrategy::SpotifyOauth
                && daemon.auth_required(),
            auth_revoked: target.strategy
                == crate::provider_factory::ProviderAuthStrategy::SpotifyOauth
                && daemon.auth_revoked(),
            credentials,
        })
    }

    pub(crate) async fn logout(
        &self,
        daemon: Arc<DaemonState>,
        provider: Option<ProviderId>,
    ) -> anyhow::Result<AuthLogoutData> {
        let target = daemon
            .configured_auth_target(provider.as_ref().map(ProviderId::as_str))
            .await?;
        let provider_id = target.provider_id.clone();
        let provider = target.provider_id.to_string();
        let _mutation = self.mutation.lock().await;

        // No worker can newly claim Committing while this guard is held.
        // A worker which won first completed before this lock was acquired.
        let mut entries = self.entries.lock().await;
        prune_expired_terminals(&mut entries);
        for entry in entries.values() {
            if entry.data.read().provider == provider_id
                && transition_active_to_terminal(
                    &entry.data,
                    &entry.lifecycle,
                    AuthSessionState::Cancelled,
                )
            {
                let _ = entry.cancel.send(true);
            }
        }
        drop(entries);

        if target.strategy == crate::provider_factory::ProviderAuthStrategy::None {
            return Ok(AuthLogoutData {
                provider: provider_id,
                removed_dev_app: false,
                removed_first_party: false,
                removed_librespot: false,
                auth_required: false,
            });
        }

        let purge_provider = provider.clone();
        let purged = tokio::task::spawn_blocking(move || {
            spotuify_spotify::auth::purge_all_credentials_for(&purge_provider)
        })
        .await
        .map_err(|error| anyhow::anyhow!("auth logout task failed: {error}"))??;
        daemon.finish_logout(&provider_id).await?;
        Ok(AuthLogoutData {
            provider: provider_id,
            removed_dev_app: purged.removed_dev_app,
            removed_first_party: purged.removed_first_party,
            removed_librespot: purged.removed_librespot,
            auth_required: true,
        })
    }

    pub(crate) async fn poll(&self, session_id: AuthSessionId) -> anyhow::Result<AuthSessionData> {
        let mut entries = self.entries.lock().await;
        prune_expired_terminals(&mut entries);
        let entry = entries
            .get(&session_id)
            .ok_or_else(|| auth_session_not_found(session_id))?;
        expire_if_needed(entry);
        let snapshot = entry.data.read().clone();
        Ok(snapshot)
    }

    pub(crate) async fn cancel(
        &self,
        session_id: AuthSessionId,
    ) -> anyhow::Result<AuthSessionData> {
        let mut entries = self.entries.lock().await;
        prune_expired_terminals(&mut entries);
        let entry = entries
            .get(&session_id)
            .ok_or_else(|| auth_session_not_found(session_id))?;
        // Active -> Terminal is the cancellation barrier. Once the worker has
        // claimed Committing it owns credential persistence, so cancellation
        // must return the current non-Cancelled snapshot and let that commit
        // publish Authorized/Failed. This prevents a Cancelled snapshot from
        // ever coexisting with a credential write.
        if transition_active_to_terminal(&entry.data, &entry.lifecycle, AuthSessionState::Cancelled)
        {
            let _ = entry.cancel.send(true);
        }
        let snapshot = entry.data.read().clone();
        Ok(snapshot)
    }
}

#[derive(Clone, Copy, Debug)]
enum AuthMethod {
    DevApp,
    // The non-embedded build keeps the wire/config branch so it can return a
    // typed Unsupported error, but can never construct this successful mode.
    #[cfg_attr(not(feature = "embedded-playback"), allow(dead_code))]
    FirstParty,
}

impl AuthMethod {
    fn label(self) -> &'static str {
        match self {
            Self::DevApp => "dev_app",
            Self::FirstParty => "first_party",
        }
    }

    fn as_data(self) -> AuthMethodData {
        match self {
            Self::DevApp => AuthMethodData::DevApp,
            Self::FirstParty => AuthMethodData::FirstParty,
        }
    }
}

fn resolve_method(provider: &str, requested: Option<&str>) -> anyhow::Result<AuthMethod> {
    match requested {
        Some("dev_app" | "dev-app") => Ok(AuthMethod::DevApp),
        Some("first_party" | "first-party") => first_party_method(),
        Some(other) => Err(invalid_input(
            "method",
            format!("unsupported Spotify auth method `{other}`"),
        )),
        None => match preferred_method(provider) {
            AuthMethod::DevApp => Ok(AuthMethod::DevApp),
            AuthMethod::FirstParty => first_party_method(),
        },
    }
}

fn preferred_method(provider: &str) -> AuthMethod {
    match spotuify_spotify::config::first_party_env_override() {
        Some(true) => AuthMethod::FirstParty,
        Some(false) => AuthMethod::DevApp,
        None if spotuify_spotify::auth::stored_first_party_only_for(provider) => {
            AuthMethod::FirstParty
        }
        None => AuthMethod::DevApp,
    }
}

fn validate_no_auth_method(provider: &str, requested: Option<&str>) -> anyhow::Result<()> {
    match requested {
        None | Some("none") => Ok(()),
        Some(method) => Err(invalid_input(
            "method",
            format!("provider `{provider}` does not use auth method `{method}`"),
        )),
    }
}

#[cfg(feature = "embedded-playback")]
fn first_party_method() -> anyhow::Result<AuthMethod> {
    Ok(AuthMethod::FirstParty)
}

#[cfg(not(feature = "embedded-playback"))]
fn first_party_method() -> anyhow::Result<AuthMethod> {
    Err(ProviderError::Unsupported {
        operation: "first_party authentication requires embedded playback".to_string(),
    }
    .into())
}

enum AuthCredential {
    DevApp(spotuify_spotify::auth::StoredToken),
    FirstParty(spotuify_spotify::auth::StoredToken),
}

impl AuthCredential {
    fn save(self, provider: &str) -> anyhow::Result<()> {
        match self {
            Self::DevApp(token) => spotuify_spotify::auth::save_dev_app_token_for(provider, &token)
                .map_err(anyhow::Error::from),
            Self::FirstParty(token) => {
                let credentials = spotuify_spotify::first_party::FirstPartyCredentials::new(
                    token.refresh_token,
                    token
                        .scope
                        .split_whitespace()
                        .map(ToString::to_string)
                        .collect(),
                );
                spotuify_spotify::auth::save_first_party_credentials_for(provider, &credentials)
                    .map_err(anyhow::Error::from)
            }
        }
    }
}

fn apply_progress(
    data: &Arc<RwLock<AuthSessionData>>,
    lifecycle: &Arc<AtomicU8>,
    event: spotuify_spotify::auth::LoginProgress,
) {
    use spotuify_spotify::auth::LoginProgress;
    match event {
        LoginProgress::OpeningBrowser {
            auth_url,
            redirect_uri,
        } => set_active_state(
            data,
            lifecycle,
            AuthSessionState::AwaitingUser {
                authorization_url: auth_url,
                redirect_uri,
                browser_error: None,
            },
        ),
        LoginProgress::BrowserLaunchFailed {
            auth_url,
            redirect_uri,
            error,
        } => set_active_state(
            data,
            lifecycle,
            AuthSessionState::AwaitingUser {
                authorization_url: auth_url,
                redirect_uri,
                browser_error: Some(error),
            },
        ),
        LoginProgress::WaitingForCallback => {
            let current = data.read().state.clone();
            if let AuthSessionState::AwaitingUser {
                authorization_url,
                redirect_uri,
                browser_error,
            } = current
            {
                set_active_state(
                    data,
                    lifecycle,
                    AuthSessionState::Waiting {
                        authorization_url,
                        redirect_uri,
                        browser_error,
                    },
                );
            }
        }
        LoginProgress::Saved => {}
    }
}

fn set_active_state(
    data: &Arc<RwLock<AuthSessionData>>,
    lifecycle: &AtomicU8,
    state: AuthSessionState,
) {
    if AuthLifecycle::load(lifecycle) != AuthLifecycle::Active {
        return;
    }
    let mut data = data.write();
    if AuthLifecycle::load(lifecycle) == AuthLifecycle::Active {
        data.state = state;
    }
}

fn claim_commit(lifecycle: &AtomicU8) -> bool {
    lifecycle
        .compare_exchange(
            AuthLifecycle::Active as u8,
            AuthLifecycle::Committing as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
}

fn transition_active_to_terminal(
    data: &Arc<RwLock<AuthSessionData>>,
    lifecycle: &AtomicU8,
    state: AuthSessionState,
) -> bool {
    if lifecycle
        .compare_exchange(
            AuthLifecycle::Active as u8,
            AuthLifecycle::Terminal as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return false;
    }
    write_terminal_state(data, state);
    true
}

fn finish_commit(
    data: &Arc<RwLock<AuthSessionData>>,
    lifecycle: &AtomicU8,
    state: AuthSessionState,
) {
    if lifecycle
        .compare_exchange(
            AuthLifecycle::Committing as u8,
            AuthLifecycle::Terminal as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
    {
        write_terminal_state(data, state);
    }
}

fn write_terminal_state(data: &Arc<RwLock<AuthSessionData>>, state: AuthSessionState) {
    debug_assert!(state.is_terminal());
    let mut data = data.write();
    data.expires_at_ms = terminal_expiry_ms();
    data.state = state;
}

fn expire_if_needed(entry: &AuthSessionEntry) {
    let expired = {
        let data = entry.data.read();
        now_ms() > data.expires_at_ms && !data.state.is_terminal()
    };
    if expired
        && transition_active_to_terminal(
            &entry.data,
            &entry.lifecycle,
            AuthSessionState::Failed {
                message: "authentication session expired".to_string(),
            },
        )
    {
        let _ = entry.cancel.send(true);
    }
}

fn terminal_expiry_ms() -> i64 {
    now_ms().saturating_add(AUTH_SESSION_TTL.as_millis() as i64)
}

fn new_session_data(
    provider: ProviderId,
    method: String,
    state: AuthSessionState,
) -> AuthSessionData {
    let created_at_ms = now_ms();
    AuthSessionData {
        session_id: AuthSessionId::new_v7(),
        provider,
        method,
        state,
        created_at_ms,
        expires_at_ms: created_at_ms.saturating_add(AUTH_SESSION_TTL.as_millis() as i64),
    }
}

fn prune_expired_terminals(entries: &mut HashMap<AuthSessionId, AuthSessionEntry>) {
    let now = now_ms();
    entries.retain(|_, entry| {
        let data = entry.data.read();
        !(data.state.is_terminal() && now > data.expires_at_ms)
    });
}

fn active_session(
    entries: &HashMap<AuthSessionId, AuthSessionEntry>,
    provider: &ProviderId,
) -> Option<AuthSessionData> {
    entries.values().find_map(|entry| {
        let data = entry.data.read();
        (&data.provider == provider
            && AuthLifecycle::load(&entry.lifecycle) != AuthLifecycle::Terminal)
            .then(|| data.clone())
    })
}

fn ensure_no_active_session(
    entries: &HashMap<AuthSessionId, AuthSessionEntry>,
    provider: &ProviderId,
) -> anyhow::Result<()> {
    let Some(existing) = active_session(entries, provider) else {
        return Ok(());
    };
    Err(invalid_input(
        "auth_session",
        format!(
            "authentication already in progress for provider `{provider}` method `{}` (session {})",
            existing.method, existing.session_id
        ),
    ))
}

fn credential_statuses(
    inventory: spotuify_spotify::auth::CredentialInventory,
) -> Vec<AuthCredentialStatus> {
    let dev_app = inventory.dev_app;
    let first_party = inventory.first_party;
    vec![
        AuthCredentialStatus {
            kind: AuthCredentialKind::DevApp,
            present: dev_app.is_some(),
            expires_at_ms: dev_app.as_ref().map(|credential| {
                i64::try_from(credential.expires_at)
                    .unwrap_or(i64::MAX)
                    .saturating_mul(1_000)
            }),
            scopes: dev_app
                .as_ref()
                .map_or_else(Vec::new, |credential| credential.scopes.clone()),
            missing_scopes: dev_app.map_or_else(Vec::new, |credential| credential.missing_scopes),
        },
        AuthCredentialStatus {
            kind: AuthCredentialKind::FirstParty,
            present: first_party.is_some(),
            expires_at_ms: None,
            scopes: first_party.map_or_else(Vec::new, |credential| credential.scopes),
            missing_scopes: Vec::new(),
        },
    ]
}

fn invalid_input(field: &str, message: String) -> anyhow::Error {
    ProviderError::InvalidInput {
        field: field.to_string(),
        message,
    }
    .into()
}

fn auth_session_not_found(session_id: AuthSessionId) -> anyhow::Error {
    ProviderError::NotFound {
        resource: format!("auth-session:{session_id}"),
    }
    .into()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn entry(state: AuthSessionState, expires_at_ms: i64) -> AuthSessionEntry {
        let (cancel, _rx) = watch::channel(false);
        let lifecycle = if state.is_terminal() {
            AuthLifecycle::Terminal
        } else {
            AuthLifecycle::Active
        };
        AuthSessionEntry {
            data: Arc::new(RwLock::new(AuthSessionData {
                session_id: AuthSessionId::new_v7(),
                provider: ProviderId::new("spotify").unwrap(),
                method: "dev_app".to_string(),
                state,
                created_at_ms: 0,
                expires_at_ms,
            })),
            cancel,
            lifecycle: Arc::new(AtomicU8::new(lifecycle as u8)),
        }
    }

    #[test]
    fn expired_running_session_fails_and_cancels() {
        let entry = entry(AuthSessionState::Starting, now_ms() - 1);
        let mut cancelled = entry.cancel.subscribe();
        expire_if_needed(&entry);
        assert!(*cancelled.borrow_and_update());
        assert!(matches!(
            &entry.data.read().state,
            AuthSessionState::Failed { .. }
        ));
    }

    #[test]
    fn terminal_session_is_not_rewritten_by_late_progress() {
        let entry = entry(AuthSessionState::Cancelled, now_ms() + 1_000);
        set_active_state(&entry.data, &entry.lifecycle, AuthSessionState::Authorized);
        assert!(matches!(
            &entry.data.read().state,
            AuthSessionState::Cancelled
        ));
    }

    #[test]
    fn active_session_conflict_is_detected() {
        let active = entry(
            AuthSessionState::Waiting {
                authorization_url: "https://example.test/auth".to_string(),
                redirect_uri: "http://127.0.0.1/callback".to_string(),
                browser_error: None,
            },
            now_ms() + 1_000,
        );
        let expected = active.data.read().session_id;
        let entries = HashMap::from([(expected, active)]);

        let provider = ProviderId::new("spotify").unwrap();
        let error = ensure_no_active_session(&entries, &provider)
            .expect_err("second client must not share a cancellable session");

        assert!(error.to_string().contains(&expected.to_string()));
        assert!(matches!(
            error.downcast_ref::<ProviderError>(),
            Some(ProviderError::InvalidInput { field, .. }) if field == "auth_session"
        ));
    }

    #[test]
    fn unknown_auth_method_is_typed_invalid_input() {
        let error = resolve_method("spotify", Some("password"))
            .expect_err("unsupported method should fail");
        assert!(matches!(
            error.downcast_ref::<ProviderError>(),
            Some(ProviderError::InvalidInput { field, .. }) if field == "method"
        ));
    }

    #[test]
    fn no_auth_provider_method_matrix_accepts_only_none() {
        validate_no_auth_method("fake", None).expect("omitted method is valid");
        validate_no_auth_method("fake", Some("none")).expect("none method is valid");

        let error = validate_no_auth_method("fake", Some("dev_app"))
            .expect_err("OAuth method must be rejected for a no-auth provider");
        assert!(matches!(
            error.downcast_ref::<ProviderError>(),
            Some(ProviderError::InvalidInput { field, .. }) if field == "method"
        ));
    }

    #[tokio::test]
    async fn unknown_poll_and_cancel_are_typed_not_found() {
        let sessions = AuthSessions::new();
        let session_id = AuthSessionId::new_v7();
        for error in [
            sessions
                .poll(session_id)
                .await
                .expect_err("unknown poll must fail"),
            sessions
                .cancel(session_id)
                .await
                .expect_err("unknown cancel must fail"),
        ] {
            assert!(matches!(
                error.downcast_ref::<ProviderError>(),
                Some(ProviderError::NotFound { resource })
                    if resource == &format!("auth-session:{session_id}")
            ));
        }
    }

    #[cfg(not(feature = "embedded-playback"))]
    #[test]
    fn first_party_requires_embedded_playback() {
        let error = resolve_method("spotify", Some("first_party"))
            .expect_err("first-party auth must be unavailable without embedded playback");
        assert!(matches!(
            error.downcast_ref::<ProviderError>(),
            Some(ProviderError::Unsupported { .. })
        ));
    }

    #[test]
    fn cancel_claimed_before_commit_prevents_persistence() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Barrier;

        let entry = entry(
            AuthSessionState::Waiting {
                authorization_url: "https://example.test/auth".to_string(),
                redirect_uri: "http://127.0.0.1/callback".to_string(),
                browser_error: None,
            },
            now_ms() + 1_000,
        );
        let barrier = Arc::new(Barrier::new(2));
        let persisted = Arc::new(AtomicBool::new(false));
        let worker_lifecycle = entry.lifecycle.clone();
        let worker_barrier = barrier.clone();
        let worker_persisted = persisted.clone();
        let worker = std::thread::spawn(move || {
            worker_barrier.wait();
            if claim_commit(&worker_lifecycle) {
                worker_persisted.store(true, Ordering::Release);
            }
        });

        assert!(transition_active_to_terminal(
            &entry.data,
            &entry.lifecycle,
            AuthSessionState::Cancelled,
        ));
        barrier.wait();
        worker.join().expect("worker joins");

        assert!(!persisted.load(Ordering::Acquire));
        assert!(matches!(
            &entry.data.read().state,
            AuthSessionState::Cancelled
        ));
    }

    #[test]
    fn cancel_after_commit_claim_never_reports_cancelled() {
        use std::sync::Barrier;

        let entry = entry(
            AuthSessionState::Waiting {
                authorization_url: "https://example.test/auth".to_string(),
                redirect_uri: "http://127.0.0.1/callback".to_string(),
                browser_error: None,
            },
            now_ms() + 1_000,
        );
        let claimed = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let worker_data = entry.data.clone();
        let worker_lifecycle = entry.lifecycle.clone();
        let worker_claimed = claimed.clone();
        let worker_release = release.clone();
        let worker = std::thread::spawn(move || {
            assert!(claim_commit(&worker_lifecycle));
            worker_claimed.wait();
            worker_release.wait();
            finish_commit(
                &worker_data,
                &worker_lifecycle,
                AuthSessionState::Authorized,
            );
        });

        claimed.wait();
        assert!(!transition_active_to_terminal(
            &entry.data,
            &entry.lifecycle,
            AuthSessionState::Cancelled,
        ));
        assert!(!matches!(
            &entry.data.read().state,
            AuthSessionState::Cancelled
        ));
        release.wait();
        worker.join().expect("worker joins");
        assert!(matches!(
            &entry.data.read().state,
            AuthSessionState::Authorized
        ));
    }

    #[tokio::test]
    async fn cancel_during_commit_returns_current_non_cancelled_snapshot() {
        let sessions = AuthSessions::new();
        let entry = entry(
            AuthSessionState::Waiting {
                authorization_url: "https://example.test/auth".to_string(),
                redirect_uri: "http://127.0.0.1/callback".to_string(),
                browser_error: None,
            },
            now_ms() + 1_000,
        );
        entry
            .lifecycle
            .store(AuthLifecycle::Committing as u8, Ordering::Release);
        let session_id = entry.data.read().session_id;
        sessions.entries.lock().await.insert(session_id, entry);

        let snapshot = sessions
            .cancel(session_id)
            .await
            .expect("committing session remains queryable");

        assert!(matches!(&snapshot.state, AuthSessionState::Waiting { .. }));
        assert!(!matches!(&snapshot.state, AuthSessionState::Cancelled));
    }

    #[tokio::test]
    async fn config_reload_guard_cancels_active_sessions_before_commit() {
        let sessions = AuthSessions::new();
        let active = entry(
            AuthSessionState::Waiting {
                authorization_url: "https://example.test/auth".to_string(),
                redirect_uri: "http://127.0.0.1/callback".to_string(),
                browser_error: None,
            },
            now_ms() + 1_000,
        );
        let mut cancelled = active.cancel.subscribe();
        let session_id = active.data.read().session_id;
        sessions.entries.lock().await.insert(session_id, active);

        let _reload = sessions.config_reload_guard().await;
        let snapshot = sessions
            .poll(session_id)
            .await
            .expect("session remains visible");

        assert!(*cancelled.borrow_and_update());
        assert!(matches!(snapshot.state, AuthSessionState::Cancelled));
    }
}
