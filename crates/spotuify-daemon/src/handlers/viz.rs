//! `viz` request handlers (split out of the dispatch god-function).

use std::sync::Arc;

use spotuify_protocol::{DaemonEvent, OperationSource, Request, ResponseData};

use crate::state::DaemonState;

pub(crate) async fn dispatch(
    state: Arc<DaemonState>,
    request: Request,
    source: Option<OperationSource>,
) -> anyhow::Result<ResponseData> {
    match request {
        Request::SetVizEnabled { enabled } => {
            state.viz_coordinator().set_enabled(enabled).await;
            Ok(ResponseData::Ack {
                message: format!(
                    "visualization {}",
                    if enabled { "enabled" } else { "disabled" }
                ),
            })
        }
        Request::SetVizSource { kind } => {
            state.viz_coordinator().set_source(kind).await;
            Ok(ResponseData::Ack {
                message: format!("visualization source set to {}", kind.as_str()),
            })
        }
        Request::SetVizStyle { style } => {
            if !spotuify_protocol::viz_style_is_known(&style) {
                anyhow::bail!(
                    "unknown visualizer style `{style}`; run `spotuify viz styles` for the list"
                );
            }
            // The style is a persisted preference, not runtime state, so it
            // goes through the config file. Clients then apply the fresh
            // preferences straight from the event — nothing was reloaded, so
            // this must not look like a config reload to them.
            let path = spotuify_config::ConfigPath::parse("viz.style")?;
            spotuify_config::set_config_value(&path, &style)?;
            state.emit_event(DaemonEvent::ClientPreferencesChanged {
                preferences: super::client_preferences()?,
            });
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
