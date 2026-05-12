use std::sync::Arc;

use crate::daemon::state::DaemonState;
use crate::protocol::{Request, Response, ResponseData};

pub(crate) async fn handle_request(state: Arc<DaemonState>, request: Request) -> Response {
    match dispatch(state, request).await {
        Ok(data) => Response::Ok { data },
        Err(err) => Response::error(err.to_string()),
    }
}

async fn dispatch(state: Arc<DaemonState>, request: Request) -> anyhow::Result<ResponseData> {
    match request {
        Request::Ping => Ok(ResponseData::Pong),
        Request::GetDaemonStatus => Ok(ResponseData::DaemonStatus {
            status: state.status(),
        }),
        Request::GetDoctorReport => Ok(ResponseData::DoctorReport {
            report: crate::diagnostics::collect_report(state.status()).await?,
        }),
        Request::Shutdown => {
            state.request_shutdown();
            Ok(ResponseData::Shutdown)
        }
    }
}
