//! Tantivy search service for spotuify. Rebuildable derived state from the
//! SQLite store. Depends on `spotuify-core`, `spotuify-protocol`, and
//! `spotuify-store`.

pub mod reindex;

use std::path::Path;

use anyhow::Result;
use tantivy::collector::{Count, TopDocs};
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, Value, FAST, INDEXED, STORED, STRING, TEXT,
};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use spotuify_protocol::SearchScopeData;
use spotuify_store::IndexedMediaItem;

#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub uri: String,
    pub score: f32,
}

#[derive(Debug, Clone, Default)]
pub struct SearchUpdateBatch {
    pub entries: Vec<IndexedMediaItem>,
    pub removed_uris: Vec<String>,
}

#[derive(Clone)]
pub struct SearchServiceHandle {
    tx: mpsc::Sender<SearchCommand>,
}

pub struct SearchIndex {
    index: Index,
    reader: IndexReader,
    writer: IndexWriter,
    schema: MusicSchema,
}

struct MusicSchema {
    schema: Schema,
    uri: Field,
    spotify_id: Field,
    kind: Field,
    name: Field,
    subtitle: Field,
    context: Field,
    source: Field,
    liked: Field,
    saved: Field,
    added_at_ms: Field,
    duration_ms: Field,
}

enum SearchCommand {
    ApplyBatch {
        batch: SearchUpdateBatch,
        resp: oneshot::Sender<Result<()>>,
    },
    Search {
        query: String,
        scope: SearchScopeData,
        limit: usize,
        resp: oneshot::Sender<Result<Vec<SearchHit>>>,
    },
    Clear {
        resp: oneshot::Sender<Result<()>>,
    },
    NumDocs {
        resp: oneshot::Sender<Result<u64>>,
    },
    Shutdown {
        resp: oneshot::Sender<()>,
    },
}

impl SearchServiceHandle {
    pub fn start(index: SearchIndex) -> (Self, JoinHandle<()>) {
        let (tx, mut rx) = mpsc::channel::<SearchCommand>(32);
        let handle = tokio::spawn(async move {
            let mut index = index;
            while let Some(command) = rx.recv().await {
                match command {
                    SearchCommand::ApplyBatch { batch, resp } => {
                        let _ = resp.send(apply_batch(&mut index, batch));
                    }
                    SearchCommand::Search {
                        query,
                        scope,
                        limit,
                        resp,
                    } => {
                        let _ = resp.send(index.search(&query, scope, limit));
                    }
                    SearchCommand::Clear { resp } => {
                        let _ = resp.send(index.clear());
                    }
                    SearchCommand::NumDocs { resp } => {
                        let _ = resp.send(Ok(index.num_docs()));
                    }
                    SearchCommand::Shutdown { resp } => {
                        let _ = resp.send(());
                        break;
                    }
                }
            }
        });
        (Self { tx }, handle)
    }

    pub async fn apply_batch(&self, batch: SearchUpdateBatch) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(SearchCommand::ApplyBatch {
                batch,
                resp: resp_tx,
            })
            .await
            .map_err(|err| anyhow::anyhow!("search service unavailable: {err}"))?;
        resp_rx
            .await
            .map_err(|_| anyhow::anyhow!("search service worker stopped"))?
    }

    pub async fn search(
        &self,
        query: &str,
        scope: SearchScopeData,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(SearchCommand::Search {
                query: query.to_string(),
                scope,
                limit,
                resp: resp_tx,
            })
            .await
            .map_err(|err| anyhow::anyhow!("search service unavailable: {err}"))?;
        resp_rx
            .await
            .map_err(|_| anyhow::anyhow!("search service worker stopped"))?
    }

    pub async fn clear(&self) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(SearchCommand::Clear { resp: resp_tx })
            .await
            .map_err(|err| anyhow::anyhow!("search service unavailable: {err}"))?;
        resp_rx
            .await
            .map_err(|_| anyhow::anyhow!("search service worker stopped"))?
    }

    pub async fn num_docs(&self) -> Result<u64> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(SearchCommand::NumDocs { resp: resp_tx })
            .await
            .map_err(|err| anyhow::anyhow!("search service unavailable: {err}"))?;
        resp_rx
            .await
            .map_err(|_| anyhow::anyhow!("search service worker stopped"))?
    }

    pub async fn request_shutdown(&self) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(SearchCommand::Shutdown { resp: resp_tx })
            .await
            .map_err(|err| anyhow::anyhow!("search service unavailable: {err}"))?;
        resp_rx
            .await
            .map_err(|_| anyhow::anyhow!("search service worker stopped"))?;
        Ok(())
    }
}

impl SearchIndex {
    pub fn open(index_path: &Path) -> Result<Self> {
        if let Some(parent) = index_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::create_dir_all(index_path)?;
        let schema = MusicSchema::build();
        let dir = tantivy::directory::MmapDirectory::open(index_path)?;
        let index = match Index::open_or_create(dir, schema.schema.clone()) {
            Ok(index) => index,
            Err(err) if err.to_string().contains("schema does not match") => {
                std::fs::remove_dir_all(index_path)?;
                std::fs::create_dir_all(index_path)?;
                let dir = tantivy::directory::MmapDirectory::open(index_path)?;
                Index::open_or_create(dir, schema.schema.clone())?
            }
            Err(err) => return Err(err.into()),
        };
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;
        let writer = index.writer(50_000_000)?;
        Ok(Self {
            index,
            reader,
            writer,
            schema,
        })
    }

    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        let schema = MusicSchema::build();
        let index = Index::create_in_ram(schema.schema.clone());
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let writer = index.writer(15_000_000)?;
        Ok(Self {
            index,
            reader,
            writer,
            schema,
        })
    }

    pub fn index_item(&mut self, entry: &IndexedMediaItem) -> Result<()> {
        let term = Term::from_field_text(self.schema.uri, &entry.item.uri);
        self.writer.delete_term(term);

        let mut doc = TantivyDocument::new();
        let schema = &self.schema;
        doc.add_text(schema.uri, &entry.item.uri);
        doc.add_text(schema.spotify_id, entry.item.id.as_deref().unwrap_or(""));
        doc.add_text(schema.kind, entry.item.kind.label());
        doc.add_text(schema.name, &entry.item.name);
        doc.add_text(schema.subtitle, &entry.item.subtitle);
        doc.add_text(schema.context, &entry.item.context);
        doc.add_text(schema.source, &entry.source);
        doc.add_bool(schema.liked, entry.liked);
        doc.add_bool(schema.saved, entry.saved);
        doc.add_i64(schema.added_at_ms, entry.added_at_ms.unwrap_or_default());
        doc.add_u64(schema.duration_ms, entry.item.duration_ms);
        self.writer.add_document(doc)?;
        Ok(())
    }

    pub fn remove_uri(&mut self, uri: &str) {
        let term = Term::from_field_text(self.schema.uri, uri);
        self.writer.delete_term(term);
    }

    pub fn commit(&mut self) -> Result<()> {
        self.writer.commit()?;
        self.reader.reload()?;
        Ok(())
    }

    pub fn clear(&mut self) -> Result<()> {
        self.writer.delete_all_documents()?;
        self.commit()
    }

    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    pub fn search(
        &self,
        query: &str,
        scope: SearchScopeData,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        let mut parser = QueryParser::for_index(
            &self.index,
            vec![
                self.schema.name,
                self.schema.subtitle,
                self.schema.context,
                self.schema.uri,
            ],
        );
        // Tantivy's QueryParser defaults to OR — `get lifted` would
        // match docs containing either word. For a music search bar,
        // that's almost always the wrong default: typing two words
        // means "find tracks where both appear" (in the track name,
        // artist, album, or wherever). Flip to AND so multi-word
        // queries do what the user expects. UPPERCASE OR/NOT and
        // explicit grouping still override.
        parser.set_conjunction_by_default();
        let text_query = parser.parse_query(query)?;
        let query: Box<dyn Query> = if scope == SearchScopeData::All {
            text_query
        } else {
            Box::new(BooleanQuery::new(vec![
                (Occur::Must, text_query),
                (
                    Occur::Must,
                    Box::new(TermQuery::new(
                        Term::from_field_text(self.schema.kind, scope.label()),
                        IndexRecordOption::Basic,
                    )),
                ),
            ]))
        };

        let searcher = self.reader.searcher();
        let _total = searcher.search(&*query, &Count)?;
        let top_docs = searcher.search(&*query, &TopDocs::with_limit(limit))?;
        let mut hits = Vec::with_capacity(top_docs.len());
        for (score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_address)?;
            let Some(uri) = doc
                .get_first(self.schema.uri)
                .and_then(|value| value.as_str())
            else {
                continue;
            };
            hits.push(SearchHit {
                uri: uri.to_string(),
                score,
            });
        }
        Ok(hits)
    }
}

impl MusicSchema {
    fn build() -> Self {
        let mut builder = Schema::builder();
        let uri = builder.add_text_field("uri", STRING | STORED);
        let spotify_id = builder.add_text_field("spotify_id", STRING | STORED);
        let kind = builder.add_text_field("kind", STRING | STORED);
        let name = builder.add_text_field("name", TEXT | STORED);
        let subtitle = builder.add_text_field("subtitle", TEXT | STORED);
        let context = builder.add_text_field("context", TEXT | STORED);
        let source = builder.add_text_field("source", STRING | STORED);
        let liked = builder.add_bool_field("liked", INDEXED | STORED);
        let saved = builder.add_bool_field("saved", INDEXED | STORED);
        let added_at_ms = builder.add_i64_field("added_at_ms", FAST | STORED);
        let duration_ms = builder.add_u64_field("duration_ms", FAST | STORED);
        let schema = builder.build();
        Self {
            schema,
            uri,
            spotify_id,
            kind,
            name,
            subtitle,
            context,
            source,
            liked,
            saved,
            added_at_ms,
            duration_ms,
        }
    }
}

fn apply_batch(index: &mut SearchIndex, batch: SearchUpdateBatch) -> Result<()> {
    for uri in batch.removed_uris {
        index.remove_uri(&uri);
    }
    for entry in batch.entries {
        index.index_item(&entry)?;
    }
    index.commit()
}

#[cfg(test)]
mod tests {
    use super::*;
    use spotuify_core::{MediaItem, MediaKind};

    #[test]
    fn index_finds_cached_music_by_name_and_artist() -> Result<()> {
        let mut index = SearchIndex::in_memory()?;
        let entry = IndexedMediaItem {
            item: track("spotify:track:1", "Never Too Much", "Luther Vandross"),
            liked: true,
            saved: true,
            added_at_ms: Some(1_700_000_000_000),
            source: "spotify".to_string(),
        };

        index.index_item(&entry)?;
        index.commit()?;

        let hits = index.search("luther", SearchScopeData::Track, 10)?;

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].uri, "spotify:track:1");
        Ok(())
    }

    #[test]
    fn multi_word_query_requires_all_terms_to_match() -> Result<()> {
        // Adversarial: a user typing "get lifted" expects results
        // containing BOTH words, not "get" OR "lifted". Tantivy's
        // QueryParser default is OR, so without
        // set_conjunction_by_default() this returns the wrong items.
        let mut index = SearchIndex::in_memory()?;
        for entry in [
            IndexedMediaItem {
                item: track("spotify:track:get-only", "Let's Get Together", "Artist A"),
                liked: false,
                saved: false,
                added_at_ms: None,
                source: "spotify".to_string(),
            },
            IndexedMediaItem {
                item: track(
                    "spotify:track:lifted-only",
                    "Burdens Are Lifted at Calvary",
                    "Artist B",
                ),
                liked: false,
                saved: false,
                added_at_ms: None,
                source: "spotify".to_string(),
            },
            IndexedMediaItem {
                item: track("spotify:track:both", "Get Lifted", "Artist C"),
                liked: false,
                saved: false,
                added_at_ms: None,
                source: "spotify".to_string(),
            },
        ] {
            index.index_item(&entry)?;
        }
        index.commit()?;

        let hits = index.search("get lifted", SearchScopeData::Track, 10)?;
        let uris: Vec<&str> = hits.iter().map(|h| h.uri.as_str()).collect();
        assert_eq!(uris, vec!["spotify:track:both"]);
        Ok(())
    }

    #[test]
    fn reindexing_same_uri_replaces_stale_document() -> Result<()> {
        let mut index = SearchIndex::in_memory()?;
        let mut item = track("spotify:track:1", "Old Name", "Artist");
        index.index_item(&IndexedMediaItem {
            item: item.clone(),
            liked: false,
            saved: false,
            added_at_ms: None,
            source: "spotify".to_string(),
        })?;
        item.name = "New Name".to_string();
        index.index_item(&IndexedMediaItem {
            item,
            liked: false,
            saved: false,
            added_at_ms: None,
            source: "spotify".to_string(),
        })?;
        index.commit()?;

        assert!(index.search("old", SearchScopeData::Track, 10)?.is_empty());
        assert_eq!(index.num_docs(), 1);
        Ok(())
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
            explicit: None,
            is_playable: None,
        }
    }
}
