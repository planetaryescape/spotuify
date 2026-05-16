//! Re-export bridges for spotuify-daemon's modules. The implementation
//! lives in `crates/spotuify-daemon/`. The shim modules below keep the
//! binary's `crate::daemon::{server,state,status,handler,ipc_client}::*`
//! import paths compiling.

#![allow(unused_imports)]

pub mod server {
    pub use spotuify_daemon::server::*;
}
pub mod status {
    pub use spotuify_daemon::status::*;
}
pub mod state {
    pub use spotuify_daemon::state::*;
}
pub mod handler {
    pub use spotuify_daemon::handler::*;
}

pub mod ipc_client {
    pub use spotuify_protocol::ipc_client::*;
}
