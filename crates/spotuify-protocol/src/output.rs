//! Lightweight shared output format enum referenced across crates.
//!
//! Rendering helpers (`write_*` functions, table/JSON/JSONL/CSV
//! formatters) live in `spotuify-cli::output`. This module hosts
//! only the format selector because both the daemon's diagnostics
//! and the CLI/TUI's renderers need to agree on the choices.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Table,
    Json,
    Jsonl,
    Csv,
    Ids,
}
