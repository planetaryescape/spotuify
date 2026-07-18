//! Phase 8 — connect spotuify-mcp to a running spotuify daemon.
//!
//! Sends a typed `spotuify_protocol::Request` over the daemon IPC stream
//! and returns the `Response`. Used by `tools/call` to actually
//! execute mutations after the catalogue + confirm gating and
//! bridge translation.
//!
//! Async client; the rpc dispatch is sync because MCP is line-at-a-time.
//! The stdio loop wraps each tools/call in a tokio current-thread
//! runtime to bridge.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures::{SinkExt, StreamExt};
use spotuify_protocol::{
    default_socket_path as protocol_socket_path, IpcCodec, IpcMessage, IpcPayload, MutationId,
    OperationSource, Request, Response,
};
use tokio_util::codec::Framed;

/// Default daemon IPC address.
pub fn default_socket_path() -> PathBuf {
    protocol_socket_path()
}

// The daemon's default request deadline is 30 seconds. Leave enough margin
// for it to return a typed timeout instead of abandoning a still-running
// mutation and tempting the caller to retry with a new key.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(40);

/// Shorter deadline for provider-catalog discovery on the `tools/list` path.
/// MCP hosts initialize before the daemon is up, so a full [`REQUEST_TIMEOUT`]
/// block would stall client startup; `tools/list` fails open to the static
/// manifest instead of waiting.
pub const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug)]
enum PostSendCompatibilityKind {
    ResponseDecode,
    ClosedWithoutResponse,
}

#[derive(Debug)]
struct PostSendCompatibilityError {
    kind: PostSendCompatibilityKind,
    detail: String,
}

impl std::fmt::Display for PostSendCompatibilityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            PostSendCompatibilityKind::ResponseDecode => {
                write!(
                    f,
                    "daemon response was not wire-compatible: {}",
                    self.detail
                )
            }
            PostSendCompatibilityKind::ClosedWithoutResponse => {
                f.write_str("daemon closed the connection without responding")
            }
        }
    }
}

impl std::error::Error for PostSendCompatibilityError {}

/// True only after a request was connected and sent, then failed with the
/// legacy-daemon signatures used when an older decoder does not recognize a
/// newer request. Connection, send, and deadline failures remain hard errors.
pub(crate) fn is_post_send_compatibility_error(error: &anyhow::Error) -> bool {
    error.downcast_ref::<PostSendCompatibilityError>().is_some()
}

#[cfg(test)]
pub(crate) fn test_post_send_compatibility_error() -> anyhow::Error {
    anyhow::Error::new(PostSendCompatibilityError {
        kind: PostSendCompatibilityKind::ClosedWithoutResponse,
        detail: String::new(),
    })
}

#[cfg(test)]
fn test_post_send_decode_compatibility_error() -> anyhow::Error {
    anyhow::Error::new(PostSendCompatibilityError {
        kind: PostSendCompatibilityKind::ResponseDecode,
        detail: "missing field in legacy response".to_string(),
    })
}

/// Round-trip a single Request against the daemon.
///
/// Returns Err if the daemon isn't reachable (missing socket / hung).
/// Successful protocol exchanges -- including daemon-side error
/// envelopes -- come back as Ok(Response::Error { .. }).
pub async fn round_trip(socket_path: &Path, request: Request) -> Result<Response> {
    let mutation_id = request.requires_mutation_id().then(MutationId::new_v7);
    round_trip_with_mutation_id(socket_path, request, mutation_id).await
}

/// Round-trip a read-only request with a caller-chosen deadline.
///
/// Used by `tools/list` provider discovery, which needs a shorter timeout than
/// the default so an unreachable daemon doesn't stall client startup.
pub async fn round_trip_with_timeout(
    socket_path: &Path,
    request: Request,
    request_timeout: Duration,
) -> Result<Response> {
    let mutation_id = request.requires_mutation_id().then(MutationId::new_v7);
    round_trip_with_mutation_id_and_timeout(socket_path, request, mutation_id, request_timeout)
        .await
}

/// Round-trip a request with a caller-owned mutation key.
///
/// Callers that may retry must retain and reuse `mutation_id`; generating a
/// fresh key after a lost response can execute the provider write twice.
pub async fn round_trip_with_mutation_id(
    socket_path: &Path,
    request: Request,
    mutation_id: Option<MutationId>,
) -> Result<Response> {
    round_trip_with_mutation_id_and_timeout(socket_path, request, mutation_id, REQUEST_TIMEOUT)
        .await
}

async fn round_trip_with_mutation_id_and_timeout(
    socket_path: &Path,
    request: Request,
    mutation_id: Option<MutationId>,
    request_timeout: Duration,
) -> Result<Response> {
    let stream = tokio::time::timeout(
        Duration::from_secs(2),
        spotuify_protocol::ipc_stream::connect(socket_path),
    )
    .await
    .map_err(|_| anyhow!("timed out connecting to daemon IPC {socket_path:?}"))?
    .with_context(|| format!("connect to daemon IPC {socket_path:?}"))?;

    let mut framed = Framed::new(stream, IpcCodec::new());

    match (request.requires_mutation_id(), mutation_id.is_some()) {
        (true, false) => return Err(anyhow!("live mutation requires a caller-owned mutation id")),
        (false, true) => return Err(anyhow!("read-only request must not carry a mutation id")),
        _ => {}
    }
    let envelope = IpcMessage {
        id: 1,
        source: Some(OperationSource::Mcp),
        mutation_id,
        payload: IpcPayload::Request(request),
    };
    framed
        .send(envelope)
        .await
        .context("send Request over daemon IPC")?;

    let resp = tokio::time::timeout(request_timeout, framed.next())
        .await
        .map_err(|_| anyhow!("daemon did not respond within {request_timeout:?}"))?;

    match resp {
        Some(Ok(msg)) => match msg.payload {
            IpcPayload::Response(r) => Ok(r),
            other => Err(anyhow!(
                "daemon sent unexpected payload {:?}",
                payload_kind(&other)
            )),
        },
        Some(Err(err)) => Err(anyhow::Error::new(PostSendCompatibilityError {
            kind: PostSendCompatibilityKind::ResponseDecode,
            detail: err.to_string(),
        })),
        None => Err(anyhow::Error::new(PostSendCompatibilityError {
            kind: PostSendCompatibilityKind::ClosedWithoutResponse,
            detail: String::new(),
        })),
    }
}

fn payload_kind(p: &IpcPayload) -> &'static str {
    match p {
        IpcPayload::Request(_) => "request",
        IpcPayload::Response(_) => "response",
        IpcPayload::Event(_) => "event",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use futures::{SinkExt, StreamExt};
    use spotuify_protocol::{IpcMessage, IpcPayload, ResponseData};
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::sync::Mutex;
    use tokio_util::codec::Framed;

    // Process-wide env is shared across parallel cargo tests; serialise
    // the env-mutating socket-path tests through a single mutex.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn default_socket_path_honours_env_override() {
        let _g = ENV_LOCK.lock().expect("env lock should not be poisoned");
        std::env::set_var("SPOTUIFY_SOCKET", "/tmp/spotuify-test.sock");
        let p = default_socket_path();
        assert_eq!(p, PathBuf::from("/tmp/spotuify-test.sock"));
        std::env::remove_var("SPOTUIFY_SOCKET");
    }

    #[test]
    fn default_socket_path_uses_shared_runtime_resolver() {
        let _g = ENV_LOCK.lock().expect("env lock should not be poisoned");
        std::env::remove_var("SPOTUIFY_SOCKET");
        std::env::set_var("SPOTUIFY_RUNTIME_DIR", "/tmp/spotuify-runtime-test");
        let p = default_socket_path();
        #[cfg(unix)]
        assert_eq!(p, PathBuf::from("/tmp/spotuify-runtime-test/daemon.sock"));
        #[cfg(windows)]
        assert!(
            p.to_string_lossy().starts_with(r"\\.\pipe\"),
            "windows IPC should use a named-pipe address"
        );
        std::env::remove_var("SPOTUIFY_RUNTIME_DIR");
    }

    #[test]
    fn legacy_compatibility_classifier_rejects_connect_and_timeout_errors() {
        assert!(is_post_send_compatibility_error(
            &test_post_send_compatibility_error()
        ));
        assert!(is_post_send_compatibility_error(
            &test_post_send_decode_compatibility_error()
        ));
        assert!(!is_post_send_compatibility_error(&anyhow!(
            "connect to daemon IPC failed"
        )));
        assert!(!is_post_send_compatibility_error(&anyhow!(
            "daemon did not respond within 40s"
        )));
    }

    #[tokio::test]
    async fn timed_out_retry_reuses_mutation_id_and_applies_one_logical_write() {
        let unique = format!(
            "spotuify-mcp-retry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        #[cfg(unix)]
        let socket = std::env::temp_dir().join(format!("{unique}.sock"));
        #[cfg(windows)]
        let socket = PathBuf::from(format!(r"\\.\pipe\{unique}"));
        let mut listener = spotuify_protocol::ipc_stream::IpcListener::bind(&socket)
            .expect("bind test IPC listener");
        let writes = Arc::new(AtomicUsize::new(0));
        let writes_for_server = writes.clone();

        let server = tokio::spawn(async move {
            let mut seen = HashSet::new();
            for attempt in 0..2 {
                let stream = listener.accept().await.expect("accept MCP client");
                let mut framed = Framed::new(stream, IpcCodec::new());
                let message = framed
                    .next()
                    .await
                    .expect("client frame")
                    .expect("valid client frame");
                let mutation_id = message.mutation_id.expect("mutation id");
                if seen.insert(mutation_id) {
                    writes_for_server.fetch_add(1, Ordering::SeqCst);
                }
                if attempt == 0 {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                let _ = framed
                    .send(IpcMessage {
                        id: message.id,
                        source: None,
                        mutation_id: None,
                        payload: IpcPayload::Response(Response::Ok {
                            data: ResponseData::Pong,
                        }),
                    })
                    .await;
            }
        });

        let mutation_id: MutationId = "018f2f76-7c5d-7b1d-8000-000000000001".parse().unwrap();
        let request = Request::QueueAdd {
            uri: "spotify:track:one".to_string(),
        };
        let first = round_trip_with_mutation_id_and_timeout(
            &socket,
            request.clone(),
            Some(mutation_id),
            Duration::from_millis(10),
        )
        .await;
        assert!(first.unwrap_err().to_string().contains("did not respond"));

        let second = round_trip_with_mutation_id_and_timeout(
            &socket,
            request,
            Some(mutation_id),
            Duration::from_millis(250),
        )
        .await
        .expect("retry response");
        assert!(matches!(
            second,
            Response::Ok {
                data: ResponseData::Pong
            }
        ));
        server.await.expect("server task");
        assert_eq!(writes.load(Ordering::SeqCst), 1);
        let _ = std::fs::remove_file(socket);
    }
}
