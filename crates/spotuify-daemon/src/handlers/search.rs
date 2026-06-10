//! `search` request handlers (split out of the dispatch god-function).

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
        Request::Search {
            query,
            scope,
            source,
            limit,
            kinds,
            sort,
        } => Ok(ResponseData::SearchResults {
            items: search_with_source(state.clone(), query, scope, source, limit, kinds, sort)
                .await?,
        }),
        Request::SearchStream {
            query,
            scope,
            source,
            version,
        } => {
            spawn_search_stream(state.clone(), query.clone(), scope, source, version);
            Ok(ResponseData::SearchStarted { query, version })
        }
        Request::SearchPage {
            query,
            kind,
            offset,
            version,
        } => {
            spawn_search_page(state.clone(), query.clone(), kind, offset, version);
            Ok(ResponseData::SearchStarted { query, version })
        }
        _ => unreachable!("non-search request routed to search dispatcher"),
    }
}
