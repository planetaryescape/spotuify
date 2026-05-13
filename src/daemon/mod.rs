pub mod server;
pub mod status;

mod handler;
pub(crate) mod state;

// ipc_client moved to spotuify-protocol; re-exported here so existing
// `crate::daemon::ipc_client::IpcClient` call sites keep compiling.
pub mod ipc_client {
    pub use spotuify_protocol::ipc_client::*;
}
