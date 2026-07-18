//! Tantivy search service for spotuify. Rebuildable derived state from the
//! SQLite store. Depends on `spotuify-core`, `spotuify-protocol`, and
//! `spotuify-store`.

pub mod reindex;

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use tantivy::collector::TopDocs;
use tantivy::directory::{Directory, DirectoryLock, INDEX_WRITER_LOCK};
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, Value, FAST, INDEXED, STORED, STRING, TEXT,
};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use spotuify_core::ProviderId;
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

/// Point operations (search / num_docs / clear / shutdown) should
/// complete in milliseconds against a healthy index. If the blocking
/// worker is wedged (corrupt segment, stuck merge, a panic that left the
/// channel un-drained) the caller must not hang the daemon request path
/// or the sync loop forever.
const SEARCH_OP_TIMEOUT: Duration = Duration::from_secs(5);
/// Bulk reindex batches legitimately take longer than a point query.
const APPLY_BATCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Errors surfaced by [`SearchServiceHandle`] before the underlying
/// Tantivy operation even runs. Tantivy's own failures stay inside the
/// inner `anyhow::Result` returned by each command.
#[derive(Debug, thiserror::Error)]
pub enum SearchServiceError {
    /// The worker channel is closed — the service was never started or
    /// has shut down.
    #[error("search service unavailable: {0}")]
    Unavailable(String),
    /// The worker dropped the response sender without replying (e.g. it
    /// panicked mid-operation).
    #[error("search service worker stopped")]
    WorkerStopped,
    /// The worker did not respond within the operation's deadline.
    #[error("search service timed out after {0:?} on `{1}`")]
    Timeout(Duration, &'static str),
}

pub struct SearchIndex {
    index: Index,
    reader: IndexReader,
    writer: IndexWriter,
    schema: MusicSchema,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchIndexRebuildReason {
    SchemaMismatch,
    StartupRepair,
    DocumentCountMismatch,
}

impl SearchIndexRebuildReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SchemaMismatch => "schema_mismatch",
            Self::StartupRepair => "startup_repair",
            Self::DocumentCountMismatch => "document_count_mismatch",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchIndexRebuild {
    pub reason: SearchIndexRebuildReason,
    pub detail: String,
}

pub struct SearchIndexOpenResult {
    pub index: SearchIndex,
    pub rebuild: Option<SearchIndexRebuild>,
}

struct MusicSchema {
    schema: Schema,
    uri: Field,
    provider: Field,
    kind: Field,
    name: Field,
    subtitle: Field,
    context: Field,
    search_origin: Field,
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
        provider: Option<String>,
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
        let handle = tokio::task::spawn_blocking(move || {
            let mut index = index;
            while let Some(command) = rx.blocking_recv() {
                match command {
                    SearchCommand::ApplyBatch { batch, resp } => {
                        let _ = resp.send(apply_batch(&mut index, batch));
                    }
                    SearchCommand::Search {
                        query,
                        scope,
                        limit,
                        provider,
                        resp,
                    } => {
                        let _ = resp.send(index.search_for_provider_label(
                            &query,
                            scope,
                            limit,
                            provider.as_deref(),
                        ));
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

    /// Send a command and await its reply under a deadline. Returns the
    /// raw payload `T` the worker sent back; callers flatten any inner
    /// `Result`. A timeout is logged distinctly so it is visible in the
    /// daemon log even when the caller degrades the error to a default.
    async fn dispatch<T>(
        &self,
        op: &'static str,
        op_timeout: Duration,
        command: SearchCommand,
        resp_rx: oneshot::Receiver<T>,
    ) -> Result<T, SearchServiceError> {
        self.tx
            .send(command)
            .await
            .map_err(|err| SearchServiceError::Unavailable(err.to_string()))?;
        match tokio::time::timeout(op_timeout, resp_rx).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) => Err(SearchServiceError::WorkerStopped),
            Err(_) => {
                tracing::warn!(
                    op,
                    timeout_ms = op_timeout.as_millis() as u64,
                    "search service operation timed out; worker may be wedged"
                );
                Err(SearchServiceError::Timeout(op_timeout, op))
            }
        }
    }

    pub async fn apply_batch(&self, batch: SearchUpdateBatch) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.dispatch(
            "apply_batch",
            APPLY_BATCH_TIMEOUT,
            SearchCommand::ApplyBatch {
                batch,
                resp: resp_tx,
            },
            resp_rx,
        )
        .await?
    }

    pub async fn search(
        &self,
        query: &str,
        scope: SearchScopeData,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        self.search_for_provider(query, scope, limit, None).await
    }

    pub async fn search_for_provider(
        &self,
        query: &str,
        scope: SearchScopeData,
        limit: usize,
        provider: Option<&ProviderId>,
    ) -> Result<Vec<SearchHit>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.dispatch(
            "search",
            SEARCH_OP_TIMEOUT,
            SearchCommand::Search {
                query: query.to_string(),
                scope,
                limit,
                provider: provider.map(ToString::to_string),
                resp: resp_tx,
            },
            resp_rx,
        )
        .await?
    }

    pub async fn clear(&self) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.dispatch(
            "clear",
            SEARCH_OP_TIMEOUT,
            SearchCommand::Clear { resp: resp_tx },
            resp_rx,
        )
        .await?
    }

    pub async fn num_docs(&self) -> Result<u64> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.dispatch(
            "num_docs",
            SEARCH_OP_TIMEOUT,
            SearchCommand::NumDocs { resp: resp_tx },
            resp_rx,
        )
        .await?
    }

    pub async fn request_shutdown(&self) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.dispatch(
            "shutdown",
            SEARCH_OP_TIMEOUT,
            SearchCommand::Shutdown { resp: resp_tx },
            resp_rx,
        )
        .await?;
        Ok(())
    }
}

impl SearchIndex {
    pub fn open(index_path: &Path) -> Result<SearchIndexOpenResult> {
        Self::open_with_rebuild_status(index_path)
    }

    pub fn open_with_rebuild_status(index_path: &Path) -> Result<SearchIndexOpenResult> {
        if let Some(parent) = index_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::create_dir_all(index_path)?;

        match Self::open_once(index_path) {
            Ok(index) => Ok(SearchIndexOpenResult {
                index,
                rebuild: None,
            }),
            Err(err) => {
                let Some(reason) = rebuild_reason(&err) else {
                    return Err(err.into());
                };
                let detail = err.to_string();
                let _writer_guard = acquire_rebuild_guard(index_path)?;
                std::fs::remove_dir_all(index_path)?;
                std::fs::create_dir_all(index_path)?;
                let index = Self::open_once(index_path)?;
                Ok(SearchIndexOpenResult {
                    index,
                    rebuild: Some(SearchIndexRebuild { reason, detail }),
                })
            }
        }
    }

    fn open_once(index_path: &Path) -> tantivy::Result<Self> {
        let schema = MusicSchema::build();
        let dir = tantivy::directory::MmapDirectory::open(index_path)?;
        let index = Index::open_or_create(dir, schema.schema.clone())?;
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
        doc.add_text(schema.provider, &entry.provider);
        doc.add_text(schema.kind, entry.item.kind.label());
        doc.add_text(schema.name, &entry.item.name);
        doc.add_text(schema.subtitle, &entry.item.subtitle);
        doc.add_text(schema.context, &entry.item.context);
        doc.add_text(schema.search_origin, &entry.search_origin);
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
        self.search_for_provider(query, scope, limit, None)
    }

    pub fn search_for_provider(
        &self,
        query: &str,
        scope: SearchScopeData,
        limit: usize,
        provider: Option<&ProviderId>,
    ) -> Result<Vec<SearchHit>> {
        self.search_for_provider_label(query, scope, limit, provider.map(ProviderId::as_str))
    }

    fn search_for_provider_label(
        &self,
        query: &str,
        scope: SearchScopeData,
        limit: usize,
        provider: Option<&str>,
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
        let mut clauses = vec![(Occur::Must, text_query)];
        if scope != SearchScopeData::All {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.schema.kind, scope.label()),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if let Some(provider) = provider {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.schema.provider, provider),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        let query: Box<dyn Query> = if clauses.len() == 1 {
            clauses.pop().expect("text query clause").1
        } else {
            Box::new(BooleanQuery::new(clauses))
        };

        let searcher = self.reader.searcher();
        let top_docs = searcher.search(&*query, &TopDocs::with_limit(limit).order_by_score())?;
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

fn acquire_rebuild_guard(index_path: &Path) -> tantivy::Result<DirectoryLock> {
    let directory = tantivy::directory::MmapDirectory::open(index_path)?;
    directory.acquire_lock(&INDEX_WRITER_LOCK).map_err(|err| {
        tantivy::TantivyError::LockFailure(
            err,
            Some("refusing to rebuild a search index while another writer owns it".to_string()),
        )
    })
}

fn rebuild_reason(err: &tantivy::TantivyError) -> Option<SearchIndexRebuildReason> {
    use tantivy::directory::error::OpenReadError;
    use tantivy::TantivyError;

    match err {
        TantivyError::SchemaError(_) => Some(SearchIndexRebuildReason::SchemaMismatch),
        TantivyError::DataCorruption(_)
        | TantivyError::DeserializeError(_)
        | TantivyError::IncompatibleIndex(_)
        | TantivyError::IoError(_)
        | TantivyError::OpenReadError(OpenReadError::FileDoesNotExist(_))
        | TantivyError::OpenReadError(OpenReadError::IncompatibleIndex(_))
        | TantivyError::OpenReadError(OpenReadError::IoError { .. }) => {
            Some(SearchIndexRebuildReason::StartupRepair)
        }
        _ => None,
    }
}

impl MusicSchema {
    fn build() -> Self {
        let mut builder = Schema::builder();
        let uri = builder.add_text_field("uri", STRING | STORED);
        let provider = builder.add_text_field("provider", STRING | STORED);
        let kind = builder.add_text_field("kind", STRING | STORED);
        let name = builder.add_text_field("name", TEXT | STORED);
        let subtitle = builder.add_text_field("subtitle", TEXT | STORED);
        let context = builder.add_text_field("context", TEXT | STORED);
        let search_origin = builder.add_text_field("search_origin", STRING | STORED);
        let liked = builder.add_bool_field("liked", INDEXED | STORED);
        let saved = builder.add_bool_field("saved", INDEXED | STORED);
        let added_at_ms = builder.add_i64_field("added_at_ms", FAST | STORED);
        let duration_ms = builder.add_u64_field("duration_ms", FAST | STORED);
        let schema = builder.build();
        Self {
            schema,
            uri,
            provider,
            kind,
            name,
            subtitle,
            context,
            search_origin,
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
    use tantivy::directory::error::LockError;

    #[test]
    fn index_schema_stores_provider_and_search_origin_without_vendor_id() {
        let schema = MusicSchema::build().schema;
        assert!(schema.get_field("provider").is_ok());
        assert!(schema.get_field("search_origin").is_ok());
        assert!(schema.get_field("spotify_id").is_err());
        assert!(schema.get_field("source").is_err());
    }

    #[test]
    fn index_finds_cached_music_by_name_and_artist() -> Result<()> {
        let mut index = SearchIndex::in_memory()?;
        let entry = IndexedMediaItem {
            item: track("spotify:track:1", "Never Too Much", "Luther Vandross"),
            provider: "spotify".to_string(),
            liked: true,
            saved: true,
            added_at_ms: Some(1_700_000_000_000),
            search_origin: "spotify".to_string(),
        };

        index.index_item(&entry)?;
        index.commit()?;

        let hits = index.search("luther", SearchScopeData::Track, 10)?;

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].uri, "spotify:track:1");
        Ok(())
    }

    #[test]
    fn provider_filter_is_optional_and_never_leaks_same_query_results() -> Result<()> {
        let mut index = SearchIndex::in_memory()?;
        for (uri, provider) in [
            ("spotify:track:work", "work"),
            ("fake:track:personal", "personal"),
        ] {
            index.index_item(&IndexedMediaItem {
                item: track(uri, "Shared Query", "Artist"),
                provider: provider.to_string(),
                liked: false,
                saved: false,
                added_at_ms: None,
                search_origin: "remote".to_string(),
            })?;
        }
        index.commit()?;

        let aggregate = index.search("shared query", SearchScopeData::Track, 10)?;
        let work_provider = ProviderId::new("work")?;
        let personal_provider = ProviderId::new("personal")?;
        let work = index.search_for_provider(
            "shared query",
            SearchScopeData::Track,
            10,
            Some(&work_provider),
        )?;
        let personal = index.search_for_provider(
            "shared query",
            SearchScopeData::Track,
            10,
            Some(&personal_provider),
        )?;

        assert_eq!(aggregate.len(), 2);
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].uri, "spotify:track:work");
        assert_eq!(personal.len(), 1);
        assert_eq!(personal[0].uri, "fake:track:personal");
        Ok(())
    }

    #[tokio::test]
    async fn search_service_forwards_provider_filter_to_worker() -> Result<()> {
        let mut index = SearchIndex::in_memory()?;
        for (uri, provider) in [
            ("spotify:track:work-service", "work"),
            ("fake:track:personal-service", "personal"),
        ] {
            index.index_item(&IndexedMediaItem {
                item: track(uri, "Shared Service Query", "Artist"),
                provider: provider.to_string(),
                liked: false,
                saved: false,
                added_at_ms: None,
                search_origin: "remote".to_string(),
            })?;
        }
        index.commit()?;
        let (search, worker) = SearchServiceHandle::start(index);
        let work = ProviderId::new("work")?;

        let hits = search
            .search_for_provider(
                "shared service query",
                SearchScopeData::Track,
                10,
                Some(&work),
            )
            .await?;

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].uri, "spotify:track:work-service");
        search.request_shutdown().await?;
        worker.await?;
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
                provider: "spotify".to_string(),
                liked: false,
                saved: false,
                added_at_ms: None,
                search_origin: "spotify".to_string(),
            },
            IndexedMediaItem {
                item: track(
                    "spotify:track:lifted-only",
                    "Burdens Are Lifted at Calvary",
                    "Artist B",
                ),
                provider: "spotify".to_string(),
                liked: false,
                saved: false,
                added_at_ms: None,
                search_origin: "spotify".to_string(),
            },
            IndexedMediaItem {
                item: track("spotify:track:both", "Get Lifted", "Artist C"),
                provider: "spotify".to_string(),
                liked: false,
                saved: false,
                added_at_ms: None,
                search_origin: "spotify".to_string(),
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
            provider: "spotify".to_string(),
            liked: false,
            saved: false,
            added_at_ms: None,
            search_origin: "spotify".to_string(),
        })?;
        item.name = "New Name".to_string();
        index.index_item(&IndexedMediaItem {
            item,
            provider: "spotify".to_string(),
            liked: false,
            saved: false,
            added_at_ms: None,
            search_origin: "spotify".to_string(),
        })?;
        index.commit()?;

        assert!(index.search("old", SearchScopeData::Track, 10)?.is_empty());
        assert_eq!(index.num_docs(), 1);
        Ok(())
    }

    #[test]
    fn schema_mismatch_rebuilds_and_reports_reason() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let index_path = temp.path().join("index");
        std::fs::create_dir_all(&index_path)?;
        let mut stale_schema = Schema::builder();
        stale_schema.add_text_field("stale", STRING | STORED);
        Index::create_in_dir(&index_path, stale_schema.build())?;

        let opened = SearchIndex::open_with_rebuild_status(&index_path)?;

        let rebuild = opened.rebuild.expect("schema mismatch must rebuild");
        assert_eq!(rebuild.reason, SearchIndexRebuildReason::SchemaMismatch);
        assert!(rebuild.detail.contains("schema"));
        assert_eq!(opened.index.num_docs(), 0);
        Ok(())
    }

    #[test]
    fn corrupt_metadata_rebuilds_and_reports_startup_repair() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let index_path = temp.path().join("index");
        drop(SearchIndex::open(&index_path)?.index);
        std::fs::write(index_path.join("meta.json"), b"not valid tantivy metadata")?;

        let opened = SearchIndex::open_with_rebuild_status(&index_path)?;

        let rebuild = opened.rebuild.expect("corrupt metadata must rebuild");
        assert_eq!(rebuild.reason, SearchIndexRebuildReason::StartupRepair);
        assert!(!rebuild.detail.is_empty());
        assert_eq!(opened.index.num_docs(), 0);
        Ok(())
    }

    #[test]
    fn writer_lock_contention_never_wipes_index_directory() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let index_path = temp.path().join("index");
        let _first_writer = SearchIndex::open(&index_path)?.index;
        let marker = index_path.join("keep-me");
        std::fs::write(&marker, b"healthy index")?;

        let err = match SearchIndex::open_with_rebuild_status(&index_path) {
            Ok(_) => anyhow::bail!("second writer unexpectedly acquired the index"),
            Err(err) => err,
        };

        assert!(matches!(
            err.downcast_ref::<tantivy::TantivyError>(),
            Some(tantivy::TantivyError::LockFailure(LockError::LockBusy, _))
        ));
        assert!(marker.exists(), "lock contention must not wipe the index");
        assert!(index_path.join("meta.json").exists());
        Ok(())
    }

    #[test]
    fn schema_mismatch_under_writer_contention_never_wipes_index_directory() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let index_path = temp.path().join("index");
        std::fs::create_dir_all(&index_path)?;
        let mut stale_schema = Schema::builder();
        stale_schema.add_text_field("stale", STRING | STORED);
        let stale_index = Index::create_in_dir(&index_path, stale_schema.build())?;
        let _first_writer = stale_index.writer::<TantivyDocument>(15_000_000)?;
        let marker = index_path.join("keep-me");
        std::fs::write(&marker, b"owned stale index")?;

        let err = match SearchIndex::open_with_rebuild_status(&index_path) {
            Ok(_) => anyhow::bail!("schema mismatch bypassed an active writer"),
            Err(err) => err,
        };

        assert!(matches!(
            err.downcast_ref::<tantivy::TantivyError>(),
            Some(tantivy::TantivyError::LockFailure(LockError::LockBusy, _))
        ));
        assert!(marker.exists(), "contended schema repair must not wipe");
        assert!(index_path.join("meta.json").exists());
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn search_times_out_when_worker_never_replies() {
        // Hold the worker receiver but never drain it: the send buffers
        // successfully, yet no reply is ever produced — the exact shape
        // of a wedged Tantivy worker. With the clock paused, the 5s
        // deadline elapses instantly once the task is idle.
        let (tx, _rx) = mpsc::channel::<SearchCommand>(4);
        let handle = SearchServiceHandle { tx };
        let err = handle
            .search("anything", SearchScopeData::Track, 10)
            .await
            .expect_err("search should time out");
        assert!(
            err.downcast_ref::<SearchServiceError>()
                .is_some_and(|e| matches!(e, SearchServiceError::Timeout(..))),
            "expected Timeout, got {err:?}"
        );
    }

    #[tokio::test]
    async fn reports_unavailable_when_worker_channel_is_closed() {
        let (tx, rx) = mpsc::channel::<SearchCommand>(4);
        drop(rx);
        let handle = SearchServiceHandle { tx };
        let err = handle.num_docs().await.expect_err("should be unavailable");
        assert!(
            err.downcast_ref::<SearchServiceError>()
                .is_some_and(|e| matches!(e, SearchServiceError::Unavailable(_))),
            "expected Unavailable, got {err:?}"
        );
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
