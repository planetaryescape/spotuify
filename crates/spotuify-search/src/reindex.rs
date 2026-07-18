use anyhow::Result;

use spotuify_protocol::ReindexStats;
use spotuify_store::Store;

use crate::{SearchServiceHandle, SearchUpdateBatch};

pub async fn reindex(store: &Store, search: &SearchServiceHandle) -> Result<ReindexStats> {
    search.clear().await?;

    let batch_size = 500;
    let mut indexed = 0;
    let mut offset = 0;
    loop {
        let entries = store.list_media_for_index(batch_size, offset, None).await?;
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
    anyhow::ensure!(
        index_documents == indexed as u64,
        "search reindex count mismatch: indexed {indexed} SQLite rows but Tantivy reports {index_documents} documents"
    );
    Ok(ReindexStats {
        indexed,
        index_documents,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use spotuify_core::{MediaItem, MediaKind, ProviderId};
    use spotuify_protocol::SearchScopeData;

    use crate::SearchIndex;

    #[tokio::test]
    async fn reindex_builds_search_documents_from_sqlite_cache() -> Result<()> {
        let store = Store::in_memory().await?;
        store
            .cache_provider_search_results(
                &ProviderId::new("spotify")?,
                "luther vandross",
                SearchScopeData::Track,
                "remote",
                &[track(
                    "spotify:track:1",
                    "Never Too Much",
                    "Luther Vandross",
                )],
            )
            .await?;
        let (search, _worker) = SearchServiceHandle::start(SearchIndex::in_memory()?);

        let stats = reindex(&store, &search).await?;

        assert_eq!(stats.indexed, 1);
        assert_eq!(stats.index_documents, 1);
        let hits = search.search("luther", SearchScopeData::Track, 10).await?;
        assert_eq!(hits[0].uri, "spotify:track:1");
        Ok(())
    }

    fn track(uri: &str, name: &str, artist: &str) -> MediaItem {
        MediaItem {
            id: spotuify_core::ResourceUri::parse(uri)
                .ok()
                .map(|resource| resource.bare_id().to_string()),
            uri: uri.to_string(),
            name: name.to_string(),
            subtitle: artist.to_string(),
            context: "Album".to_string(),
            duration_ms: 180_000,
            image_url: None,
            kind: MediaKind::Track,
            source: Some("spotify".into()),
            freshness: None,
            explicit: None,
            is_playable: None,
            ..Default::default()
        }
    }
}
