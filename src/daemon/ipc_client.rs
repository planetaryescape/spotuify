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
        .map_err(|_| anyhow::anyhow!("IPC request timed out after {}", format_timeout(duration)))?
    }
}

fn format_timeout(duration: Duration) -> String {
    if duration.as_millis() < 1_000 {
        format!("{}ms", duration.as_millis())
    } else {
        format!("{}s", duration.as_secs())
    }
}

fn describe_ipc_failure(message: &str) -> String {
    if message.contains("unknown variant") || message.contains("missing field") {
        format!("IPC protocol mismatch: {message}. Restart the daemon after upgrading.")
    } else {
        format!("IPC error: {message}")
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures::{SinkExt, StreamExt};
    use tempfile::TempDir;
    use tokio::net::UnixListener;
    use tokio_util::codec::Framed;

    use super::IpcClient;
    use crate::protocol::{
        DaemonEvent, IpcCodec, IpcMessage, IpcPayload, Request, Response, ResponseData,
    };

    #[tokio::test]
    async fn request_with_timeout_returns_actionable_error_when_daemon_stalls() {
        let temp = TempDir::new().unwrap();
        let socket = temp.path().join("stall.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
        });
        let mut client = IpcClient::connect_to(&socket).await.unwrap();

        let err = client
            .request_with_timeout(Request::Ping, Duration::from_millis(20))
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("IPC request timed out after 20ms"),
            "timeout should be surfaced to callers, got {err:#}"
        );
    }

    #[tokio::test]
    async fn request_ignores_events_until_matching_response_arrives() {
        let temp = TempDir::new().unwrap();
        let socket = temp.path().join("events.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut framed = Framed::new(stream, IpcCodec::new());
            let request = framed.next().await.unwrap().unwrap();
            framed
                .send(IpcMessage {
                    id: 0,
                    payload: IpcPayload::Event(DaemonEvent::ShutdownRequested),
                })
                .await
                .unwrap();
            framed
                .send(IpcMessage {
                    id: request.id,
                    payload: IpcPayload::Response(Response::Ok {
                        data: ResponseData::Pong,
                    }),
                })
                .await
                .unwrap();
        });
        let mut client = IpcClient::connect_to(&socket).await.unwrap();

        let response = client
            .request_with_timeout(Request::Ping, Duration::from_secs(1))
            .await
            .unwrap();

        assert!(matches!(
            response,
            Response::Ok {
                data: ResponseData::Pong
            }
        ));
    }
}
