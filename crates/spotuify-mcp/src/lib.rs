//! Phase 8: MCP (Model Context Protocol) server for spotuify.
//!
//! Lets LLM clients (Claude Code, Cursor, Continue, agent harnesses)
//! consume spotuify via the standardised MCP transport rather than
//! shelling out to the CLI.
//!
//! Architecture in three pieces:
//!
//! 1. [`tools`] — tool catalogue + JSON-Schema for each tool. Pure data,
//!    trivially testable, generates the MCP manifest.
//!
//! 2. [`bridge`] — `tool_name + args → spotuify_protocol::Request` and
//!    `Response → MCP tool result`. Pure functions; the actual JSON-RPC
//!    transport is a thin shim that calls these.
//!
//! 3. [`confirm`] — destructive-action gating. Every destructive tool
//!    takes a `confirm: bool` arg; when false, returns a preview;
//!    when true, executes and returns a receipt.

pub mod bridge;
pub mod confirm;
pub mod resources;
pub mod tools;

pub use confirm::{ConfirmDecision, ConfirmationRequired};
pub use resources::{resource_uris_invalidated_by, Resource, ResourceCatalogue};
pub use tools::{Tool, ToolCatalogue, ToolKind, ToolManifest};
