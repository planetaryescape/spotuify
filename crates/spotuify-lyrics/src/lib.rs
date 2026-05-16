//! Phase 16 lyrics support: parsing, alignment, and provider adapters.

pub mod alignment;
pub mod lrclib_provider;
pub mod parser;
pub mod spotify_provider;

pub use alignment::active_line_index;
pub use lrclib_provider::LrclibProvider;
pub use parser::{is_rtl, parse_lrc, plain_text_lines};
pub use spotify_provider::{mercury_uri_for_track_uri, parse_spotify_mercury};

#[derive(Debug, thiserror::Error)]
pub enum LyricsError {
    #[error("lyrics not found")]
    NotFound,
    #[error("lyrics provider rate limited")]
    RateLimited,
    #[error("invalid lyrics payload: {0}")]
    InvalidPayload(String),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}
