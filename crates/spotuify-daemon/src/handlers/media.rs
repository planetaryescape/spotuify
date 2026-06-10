//! `media` request handlers (split out of the dispatch god-function).

use std::sync::Arc;

use spotuify_protocol::{OperationSource, Request, ResponseData};

use crate::handler::*;
use crate::state::DaemonState;

pub(crate) async fn dispatch(
    state: Arc<DaemonState>,
    request: Request,
    _source: Option<OperationSource>,
) -> anyhow::Result<ResponseData> {
    match request {
        Request::Image { url } => {
            let entry = state
                .system_integration
                .cover_cache
                .get_or_fetch_entry(&url)
                .await?;
            Ok(ResponseData::Image {
                bytes: tokio::fs::read(entry.path).await?,
            })
        }
        Request::CoverArt { url } => {
            let entry = state
                .system_integration
                .cover_cache
                .get_or_fetch_entry(&url)
                .await?;
            Ok(ResponseData::CoverArt {
                path: entry.path.display().to_string(),
                cache_hit: entry.cache_hit,
                bytes: entry.bytes,
                fetched_at_ms: entry.fetched_at_ms,
            })
        }
        Request::LyricsGet {
            track_uri,
            force_refresh,
        } => lyrics_get(state, track_uri, force_refresh).await,
        Request::LyricsOffsetSet {
            track_uri,
            offset_ms,
        } => {
            state
                .store()
                .set_lyrics_offset_ms(&track_uri, offset_ms)
                .await?;
            Ok(ResponseData::LyricsOffset {
                track_uri,
                offset_ms,
            })
        }
        _ => unreachable!("non-media request routed to media dispatcher"),
    }
}
