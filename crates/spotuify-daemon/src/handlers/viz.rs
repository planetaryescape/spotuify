//! `viz` request handlers (split out of the dispatch god-function).

use std::sync::Arc;

use spotuify_protocol::{DaemonEvent, OperationSource, Request, ResponseData};

use crate::state::DaemonState;

/// Write one `[viz]` key to the config file, off the tokio workers.
///
/// The write takes a file lock and fsyncs the file and its directory, so it
/// runs on the blocking pool. Shares `set-theme`'s lane: everything that
/// writes this file has the same interleaving failure mode.
async fn persist_viz_setting(
    state: &DaemonState,
    key: &'static str,
    value: String,
) -> anyhow::Result<()> {
    let _lane = state.preferences_write_guard().await;
    tokio::task::spawn_blocking(move || {
        let path = spotuify_config::ConfigPath::parse(key)?;
        spotuify_config::set_config_value(&path, &value)
    })
    .await??;
    Ok(())
}

pub(crate) async fn dispatch(
    state: Arc<DaemonState>,
    request: Request,
    source: Option<OperationSource>,
) -> anyhow::Result<ResponseData> {
    match request {
        Request::SetVizEnabled { enabled } => {
            // Persisted like the style: `viz enable` is a preference, not a
            // session toggle, and a daemon restart that silently reverted it
            // to `[viz] enabled` was the surprise.
            persist_viz_setting(&state, "viz.enabled", enabled.to_string()).await?;
            state.viz_coordinator().set_enabled(enabled).await;
            Ok(ResponseData::Ack {
                message: format!(
                    "visualization {}",
                    if enabled { "enabled" } else { "disabled" }
                ),
            })
        }
        Request::SetVizSource { kind } => {
            // `VizSourceKindData` is the canonical form already — the enum is
            // the validation — so `as_str` is what the config file gets.
            persist_viz_setting(&state, "viz.source", kind.as_str().to_string()).await?;
            state.viz_coordinator().set_source(kind).await;
            Ok(ResponseData::Ack {
                message: format!("visualization source set to {}", kind.as_str()),
            })
        }
        Request::SetVizStyle { style } => {
            // Accept what the config loader accepts: trim + lowercase, then
            // validate. Rejecting `Classic-Peak` here while `viz.style =
            // " Classic-Peak "` loads fine would be two different contracts
            // for the same setting.
            let style = spotuify_protocol::canonical_viz_style(&style).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown visualizer style `{style}`; run `spotuify viz styles` for the list"
                )
            })?;
            // The style is a persisted preference, not runtime state, so it
            // goes through the config file. That write takes a file lock and
            // fsyncs the file and its directory, so it runs on the blocking
            // pool rather than stalling a tokio worker for up to the lock
            // timeout.
            // Shares `set-theme`'s lane: both write the same config file and
            // broadcast the same `ClientPreferences`, so interleaving them
            // has the same failure mode.
            let _lane = state.preferences_write_guard().await;
            let theme = state.active_theme();
            let preferences = tokio::task::spawn_blocking(move || {
                let path = spotuify_config::ConfigPath::parse("viz.style")?;
                spotuify_config::set_config_value(&path, style)?;
                super::client_preferences(theme)
            })
            .await??;
            // Only after the write lands: the daemon's cached copy, and the
            // clients. Clients apply the fresh preferences straight from the
            // event — nothing was reloaded, so this must not look like a
            // config reload to them.
            state.viz_coordinator().set_style(style);
            state.emit_event(DaemonEvent::ClientPreferencesChanged { preferences });
            Ok(ResponseData::Ack {
                message: format!("visualization style set to {style}"),
            })
        }
        Request::GetVizStatus => Ok(ResponseData::VizStatus {
            diagnostics: state.viz_coordinator().diagnostics().await,
        }),
        Request::SetVizFocus { focused } => {
            // Vote per client kind: the unfocused TUI must not drop the
            // shared SpectrumFrame broadcast to 1 Hz for the macOS app
            // (or any other subscriber). Source-less clients (the macOS
            // app's raw socket) share the "unknown" bucket.
            let client = source.map_or("unknown", |s| s.label());
            state.viz_coordinator().set_focused(client, focused).await;
            Ok(ResponseData::Ack {
                message: format!("viz focus[{client}] = {focused}"),
            })
        }
        _ => unreachable!("non-viz request routed to viz dispatcher"),
    }
}
