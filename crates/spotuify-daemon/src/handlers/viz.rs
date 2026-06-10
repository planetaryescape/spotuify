//! `viz` request handlers (split out of the dispatch god-function).

use std::sync::Arc;

use spotuify_protocol::{OperationSource, Request, ResponseData};

use crate::state::DaemonState;

pub(crate) async fn dispatch(
    state: Arc<DaemonState>,
    request: Request,
    _source: Option<OperationSource>,
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
        Request::GetVizStatus => Ok(ResponseData::VizStatus {
            diagnostics: state.viz_coordinator().diagnostics().await,
        }),
        Request::SetVizFocus { focused } => {
            state.viz_coordinator().set_focused(focused).await;
            Ok(ResponseData::Ack {
                message: format!("viz focus = {focused}"),
            })
        }
        _ => unreachable!("non-viz request routed to viz dispatcher"),
    }
}
