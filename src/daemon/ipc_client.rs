use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{bail, Result};
use futures::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio::time::timeout;
use tokio_util::codec::Framed;

use crate::daemon::state::DaemonState;
use crate::protocol::{IpcCodec, IpcMessage, IpcPayload, Request, Response};

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub struct IpcClient {
    framed: Framed<UnixStream, IpcCodec>,
    next_id: AtomicU64,
}

impl IpcClient {
    pub async fn connect() -> Result<Self> {
        Self::connect_to(&DaemonState::socket_path()).await
    }

    pub async fn connect_to(socket_path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(socket_path).await.map_err(|err| {
            anyhow::anyhow!(
                "Cannot connect to daemon at {}: {}. Try: spotuify daemon start",
                socket_path.display(),
                err
            )
        })?;
        Ok(Self {
            framed: Framed::new(stream, IpcCodec::new()),
            next_id: AtomicU64::new(1),
        })
    }

    pub async fn request(&mut self, request: Request) -> Result<Response> {
        self.request_with_timeout(request, DEFAULT_REQUEST_TIMEOUT)
            .await
    }

    pub async fn request_with_timeout(
        &mut self,
        request: Request,
        duration: Duration,
    ) -> Result<Response> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.framed
            .send(IpcMessage {
                id,
                payload: IpcPayload::Request(request),
            })
            .await?;

        timeout(duration, async {
            loop {
                match self.framed.next().await {
                    Some(Ok(message)) => match message.payload {
                        IpcPayload::Response(response) if message.id == id => return Ok(response),
                        IpcPayload::Event(_event) => {}
                        IpcPayload::Response(_) => bail!(
                            "IPC protocol error: received response id {} while waiting for {id}",
                            message.id
                        ),
                        IpcPayload::Request(_) => bail!(
                            "IPC protocol error: received request while waiting for response {id}"
                        ),
                    },
                    Some(Err(err)) => bail!("{}", describe_ipc_failure(&err.to_string())),
                    None => bail!(
                        "Connection closed. Restart the daemon after upgrading: spotuify daemon restart"
                    ),
                }
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("IPC request timed out after {}s", duration.as_secs()))?
    }
}

fn describe_ipc_failure(message: &str) -> String {
    if message.contains("unknown variant") || message.contains("missing field") {
        format!("IPC protocol mismatch: {message}. Restart the daemon after upgrading.")
    } else {
        format!("IPC error: {message}")
    }
}
