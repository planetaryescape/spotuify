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
            provider,
            kinds,
            sort,
        } => Ok(ResponseData::SearchResults {
            items: search_with_source(
                state.clone(),
                SearchParams {
                    query,
                    scope,
                    source,
                    limit,
                    requested_provider: provider,
                    kinds,
                    sort,
                },
            )
            .await?,
        }),
        Request::SearchStream {
            query,
            scope,
            source,
            version,
            provider,
        } => {
            let (provider, _) = resolve_search_provider(&state, &source, provider.as_ref()).await?;
            spawn_search_stream(
                state.clone(),
                query.clone(),
                scope,
                source,
                version,
                provider.clone(),
            );
            Ok(ResponseData::SearchStarted {
                query,
                version,
                provider: Some(provider),
            })
        }
        Request::SearchPage {
            query,
            kind,
            offset,
            version,
            provider,
        } => {
            let (provider, _) = state.provider_or_default(provider.as_ref()).await?;
            spawn_search_page(
                state.clone(),
                query.clone(),
                kind,
                offset,
                version,
                provider.clone(),
            );
            Ok(ResponseData::SearchStarted {
                query,
                version,
                provider: Some(provider),
            })
        }
        _ => unreachable!("non-search request routed to search dispatcher"),
    }
}
