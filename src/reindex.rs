use anyhow::Result;

use crate::protocol::ReindexStats;
use crate::search::{SearchServiceHandle, SearchUpdateBatch};
use crate::store::Store;

pub async fn reindex(store: &Store, search: &SearchServiceHandle) -> Result<ReindexStats> {
    search.clear().await?;

    let batch_size = 500;
    let mut indexed = 0;
    let mut offset = 0;
    loop {
        let entries = store.list_media_for_index(batch_size, offset).await?;
        if entries.is_empty() {
            break;
        }
        indexed += entries.len() as u32;
        search
            .apply_batch(SearchUpdateBatch {
                entries,
                removed_uris: Vec::new(),
            })
            .await?;
        offset += batch_size;
    }

    let index_documents = search.num_docs().await?;
    Ok(ReindexStats {
        indexed,
        index_documents,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{SearchScopeData, SearchSourceData};
    use crate::search::SearchIndex;
    use crate::spotify::{MediaItem, MediaKind};

    #[tokio::test]
    async fn reindex_builds_search_documents_from_sqlite_cache() {
        let store = Store::in_memory().await.unwrap();
        store
            .cache_search_results(
                "luther vandross",
                SearchScopeData::Track,
                SearchSourceData::Spotify,
                &[track(
                    "spotify:track:1",
                    "Never Too Much",
                    "Luther Vandross",
                )],
            )
            .await
            .unwrap();
        let (search, _worker) = SearchServiceHandle::start(SearchIndex::in_memory().unwrap());

        let stats = reindex(&store, &search).await.unwrap();

        assert_eq!(stats.indexed, 1);
        assert_eq!(stats.index_documents, 1);
        let hits = search
            .search("luther", SearchScopeData::Track, 10)
            .await
            .unwrap();
        assert_eq!(hits[0].uri, "spotify:track:1");
    }

    fn track(uri: &str, name: &str, artist: &str) -> MediaItem {
        MediaItem {
            id: uri.rsplit(':').next().map(str::to_string),
            uri: uri.to_string(),
            name: name.to_string(),
            subtitle: artist.to_string(),
            context: "Album".to_string(),
            duration_ms: 180_000,
            image_url: None,
            kind: MediaKind::Track,
            source: Some("spotify".to_string()),
            freshness: None,
        }
    }
}
