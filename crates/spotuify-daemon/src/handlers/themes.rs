//! Terminal-theme request handlers.
//!
//! The daemon is the only process that reads theme files. Clients get the
//! finished [`spotuify_core::ThemeSpec`] — in the seed, in
//! `ClientPreferencesChanged`, and in `ThemesList` — so a client never
//! needs to know where the config directory is.

use std::sync::Arc;

use spotuify_protocol::{DaemonEvent, OperationSource, Request, ResponseData};

use crate::state::DaemonState;

pub(crate) async fn dispatch(
    state: Arc<DaemonState>,
    request: Request,
    _source: Option<OperationSource>,
) -> anyhow::Result<ResponseData> {
    match request {
        Request::ThemesList => {
            // Directory read: off the tokio workers, same as the config write.
            let (catalog, themes_dir) = tokio::task::spawn_blocking(|| {
                (
                    spotuify_config::load_themes(),
                    spotuify_config::themes_dir(),
                )
            })
            .await?;
            for warning in &catalog.warnings {
                tracing::warn!(%warning, "skipping unreadable theme file");
            }
            Ok(ResponseData::Themes {
                themes: catalog.themes,
                active: state.active_theme(),
                themes_dir: themes_dir.display().to_string(),
            })
        }
        Request::SetTheme { name } => {
            // Persisting is a locked, fsynced config write plus a directory
            // read, so it runs on the blocking pool. Validation happens
            // there too: it needs the same catalog the write validates on.
            let (preferences, theme) = tokio::task::spawn_blocking(move || {
                let catalog = spotuify_config::load_themes();
                // `get` canonicalises, so ` Winamp ` resolves the same file
                // `config set tui.theme " Winamp "` would write.
                let theme = catalog.get(&name).cloned().ok_or_else(|| {
                    anyhow::anyhow!(
                        "unknown theme `{name}`; expected one of {}",
                        catalog.names()
                    )
                })?;
                let path = spotuify_config::ConfigPath::parse("tui.theme")?;
                spotuify_config::set_config_value(&path, &theme.name)?;
                let preferences = super::client_preferences(theme.clone())?;
                anyhow::Ok((preferences, theme))
            })
            .await??;
            // Only after the write lands: the daemon's cached copy, then
            // every client, which applies the resolved spec straight from
            // the event without re-reading anything.
            let message = format!("theme set to {}", theme.name);
            state.set_active_theme(theme);
            state.emit_event(DaemonEvent::ClientPreferencesChanged { preferences });
            Ok(ResponseData::Ack { message })
        }
        _ => unreachable!("non-theme request routed to theme dispatcher"),
    }
}
