//! Cross-platform IPC stream abstraction.
//!
//! Unix builds use Unix-domain sockets. Windows builds use Tokio named
//! pipes behind the same async read/write stream type so the daemon,
//! CLI, and MCP bridge share one codec path.

use std::io;
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;

use tokio::io::{AsyncRead, AsyncWrite};

pub trait IpcReadWrite: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T> IpcReadWrite for T where T: AsyncRead + AsyncWrite + Unpin + Send {}

pub type IpcStream = Box<dyn IpcReadWrite>;

#[cfg(unix)]
pub struct IpcListener {
    inner: tokio::net::UnixListener,
    /// uid that owns the socket file — i.e. the daemon's own uid, since
    /// it just created the socket. Peers connecting as a different uid
    /// are rejected on accept (defense in depth on top of the 0600
    /// socket perms). `None` only if we couldn't stat the socket, in
    /// which case the file permissions remain the sole gate.
    expected_uid: Option<u32>,
}

#[cfg(unix)]
impl IpcListener {
    pub fn bind(path: &Path) -> io::Result<Self> {
        let inner = tokio::net::UnixListener::bind(path)?;
        // Capture the socket owner's uid without libc/geteuid — the
        // workspace denies unsafe, and the daemon owns the socket it
        // just bound, so its owner uid is our own uid.
        let expected_uid = std::fs::metadata(path)
            .map(|meta| std::os::unix::fs::MetadataExt::uid(&meta))
            .ok();
        Ok(Self {
            inner,
            expected_uid,
        })
    }

    pub async fn accept(&mut self) -> io::Result<IpcStream> {
        loop {
            let (stream, _) = self.inner.accept().await?;
            let Some(expected) = self.expected_uid else {
                // Couldn't determine our uid; rely on the 0600 socket.
                return Ok(Box::new(stream));
            };
            match stream.peer_cred() {
                Ok(cred) if cred.uid() == expected => return Ok(Box::new(stream)),
                Ok(cred) => {
                    tracing::warn!(
                        peer_uid = cred.uid(),
                        expected_uid = expected,
                        "rejecting IPC connection from a different uid"
                    );
                }
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "failed to read IPC peer credentials; rejecting connection"
                    );
                }
            }
            // Reject: drop the stream and keep accepting.
        }
    }
}

#[cfg(unix)]
pub async fn connect(path: &Path) -> io::Result<IpcStream> {
    tokio::net::UnixStream::connect(path)
        .await
        .map(|stream| Box::new(stream) as IpcStream)
}

// Windows: the named pipe is created with the default security
// descriptor, which grants access only to the creating user (and
// admins). A different local user therefore cannot open it, so the
// per-peer uid check the Unix path performs is unnecessary here.
// Tightening further (e.g. GetNamedPipeClientProcessId) is deferred.
#[cfg(windows)]
pub struct IpcListener {
    path: PathBuf,
    pending: tokio::net::windows::named_pipe::NamedPipeServer,
}

#[cfg(windows)]
impl IpcListener {
    pub fn bind(path: &Path) -> io::Result<Self> {
        let pending = create_server(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            pending,
        })
    }

    pub async fn accept(&mut self) -> io::Result<IpcStream> {
        self.pending.connect().await?;
        let next = create_server(&self.path)?;
        let connected = std::mem::replace(&mut self.pending, next);
        Ok(Box::new(connected))
    }
}

#[cfg(windows)]
fn create_server(path: &Path) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    tokio::net::windows::named_pipe::ServerOptions::new()
        .first_pipe_instance(false)
        .create(path)
}

#[cfg(windows)]
pub async fn connect(path: &Path) -> io::Result<IpcStream> {
    use tokio::net::windows::named_pipe::ClientOptions;

    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_PIPE_BUSY: i32 = 231;
    const ATTEMPTS: usize = 20;
    const DELAY: std::time::Duration = std::time::Duration::from_millis(50);

    for attempt in 0..ATTEMPTS {
        match ClientOptions::new().open(path) {
            Ok(client) => return Ok(Box::new(client)),
            Err(err)
                if attempt + 1 < ATTEMPTS
                    && matches!(
                        err.raw_os_error(),
                        Some(ERROR_FILE_NOT_FOUND | ERROR_PIPE_BUSY)
                    ) =>
            {
                tokio::time::sleep(DELAY).await;
            }
            Err(err) => return Err(err),
        }
    }

    unreachable!("loop either returns a client or the last open error")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn accept_admits_same_uid_peer() {
        // The peer-credential gate must not reject the common case: a
        // client running as the same user (the daemon's own uid). This
        // round-trips a byte over a same-uid connection.
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("spotuify-test.sock");
        let mut listener = IpcListener::bind(&socket).expect("bind");

        let accept = tokio::spawn(async move {
            let mut stream = listener.accept().await.expect("accept same-uid peer");
            let mut buf = [0_u8; 1];
            stream.read_exact(&mut buf).await.expect("read");
            buf[0]
        });

        let mut client = connect(&socket).await.expect("connect");
        client.write_all(&[42]).await.expect("write");
        client.flush().await.expect("flush");

        assert_eq!(accept.await.expect("join"), 42);
    }
}
