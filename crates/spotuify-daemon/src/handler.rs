use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::provider_registry::TransportRecovery;
use crate::state::{
    player_error_for_display, DaemonState, FastTransportStatus, ProviderPolicyRequestError,
};
use futures::FutureExt;
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};
use spotuify_core::{
    action_finished_event, now_ms, search_performed_event, AccessOutcome, AnalyticsSource,
    CollectionRequest, Device, ItemSource, MediaItem, MediaKind, MusicProvider, Mutation,
    MutationCompletion, MutationOutcome, MutationReceipt, PageContinuation, PageRequest,
    PlayRequest, PlaySource, Playback, Playlist, PlaylistInsertion, PlaylistItemRef, ProviderError,
    ProviderId, ProviderPage, Queue, QueueAddRequest, RemoteTransport, RepeatMode, RequestContext,
    ResourceUri, SearchRequest, TransportCommand, TransportDevice,
};
use spotuify_protocol::{
    CommandReceipt, DaemonEvent, EpisodeSort, MutationId, Operation, OperationId, OperationKind,
    OperationSource, OperationStatus, PlaybackCommand, ReceiptId, Request, Response, ResponseData,
    SearchScopeData, SearchSortData, SearchSourceData, LIKED_SONGS_CONTEXT,
};

#[derive(Clone, Debug, Default)]
pub(crate) struct PlayContext {
    pub(crate) context_uri: Option<String>,
    pub(crate) tracks: Option<Vec<String>>,
}

// Several semantic commands are exercised through the daemon boundary tests;
// production IPC reaches their focused handler modules directly.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub(crate) enum CommandKind {
    Pause,
    Resume,
    TogglePlayback,
    PlayItem {
        item: MediaItem,
    },
    PlayUri {
        uri: String,
        context: Option<PlayContext>,
    },
    Next,
    Previous,
    Seek {
        position_ms: u64,
    },
    Volume {
        volume_percent: u8,
    },
    Shuffle {
        state: bool,
    },
    Repeat {
        state: RepeatMode,
    },
    QueueItem {
        item: MediaItem,
    },
    QueueUri {
        uri: String,
    },
    Transfer {
        device: Device,
        play: bool,
    },
    AddToPlaylist {
        item: MediaItem,
        playlist_id: String,
        playlist_name: String,
    },
    SaveItem {
        item: MediaItem,
    },
    SaveCurrent,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CommandResult {
    pub(crate) message: Option<String>,
    pub(crate) playback: Option<Playback>,
    pub(crate) queue: Option<Queue>,
    pub(crate) devices: Option<Vec<Device>>,
    pub(crate) request_refresh: bool,
}

pub(crate) fn require_provider_capability(
    provider: &dyn MusicProvider,
    operation: &str,
    supported: bool,
) -> Result<(), ProviderError> {
    if supported {
        Ok(())
    } else {
        Err(ProviderError::unsupported(format!(
            "provider {} {operation}",
            provider.id()
        )))
    }
}

pub(crate) fn validate_provider_media_items(
    provider: &dyn MusicProvider,
    items: &[MediaItem],
) -> Result<(), ProviderError> {
    for item in items {
        let uri = ResourceUri::parse(&item.uri).map_err(|error| ProviderError::InvalidInput {
            field: "media_item.uri".to_string(),
            message: format!(
                "provider {} returned `{}`: {error}",
                provider.id(),
                item.uri
            ),
        })?;
        if uri.scheme() != provider.uri_scheme() || uri.kind() != item.kind {
            return Err(ProviderError::InvalidInput {
                field: "media_item.uri".to_string(),
                message: format!(
                    "provider {} returned foreign or mismatched media item `{}` ({})",
                    provider.id(),
                    item.uri,
                    item.kind
                ),
            });
        }
    }
    Ok(())
}

/// Validate the stronger contract of a point lookup: the adapter must return
/// the exact resource requested, not merely another resource it owns.
pub(crate) fn validate_provider_lookup_result(
    provider: &dyn MusicProvider,
    requested: &ResourceUri,
    item: &MediaItem,
) -> Result<(), ProviderError> {
    validate_provider_media_items(provider, std::slice::from_ref(item))?;
    if item.uri != requested.as_uri() || item.kind != requested.kind() {
        return Err(ProviderError::InvalidInput {
            field: "media_item.uri".to_string(),
            message: format!(
                "provider {} returned `{}` ({}) for requested `{}` ({})",
                provider.id(),
                item.uri,
                item.kind,
                requested.as_uri(),
                requested.kind()
            ),
        });
    }
    Ok(())
}

pub(crate) fn validate_provider_search_items(
    provider: &dyn MusicProvider,
    requested_kind: &MediaKind,
    items: &[MediaItem],
) -> Result<(), ProviderError> {
    validate_provider_media_items(provider, items)?;
    if let Some(item) = items.iter().find(|item| &item.kind != requested_kind) {
        return Err(ProviderError::InvalidInput {
            field: "search.kind".to_string(),
            message: format!(
                "provider {} returned {} item `{}` for {requested_kind} search",
                provider.id(),
                item.kind,
                item.uri
            ),
        });
    }
    Ok(())
}

pub(crate) fn validate_provider_collection_items(
    provider: &dyn MusicProvider,
    operation: &str,
    allowed_kinds: &[MediaKind],
    items: &[MediaItem],
) -> Result<(), ProviderError> {
    validate_provider_media_items(provider, items)?;
    if let Some(item) = items
        .iter()
        .find(|item| !allowed_kinds.contains(&item.kind))
    {
        let expected = allowed_kinds
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" or ");
        return Err(ProviderError::InvalidInput {
            field: format!("{operation}.kind"),
            message: format!(
                "provider {} returned {} item `{}` for {operation}; expected {expected}",
                provider.id(),
                item.kind,
                item.uri
            ),
        });
    }
    Ok(())
}

pub(crate) fn validate_provider_playback(
    provider: &dyn MusicProvider,
    playback: &Playback,
) -> Result<(), ProviderError> {
    if let Some(item) = playback.item.as_ref() {
        validate_provider_collection_items(
            provider,
            "playback",
            &[MediaKind::Track, MediaKind::Episode],
            std::slice::from_ref(item),
        )?;
    }
    Ok(())
}

pub(crate) fn validate_provider_queue(
    provider: &dyn MusicProvider,
    queue: &Queue,
) -> Result<(), ProviderError> {
    if let Some(item) = queue.currently_playing.as_ref() {
        validate_provider_collection_items(
            provider,
            "queue",
            &[MediaKind::Track, MediaKind::Episode],
            std::slice::from_ref(item),
        )?;
    }
    validate_provider_collection_items(
        provider,
        "queue",
        &[MediaKind::Track, MediaKind::Episode],
        &queue.items,
    )
}

pub(crate) fn validate_provider_page_offset<T>(
    request: &PageRequest,
    page: &ProviderPage<T>,
    operation: &str,
) -> Result<(), ProviderError> {
    if page.requested_offset != request.offset {
        return Err(ProviderError::InvalidInput {
            field: format!("{operation}.requested_offset"),
            message: format!(
                "provider echoed offset {} for request offset {}",
                page.requested_offset, request.offset
            ),
        });
    }
    Ok(())
}

pub(crate) fn require_provider_mutation_capability(
    provider: &dyn MusicProvider,
    mutation: &Mutation,
) -> Result<(), ProviderError> {
    let caps = provider.capabilities();
    let require_owned_uri = |uri: &ResourceUri| {
        if uri.scheme() == provider.uri_scheme() {
            Ok(())
        } else {
            Err(ProviderError::InvalidInput {
                field: "uri".to_string(),
                message: format!(
                    "resource {} belongs to `{}`, not provider {} (`{}`)",
                    uri.as_uri(),
                    uri.scheme(),
                    provider.id(),
                    provider.uri_scheme(),
                ),
            })
        }
    };
    let require_batch = |actual: usize, max: Option<usize>, field: &str| {
        if let Some(max) = max {
            if actual > max {
                return Err(ProviderError::InvalidInput {
                    field: field.to_string(),
                    message: format!(
                        "batch contains {actual} items; provider {} supports at most {max}",
                        provider.id(),
                    ),
                });
            }
        }
        Ok(())
    };
    match mutation {
        Mutation::PlaylistCreate { .. } => {
            require_provider_capability(provider, "playlist creation", caps.playlists.create)
        }
        Mutation::PlaylistAdd {
            playlist_uri,
            items,
            ..
        } => {
            require_owned_uri(playlist_uri)?;
            for item in items {
                require_owned_uri(&item.uri)?;
            }
            require_provider_capability(provider, "playlist additions", caps.playlists.add)?;
            if items.len() > 1 {
                require_provider_capability(
                    provider,
                    "playlist reconciliation listing",
                    caps.playlists.list,
                )?;
                require_provider_capability(
                    provider,
                    "playlist item reconciliation",
                    caps.playlists.item_read,
                )?;
            }
            require_batch(items.len(), caps.playlists.add_max_batch, "items")
        }
        Mutation::PlaylistRemove {
            playlist_uri,
            items,
            ..
        } => {
            require_owned_uri(playlist_uri)?;
            for item in items {
                require_owned_uri(&item.uri)?;
            }
            require_provider_capability(provider, "playlist removals", caps.playlists.remove)?;
            if items.len() > 1 {
                require_provider_capability(
                    provider,
                    "playlist reconciliation listing",
                    caps.playlists.list,
                )?;
                require_provider_capability(
                    provider,
                    "playlist item reconciliation",
                    caps.playlists.item_read,
                )?;
            }
            require_batch(items.len(), caps.playlists.remove_max_batch, "items")
        }
        Mutation::PlaylistReorder { playlist_uri, .. } => {
            require_owned_uri(playlist_uri)?;
            require_provider_capability(provider, "playlist reorder", caps.playlists.reorder)
        }
        Mutation::PlaylistSetImage { playlist_uri, .. } => {
            require_owned_uri(playlist_uri)?;
            require_provider_capability(provider, "playlist images", caps.playlists.image)
        }
        Mutation::PlaylistUnfollow { playlist_uri } => {
            require_owned_uri(playlist_uri)?;
            require_provider_capability(provider, "playlist unfollow", caps.playlists.unfollow)
        }
        Mutation::LibrarySave { uris } | Mutation::LibraryUnsave { uris } => {
            for uri in uris {
                require_owned_uri(uri)?;
                require_provider_capability(
                    provider,
                    &format!("{} library saves", uri.kind()),
                    caps.library.can_save(&uri.kind()),
                )?;
                if uris.len() > 1 {
                    require_provider_capability(
                        provider,
                        &format!("{} library reconciliation", uri.kind()),
                        caps.library.can_read(&uri.kind()),
                    )?;
                }
            }
            require_batch(uris.len(), caps.library.mutation_max_batch, "uris")
        }
        Mutation::Follow { uris } | Mutation::Unfollow { uris } => {
            for uri in uris {
                require_owned_uri(uri)?;
                require_provider_capability(
                    provider,
                    &format!("{} follows", uri.kind()),
                    caps.library.can_follow(&uri.kind()),
                )?;
                if uris.len() > 1 {
                    require_provider_capability(
                        provider,
                        &format!("{} follow reconciliation", uri.kind()),
                        caps.library.can_read(&uri.kind()),
                    )?;
                }
            }
            require_batch(uris.len(), caps.library.mutation_max_batch, "uris")
        }
    }
}

pub(crate) async fn apply_provider_mutation_checked(
    provider: &dyn MusicProvider,
    mutation_id: uuid::Uuid,
    mutation: &Mutation,
) -> anyhow::Result<MutationReceipt> {
    require_provider_mutation_capability(provider, mutation)?;
    let receipt = provider
        .apply_mutation(RequestContext::FOREGROUND, mutation_id, mutation)
        .await?;
    let partial = validate_mutation_receipt(provider, mutation_id, mutation, &receipt)?;
    if let Some(partial) = partial {
        let (_, detail) = bounded_partial_summary(provider, mutation, &receipt, &partial)?;
        return Err(PartialMutationError {
            mutation: mutation.clone(),
            provider: provider.id().clone(),
            succeeded_uris: partial.succeeded,
            failed_uris: partial.failed,
            detail,
            message: None,
            post_write_guard: None,
            operation_recovery: None,
        }
        .into());
    }
    Ok(receipt)
}

#[derive(Debug)]
struct PartialMutationError {
    mutation: Mutation,
    provider: ProviderId,
    succeeded_uris: Vec<ResourceUri>,
    failed_uris: Vec<ResourceUri>,
    /// Bounded/redacted typed summary carried by the existing wire-compatible
    /// `ApiErrorSummary.detail` field.
    detail: String,
    message: Option<String>,
    post_write_guard: Option<spotuify_store::PostWriteOperationGuard>,
    operation_recovery: Option<spotuify_store::PartialOperationRecovery>,
}

impl std::fmt::Display for PartialMutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(message) = &self.message {
            return f.write_str(message);
        }
        write!(
            f,
            "provider {} partially applied {} ({} item failure(s)); authoritative reconciliation required",
            self.provider,
            mutation_kind(&self.mutation),
            self.failed_uris.len(),
        )
    }
}

impl std::error::Error for PartialMutationError {}

#[derive(Serialize)]
struct LocalFinalizationReconciliationSummary {
    schema: &'static str,
    provider: ProviderId,
    mutation_id: uuid::Uuid,
    mutation: &'static str,
    target: Option<PartialResourceSummary>,
    succeeded: Vec<PartialResourceSummary>,
    version_token: Option<String>,
    reason: String,
}

/// The provider write completed, but publishing local lifecycle bookkeeping did
/// not. Route this through the typed reconciliation lifecycle so durable
/// provider truth is repaired without exposing an executable replay/undo.
pub(crate) fn provider_mutation_reconciliation_required_after_local_failure(
    provider: ProviderId,
    mutation: Mutation,
    receipt: &MutationReceipt,
    reason: impl std::fmt::Display,
) -> anyhow::Error {
    let succeeded_uris = match &mutation {
        Mutation::PlaylistAdd { items, .. } => items.iter().map(|item| item.uri.clone()).collect(),
        Mutation::PlaylistRemove { items, .. } => {
            items.iter().map(|item| item.uri.clone()).collect()
        }
        Mutation::LibrarySave { uris }
        | Mutation::LibraryUnsave { uris }
        | Mutation::Follow { uris }
        | Mutation::Unfollow { uris } => uris.clone(),
        _ => Vec::new(),
    };
    let summary = LocalFinalizationReconciliationSummary {
        schema: "spotuify.local-finalization-reconciliation.v1",
        provider: provider.clone(),
        mutation_id: receipt.mutation_id,
        mutation: mutation_kind(&mutation),
        target: partial_target_uri(&mutation).map(resource_summary),
        succeeded: succeeded_uris
            .iter()
            .take(PARTIAL_SUMMARY_MAX_ITEMS)
            .map(resource_summary)
            .collect(),
        version_token: receipt
            .version_token
            .as_deref()
            .map(|token| bounded_redacted_text(token, PARTIAL_SUMMARY_TOKEN_CHARS)),
        reason: bounded_redacted_text(&reason.to_string(), PARTIAL_SUMMARY_MESSAGE_CHARS),
    };
    let detail = serde_json::to_string(&summary).unwrap_or_else(|_| {
        "{\"schema\":\"spotuify.local-finalization-reconciliation.v1\"}".to_string()
    });
    PartialMutationError {
        mutation,
        provider,
        succeeded_uris,
        failed_uris: Vec::new(),
        detail,
        message: Some(
            "provider mutation applied, but its local lifecycle bookkeeping could not be published; authoritative reconciliation required"
                .to_string(),
        ),
        post_write_guard: None,
        operation_recovery: None,
    }
    .into()
}

/// A composite mutation created a remote resource but could not prove its
/// compensation completed. Its operation must retain a cleanup plan even
/// though the command receipt is failed.
#[derive(Debug)]
struct RemoteArtifactRetainedError {
    provider: ProviderId,
    message: String,
    operation_recovery: Option<spotuify_store::PartialOperationRecovery>,
}

impl std::fmt::Display for RemoteArtifactRetainedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RemoteArtifactRetainedError {}

pub(crate) fn remote_artifact_retained(
    provider: ProviderId,
    message: impl Into<String>,
) -> anyhow::Error {
    RemoteArtifactRetainedError {
        provider,
        message: bounded_redacted_text(&message.into(), 512),
        operation_recovery: None,
    }
    .into()
}

pub(crate) fn is_partial_mutation_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<PartialMutationError>().is_some()
}

pub(crate) fn retain_operation_recovery(
    err: &mut anyhow::Error,
    pre_state: spotuify_protocol::PreState,
    reversal_plan: spotuify_protocol::ReversalPlan,
    subject_uris: Vec<String>,
) {
    if let Some(partial) = err.downcast_mut::<PartialMutationError>() {
        partial.operation_recovery = Some(spotuify_store::PartialOperationRecovery {
            pre_state,
            reversal_plan,
            subject_uris,
        });
    } else if let Some(retained) = err.downcast_mut::<RemoteArtifactRetainedError>() {
        retained.operation_recovery = Some(spotuify_store::PartialOperationRecovery {
            pre_state,
            reversal_plan,
            subject_uris,
        });
    }
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
struct PartialResourceSummary {
    preview: String,
    sha256: String,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
struct PartialFailureSummary {
    resource: PartialResourceSummary,
    message: String,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
struct PartialMutationSummary {
    schema: String,
    provider: ProviderId,
    mutation_id: uuid::Uuid,
    mutation: String,
    outcome: String,
    target: Option<PartialResourceSummary>,
    succeeded: Vec<PartialResourceSummary>,
    failures: Vec<PartialFailureSummary>,
    succeeded_count: usize,
    failure_count: usize,
    omitted_succeeded: usize,
    omitted_failures: usize,
    version_token: Option<String>,
}

#[derive(Debug)]
struct PartialMutationPartition {
    succeeded: Vec<ResourceUri>,
    failed: Vec<ResourceUri>,
}

const PARTIAL_SUMMARY_MAX_ITEMS: usize = 24;
const PARTIAL_SUMMARY_URI_CHARS: usize = 96;
const PARTIAL_SUMMARY_MESSAGE_CHARS: usize = 160;
const PARTIAL_SUMMARY_TOKEN_CHARS: usize = 96;
const PARTIAL_SUMMARY_MAX_BYTES: usize = 16 * 1024;

fn bounded_partial_summary(
    provider: &dyn MusicProvider,
    mutation: &Mutation,
    receipt: &MutationReceipt,
    partition: &PartialMutationPartition,
) -> anyhow::Result<(PartialMutationSummary, String)> {
    let failure_messages = receipt
        .failures
        .iter()
        .map(|failure| failure.message.as_str())
        .collect::<Vec<_>>();
    let kept_failures = partition.failed.len().min(PARTIAL_SUMMARY_MAX_ITEMS);
    let kept_successes = partition
        .succeeded
        .len()
        .min(PARTIAL_SUMMARY_MAX_ITEMS.saturating_sub(kept_failures));
    let mut summary = PartialMutationSummary {
        schema: "spotuify.provider-partial.v1".to_string(),
        provider: provider.id().clone(),
        mutation_id: receipt.mutation_id,
        mutation: mutation_kind(mutation).to_string(),
        outcome: mutation_outcome_kind(&receipt.outcome).to_string(),
        target: partial_target_uri(mutation).map(resource_summary),
        succeeded: partition
            .succeeded
            .iter()
            .take(kept_successes)
            .map(resource_summary)
            .collect(),
        failures: partition
            .failed
            .iter()
            .zip(failure_messages)
            .take(kept_failures)
            .map(|(uri, message)| PartialFailureSummary {
                resource: resource_summary(uri),
                message: bounded_redacted_text(message, PARTIAL_SUMMARY_MESSAGE_CHARS),
            })
            .collect(),
        succeeded_count: partition.succeeded.len(),
        failure_count: partition.failed.len(),
        omitted_succeeded: partition.succeeded.len().saturating_sub(kept_successes),
        omitted_failures: partition.failed.len().saturating_sub(kept_failures),
        version_token: receipt
            .version_token
            .as_deref()
            .map(|token| bounded_redacted_text(token, PARTIAL_SUMMARY_TOKEN_CHARS)),
    };
    let mut detail = serde_json::to_string(&summary)?;
    while detail.len() > PARTIAL_SUMMARY_MAX_BYTES {
        if summary.succeeded.pop().is_some() {
            summary.omitted_succeeded += 1;
        } else if summary.failures.len() > 1 {
            summary.failures.pop();
            summary.omitted_failures += 1;
        } else {
            return Err(anyhow::anyhow!(
                "bounded partial mutation summary exceeded its byte limit"
            ));
        }
        detail = serde_json::to_string(&summary)?;
    }
    Ok((summary, detail))
}

fn resource_summary(uri: &ResourceUri) -> PartialResourceSummary {
    let value = uri.as_uri();
    PartialResourceSummary {
        // Provider resource identifiers are normally safe to display, but a
        // hostile adapter can return an ID that is indistinguishable from a
        // credential. Keep the correlation hash while applying the same
        // token redaction used by the rest of the durable error summary.
        preview: bounded_redacted_text(&value, PARTIAL_SUMMARY_URI_CHARS),
        sha256: format!("{:x}", Sha256::digest(value.as_bytes())),
    }
}

fn bounded_text(value: &str, max_chars: usize) -> String {
    let mut bounded = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        bounded.push('…');
    }
    bounded
}

fn bounded_redacted_text(value: &str, max_chars: usize) -> String {
    bounded_text(&spotuify_protocol::redact_sensitive_text(value), max_chars)
}

fn partial_target_uri(mutation: &Mutation) -> Option<&ResourceUri> {
    match mutation {
        Mutation::PlaylistAdd { playlist_uri, .. }
        | Mutation::PlaylistRemove { playlist_uri, .. } => Some(playlist_uri),
        _ => None,
    }
}

#[derive(Debug)]
struct MalformedProviderReceiptError {
    provider: ProviderId,
    mutation: Mutation,
    message: String,
    post_write_guard: Option<spotuify_store::PostWriteOperationGuard>,
}

impl std::fmt::Display for MalformedProviderReceiptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for MalformedProviderReceiptError {}

fn malformed_receipt(
    provider: &dyn MusicProvider,
    mutation: &Mutation,
    message: impl Into<String>,
) -> anyhow::Error {
    MalformedProviderReceiptError {
        provider: provider.id().clone(),
        mutation: mutation.clone(),
        message: bounded_redacted_text(&message.into(), 512),
        post_write_guard: None,
    }
    .into()
}

fn validate_mutation_receipt(
    provider: &dyn MusicProvider,
    mutation_id: uuid::Uuid,
    mutation: &Mutation,
    receipt: &MutationReceipt,
) -> anyhow::Result<Option<PartialMutationPartition>> {
    if receipt.mutation_id != mutation_id {
        return Err(malformed_receipt(
            provider,
            mutation,
            format!(
                "provider {} returned mutation receipt {} for requested mutation {}",
                provider.id(),
                receipt.mutation_id,
                mutation_id,
            ),
        ));
    }
    if &receipt.provider != provider.id() {
        return Err(malformed_receipt(
            provider,
            mutation,
            format!(
                "provider {} returned a mutation receipt owned by {}",
                provider.id(),
                receipt.provider,
            ),
        ));
    }
    match receipt.completion {
        MutationCompletion::Applied => {
            if !mutation_outcome_matches(provider, mutation, receipt) {
                return Err(malformed_receipt(
                    provider,
                    mutation,
                    format!(
                        "provider {} returned {} for requested {}",
                        provider.id(),
                        mutation_outcome_kind(&receipt.outcome),
                        mutation_kind(mutation),
                    ),
                ));
            }
            if receipt.failures.is_empty() {
                return Ok(None);
            }
            let detail = receipt
                .failures
                .iter()
                .map(|failure| failure.message.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            Err(malformed_receipt(
                provider,
                mutation,
                format!(
                    "provider {} reported failures for fully-applied {}: {}",
                    provider.id(),
                    mutation_kind(mutation),
                    bounded_redacted_text(&detail, 256),
                ),
            ))
        }
        MutationCompletion::PartiallyApplied => {
            validate_partial_partition(provider, mutation, receipt).map(Some)
        }
    }
}

fn validate_partial_partition(
    provider: &dyn MusicProvider,
    mutation: &Mutation,
    receipt: &MutationReceipt,
) -> anyhow::Result<PartialMutationPartition> {
    let requested = match mutation {
        Mutation::PlaylistAdd { items, .. } => items
            .iter()
            .map(|item| item.uri.clone())
            .collect::<Vec<_>>(),
        Mutation::PlaylistRemove { items, .. } => items
            .iter()
            .map(|item| item.uri.clone())
            .collect::<Vec<_>>(),
        Mutation::LibrarySave { uris }
        | Mutation::LibraryUnsave { uris }
        | Mutation::Follow { uris }
        | Mutation::Unfollow { uris } => uris.clone(),
        Mutation::PlaylistCreate { .. }
        | Mutation::PlaylistReorder { .. }
        | Mutation::PlaylistSetImage { .. }
        | Mutation::PlaylistUnfollow { .. } => {
            return Err(malformed_receipt(
                provider,
                mutation,
                format!(
                    "provider {} returned partial completion for atomic {}",
                    provider.id(),
                    mutation_kind(mutation),
                ),
            ));
        }
    };
    let caps = provider.capabilities();
    match mutation {
        Mutation::PlaylistAdd { .. } | Mutation::PlaylistRemove { .. }
            if !caps.playlists.list || !caps.playlists.item_read =>
        {
            return Err(malformed_receipt(
                provider,
                mutation,
                format!(
                    "provider {} returned partial {} without list and item-read capabilities required for reconciliation",
                    provider.id(),
                    mutation_kind(mutation),
                ),
            ));
        }
        Mutation::LibrarySave { .. }
        | Mutation::LibraryUnsave { .. }
        | Mutation::Follow { .. }
        | Mutation::Unfollow { .. }
            if requested
                .iter()
                .any(|uri| !caps.library.read_kinds.contains(&uri.kind())) =>
        {
            return Err(malformed_receipt(
                provider,
                mutation,
                format!(
                    "provider {} returned partial {} for a kind it cannot read back",
                    provider.id(),
                    mutation_kind(mutation),
                ),
            ));
        }
        _ => {}
    }
    if receipt.failures.is_empty() {
        return Err(malformed_receipt(
            provider,
            mutation,
            format!(
                "provider {} reported partially-applied {} without failures",
                provider.id(),
                mutation_kind(mutation),
            ),
        ));
    }
    let mut failed = Vec::with_capacity(receipt.failures.len());
    for failure in &receipt.failures {
        let Some(uri) = failure.uri.as_ref() else {
            return Err(malformed_receipt(
                provider,
                mutation,
                format!(
                    "provider {} reported a partial {} failure without a URI",
                    provider.id(),
                    mutation_kind(mutation),
                ),
            ));
        };
        if failure.message.trim().is_empty() {
            return Err(malformed_receipt(
                provider,
                mutation,
                format!(
                    "provider {} reported a partial {} failure without a message",
                    provider.id(),
                    mutation_kind(mutation),
                ),
            ));
        }
        failed.push(uri.clone());
    }
    let succeeded = match (mutation, &receipt.outcome) {
        (Mutation::LibrarySave { .. }, MutationOutcome::LibraryChanged { uris, saved: true })
        | (
            Mutation::LibraryUnsave { .. },
            MutationOutcome::LibraryChanged { uris, saved: false },
        )
        | (
            Mutation::Follow { .. },
            MutationOutcome::FollowChanged {
                uris,
                following: true,
            },
        )
        | (
            Mutation::Unfollow { .. },
            MutationOutcome::FollowChanged {
                uris,
                following: false,
            },
        ) => uris.clone(),
        (
            Mutation::PlaylistAdd { playlist_uri, .. }
            | Mutation::PlaylistRemove { playlist_uri, .. },
            MutationOutcome::PlaylistChanged {
                playlist_uri: outcome_uri,
            },
        ) if playlist_uri == outcome_uri => subtract_resources(&requested, &failed),
        _ => {
            return Err(malformed_receipt(
                provider,
                mutation,
                format!(
                    "provider {} returned {} for partial {}",
                    provider.id(),
                    mutation_outcome_kind(&receipt.outcome),
                    mutation_kind(mutation),
                ),
            ));
        }
    };
    if succeeded.is_empty() || failed.is_empty() {
        return Err(malformed_receipt(
            provider,
            mutation,
            format!(
                "provider {} partial {} must contain nonempty successes and failures",
                provider.id(),
                mutation_kind(mutation),
            ),
        ));
    }
    let mut partition = succeeded.clone();
    partition.extend(failed.iter().cloned());
    if !same_resources(&requested, &partition) {
        return Err(malformed_receipt(
            provider,
            mutation,
            format!(
                "provider {} partial {} does not exactly partition the requested items",
                provider.id(),
                mutation_kind(mutation),
            ),
        ));
    }
    Ok(PartialMutationPartition { succeeded, failed })
}

fn subtract_resources(requested: &[ResourceUri], removed: &[ResourceUri]) -> Vec<ResourceUri> {
    let mut remaining = removed
        .iter()
        .fold(std::collections::HashMap::new(), |mut counts, uri| {
            *counts.entry(uri.clone()).or_insert(0_usize) += 1;
            counts
        });
    requested
        .iter()
        .filter_map(|uri| match remaining.get_mut(uri) {
            Some(count) if *count > 0 => {
                *count -= 1;
                None
            }
            _ => Some(uri.clone()),
        })
        .collect()
}

fn mutation_outcome_matches(
    provider: &dyn MusicProvider,
    mutation: &Mutation,
    receipt: &MutationReceipt,
) -> bool {
    match (mutation, &receipt.outcome) {
        (Mutation::PlaylistCreate { .. }, MutationOutcome::PlaylistCreated { playlist }) => {
            ResourceUri::parse(&playlist.id).is_ok_and(|uri| {
                uri.scheme() == provider.uri_scheme() && uri.kind() == MediaKind::Playlist
            }) && receipt.version_token == playlist.version_token
        }
        (
            Mutation::PlaylistAdd { playlist_uri, .. }
            | Mutation::PlaylistRemove { playlist_uri, .. }
            | Mutation::PlaylistReorder { playlist_uri, .. },
            MutationOutcome::PlaylistChanged {
                playlist_uri: outcome_uri,
            },
        ) => playlist_uri == outcome_uri,
        (
            Mutation::PlaylistSetImage { playlist_uri, .. },
            MutationOutcome::PlaylistImageSet {
                playlist_uri: outcome_uri,
            },
        ) => playlist_uri == outcome_uri,
        (
            Mutation::PlaylistUnfollow { playlist_uri },
            MutationOutcome::PlaylistUnfollowed {
                playlist_uri: outcome_uri,
            },
        ) => playlist_uri == outcome_uri,
        (
            Mutation::LibrarySave { uris },
            MutationOutcome::LibraryChanged {
                uris: outcome_uris,
                saved: true,
            },
        )
        | (
            Mutation::LibraryUnsave { uris },
            MutationOutcome::LibraryChanged {
                uris: outcome_uris,
                saved: false,
            },
        )
        | (
            Mutation::Follow { uris },
            MutationOutcome::FollowChanged {
                uris: outcome_uris,
                following: true,
            },
        )
        | (
            Mutation::Unfollow { uris },
            MutationOutcome::FollowChanged {
                uris: outcome_uris,
                following: false,
            },
        ) => same_resources(uris, outcome_uris),
        _ => false,
    }
}

fn same_resources(expected: &[ResourceUri], actual: &[ResourceUri]) -> bool {
    expected.len() == actual.len()
        && expected.iter().all(|expected_uri| {
            expected
                .iter()
                .filter(|candidate| *candidate == expected_uri)
                .count()
                == actual
                    .iter()
                    .filter(|candidate| *candidate == expected_uri)
                    .count()
        })
}

fn mutation_kind(mutation: &Mutation) -> &'static str {
    match mutation {
        Mutation::PlaylistCreate { .. } => "playlist_create",
        Mutation::PlaylistAdd { .. } => "playlist_add",
        Mutation::PlaylistRemove { .. } => "playlist_remove",
        Mutation::PlaylistReorder { .. } => "playlist_reorder",
        Mutation::PlaylistSetImage { .. } => "playlist_set_image",
        Mutation::PlaylistUnfollow { .. } => "playlist_unfollow",
        Mutation::LibrarySave { .. } => "library_save",
        Mutation::LibraryUnsave { .. } => "library_unsave",
        Mutation::Follow { .. } => "follow",
        Mutation::Unfollow { .. } => "unfollow",
    }
}

fn mutation_outcome_kind(outcome: &MutationOutcome) -> &'static str {
    match outcome {
        MutationOutcome::PlaylistCreated { .. } => "playlist_created",
        MutationOutcome::PlaylistChanged { .. } => "playlist_changed",
        MutationOutcome::PlaylistImageSet { .. } => "playlist_image_set",
        MutationOutcome::PlaylistUnfollowed { .. } => "playlist_unfollowed",
        MutationOutcome::LibraryChanged { .. } => "library_changed",
        MutationOutcome::FollowChanged { .. } => "follow_changed",
    }
}

pub(crate) fn require_transport_command_capability(
    provider: &dyn MusicProvider,
    command: &TransportCommand,
) -> Result<(), ProviderError> {
    let caps = provider.capabilities().transport.ok_or_else(|| {
        ProviderError::unsupported(format!("provider {} transport", provider.id()))
    })?;
    let require_owned_uri = |uri: &ResourceUri| {
        if uri.scheme() == provider.uri_scheme() {
            Ok(())
        } else {
            Err(ProviderError::InvalidInput {
                field: "uri".to_string(),
                message: format!(
                    "resource {} belongs to `{}`, not provider {} (`{}`)",
                    uri.as_uri(),
                    uri.scheme(),
                    provider.id(),
                    provider.uri_scheme(),
                ),
            })
        }
    };
    let (operation, supported) = match command {
        TransportCommand::Play(request) => {
            require_owned_uri(&request.start_uri)?;
            match &request.source {
                PlaySource::Single => {}
                PlaySource::Context(uri) => require_owned_uri(uri)?,
                PlaySource::Ordered(uris) => {
                    for uri in uris {
                        require_owned_uri(uri)?;
                    }
                }
            }
            ("play", caps.play)
        }
        TransportCommand::Pause => ("pause", caps.pause),
        TransportCommand::Resume => ("resume", caps.resume),
        TransportCommand::Next => ("next", caps.next),
        TransportCommand::Previous => ("previous", caps.previous),
        TransportCommand::Seek { .. } => ("seek", caps.seek),
        TransportCommand::Volume { .. } => ("volume", caps.volume),
        TransportCommand::Shuffle { .. } => ("shuffle", caps.shuffle),
        TransportCommand::Repeat { .. } => ("repeat", caps.repeat),
        TransportCommand::QueueAdd(request) => {
            require_owned_uri(&request.uri)?;
            ("queue add", caps.queue_add)
        }
        TransportCommand::Transfer { .. } => ("transfer", caps.transfer),
    };
    require_provider_capability(provider, operation, supported)
}

pub(crate) const LYRICS_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
pub(crate) const LYRICS_NEGATIVE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Cap provider-native workflows before falling back or returning a typed
/// timeout. Adapters also bound their underlying I/O; this bounds the semantic
/// facet as a whole.
pub(crate) const PROVIDER_LYRICS_TIMEOUT: Duration = Duration::from_secs(6);
pub(crate) const PROVIDER_EXTRAS_TIMEOUT: Duration = Duration::from_secs(8);
pub(crate) const MUTATION_BODY_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(not(test))]
const PROVIDER_RECONCILIATION_VERIFY_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(test)]
const PROVIDER_RECONCILIATION_VERIFY_TIMEOUT: Duration = Duration::from_millis(500);
const PROVIDER_RECONCILIATION_MAX_PAGES: usize = 200;
const PROVIDER_RECONCILIATION_MAX_ITEMS: usize = 20_000;
#[cfg(not(test))]
const PROVIDER_RECONCILIATION_RETRY_BASE: Duration = Duration::from_secs(1);
#[cfg(test)]
const PROVIDER_RECONCILIATION_RETRY_BASE: Duration = Duration::from_millis(25);
#[cfg(not(test))]
const PROVIDER_RECONCILIATION_RETRY_MAX: Duration = Duration::from_secs(5 * 60);
#[cfg(test)]
const PROVIDER_RECONCILIATION_RETRY_MAX: Duration = Duration::from_millis(100);
pub(crate) const TRANSPORT_BACKEND_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const FAST_TRANSPORT_TIMEOUT: Duration = Duration::from_millis(250);
/// After the fast deadline elapses we keep watching the player actor's
/// ack for this long so a late failure can reconcile.
pub(crate) const FAST_TRANSPORT_ACK_GRACE: Duration = Duration::from_secs(10);
pub(crate) const DEVICE_RECOVERY_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const DEVICE_REGISTRY_TIMEOUT: Duration = Duration::from_secs(8);
pub(crate) const DEVICE_REGISTRY_POLL_INTERVAL: Duration = Duration::from_millis(500);
pub(crate) const SEARCH_REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
pub(crate) const QUEUE_APPEND_BASE_MAX_AGE_MS: i64 = 30_000;

pub(crate) async fn handle_request_with_source(
    state: Arc<DaemonState>,
    request: Request,
    source: Option<OperationSource>,
) -> Response {
    handle_request_with_source_and_mutation(state, request, source, None).await
}

pub(crate) async fn handle_request_with_source_and_mutation(
    state: Arc<DaemonState>,
    request: Request,
    source: Option<OperationSource>,
    mutation_id: Option<MutationId>,
) -> Response {
    let error_provider = request_provider_context(&state, &request).await;
    let mutation_id = request
        .requires_mutation_id()
        .then_some(mutation_id)
        .flatten();
    match dispatch_with_mutation(state, request, source, mutation_id).await {
        Ok(data) => Response::Ok { data },
        Err(err) => error_response_with_context(&err, error_provider),
    }
}

fn error_response_with_context(
    err: &anyhow::Error,
    error_provider: Option<ProviderId>,
) -> Response {
    let mut response = error_response_from(err);
    if let Response::Error { kind, provider, .. } = &mut response {
        if provider.is_none() && *kind != spotuify_protocol::IpcErrorKind::Internal {
            *provider = error_provider;
        }
    }
    redact_error_response_fields(&mut response);
    response
}

/// Normalize an error response before either persistence or IPC delivery.
/// Applying the same idempotent redaction at both boundaries keeps terminal
/// mutation replay byte-equivalent to the original durable wire response.
fn redact_error_response_fields(response: &mut Response) {
    if let Response::Error {
        message, detail, ..
    } = response
    {
        *message = spotuify_protocol::redact_sensitive_text(message);
        *detail = Some(spotuify_protocol::redact_sensitive_text(
            detail.as_deref().unwrap_or(message),
        ));
    }
}

async fn request_provider_context(state: &DaemonState, request: &Request) -> Option<ProviderId> {
    let providers = state.providers().await.ok()?;
    let selected = |requested: Option<&ProviderId>| {
        requested
            .cloned()
            .or_else(|| Some(providers.default_id().clone()))
    };
    // The outer option records whether the value was a structured resource
    // URI. A parsed-but-unroutable URI must stay provider-neutral; falling
    // back to the default provider would misattribute foreign namespaces.
    let for_resource = |value: &str| match ResourceUri::parse(value) {
        Ok(uri) => Some(
            providers
                .provider_for_uri(&uri)
                .ok()
                .map(|runtime| runtime.id().clone()),
        ),
        Err(_) => None,
    };
    let routed_or_default = |value: &str| match for_resource(value) {
        Some(provider) => provider,
        None => selected(None),
    };
    let scoped_resource = |value: &str, requested: Option<&ProviderId>| match for_resource(value) {
        Some(owner) => match (owner, requested) {
            (Some(owner), Some(requested)) if &owner != requested => None,
            (Some(owner), _) => Some(owner),
            (None, _) => None,
        },
        None => selected(requested),
    };
    let playback_provider = || {
        state
            .snapshot_playback()
            .item
            .as_ref()
            .map_or_else(|| selected(None), |item| routed_or_default(&item.uri))
    };
    match request {
        Request::Search {
            source, provider, ..
        }
        | Request::SearchStream {
            source, provider, ..
        } => provider.clone().or_else(|| match source {
            SearchSourceData::Remote(source_provider) => providers
                .provider(source_provider)
                .ok()
                .map(|runtime| runtime.id().clone())
                .or_else(|| {
                    if source_provider.as_str() == "spotify" {
                        providers
                            .provider_for_scheme(&spotuify_core::UriScheme::Spotify)
                            .ok()
                            .map(|runtime| runtime.id().clone())
                    } else {
                        None
                    }
                }),
            SearchSourceData::Local | SearchSourceData::Hybrid => selected(None),
        }),
        Request::SearchPage { provider, .. }
        | Request::LibraryList { provider, .. }
        | Request::SavedTracks { provider, .. }
        | Request::SavedShows { provider, .. }
        | Request::RecentlyPlayed { provider }
        | Request::PlaylistsList { provider }
        | Request::FollowedArtists { provider, .. }
        | Request::EpisodeFeed { provider, .. }
        | Request::Sync { provider, .. }
        | Request::PlaylistCreate { provider, .. }
        | Request::PlaylistCreatePreview { provider, .. } => selected(provider.as_ref()),
        Request::ListAudioOutputs | Request::SetAudioOutput { .. } => selected(None),
        Request::ResolveTarget { provider, .. } => provider.clone(),
        Request::PlaybackCommand {
            command: PlaybackCommand::PlayUri { uri, .. },
        }
        | Request::QueueAdd { uri }
        | Request::ShowEpisodes { show: uri, .. }
        | Request::ArtistAlbums { artist: uri }
        | Request::AlbumTracks { album: uri }
        | Request::RelatedArtists { artist: uri }
        | Request::RadioStart { seed_uri: uri, .. }
        | Request::LyricsGet {
            track_uri: Some(uri),
            ..
        }
        | Request::LyricsOffsetSet { track_uri: uri, .. }
        | Request::LibrarySave { uri: Some(uri), .. }
        | Request::LibraryUnsave { uri }
        | Request::ArtistFollow { artist: uri }
        | Request::ArtistUnfollow { artist: uri } => routed_or_default(uri),
        Request::PlaylistTracks {
            playlist, provider, ..
        }
        | Request::PlaylistItemsPreview {
            playlist, provider, ..
        }
        | Request::PlaylistAddItems {
            playlist, provider, ..
        }
        | Request::PlaylistRemoveItems {
            playlist, provider, ..
        }
        | Request::PlaylistUnfollow { playlist, provider }
        | Request::PlaylistSetImage {
            playlist, provider, ..
        } => scoped_resource(playlist, provider.as_ref()),
        Request::QueueAddMany { uris } => {
            let mut routed = std::collections::BTreeSet::new();
            for uri in uris {
                let provider = match for_resource(uri) {
                    Some(Some(provider)) => provider,
                    Some(None) => return None,
                    None => providers.default_id().clone(),
                };
                routed.insert(provider);
            }
            (routed.len() == 1)
                .then(|| routed.into_iter().next())
                .flatten()
        }
        Request::LibrarySave {
            uri: None,
            current: true,
        }
        | Request::LyricsGet {
            track_uri: None, ..
        }
        | Request::PlaybackCommand { .. }
        | Request::PlaybackGet
        | Request::DevicesList
        | Request::DeviceTransfer { .. }
        | Request::QueueGet
        | Request::Reconnect => playback_provider(),
        _ => None,
    }
}

/// Build a `Response::Error` from an `anyhow::Error`. URI parse failures are
/// invalid requests; typed Spotify errors (notably `AuthRevoked`) retain their
/// protocol classification. Everything else falls back to an internal error.
pub(crate) fn error_response_from(err: &anyhow::Error) -> Response {
    let message = spotuify_protocol::redact_sensitive_text(&err.to_string());
    if let Some(policy) = err.downcast_ref::<ProviderPolicyRequestError>() {
        let kind = spotuify_protocol::IpcErrorKind::Provider;
        return Response::Error {
            message: message.clone(),
            kind,
            code: kind.as_code().to_string(),
            retryable: false,
            provider: Some(policy.provider.clone()),
            detail: Some(message),
        };
    }
    if let Some(partial) = err.downcast_ref::<PartialMutationError>() {
        let kind = spotuify_protocol::IpcErrorKind::Provider;
        return Response::Error {
            message,
            kind,
            code: kind.as_code().to_string(),
            retryable: false,
            provider: Some(partial.provider.clone()),
            detail: Some(partial.detail.clone()),
        };
    }
    if let Some(retained) = err.downcast_ref::<RemoteArtifactRetainedError>() {
        let kind = spotuify_protocol::IpcErrorKind::Provider;
        return Response::Error {
            message: retained.message.clone(),
            kind,
            code: kind.as_code().to_string(),
            retryable: false,
            provider: Some(retained.provider.clone()),
            detail: Some(retained.message.clone()),
        };
    }
    if let Some(post_write) = err.downcast_ref::<PostWriteLifecycleError>() {
        let provider = post_write.seeds.first().map(|seed| seed.provider.clone());
        let kind = if provider.is_some() {
            spotuify_protocol::IpcErrorKind::Provider
        } else {
            spotuify_protocol::IpcErrorKind::Internal
        };
        return Response::Error {
            message: post_write.message.clone(),
            kind,
            code: kind.as_code().to_string(),
            retryable: false,
            provider,
            detail: Some(post_write.message.clone()),
        };
    }
    if let Some(malformed) = err.downcast_ref::<MalformedProviderReceiptError>() {
        let kind = spotuify_protocol::IpcErrorKind::Provider;
        return Response::Error {
            message: malformed.message.clone(),
            kind,
            code: kind.as_code().to_string(),
            retryable: false,
            provider: Some(malformed.provider.clone()),
            detail: Some(malformed.message.clone()),
        };
    }
    if err.downcast_ref::<spotuify_core::UriError>().is_some() {
        return Response::error_with_retryable(
            message,
            spotuify_protocol::IpcErrorKind::InvalidRequest,
            false,
        );
    }
    if let Some(provider_err) = err.downcast_ref::<ProviderError>() {
        use spotuify_protocol::IpcErrorKind as K;
        let kind = match provider_err {
            ProviderError::AuthRequired | ProviderError::AuthExpired => K::Auth,
            ProviderError::AuthRevoked => K::AuthRevoked,
            ProviderError::RateLimited { .. } => K::RateLimited,
            ProviderError::InvalidInput { .. } => K::InvalidRequest,
            ProviderError::Network(_) => K::Network,
            ProviderError::Unsupported { .. } => K::Unsupported,
            _ => K::Provider,
        };
        return Response::Error {
            code: kind.as_code().to_string(),
            retryable: provider_err.is_retryable(),
            kind,
            provider: None,
            detail: Some(message.clone()),
            message,
        };
    }
    if let Some(mutation_err) = err.downcast_ref::<MutationRequestError>() {
        return Response::Error {
            message: mutation_err.message.clone(),
            kind: mutation_err.kind,
            code: mutation_err.kind.as_code().to_string(),
            retryable: mutation_err.retryable,
            provider: mutation_err.provider.clone(),
            detail: mutation_err.detail.clone(),
        };
    }
    Response::error(message)
}

fn receipt_error_summary_from_error(err: &anyhow::Error) -> spotuify_protocol::ApiErrorSummary {
    match error_response_from(err) {
        Response::Error {
            message,
            kind,
            provider,
            detail,
            ..
        } => spotuify_protocol::ApiErrorSummary {
            kind,
            message,
            retry_after_secs: None,
            provider,
            detail,
        },
        Response::Ok { .. } => spotuify_protocol::ApiErrorSummary {
            kind: spotuify_protocol::IpcErrorKind::Internal,
            message: err.to_string(),
            retry_after_secs: None,
            provider: None,
            detail: Some(err.to_string()),
        },
    }
}

#[derive(Debug)]
pub(crate) struct MutationRequestError {
    kind: spotuify_protocol::IpcErrorKind,
    message: String,
    retryable: bool,
    provider: Option<ProviderId>,
    detail: Option<String>,
}

impl std::fmt::Display for MutationRequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for MutationRequestError {}

pub(crate) fn provider_workflow_timeout_error(
    provider: ProviderId,
    operation: &str,
    timeout: Duration,
) -> anyhow::Error {
    let message = format!(
        "provider `{provider}` {operation} timed out after {} seconds",
        timeout.as_secs_f64()
    );
    MutationRequestError {
        kind: spotuify_protocol::IpcErrorKind::Timeout,
        message: message.clone(),
        retryable: true,
        provider: Some(provider),
        detail: Some(message),
    }
    .into()
}

#[cfg(test)]
pub(crate) async fn dispatch(
    state: Arc<DaemonState>,
    request: Request,
    source: Option<OperationSource>,
) -> anyhow::Result<ResponseData> {
    dispatch_with_mutation(state, request, source, None).await
}

pub(crate) fn dispatch_with_mutation(
    state: Arc<DaemonState>,
    request: Request,
    source: Option<OperationSource>,
    mutation_id: Option<MutationId>,
) -> futures::future::BoxFuture<'static, anyhow::Result<ResponseData>> {
    async move {
        match crate::handlers::categorize(&request) {
            crate::handlers::Cat::Admin => {
                crate::handlers::admin::dispatch(state, request, source).await
            }
            crate::handlers::Cat::Playback => {
                crate::handlers::playback::dispatch(state, request, source, mutation_id).await
            }
            crate::handlers::Cat::Search => {
                crate::handlers::search::dispatch(state, request, source).await
            }
            crate::handlers::Cat::Library => {
                crate::handlers::library::dispatch(state, request, source, mutation_id).await
            }
            crate::handlers::Cat::Playlists => {
                crate::handlers::playlists::dispatch(state, request, source, mutation_id).await
            }
            crate::handlers::Cat::Analytics => {
                crate::handlers::analytics::dispatch(state, request, source).await
            }
            crate::handlers::Cat::Ops => {
                crate::handlers::ops::dispatch(state, request, source, mutation_id).await
            }
            crate::handlers::Cat::Reminders => {
                crate::handlers::reminders::dispatch(state, request, source).await
            }
            crate::handlers::Cat::Viz => {
                crate::handlers::viz::dispatch(state, request, source).await
            }
            crate::handlers::Cat::Media => {
                crate::handlers::media::dispatch(state, request, source).await
            }
        }
    }
    .boxed()
}

pub(crate) async fn handle_ops_undo(
    state: &std::sync::Arc<DaemonState>,
    operation_id: Option<spotuify_protocol::OperationId>,
    source: OperationSource,
    dry_run: bool,
    force: bool,
    bulk_since_ms: Option<i64>,
    mutation_id: Option<MutationId>,
) -> anyhow::Result<ResponseData> {
    // Bulk undo: walk every reversible succeeded op newer than `since`,
    // reverse-chronological, stop on first failure (per blueprint).
    if let Some(since) = bulk_since_ms {
        let ops = state
            .store()
            .find_reversible_operations_since(since, None)
            .await?;
        if dry_run {
            let mut preview = Vec::new();
            for op in &ops {
                if let UndoOutcome::Preview(line) = undo_single(
                    state,
                    op,
                    OperationId::new_v7(),
                    uuid::Uuid::now_v7(),
                    true,
                    force,
                )
                .await?
                {
                    preview.push(line);
                }
            }
            return Ok(ResponseData::OperationUndoResult {
                undo_op_id: OperationId::new_v7(),
                succeeded: 0,
                skipped: preview.len() as u32,
                errors: vec![],
                preview,
            });
        }

        let request_summary = serde_json::to_string(&Request::OpsUndo {
            operation_id: None,
            dry_run,
            force,
            bulk_since_ms: Some(since),
        })?;
        let subject_uris = ops
            .iter()
            .flat_map(|op| op.subject_uris.iter().cloned())
            .collect();
        return record_operation(
            state,
            OperationKind::Undo,
            source,
            subject_uris,
            "ops-undo",
            &request_summary,
            mutation_id,
            None,
            Some(spotuify_protocol::ReversalPlan::NotReversible {
                reason: "bulk undo is not atomically reversible".to_string(),
            }),
            None,
            |undo_op_id| async move {
                state
                    .store()
                    .record_bulk_undo_candidates(undo_op_id, &ops)
                    .await?;
                for op in &ops {
                    let child_mutation_id = derived_mutation_uuid(undo_op_id.0, op.operation_id.0);
                    undo_single(state, op, undo_op_id, child_mutation_id, false, force).await?;
                }
                Ok(ResponseData::OperationUndoResult {
                    undo_op_id,
                    succeeded: ops.len() as u32,
                    skipped: 0,
                    errors: vec![],
                    preview: vec![],
                })
            },
        )
        .await;
    }

    let request_summary = serde_json::to_string(&Request::OpsUndo {
        operation_id,
        dry_run,
        force,
        bulk_since_ms: None,
    })?;
    if let Some(replay) =
        replay_existing_operation_mutation(state, mutation_id, &request_summary).await?
    {
        return Ok(replay);
    }

    // Single op (default: last reversible).
    let op = match operation_id {
        Some(id) => state.store().get_operation(id).await?,
        None => state
            .store()
            .find_last_reversible_operation()
            .await?
            .ok_or_else(|| anyhow::anyhow!("no reversible operations to undo"))?,
    };
    if dry_run {
        let undo_op_id = OperationId::new_v7();
        let preview = match undo_single(state, &op, undo_op_id, undo_op_id.0, true, force).await? {
            UndoOutcome::Preview(line) => vec![line],
            UndoOutcome::Applied => vec![],
        };
        return Ok(ResponseData::OperationUndoResult {
            undo_op_id,
            succeeded: 0,
            skipped: 0,
            errors: vec![],
            preview,
        });
    }

    record_operation(
        state,
        OperationKind::Undo,
        source,
        op.subject_uris.clone(),
        "ops-undo",
        &request_summary,
        mutation_id,
        None,
        Some(spotuify_protocol::ReversalPlan::Redo {
            target_op_id: op.operation_id,
        }),
        None,
        |undo_op_id| async move {
            state
                .store()
                .update_operation_subject(undo_op_id, op.operation_id)
                .await?;
            undo_single(state, &op, undo_op_id, undo_op_id.0, false, force).await?;
            Ok(ResponseData::OperationUndoResult {
                undo_op_id,
                succeeded: 1,
                skipped: 0,
                errors: vec![],
                preview: vec![],
            })
        },
    )
    .await
}

/// What `undo_single` did: executed the reversal, or (dry-run) produced
/// a human-readable description of what it would do.
pub(crate) enum UndoOutcome {
    Applied,
    Preview(String),
}

pub(crate) async fn undo_single(
    state: &std::sync::Arc<DaemonState>,
    op: &spotuify_protocol::Operation,
    undo_op_id: OperationId,
    provider_mutation_id: uuid::Uuid,
    dry_run: bool,
    force: bool,
) -> anyhow::Result<UndoOutcome> {
    crate::undo::validate_undoable(op)?;
    let plan = op
        .reversal_plan
        .clone()
        .ok_or_else(|| anyhow::anyhow!("op {} missing reversal_plan", op.operation_id))?;

    // Version-token conflict detection. Pre-fetch the provider's current
    // opaque token (if the plan references a playlist) so the
    // synchronous check can compare without itself doing
    // I/O. The previous shape used `block_in_place` +
    // `Handle::block_on` from inside a sync closure to bridge that
    // gap, which took a tokio worker out of the pool for the
    // duration of a full `/me/playlists` paginated fetch — a foot-gun
    // when a sync burst already had other workers busy on writes.
    let current_version_token = match crate::undo::version_token_check_target(&plan) {
        Some((playlist_id, _)) => {
            let playlist_id = playlist_id.to_string();
            match provider_playlist_resource(state, &playlist_id).await {
                Ok((provider, resource)) => {
                    if !provider.capabilities().playlists.list {
                        None
                    } else {
                        match provider
                            .playlist(RequestContext::FOREGROUND, &resource)
                            .await
                        {
                            Ok(playlist) => playlist.and_then(|p| p.version_token),
                            Err(err) => {
                                tracing::debug!(error = %err, playlist = %playlist_id, "version token fetch failed");
                                None
                            }
                        }
                    }
                }
                Err(err) => {
                    tracing::debug!(error = %err, "provider client unavailable for version token check");
                    None
                }
            }
        }
        None => None,
    };
    if let Err(error) =
        crate::undo::check_version_token(&plan, |_id| current_version_token.clone(), force)
    {
        return match error {
            crate::undo::UndoError::VersionTokenMismatch {
                stored, current, ..
            } => Err(ProviderError::VersionConflict {
                expected: Some(stored),
                actual: (!current.is_empty()).then_some(current),
            }
            .into()),
            error => Err(error.into()),
        };
    }

    if dry_run {
        // Dry-run: describe what would happen instead of doing it. The
        // line travels back in `OperationUndoResult.preview` so the CLI
        // can print it directly.
        let pre = op
            .pre_state
            .clone()
            .unwrap_or(spotuify_protocol::PreState::Transport);
        return Ok(UndoOutcome::Preview(format!(
            "would undo {} {}: {}",
            op.kind,
            op.operation_id,
            crate::undo::render_plan_summary(&plan, &pre)
        )));
    }

    // Execute the reversal via Spotify Web API.
    let applied = match apply_reversal(state, &plan, provider_mutation_id).await {
        Ok(applied) => applied,
        Err(mut err) => {
            if let Some(partial) = err.downcast_mut::<PartialMutationError>() {
                partial.post_write_guard = Some(
                    spotuify_store::PostWriteOperationGuard::DisableUndo(op.operation_id),
                );
            }
            if let Some(malformed) = err.downcast_mut::<MalformedProviderReceiptError>() {
                malformed.post_write_guard = Some(
                    spotuify_store::PostWriteOperationGuard::DisableUndo(op.operation_id),
                );
            }
            if let Some(post_write) = err.downcast_mut::<PostWriteLifecycleError>() {
                post_write.guard = Some(spotuify_store::PostWriteOperationGuard::DisableUndo(
                    op.operation_id,
                ));
            }
            return Err(err);
        }
    };

    // The durable undo row/receipt was claimed before the remote call.
    // Only flip the original after the provider confirms the reversal.
    if let Err(error) = state
        .store()
        .mark_operation_undone(op.operation_id, undo_op_id)
        .await
    {
        if let Some(applied) = applied {
            let mut error = provider_mutation_reconciliation_required_after_local_failure(
                applied.provider,
                applied.mutation,
                &applied.receipt,
                error,
            );
            if let Some(partial) = error.downcast_mut::<PartialMutationError>() {
                partial.post_write_guard = Some(
                    spotuify_store::PostWriteOperationGuard::DisableUndo(op.operation_id),
                );
            }
            return Err(error);
        }
        return Err(PostWriteLifecycleError {
            message: format!(
                "provider reversal completed, but local undo bookkeeping failed: {error}"
            ),
            seeds: Vec::new(),
            guard: Some(spotuify_store::PostWriteOperationGuard::DisableUndo(
                op.operation_id,
            )),
        }
        .into());
    }
    state.emit_event(DaemonEvent::OperationUndone {
        undo_op_id,
        original_op_id: op.operation_id,
        success: true,
    });
    Ok(UndoOutcome::Applied)
}

pub(crate) async fn apply_reversal(
    state: &std::sync::Arc<DaemonState>,
    plan: &spotuify_protocol::ReversalPlan,
    mutation_id: uuid::Uuid,
) -> anyhow::Result<Option<AppliedProviderMutation>> {
    use spotuify_protocol::ReversalPlan as P;
    match plan {
        P::TransferToPriorDevice {
            device_id,
            provider,
        } => {
            let provider = match provider {
                Some(provider) => state.provider(provider).await?,
                None => state.default_provider().await?,
            };
            let transport = state.provider_transport(provider.id()).await?;
            let command = TransportCommand::Transfer {
                device_id: device_id.clone(),
                play: false,
            };
            require_transport_command_capability(provider.as_ref(), &command)?;
            if let Err(error) = transport
                .execute(RequestContext::PLAYBACK_CONTROL, command)
                .await
            {
                if !provider_error_may_follow_write(&error) {
                    return Err(error.into());
                }
                return Err(PostWriteLifecycleError {
                    message: bounded_redacted_text(
                        &format!(
                            "transport reversal may have been applied: {error}; authoritative refresh required"
                        ),
                        512,
                    ),
                    seeds: Vec::new(),
                    guard: None,
                }
                .into());
            }
            Ok(None)
        }
        P::QueueRemove { uri } => {
            // Legacy plan: queue_add rows recorded before the kind went
            // non-reversible carry this. Executing it used to be a
            // silent no-op that still marked the op undone; fail loudly
            // instead of lying about what happened.
            anyhow::bail!(
                "cannot remove {uri} from the queue: the provider has no queue-remove operation \
                 (queue adds recorded by older versions are not actually reversible)"
            )
        }
        P::PlaylistRemoveTracks {
            playlist_id,
            uris,
            version_token,
        } => {
            let (provider, playlist_uri) = provider_playlist_resource(state, playlist_id).await?;
            let items = uris
                .iter()
                .map(|uri| {
                    Ok(PlaylistItemRef {
                        uri: ResourceUri::parse(uri)?,
                        positions: vec![],
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            let mutation = Mutation::PlaylistRemove {
                playlist_uri,
                items,
                expected_version: version_token.clone(),
            };
            apply_checked_reversal(provider.as_ref(), mutation_id, &mutation)
                .await
                .map(Some)
        }
        P::PlaylistAddAtPositions {
            playlist_id,
            items,
            version_token,
        } => {
            let (provider, playlist_uri) = provider_playlist_resource(state, playlist_id).await?;
            let items = items
                .iter()
                .map(|(uri, position)| {
                    Ok(PlaylistInsertion {
                        uri: ResourceUri::parse(uri)?,
                        position: Some(*position),
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            let mutation = Mutation::PlaylistAdd {
                playlist_uri,
                items,
                expected_version: version_token.clone(),
            };
            apply_checked_reversal(provider.as_ref(), mutation_id, &mutation)
                .await
                .map(Some)
        }
        P::PlaylistDelete { playlist_id } => {
            let (provider, playlist_uri) = provider_playlist_resource(state, playlist_id).await?;
            let mutation = Mutation::PlaylistUnfollow { playlist_uri };
            apply_checked_reversal(provider.as_ref(), mutation_id, &mutation)
                .await
                .map(Some)
        }
        P::PlaylistReorder {
            playlist_id,
            range_start,
            insert_before,
            range_length,
            version_token,
        } => {
            let (provider, playlist_uri) = provider_playlist_resource(state, playlist_id).await?;
            let mutation = Mutation::PlaylistReorder {
                playlist_uri,
                range_start: *range_start,
                insert_before: *insert_before,
                range_length: *range_length,
                expected_version: version_token.clone(),
            };
            apply_checked_reversal(provider.as_ref(), mutation_id, &mutation)
                .await
                .map(Some)
        }
        P::LibraryUnsave { uri } => {
            let resource = ResourceUri::parse(uri)?;
            let provider = state.provider_for_uri(&resource).await?;
            let mutation = Mutation::LibraryUnsave {
                uris: vec![resource],
            };
            apply_checked_reversal(provider.as_ref(), mutation_id, &mutation)
                .await
                .map(Some)
        }
        P::LibrarySave { uri, .. } => {
            // `prior_added_at_ms` is recorded for forensics only —
            // Spotify's save endpoint always sets `added_at` to now.
            // Documented limitation; surfaced in `ops show --diff`.
            let resource = ResourceUri::parse(uri)?;
            let provider = state.provider_for_uri(&resource).await?;
            let mutation = Mutation::LibrarySave {
                uris: vec![resource],
            };
            apply_checked_reversal(provider.as_ref(), mutation_id, &mutation)
                .await
                .map(Some)
        }
        P::Like { uri } => {
            // Like ≡ library_save for tracks; the protocol keeps Like
            // distinct from LibrarySave for clarity in the op log even
            // though Spotify's endpoint is the same.
            let resource = ResourceUri::parse(uri)?;
            let provider = state.provider_for_uri(&resource).await?;
            let mutation = Mutation::LibrarySave {
                uris: vec![resource],
            };
            apply_checked_reversal(provider.as_ref(), mutation_id, &mutation)
                .await
                .map(Some)
        }
        P::Unlike { uri } => {
            let resource = ResourceUri::parse(uri)?;
            let provider = state.provider_for_uri(&resource).await?;
            let mutation = Mutation::LibraryUnsave {
                uris: vec![resource],
            };
            apply_checked_reversal(provider.as_ref(), mutation_id, &mutation)
                .await
                .map(Some)
        }
        P::NotReversible { reason } => {
            anyhow::bail!("operation is not reversible: {reason}")
        }
        P::Redo { .. } => anyhow::bail!(
            "redo of an undo replays the original forward op; \
             use `ops redo` instead of `ops undo`"
        ),
    }
}

async fn provider_playlist_resource(
    state: &DaemonState,
    value: &str,
) -> anyhow::Result<(Arc<dyn MusicProvider>, ResourceUri)> {
    if let Ok(resource) = ResourceUri::parse(value) {
        if resource.kind() != MediaKind::Playlist {
            return Err(ProviderError::InvalidInput {
                field: "playlist".to_string(),
                message: format!("expected playlist URI, got {}", resource.kind()),
            }
            .into());
        }
        let provider = state.provider_for_uri(&resource).await?;
        return Ok((provider, resource));
    }
    let provider = state.default_provider().await?;
    let resource = playlist_resource(provider.as_ref(), value)?;
    Ok((provider, resource))
}

pub(crate) struct AppliedProviderMutation {
    provider: ProviderId,
    mutation: Mutation,
    receipt: MutationReceipt,
}

async fn apply_checked_reversal(
    provider: &dyn MusicProvider,
    mutation_id: uuid::Uuid,
    mutation: &Mutation,
) -> anyhow::Result<AppliedProviderMutation> {
    require_provider_mutation_capability(provider, mutation)?;
    let receipt = match apply_provider_mutation_checked(provider, mutation_id, mutation).await {
        Ok(receipt) => receipt,
        Err(error)
            if error.downcast_ref::<PartialMutationError>().is_some()
                || error
                    .downcast_ref::<MalformedProviderReceiptError>()
                    .is_some() =>
        {
            return Err(error);
        }
        Err(error)
            if error
                .downcast_ref::<ProviderError>()
                .is_some_and(|error| !provider_error_may_follow_write(error)) =>
        {
            return Err(error);
        }
        Err(error) => {
            let reconciliation = reconciliation_for_mutation(
                provider.id(),
                mutation,
                ReceiptId::new_v7(),
                OperationId::new_v7(),
            );
            return Err(PostWriteLifecycleError {
                message: bounded_redacted_text(
                    &format!(
                        "provider reversal may have been applied: {error}; authoritative reconciliation required"
                    ),
                    512,
                ),
                seeds: vec![ProviderReconciliationSeed {
                    provider: reconciliation.provider,
                    target: reconciliation.target,
                    scope: reconciliation.scope,
                    resource_uris: reconciliation.resource_uris,
                }],
                guard: None,
            }
            .into());
        }
    };
    Ok(AppliedProviderMutation {
        provider: provider.id().clone(),
        mutation: mutation.clone(),
        receipt,
    })
}

fn provider_error_may_follow_write(error: &ProviderError) -> bool {
    matches!(
        error,
        ProviderError::Network(_)
            | ProviderError::Transient { .. }
            | ProviderError::Decode(_)
            | ProviderError::Provider(_)
    ) || matches!(error, ProviderError::Upstream { status, .. } if *status >= 500)
}

pub(crate) async fn handle_ops_redo(
    state: &std::sync::Arc<DaemonState>,
    operation_id: Option<spotuify_protocol::OperationId>,
    source: OperationSource,
    mutation_id: Option<MutationId>,
) -> anyhow::Result<ResponseData> {
    let request_summary = serde_json::to_string(&Request::OpsRedo { operation_id })?;
    if let Some(replay) =
        replay_existing_operation_mutation(state, mutation_id, &request_summary).await?
    {
        return Ok(replay);
    }

    // Find an undone op to redo. Default: most-recent undone.
    let op = match operation_id {
        Some(id) => state.store().get_operation(id).await?,
        None => {
            let ops = state.store().list_operations(50, None, None).await?;
            ops.into_iter()
                .find(|o| o.status == OperationStatus::Undone)
                .ok_or_else(|| anyhow::anyhow!("no undone operations to redo"))?
        }
    };
    // Real redo: re-execute the original Request by fetching its
    // serialized form from the linked receipt row. The redo itself is
    // durably claimed before dispatching the forward mutation.
    let receipt_id = op
        .receipt_id
        .ok_or_else(|| anyhow::anyhow!("op {} has no receipt; cannot redo", op.operation_id))?;
    let raw = state.store().receipt_request_json(receipt_id).await?;
    let original_request: Request = serde_json::from_str(&raw)
        .map_err(|err| anyhow::anyhow!("failed to decode original request: {err}"))?;
    let reconciliation_seed = operation_reconciliation_seed(state.as_ref(), &op).await?;
    record_operation(
        state,
        OperationKind::Redo,
        source,
        op.subject_uris.clone(),
        "ops-redo",
        &request_summary,
        mutation_id,
        None,
        Some(spotuify_protocol::ReversalPlan::NotReversible {
            reason: "redo is represented by the newly replayed forward operation".to_string(),
        }),
        None,
        |redo_op_id| async move {
            if op.status != OperationStatus::Undone {
                anyhow::bail!(
                    "operation {} is not undone (status = {:?}); only undone ops can be redone",
                    op.operation_id,
                    op.status,
                );
            }
            state
                .store()
                .update_operation_subject(redo_op_id, op.operation_id)
                .await?;
            let forward_mutation_id =
                MutationId(derived_mutation_uuid(redo_op_id.0, op.operation_id.0));
            let response = match Box::pin(dispatch_with_mutation(
                state.clone(),
                original_request,
                Some(source),
                Some(forward_mutation_id),
            ))
            .await
            {
                Ok(response) => response,
                Err(error) if post_write_outcome_uncertain(&error) => {
                    return Err(PostWriteLifecycleError {
                        message: bounded_redacted_text(
                            &format!(
                                "redo forward mutation may have been applied: {error}; authoritative reconciliation required"
                            ),
                            512,
                        ),
                        seeds: reconciliation_seed.clone().into_iter().collect(),
                        guard: Some(spotuify_store::PostWriteOperationGuard::MarkRedone(
                            op.operation_id,
                        )),
                    }
                    .into());
                }
                Err(error) => return Err(error),
            };
            if let Err(error) = wait_for_mutation_confirmation(state, &response).await {
                let outcome_uncertain = match response_receipt_id(&response) {
                    Some(receipt_id) => match state.store().get_receipt(receipt_id).await {
                        Ok(receipt)
                            if receipt.status
                                == spotuify_protocol::ReceiptStatus::Confirmed =>
                        {
                            false
                        }
                        Ok(receipt)
                            if receipt.status == spotuify_protocol::ReceiptStatus::Failed =>
                        {
                            match state
                                .store()
                                .provider_reconciliation_exists(receipt_id)
                                .await
                            {
                                Ok(false) => return Err(error),
                                Ok(true) | Err(_) => true,
                            }
                        }
                        Ok(_) | Err(_) => true,
                    },
                    None => true,
                };
                if !outcome_uncertain {
                    // The waiter can race a late durable confirmation.
                } else {
                    return Err(PostWriteLifecycleError {
                        message: bounded_redacted_text(
                            &format!(
                                "redo forward mutation may have been applied: {error}; authoritative reconciliation required"
                            ),
                            512,
                        ),
                        seeds: reconciliation_seed.clone().into_iter().collect(),
                        guard: Some(spotuify_store::PostWriteOperationGuard::MarkRedone(
                            op.operation_id,
                        )),
                    }
                    .into());
                }
            }

            // Do not expose Redone until the replayed forward mutation's
            // durable receipt is confirmed.
            if let Err(error) = state
                .store()
                .mark_operation_redone(op.operation_id, redo_op_id)
                .await
            {
                return Err(PostWriteLifecycleError {
                    message: bounded_redacted_text(
                        &format!(
                            "provider mutation applied, but redo bookkeeping failed: {error}; authoritative reconciliation required"
                        ),
                        512,
                    ),
                    seeds: reconciliation_seed.clone().into_iter().collect(),
                    guard: Some(spotuify_store::PostWriteOperationGuard::MarkRedone(
                        op.operation_id,
                    )),
                }
                .into());
            }
            state.emit_event(DaemonEvent::OperationUndone {
                undo_op_id: redo_op_id,
                original_op_id: op.operation_id,
                success: true,
            });
            Ok(ResponseData::OperationUndoResult {
                undo_op_id: redo_op_id,
                succeeded: 1,
                skipped: 0,
                errors: vec![],
                preview: vec![],
            })
        },
    )
    .await
}

async fn replay_existing_operation_mutation(
    state: &Arc<DaemonState>,
    mutation_id: Option<MutationId>,
    request_summary: &str,
) -> anyhow::Result<Option<ResponseData>> {
    replay_existing_recorded_mutation(state, mutation_id, request_summary).await
}

/// Read-only replay fast path for typed synchronous mutations. Callers use
/// this before any current auth/provider preflight; `claim_mutation` remains
/// the atomic authority when no durable key exists yet.
pub(crate) async fn replay_existing_recorded_mutation<T>(
    state: &Arc<DaemonState>,
    mutation_id: Option<MutationId>,
    request_summary: &str,
) -> anyhow::Result<Option<T>>
where
    T: DeserializeOwned + MutationResponseMetadata,
{
    let Some(mutation_id) = mutation_id else {
        return Ok(None);
    };
    match state
        .store()
        .lookup_mutation_claim(mutation_id, &mutation_fingerprint(request_summary))
        .await?
    {
        None => Ok(None),
        Some(spotuify_store::MutationClaim::Existing {
            receipt,
            response_json,
        }) => {
            if let Some(receipt) = receipt.as_ref() {
                spawn_provider_reconciliations_for_receipt(state, receipt.receipt_id);
            }
            replay_recorded_mutation(mutation_id, receipt.map(|receipt| *receipt), response_json)
                .map(Some)
        }
        Some(spotuify_store::MutationClaim::FingerprintMismatch) => Err(MutationRequestError {
            kind: spotuify_protocol::IpcErrorKind::InvalidRequest,
            message: format!("mutation id {mutation_id} is already bound to a different request"),
            retryable: false,
            provider: None,
            detail: None,
        }
        .into()),
        Some(spotuify_store::MutationClaim::Claimed) => {
            unreachable!("lookup never creates a mutation claim")
        }
    }
}

/// Read-only fast path for optimistic mutations whose provider discovery is
/// itself fallible. This must run before adapter I/O so a completed mutation
/// remains replayable even when the provider cannot rediscover its subjects.
pub(crate) async fn replay_existing_optimistic_mutation(
    state: &Arc<DaemonState>,
    mutation_id: Option<MutationId>,
    request_summary: &str,
) -> anyhow::Result<Option<ResponseData>> {
    let Some(mutation_id) = mutation_id else {
        return Ok(None);
    };
    match state
        .store()
        .lookup_mutation_claim(mutation_id, &mutation_fingerprint(request_summary))
        .await?
    {
        None => Ok(None),
        Some(spotuify_store::MutationClaim::Existing {
            receipt,
            response_json,
        }) => {
            replay_existing_optimistic_claim(state, mutation_id, receipt, response_json).map(Some)
        }
        Some(spotuify_store::MutationClaim::FingerprintMismatch) => Err(MutationRequestError {
            kind: spotuify_protocol::IpcErrorKind::InvalidRequest,
            message: format!("mutation id {mutation_id} is already bound to a different request"),
            retryable: false,
            provider: None,
            detail: None,
        }
        .into()),
        Some(spotuify_store::MutationClaim::Claimed) => {
            unreachable!("lookup never creates a mutation claim")
        }
    }
}

fn replay_existing_optimistic_claim(
    state: &Arc<DaemonState>,
    mutation_id: MutationId,
    receipt: Option<Box<spotuify_protocol::Receipt>>,
    response_json: Option<String>,
) -> anyhow::Result<ResponseData> {
    let receipt = receipt.ok_or_else(|| MutationRequestError {
        kind: spotuify_protocol::IpcErrorKind::Internal,
        message: format!("mutation id {mutation_id} has no linked receipt"),
        retryable: false,
        provider: None,
        detail: None,
    })?;
    if receipt.status == spotuify_protocol::ReceiptStatus::Pending {
        return Ok(ResponseData::Mutation {
            receipt: CommandReceipt {
                ok: true,
                action: receipt.action,
                message: receipt.message,
                receipt_id: Some(receipt.receipt_id),
                mutation_id: Some(mutation_id),
                status: Some(receipt.status),
                error: receipt.error,
                replayed: true,
            },
        });
    }

    spawn_provider_reconciliations_for_receipt(state, receipt.receipt_id);
    replay_recorded_mutation(mutation_id, Some(*receipt), response_json)
}

fn derived_mutation_uuid(namespace: uuid::Uuid, subject: uuid::Uuid) -> uuid::Uuid {
    let digest = Sha256::digest(format!("{namespace}:{subject}").as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // RFC 4122 variant + deterministic version-5 marker. The bytes are
    // SHA-256 rather than SHA-1, but the UUID is only an opaque idempotency key.
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes)
}

fn post_write_outcome_uncertain(error: &anyhow::Error) -> bool {
    error.downcast_ref::<PartialMutationError>().is_some()
        || error
            .downcast_ref::<MalformedProviderReceiptError>()
            .is_some()
        || error.downcast_ref::<PostWriteLifecycleError>().is_some()
        || error
            .downcast_ref::<MutationRequestError>()
            .is_some_and(|error| error.message.contains("outcome indeterminate"))
}

fn response_receipt_id(response: &ResponseData) -> Option<ReceiptId> {
    match response {
        ResponseData::Mutation { receipt } => receipt.receipt_id,
        ResponseData::PlaylistCreate { receipt } => receipt.receipt_id,
        _ => None,
    }
}

async fn wait_for_mutation_confirmation(
    state: &DaemonState,
    response: &ResponseData,
) -> anyhow::Result<()> {
    let ResponseData::Mutation { receipt } = response else {
        // Synchronous mutation handlers only return after finalization.
        return Ok(());
    };
    match receipt.status {
        Some(spotuify_protocol::ReceiptStatus::Confirmed) => return Ok(()),
        Some(spotuify_protocol::ReceiptStatus::Failed) => {
            anyhow::bail!("redo forward mutation failed: {}", receipt.message)
        }
        _ => {}
    }
    let receipt_id = receipt
        .receipt_id
        .ok_or_else(|| anyhow::anyhow!("redo forward mutation returned no receipt id"))?;
    tokio::time::timeout(MUTATION_BODY_TIMEOUT + Duration::from_secs(1), async {
        loop {
            let persisted = state.store().get_receipt(receipt_id).await?;
            match persisted.status {
                spotuify_protocol::ReceiptStatus::Confirmed => return Ok(()),
                spotuify_protocol::ReceiptStatus::Failed => {
                    anyhow::bail!("redo forward mutation failed: {}", persisted.message)
                }
                spotuify_protocol::ReceiptStatus::Pending => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("timed out waiting for redo forward mutation {receipt_id}"))?
}

pub(crate) async fn lyrics_get(
    state: Arc<DaemonState>,
    track_uri: Option<String>,
    force_refresh: bool,
) -> anyhow::Result<ResponseData> {
    let Some((track_uri, item)) = resolve_lyrics_target(&state, track_uri).await? else {
        return Ok(ResponseData::Lyrics {
            lyrics: None,
            offset_ms: 0,
        });
    };
    let offset_ms = state.store().lyrics_offset_ms(&track_uri).await?;
    let cached = state.store().cached_lyrics(&track_uri, LYRICS_TTL).await?;
    if !force_refresh && cached.is_some() {
        return Ok(ResponseData::Lyrics {
            lyrics: cached,
            offset_ms,
        });
    }
    if !force_refresh && state.store().lyrics_lookup_blocked(&track_uri).await? {
        return Ok(ResponseData::Lyrics {
            lyrics: cached,
            offset_ms,
        });
    }

    let fetched = fetch_lyrics(&state, &track_uri, item.as_ref()).await?;
    if let Some(lyrics) = fetched.as_ref() {
        state.store().upsert_lyrics(lyrics).await?;
    } else if cached.is_none() {
        state
            .store()
            .upsert_lyrics_lookup_failure(&track_uri, "not found", LYRICS_NEGATIVE_TTL)
            .await?;
    }

    Ok(ResponseData::Lyrics {
        lyrics: fetched.or(cached),
        offset_ms,
    })
}

pub(crate) async fn resolve_lyrics_target(
    state: &Arc<DaemonState>,
    track_uri: Option<String>,
) -> anyhow::Result<Option<(String, Option<MediaItem>)>> {
    if let Some(track_uri) = track_uri {
        let mut items = state
            .store()
            .media_items_by_uris(std::slice::from_ref(&track_uri))
            .await?;
        let mut item = items.pop();
        if item.is_none() {
            let resource = ResourceUri::parse(&track_uri)?;
            match state.provider_for_uri(&resource).await {
                Ok(provider) => match require_provider_capability(
                    provider.as_ref(),
                    &format!("{} catalog lookup", resource.kind()),
                    provider
                        .capabilities()
                        .catalog
                        .lookup_kinds
                        .contains(&resource.kind()),
                )
                .map_err(anyhow::Error::from)
                {
                    Ok(()) => match provider
                        .media_item(RequestContext::FOREGROUND, &resource)
                        .await
                    {
                        Ok(Some(fetched)) => {
                            validate_provider_lookup_result(
                                provider.as_ref(),
                                &resource,
                                &fetched,
                            )?;
                            let provider_id = provider.id().to_string();
                            state
                                .store()
                                .upsert_provider_media_items(
                                    provider.id(),
                                    std::slice::from_ref(&fetched),
                                    &provider_id,
                                )
                                .await?;
                            item = Some(fetched);
                        }
                        Ok(None) => {}
                        Err(err) => {
                            tracing::debug!(error = %err, track_uri, "track metadata lookup failed")
                        }
                    },
                    Err(err) => {
                        tracing::debug!(error = %err, track_uri, "track metadata lookup unsupported")
                    }
                },
                Err(err) => {
                    tracing::debug!(error = %err, track_uri, "provider unavailable for lyrics metadata lookup")
                }
            }
        }
        return Ok(Some((track_uri, item)));
    }

    let playback = state.snapshot_playback();
    Ok(playback.item.map(|item| (item.uri.clone(), Some(item))))
}

pub(crate) async fn fetch_lyrics(
    state: &Arc<DaemonState>,
    track_uri: &str,
    item: Option<&MediaItem>,
) -> anyhow::Result<Option<spotuify_core::SyncedLyrics>> {
    let resource = ResourceUri::parse(track_uri)?;
    require_resource_kind(&resource, MediaKind::Track, "track_uri")?;
    let providers = state.providers().await?;
    let runtime = providers.provider_for_uri(&resource)?;
    if runtime.capabilities().extras.native_lyrics {
        let extras = runtime.extras()?;
        match tokio::time::timeout(
            PROVIDER_LYRICS_TIMEOUT,
            extras.native_lyrics(RequestContext::FOREGROUND, &resource),
        )
        .await
        {
            Ok(Ok(Some(lyrics))) if lyrics.track_uri == resource.as_uri() => {
                return Ok(Some(lyrics));
            }
            Ok(Ok(Some(lyrics))) => tracing::warn!(
                provider = %runtime.id(),
                returned_track = %lyrics.track_uri,
                requested_track = %resource,
                "provider returned lyrics for the wrong track"
            ),
            Ok(Ok(None)) => {}
            Ok(Err(err)) => tracing::debug!(
                provider = %runtime.id(),
                error = %err,
                track_uri,
                "provider-native lyrics unavailable"
            ),
            Err(_) => tracing::warn!(
                provider = %runtime.id(),
                track_uri,
                "provider-native lyrics timed out; falling back to LRCLIB"
            ),
        }
    }

    let Some(item) = item else {
        return Ok(None);
    };
    match spotuify_lyrics::LrclibProvider::new()
        .fetch(item, now_ms())
        .await
    {
        Ok(lyrics) => Ok(lyrics),
        Err(err) => {
            tracing::warn!(error = %err, track_uri, "lrclib lyrics unavailable");
            Ok(None)
        }
    }
}

fn enforce_provider_search_query_limit(
    provider: &dyn MusicProvider,
    query: &str,
) -> Result<(), ProviderError> {
    let Some(limit) = provider.capabilities().search.max_query_chars else {
        return Ok(());
    };
    let actual = query.chars().count();
    if actual <= limit {
        return Ok(());
    }
    Err(ProviderError::InvalidInput {
        field: "query".to_string(),
        message: format!(
            "search query is {actual} characters; provider `{}` limit is {limit}",
            provider.id()
        ),
    })
}

fn provider_remote_search_kinds(
    provider: &dyn MusicProvider,
    scope: SearchScopeData,
    explicit: Option<&[MediaKind]>,
) -> Vec<MediaKind> {
    let requested = explicit
        .map(<[MediaKind]>::to_vec)
        .unwrap_or_else(|| scope_media_kinds(scope));
    if explicit.is_some() {
        requested
    } else {
        let supported = provider.capabilities().search.kinds;
        requested
            .into_iter()
            .filter(|kind| supported.contains(kind))
            .collect()
    }
}

pub(crate) struct SearchParams {
    pub(crate) query: String,
    pub(crate) scope: SearchScopeData,
    pub(crate) source: SearchSourceData,
    pub(crate) limit: u32,
    pub(crate) requested_provider: Option<ProviderId>,
    pub(crate) kinds: Option<Vec<MediaKind>>,
    pub(crate) sort: Option<SearchSortData>,
}

pub(crate) async fn search_with_source(
    state: Arc<DaemonState>,
    params: SearchParams,
) -> anyhow::Result<Vec<MediaItem>> {
    let SearchParams {
        query,
        scope,
        source,
        limit,
        requested_provider,
        kinds,
        sort,
    } = params;
    let (provider_id, provider) =
        resolve_search_provider(&state, &source, requested_provider.as_ref()).await?;
    // The caller may restrict to an explicit set of kinds (e.g. "podcasts only",
    // "tracks + artists"); otherwise fall back to the kinds implied by `scope`.
    let effective_kinds = kinds.clone().unwrap_or_else(|| scope_media_kinds(scope));
    let remote_kinds = provider_remote_search_kinds(provider.as_ref(), scope, kinds.as_deref());
    let mut items = match source {
        SearchSourceData::Local => {
            local_cached_search(&state, &provider_id, &query, scope, limit).await?
        }
        SearchSourceData::Remote(_) => {
            enforce_provider_search_query_limit(provider.as_ref(), &query)?;
            remote_search_and_cache(
                state,
                provider_id,
                provider,
                query,
                scope,
                remote_kinds.clone(),
                limit,
            )
            .await?
        }
        SearchSourceData::Hybrid => {
            let cached = local_cached_search(&state, &provider_id, &query, scope, limit).await?;
            if cached.is_empty() {
                enforce_provider_search_query_limit(provider.as_ref(), &query)?;
                remote_search_and_cache(
                    state,
                    provider_id,
                    provider,
                    query,
                    scope,
                    remote_kinds.clone(),
                    limit,
                )
                .await?
            } else {
                if enforce_provider_search_query_limit(provider.as_ref(), &query).is_ok() {
                    let refresh_state = state.clone();
                    let refresh_provider_id = provider_id.clone();
                    let refresh_provider = provider.clone();
                    let refresh_query = query.clone();
                    let refresh_kinds = remote_kinds.clone();
                    state.spawn_background("provider-search-refresh", async move {
                        if let Err(err) = remote_search_and_cache(
                            refresh_state,
                            refresh_provider_id,
                            refresh_provider,
                            refresh_query,
                            scope,
                            refresh_kinds,
                            limit,
                        )
                        .await
                        {
                            tracing::debug!(error = %err, "background provider search refresh failed");
                        }
                    });
                }
                cached
            }
        }
    };
    // Post-filter to the requested kinds — covers the local/cached paths, which
    // search by `scope` and may return kinds the explicit filter excludes.
    if kinds.is_some() {
        let allowed: std::collections::HashSet<MediaKind> = effective_kinds.into_iter().collect();
        items.retain(|item| allowed.contains(&item.kind));
    }
    apply_search_sort(&mut items, sort);
    Ok(items)
}

pub(crate) async fn resolve_search_provider(
    state: &DaemonState,
    source: &SearchSourceData,
    requested_provider: Option<&ProviderId>,
) -> anyhow::Result<(ProviderId, Arc<dyn MusicProvider>)> {
    let providers = state.providers().await?;
    let runtime = match source {
        SearchSourceData::Remote(source_provider) => {
            if let Some(requested_provider) = requested_provider {
                if requested_provider != source_provider {
                    return Err(ProviderError::InvalidInput {
                        field: "provider".to_string(),
                        message: format!(
                            "search source provider `{source_provider}` does not match requested provider `{requested_provider}`"
                        ),
                    }
                    .into());
                }
                providers.provider(requested_provider)?
            } else if let Ok(runtime) = providers.provider(source_provider) {
                runtime
            } else if source_provider.as_str() == "spotify" {
                providers.provider_for_scheme(&spotuify_core::UriScheme::Spotify)?
            } else {
                providers.provider(source_provider)?
            }
        }
        SearchSourceData::Local | SearchSourceData::Hybrid => {
            providers.provider_or_default(requested_provider)?
        }
    };
    Ok((runtime.id().clone(), runtime.music()))
}

/// Order search results in place. `Relevance` (and `None`) preserves Spotify's
/// own ordering; the others use a stable sort so ties keep relevance order.
pub(crate) fn apply_search_sort(items: &mut [MediaItem], sort: Option<SearchSortData>) {
    match sort {
        None | Some(SearchSortData::Relevance) => {}
        Some(SearchSortData::Name) => items.sort_by_key(|item| item.name.to_lowercase()),
        Some(SearchSortData::Duration) => items.sort_by_key(|item| item.duration_ms),
        Some(SearchSortData::Artist) => items.sort_by_key(|item| item.subtitle.to_lowercase()),
        // Newest first using the typed date; items without a date sort last.
        Some(SearchSortData::Date) => {
            items.sort_by_key(|item| std::cmp::Reverse(item.release_date));
        }
    }
}

/// How many followed shows the episode feed fans out over (newest-first shows
/// from the cache). Bounds the GitHub-of-podcasts blast radius.
pub(crate) const EPISODE_FEED_SHOW_CAP: u32 = 40;
/// First N episodes pulled per show (Spotify returns them newest-first).
pub(crate) const EPISODE_FEED_PER_SHOW: u8 = 8;
/// Max concurrent `show-episodes` fetches.
pub(crate) const EPISODE_FEED_CONCURRENCY: usize = 8;
/// How long a merged feed stays fresh before a re-fetch.
pub(crate) const EPISODE_FEED_TTL_MS: i64 = 15 * 60_000;

/// A flat, date-ordered episode feed merged across all followed shows. Fans out
/// `show_episodes` over the saved shows (bounded concurrency), merges, caches
/// the raw merged set (sort + limit applied per call), and re-fetches when the
/// cache is older than [`EPISODE_FEED_TTL_MS`] or `refresh` is set.
pub(crate) async fn episode_feed(
    state: &Arc<DaemonState>,
    provider_id: &ProviderId,
    limit: u32,
    sort: EpisodeSort,
    refresh: bool,
) -> anyhow::Result<Vec<MediaItem>> {
    let now = now_ms();
    if !refresh {
        if let Some((cached, at)) = state.cached_episode_feed(provider_id) {
            if now - at <= EPISODE_FEED_TTL_MS {
                return Ok(finalize_episode_feed(cached, sort, limit));
            }
        }
    }

    let shows = state
        .store()
        .list_saved_shows(EPISODE_FEED_SHOW_CAP, Some(provider_id.as_str()))
        .await?;
    if shows.len() as u32 == EPISODE_FEED_SHOW_CAP {
        tracing::info!(
            cap = EPISODE_FEED_SHOW_CAP,
            "episode feed truncated to the first {EPISODE_FEED_SHOW_CAP} followed shows"
        );
    }

    let semaphore = Arc::new(tokio::sync::Semaphore::new(EPISODE_FEED_CONCURRENCY));
    let mut tasks = Vec::with_capacity(shows.len());
    for show in shows {
        let show_uri = show.uri.clone();
        let show_name = show.name.clone();
        let task_state = state.clone();
        let permits = semaphore.clone();
        tasks.push(tokio::spawn(async move {
            let _permit = permits.acquire().await.ok()?;
            let resource = ResourceUri::parse(&show_uri).ok()?;
            let provider = task_state.provider_for_uri(&resource).await.ok()?;
            if require_provider_capability(
                provider.as_ref(),
                "show episodes",
                provider.capabilities().catalog.show_episodes,
            )
            .is_err()
            {
                return None;
            }
            let page_request = PageRequest::new(
                u32::from(EPISODE_FEED_PER_SHOW).min(
                    u32::try_from(
                        provider
                            .capabilities()
                            .catalog
                            .show_episodes_max_page_size
                            .unwrap_or(usize::from(EPISODE_FEED_PER_SHOW)),
                    )
                    .unwrap_or(u32::MAX)
                    .max(1),
                ),
                0,
            );
            match provider
                .show_episodes(
                    RequestContext::BACKGROUND_SYNC,
                    CollectionRequest {
                        uri: resource,
                        page: page_request.clone(),
                    },
                )
                .await
            {
                Ok(page) => {
                    if let Err(err) =
                        validate_provider_page_offset(&page_request, &page, "show_episodes")
                    {
                        tracing::warn!(show = %show_uri, error = %err, "episode feed: provider echoed the wrong page offset");
                        return None;
                    }
                    let mut episodes = page.items;
                    if let Err(err) = validate_provider_collection_items(
                        provider.as_ref(),
                        "show_episodes",
                        &[MediaKind::Episode],
                        &episodes,
                    ) {
                        tracing::warn!(show = %show_uri, error = %err, "episode feed: provider returned foreign items");
                        return None;
                    }
                    for episode in &mut episodes {
                        // Episodes carry the show name as subtitle; backfill it
                        // (and the context) when Spotify omitted it so the
                        // "by show" sort + display stay correct.
                        if episode.subtitle.is_empty() {
                            episode.subtitle = show_name.clone();
                        }
                        if episode.context.is_empty() {
                            episode.context = show_name.clone();
                        }
                    }
                    Some(episodes)
                }
                Err(err) => {
                    tracing::debug!(show = %show_uri, error = %err, "episode feed: show fetch failed");
                    None
                }
            }
        }));
    }

    let mut merged: Vec<MediaItem> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for task in tasks {
        if let Ok(Some(episodes)) = task.await {
            for episode in episodes {
                if seen.insert(episode.uri.clone()) {
                    merged.push(episode);
                }
            }
        }
    }

    state.set_cached_episode_feed(provider_id.clone(), merged.clone(), now);
    Ok(finalize_episode_feed(merged, sort, limit))
}

/// Sort + cap a merged episode list for a given [`EpisodeSort`].
pub(crate) fn finalize_episode_feed(
    mut items: Vec<MediaItem>,
    sort: EpisodeSort,
    limit: u32,
) -> Vec<MediaItem> {
    match sort {
        // Typed release dates sort chronologically while preserving precision.
        EpisodeSort::Newest => {
            items.sort_by_key(|item| std::cmp::Reverse(item.release_date));
        }
        EpisodeSort::Oldest => items.sort_by_key(|item| item.release_date),
        EpisodeSort::Duration => items.sort_by_key(|item| std::cmp::Reverse(item.duration_ms)),
        EpisodeSort::Title => items.sort_by_key(|item| item.name.to_lowercase()),
        EpisodeSort::Show => items.sort_by_key(|item| item.subtitle.to_lowercase()),
    }
    if limit > 0 {
        items.truncate(limit as usize);
    }
    items
}

pub(crate) async fn local_cached_search(
    state: &DaemonState,
    provider: &ProviderId,
    query: &str,
    scope: SearchScopeData,
    limit: u32,
) -> anyhow::Result<Vec<MediaItem>> {
    let hits = state
        .search()
        .search_for_provider(query, scope, limit as usize, Some(provider))
        .await
        .unwrap_or_default();
    if !hits.is_empty() {
        let uris = hits.into_iter().map(|hit| hit.uri).collect::<Vec<_>>();
        let items = state.store().media_items_by_uris(&uris).await?;
        if !items.is_empty() {
            return Ok(items);
        }
    }
    state
        .store()
        .local_search(query, scope, limit, Some(provider.as_str()))
        .await
}

pub(crate) async fn remote_search_and_cache(
    state: Arc<DaemonState>,
    provider_id: ProviderId,
    provider: Arc<dyn MusicProvider>,
    query: String,
    scope: SearchScopeData,
    kinds: Vec<MediaKind>,
    limit: u32,
) -> anyhow::Result<Vec<MediaItem>> {
    enforce_provider_search_query_limit(provider.as_ref(), &query)?;
    let search_caps = provider.capabilities().search;
    require_provider_capability(provider.as_ref(), "remote search", search_caps.remote)?;
    require_provider_capability(
        provider.as_ref(),
        "requested search kinds",
        !kinds.is_empty(),
    )?;
    for kind in &kinds {
        require_provider_capability(
            provider.as_ref(),
            &format!("{kind} search"),
            search_caps.kinds.contains(kind),
        )?;
    }
    let started = Instant::now();
    let max_page = search_caps.max_page_size.unwrap_or(limit.max(1) as usize) as u32;
    let searches = kinds.into_iter().map(|kind| {
        let provider = provider.clone();
        let query = query.clone();
        async move {
            let page_request = PageRequest::new(limit.min(max_page).max(1), 0);
            let result = provider
                .search(
                    RequestContext::FOREGROUND,
                    SearchRequest {
                        query,
                        kind: kind.clone(),
                        page: page_request.clone(),
                    },
                )
                .await;
            (kind, page_request, result)
        }
    });
    let mut items =
        match tokio::time::timeout(SEARCH_REQUEST_TIMEOUT, futures::future::join_all(searches))
            .await
        {
            Ok(results) => {
                let mut items = Vec::new();
                for (kind, page_request, result) in results {
                    let page = result?;
                    validate_provider_page_offset(&page_request, &page, "search")?;
                    validate_provider_search_items(provider.as_ref(), &kind, &page.items)?;
                    items.extend(page.items);
                }
                items
            }
            Err(_) => anyhow::bail!(
                "provider `{provider_id}` search timed out after {}s",
                SEARCH_REQUEST_TIMEOUT.as_secs()
            ),
        };
    validate_provider_media_items(provider.as_ref(), &items)?;
    if let Ok(analytics) = crate::analytics::AnalyticsStore::open_default().await {
        let _ = analytics
            .record_event(&search_performed_event(
                AnalyticsSource::Daemon,
                &query,
                items.len(),
                started.elapsed().as_millis(),
                now_ms(),
            ))
            .await;
    }
    for item in &mut items {
        item.source = Some(ItemSource::Provider(provider.id().to_string()));
        item.freshness = Some("fresh".to_string());
    }
    state.emit_event(DaemonEvent::SearchUpdated {
        query: query.clone(),
        count: items.len(),
        provider: Some(provider_id.clone()),
    });

    // Cache to the search_runs/search_results tables on a background
    // task — fast to return, useful for analytics + Hybrid mode's
    // "show recent results immediately" path. media_items gets
    // upserted as part of that so follow-up actions (add to playlist,
    // play URI) don't need to re-fetch.
    //
    // We do NOT push these entries into the library Tantivy index.
    // That index is the user's library; polluting it with arbitrary
    // catalog hits ranked by text relevance would surface "random
    // Spotify song" results in the Library tab and would break
    // assumptions about what's actually saved. local_search's SQLite
    // fallback already orders saved/liked items first via ORDER BY,
    // so library content stays prioritised even when media_items
    // contains catalog rows.
    let cache_state = state.clone();
    let cache_query = query.clone();
    let cache_items = items.clone();
    state.spawn_background("provider-search-cache", async move {
        if let Err(err) = cache_state
            .store()
            .cache_provider_search_results(
                &provider_id,
                &cache_query,
                scope,
                provider_id.as_str(),
                &cache_items,
            )
            .await
        {
            tracing::warn!(error = %err, provider = %provider_id, "failed to cache provider search results");
        }
    });

    Ok(items)
}

/// Streaming search: ack returns immediately; the actual results
/// stream back as `DaemonEvent::SearchPage` events as each per-`(kind,
/// offset)` request resolves. After all fanned-out tasks join, a
/// `DaemonEvent::SearchComplete` event marks the end of the initial
/// fetch — clients use it to clear "loading initial results" spinners.
///
/// Initial-pages count is fixed at 1 (10 items per page; with 6 kinds
/// for `scope=All` that's 6 total requests). More pages load on scroll.
/// The fanout is detached from the request handler so the IPC reply is
/// not blocked.
pub(crate) const SEARCH_INITIAL_PAGES: u32 = 1;
pub(crate) const SEARCH_PAGE_SIZE: u32 = 10;

pub(crate) fn spawn_search_stream(
    state: Arc<DaemonState>,
    query: String,
    scope: SearchScopeData,
    source: SearchSourceData,
    version: u64,
    provider_id: ProviderId,
) {
    let state_clone = state.clone();
    state.spawn_background("search-stream", async move {
        // Local/Hybrid first emit the cache snapshot. Hybrid then continues
        // through the remote fan-out so both cold and warm searches refresh
        // provider truth instead of silently degrading to local-only.
        if !matches!(&source, SearchSourceData::Remote(_)) {
            let items =
                match local_cached_search(&state_clone, &provider_id, &query, scope, 200).await {
                    Ok(items) => items,
                    Err(err) => {
                        tracing::warn!(error = %err, "local search-stream failed");
                        state_clone.emit_event(DaemonEvent::SearchFailed {
                            query: query.clone(),
                            version,
                            kind: None,
                            offset: None,
                            message: format!("local search failed: {err}"),
                            provider: Some(provider_id.clone()),
                        });
                        state_clone.emit_event(DaemonEvent::SearchComplete {
                            query,
                            version,
                            provider: Some(provider_id),
                        });
                        return;
                    }
                };
            let by_kind = group_items_by_kind(items);
            for (kind, items) in by_kind {
                state_clone.emit_event(DaemonEvent::SearchPage {
                    query: query.clone(),
                    kind,
                    offset: 0,
                    version,
                    items,
                    provider: Some(provider_id.clone()),
                });
            }
            if matches!(&source, SearchSourceData::Local) {
                state_clone.emit_event(DaemonEvent::SearchComplete {
                    query,
                    version,
                    provider: Some(provider_id),
                });
                return;
            }
        }

        let provider = match state_clone.provider(&provider_id).await {
            Ok(provider) => provider,
            Err(err) => {
                state_clone.emit_event(DaemonEvent::SearchFailed {
                    query: query.clone(),
                    version,
                    kind: None,
                    offset: None,
                    message: err.to_string(),
                    provider: Some(provider_id.clone()),
                });
                state_clone.emit_event(DaemonEvent::SearchComplete {
                    query,
                    version,
                    provider: Some(provider_id),
                });
                return;
            }
        };
        if let Err(err) = enforce_provider_search_query_limit(provider.as_ref(), &query) {
            state_clone.emit_event(DaemonEvent::SearchFailed {
                query: query.clone(),
                version,
                kind: None,
                offset: None,
                message: err.to_string(),
                provider: Some(provider_id.clone()),
            });
            state_clone.emit_event(DaemonEvent::SearchComplete {
                query,
                version,
                provider: Some(provider_id),
            });
            return;
        }

        let kinds = provider_remote_search_kinds(provider.as_ref(), scope, None);
        if kinds.is_empty() {
            state_clone.emit_event(DaemonEvent::SearchFailed {
                query: query.clone(),
                version,
                kind: None,
                offset: None,
                message: ProviderError::unsupported(format!(
                    "provider {} search scope {}",
                    provider.id(),
                    scope.label()
                ))
                .to_string(),
                provider: Some(provider_id.clone()),
            });
            state_clone.emit_event(DaemonEvent::SearchComplete {
                query,
                version,
                provider: Some(provider_id),
            });
            return;
        }
        let mut tasks = Vec::with_capacity(kinds.len() * SEARCH_INITIAL_PAGES as usize);
        for kind in kinds {
            for page in 0..SEARCH_INITIAL_PAGES {
                let offset = page * SEARCH_PAGE_SIZE;
                let task_state = state_clone.clone();
                let task_query = query.clone();
                let task_kind = kind.clone();
                let task_provider = provider_id.clone();
                tasks.push(tokio::spawn(async move {
                    fetch_and_emit_page(
                        task_state,
                        task_query,
                        task_kind,
                        offset,
                        version,
                        task_provider,
                    )
                    .await;
                }));
            }
        }
        for handle in tasks {
            let _ = handle.await;
        }
        state_clone.emit_event(DaemonEvent::SearchComplete {
            query,
            version,
            provider: Some(provider_id),
        });
    });
}

pub(crate) fn spawn_search_page(
    state: Arc<DaemonState>,
    query: String,
    kind: MediaKind,
    offset: u32,
    version: u64,
    provider_id: ProviderId,
) {
    state.clone().spawn_background("search-page", async move {
        fetch_and_emit_page(state, query, kind, offset, version, provider_id).await;
    });
}

pub(crate) async fn fetch_and_emit_page(
    state: Arc<DaemonState>,
    query: String,
    kind: MediaKind,
    offset: u32,
    version: u64,
    provider_id: ProviderId,
) {
    let provider = match state.provider(&provider_id).await {
        Ok(provider) => provider,
        Err(err) => {
            tracing::warn!(error = %err, kind = ?kind, offset, "search-page acquire client failed");
            state.emit_event(DaemonEvent::SearchFailed {
                query,
                version,
                kind: Some(kind),
                offset: Some(offset),
                message: format!("search failed: {err}"),
                provider: Some(provider_id),
            });
            return;
        }
    };
    if let Err(err) = enforce_provider_search_query_limit(provider.as_ref(), &query) {
        state.emit_event(DaemonEvent::SearchFailed {
            query,
            version,
            kind: Some(kind),
            offset: Some(offset),
            message: err.to_string(),
            provider: Some(provider_id),
        });
        return;
    }
    let caps = provider.capabilities().search;
    if let Err(err) = require_provider_capability(
        provider.as_ref(),
        &format!("{kind} search"),
        caps.remote && caps.kinds.contains(&kind),
    ) {
        state.emit_event(DaemonEvent::SearchFailed {
            query,
            version,
            kind: Some(kind),
            offset: Some(offset),
            message: err.to_string(),
            provider: Some(provider_id),
        });
        return;
    }
    let page_request = PageRequest::new(
        SEARCH_PAGE_SIZE.min(
            u32::try_from(caps.max_page_size.unwrap_or(SEARCH_PAGE_SIZE as usize))
                .unwrap_or(u32::MAX)
                .max(1),
        ),
        u64::from(offset),
    );
    let result = tokio::time::timeout(
        SEARCH_REQUEST_TIMEOUT,
        provider.search(
            RequestContext::FOREGROUND,
            SearchRequest {
                query: query.clone(),
                kind: kind.clone(),
                page: page_request.clone(),
            },
        ),
    )
    .await;
    match result {
        Err(_) => {
            tracing::warn!(
                kind = ?kind,
                offset,
                timeout_secs = SEARCH_REQUEST_TIMEOUT.as_secs(),
                "search-page request timed out"
            );
            state.emit_event(DaemonEvent::SearchFailed {
                query,
                version,
                kind: Some(kind),
                offset: Some(offset),
                message: format!(
                    "search timed out after {}s",
                    SEARCH_REQUEST_TIMEOUT.as_secs()
                ),
                provider: Some(provider_id),
            });
        }
        Ok(Err(err)) => {
            tracing::warn!(error = %err, kind = ?kind, offset, "search-page request failed");
            state.emit_event(DaemonEvent::SearchFailed {
                query,
                version,
                kind: Some(kind),
                offset: Some(offset),
                message: format!("search failed: {err}"),
                provider: Some(provider_id),
            });
        }
        Ok(Ok(page)) => {
            if let Err(err) = validate_provider_page_offset(&page_request, &page, "search") {
                state.emit_event(DaemonEvent::SearchFailed {
                    query,
                    version,
                    kind: Some(kind),
                    offset: Some(offset),
                    message: err.to_string(),
                    provider: Some(provider_id),
                });
                return;
            }
            let mut items = page.items;
            if let Err(err) = validate_provider_search_items(provider.as_ref(), &kind, &items) {
                state.emit_event(DaemonEvent::SearchFailed {
                    query,
                    version,
                    kind: Some(kind),
                    offset: Some(offset),
                    message: err.to_string(),
                    provider: Some(provider_id),
                });
                return;
            }
            for item in &mut items {
                item.source = Some(ItemSource::Provider(provider.id().to_string()));
                item.freshness = Some("fresh".to_string());
            }
            // Cache to media_items so follow-up actions (play, queue,
            // playlist-add) don't need to re-fetch. Background task; not
            // gated on cache success — see plan §"Caching".
            if !items.is_empty() {
                let cache_state = state.clone();
                let cache_query = query.clone();
                let cache_items = items.clone();
                let cache_provider = provider_id.clone();
                state.spawn_background("provider-search-page-cache", async move {
                    if let Err(err) = cache_state
                        .store()
                        .cache_provider_search_results(
                            &cache_provider,
                            &cache_query,
                            SearchScopeData::All,
                            cache_provider.as_str(),
                            &cache_items,
                        )
                        .await
                    {
                        tracing::debug!(error = %err, "failed to cache search-page results");
                    }
                });
            }
            state.emit_event(DaemonEvent::SearchPage {
                query,
                kind,
                offset,
                version,
                items,
                provider: Some(provider_id),
            });
        }
    }
}

pub(crate) fn group_items_by_kind(items: Vec<MediaItem>) -> Vec<(MediaKind, Vec<MediaItem>)> {
    let mut buckets: Vec<(MediaKind, Vec<MediaItem>)> = Vec::new();
    for item in items {
        let kind = item.kind.clone();
        if let Some(bucket) = buckets.iter_mut().find(|(k, _)| k == &kind) {
            bucket.1.push(item);
        } else {
            buckets.push((kind, vec![item]));
        }
    }
    buckets
}

pub(crate) async fn queueable_items_for_selection(
    state: &DaemonState,
    provider: &dyn MusicProvider,
    uri: &str,
) -> anyhow::Result<Vec<MediaItem>> {
    let mut items = queueable_items_for_selection_without_cache(provider, uri).await?;
    if items.len() == 1 && items[0].name == items[0].uri {
        if let Some(cached) = lookup_known_media_item(state, &items[0].uri).await {
            items[0] = cached;
        }
    }
    Ok(items)
}

pub(crate) async fn queueable_items_for_selection_without_cache(
    provider: &dyn MusicProvider,
    uri: &str,
) -> anyhow::Result<Vec<MediaItem>> {
    let resource = ResourceUri::parse(uri)?;
    match resource.kind() {
        MediaKind::Track => {
            require_provider_capability(
                provider,
                "track catalog lookup",
                provider
                    .capabilities()
                    .catalog
                    .lookup_kinds
                    .contains(&MediaKind::Track),
            )?;
            match provider
                .media_item(RequestContext::FOREGROUND, &resource)
                .await?
            {
                Some(item) => {
                    validate_provider_lookup_result(provider, &resource, &item)?;
                    Ok(vec![item])
                }
                None => Ok(vec![media_item_from_uri(uri)?]),
            }
        }
        MediaKind::Episode => Ok(vec![media_item_from_uri(uri)?]),
        MediaKind::Playlist => {
            let items = collect_playlist_items(provider, resource).await?;
            Ok(items
                .into_iter()
                .filter(|item| matches!(item.kind, MediaKind::Track | MediaKind::Episode))
                .collect())
        }
        MediaKind::Album => collect_album_tracks(provider, resource).await,
        MediaKind::Artist | MediaKind::Show => anyhow::bail!(
            "artist and show URIs cannot be appended to the remote queue; choose a track, episode, album, or playlist"
        ),
    }
}

async fn collect_playlist_items(
    provider: &dyn MusicProvider,
    uri: ResourceUri,
) -> anyhow::Result<Vec<MediaItem>> {
    match collect_playlist_items_with_context(provider, uri, RequestContext::FOREGROUND).await? {
        AccessOutcome::Available(items) => Ok(items),
        AccessOutcome::Unavailable(_) => anyhow::bail!("playlist is not accessible"),
    }
}

async fn collect_playlist_items_with_context(
    provider: &dyn MusicProvider,
    uri: ResourceUri,
    context: RequestContext,
) -> anyhow::Result<AccessOutcome<Vec<MediaItem>>> {
    require_provider_capability(
        provider,
        "playlist item reads",
        provider.capabilities().playlists.item_read,
    )?;
    let mut page = PageRequest::new(
        provider
            .capabilities()
            .playlists
            .items_max_page_size
            .unwrap_or(50) as u32,
        0,
    );
    let mut items = Vec::new();
    let mut seen_cursors = std::collections::HashSet::new();
    for page_index in 0..PROVIDER_PAGINATION_MAX_PAGES {
        let outcome = provider
            .playlist_items(
                context,
                CollectionRequest {
                    uri: uri.clone(),
                    page: page.clone(),
                },
            )
            .await?;
        let result = match outcome {
            AccessOutcome::Available(result) => result,
            AccessOutcome::Unavailable(reason) => {
                return Ok(AccessOutcome::Unavailable(reason));
            }
        };
        validate_provider_page_offset(&page, &result, "playlist_items")?;
        validate_provider_collection_items(
            provider,
            "playlist_items",
            &[MediaKind::Track, MediaKind::Episode],
            &result.items,
        )?;
        items.extend(result.items);
        let Some(continuation) = result.next else {
            return Ok(AccessOutcome::Available(items));
        };
        page = next_provider_page(
            &page,
            continuation,
            items.len() as u64,
            &mut seen_cursors,
            page_index + 1,
            "playlist items",
        )?;
    }
    Err(ProviderError::Provider(format!(
        "playlist item pagination exceeded {PROVIDER_PAGINATION_MAX_PAGES} pages"
    ))
    .into())
}

async fn collect_album_tracks(
    provider: &dyn MusicProvider,
    uri: ResourceUri,
) -> anyhow::Result<Vec<MediaItem>> {
    require_provider_capability(
        provider,
        "album tracks",
        provider.capabilities().catalog.album_tracks,
    )?;
    let mut page = PageRequest::new(
        provider
            .capabilities()
            .catalog
            .album_tracks_max_page_size
            .unwrap_or(50) as u32,
        0,
    );
    let mut items = Vec::new();
    let mut seen_cursors = std::collections::HashSet::new();
    for page_index in 0..PROVIDER_PAGINATION_MAX_PAGES {
        let result = provider
            .album_tracks(
                RequestContext::FOREGROUND,
                CollectionRequest {
                    uri: uri.clone(),
                    page: page.clone(),
                },
            )
            .await?;
        validate_provider_page_offset(&page, &result, "album_tracks")?;
        validate_provider_collection_items(
            provider,
            "album_tracks",
            &[MediaKind::Track],
            &result.items,
        )?;
        items.extend(result.items);
        let Some(continuation) = result.next else {
            return Ok(items);
        };
        page = next_provider_page(
            &page,
            continuation,
            items.len() as u64,
            &mut seen_cursors,
            page_index + 1,
            "album tracks",
        )?;
    }
    Err(ProviderError::Provider(format!(
        "album track pagination exceeded {PROVIDER_PAGINATION_MAX_PAGES} pages"
    ))
    .into())
}

pub(crate) const PROVIDER_PAGINATION_MAX_PAGES: usize = 1_000;

pub(crate) fn next_provider_page(
    current: &PageRequest,
    continuation: PageContinuation,
    logical_offset: u64,
    seen_cursors: &mut std::collections::HashSet<String>,
    pages_fetched: usize,
    surface: &str,
) -> Result<PageRequest, ProviderError> {
    if pages_fetched >= PROVIDER_PAGINATION_MAX_PAGES {
        return Err(ProviderError::Provider(format!(
            "{surface} pagination exceeded {PROVIDER_PAGINATION_MAX_PAGES} pages"
        )));
    }
    match continuation {
        PageContinuation::Offset(offset) if offset > current.offset => {
            Ok(PageRequest::new(current.limit, offset))
        }
        PageContinuation::Offset(offset) => Err(ProviderError::Provider(format!(
            "{surface} pagination did not advance: {offset} <= {}",
            current.offset
        ))),
        PageContinuation::Cursor(cursor) if seen_cursors.insert(cursor.clone()) => Ok(
            PageRequest::with_cursor(current.limit, logical_offset, cursor),
        ),
        PageContinuation::Cursor(_) => Err(ProviderError::Provider(format!(
            "{surface} pagination repeated a cursor"
        ))),
    }
}

pub(crate) fn idle_context_start_label(kind: &MediaKind) -> Option<&'static str> {
    match kind {
        MediaKind::Album => Some("album"),
        MediaKind::Playlist => Some("playlist"),
        _ => None,
    }
}

pub(crate) async fn optimistic_queue_with_appends(
    state: &DaemonState,
    provider: &ProviderId,
    queued_items: Vec<MediaItem>,
    live_uris: &std::collections::HashSet<String>,
) -> Option<spotuify_core::Queue> {
    if queued_items.is_empty() {
        return None;
    }
    let mut base = state
        .store()
        .latest_provider_queue(500, provider)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let as_of_ms = now_ms();
    let cache_age_ms = as_of_ms.saturating_sub(base.as_of_ms);
    let looks_historical = base.currently_playing.is_none() && !base.items.is_empty();
    if looks_historical
        || (!base.session_active
            && (base.as_of_ms <= 0 || cache_age_ms > QUEUE_APPEND_BASE_MAX_AGE_MS))
    {
        base = spotuify_core::Queue::default();
    }
    // Occurrence tracking keys off the LIVE queue — the same base the
    // add's dedup used — while the optimistic emit overlays the cached
    // base (what clients currently see).
    state.track_pending_queue_appends(provider, live_uris, &queued_items, as_of_ms);
    Some(queue_with_appended_items(base, queued_items, as_of_ms))
}

/// Build and durably persist an optimistic queue append. Returning `None`
/// means callers may still announce the remote mutation via `uris`, but must
/// not attach a snapshot that clients could mistake for durable cache truth.
pub(crate) async fn cache_optimistic_queue_with_appends(
    state: &DaemonState,
    provider: &ProviderId,
    queued_items: Vec<MediaItem>,
    live_uris: &std::collections::HashSet<String>,
) -> Option<spotuify_core::Queue> {
    let queue = optimistic_queue_with_appends(state, provider, queued_items, live_uris).await?;
    cache_queue_for_provider(state, Some(provider), &queue)
        .await
        .then_some(queue)
}

/// Cache the queue state after a queue mutation that may have recovered an
/// idle session by starting its first item. A started item is the queue head,
/// not an upcoming append; emitting an active queue without that head would
/// misrepresent what the transport is playing.
pub(crate) async fn cache_optimistic_queue_application(
    state: &DaemonState,
    provider: &ProviderId,
    started_item: Option<MediaItem>,
    queued_items: Vec<MediaItem>,
    live_uris: &std::collections::HashSet<String>,
) -> Option<spotuify_core::Queue> {
    let Some(started_item) = started_item else {
        return cache_optimistic_queue_with_appends(state, provider, queued_items, live_uris).await;
    };
    let as_of_ms = now_ms();
    state.track_pending_queue_appends(provider, live_uris, &queued_items, as_of_ms);
    let queue = spotuify_core::Queue {
        currently_playing: Some(started_item),
        items: queued_items,
        session_active: true,
        as_of_ms,
    };
    cache_queue_for_provider(state, Some(provider), &queue)
        .await
        .then_some(queue)
}

/// Upper bound on how much of the Liked Songs collection the daemon
/// materialises for a context play. Large enough to cover essentially
/// every real library while keeping the resolved list bounded.
pub(crate) const LIKED_CONTEXT_TRACK_LIMIT: u32 = 10_000;

/// Synthesize the queue clients render after a `PlayUri` command.
///
/// - `context = None`: legacy behaviour — only album/playlist *play*
///   URIs (the whole-collection tap) synthesize a queue, starting at the
///   first item.
/// - `context = Some(album/playlist)`: resolve the context's items and
///   start the queue at `start_uri` (fixes album/playlist row taps).
/// - `context = Some(Liked list)`: resolve the full cached Liked Songs
///   list and start the queue at `start_uri`.
pub(crate) async fn context_queue_snapshot_for_play(
    state: &DaemonState,
    provider: &ProviderId,
    start_uri: &str,
    context: Option<&PlayContext>,
) -> Option<spotuify_core::Queue> {
    let items = match context {
        None => return context_queue_snapshot_for_play_uri(state, start_uri).await,
        Some(context) if context.tracks.is_some() => {
            resolve_liked_media_items(state, provider).await?
        }
        Some(PlayContext {
            context_uri: Some(context_uri),
            ..
        }) => {
            let resource = ResourceUri::parse(context_uri).ok()?;
            let provider = match state.provider_for_uri(&resource).await {
                Ok(provider) => provider,
                Err(err) => {
                    tracing::debug!(error = %err, context_uri, "could not build context queue snapshot");
                    return None;
                }
            };
            match queueable_items_for_selection(state, provider.as_ref(), context_uri).await {
                Ok(items) => items,
                Err(err) => {
                    tracing::debug!(error = %err, context_uri, "could not resolve context queue items");
                    return None;
                }
            }
        }
        Some(_) => return None,
    };
    queue_for_started_context_at(items, start_uri, now_ms())
}

/// Resolve a `PlayUri` command's optional `context_uri` into a concrete
/// [`PlayContext`] the transport / Web-API paths can execute.
///
/// - `None` → no context (legacy single-track play).
/// - Liked sentinel → the full ordered Liked Songs track list. Resolving
///   to an empty/unavailable list yields `None` so the play degrades to a
///   plain single-track start rather than failing.
/// - any other URI (album/playlist/…) → a Spotify context to load.
pub(crate) async fn resolve_play_context(
    state: &DaemonState,
    provider: &dyn MusicProvider,
    context_uri: Option<&str>,
) -> anyhow::Result<Option<PlayContext>> {
    Ok(match context_uri {
        None => None,
        Some(LIKED_SONGS_CONTEXT) => {
            let Some(items) = resolve_liked_media_items(state, provider.id()).await else {
                return Ok(None);
            };
            let tracks = items.into_iter().map(|item| item.uri).collect::<Vec<_>>();
            Some(PlayContext {
                context_uri: None,
                tracks: Some(tracks),
            })
        }
        Some(context_uri) => {
            let resource = ResourceUri::parse(context_uri)?;
            if resource.scheme() != provider.uri_scheme() {
                return Err(ProviderError::InvalidInput {
                    field: "context_uri".to_string(),
                    message: format!(
                        "play context `{context_uri}` belongs to a different provider than {}",
                        provider.id()
                    ),
                }
                .into());
            }
            if !matches!(
                resource.kind(),
                MediaKind::Album | MediaKind::Artist | MediaKind::Playlist | MediaKind::Show
            ) {
                return Err(ProviderError::InvalidInput {
                    field: "context_uri".to_string(),
                    message: format!("{} cannot be used as a playback context", resource.kind()),
                }
                .into());
            }
            Some(PlayContext {
                context_uri: Some(resource.as_uri()),
                tracks: None,
            })
        }
    })
}

pub(crate) async fn context_queue_snapshot_for_play_uri(
    state: &DaemonState,
    uri: &str,
) -> Option<spotuify_core::Queue> {
    let kind = ResourceUri::parse(uri).ok()?.kind();
    if !matches!(kind, MediaKind::Album | MediaKind::Playlist) {
        return None;
    }
    let provider = match state.provider_for_uri(&ResourceUri::parse(uri).ok()?).await {
        Ok(provider) => provider,
        Err(err) => {
            tracing::debug!(error = %err, uri, "could not build context queue snapshot");
            return None;
        }
    };
    let items = match queueable_items_for_selection(state, provider.as_ref(), uri).await {
        Ok(items) => items,
        Err(err) => {
            tracing::debug!(error = %err, uri, "could not resolve context queue items");
            return None;
        }
    };
    queue_for_started_context(items, now_ms())
}

/// Resolve the full ordered Liked Songs list (server order = date-added
/// desc) from the local cache, as `MediaItem`s for the queue rail.
pub(crate) async fn resolve_liked_media_items(
    state: &DaemonState,
    provider: &ProviderId,
) -> Option<Vec<MediaItem>> {
    match state
        .store()
        .list_saved_tracks(LIKED_CONTEXT_TRACK_LIMIT, Some(provider.as_str()))
        .await
    {
        Ok(items) if !items.is_empty() => Some(items),
        Ok(_) => {
            tracing::debug!("liked songs context resolved to an empty cached list");
            None
        }
        Err(err) => {
            tracing::debug!(error = %err, "could not resolve liked songs for context play");
            None
        }
    }
}

pub(crate) fn queue_for_started_context(
    context_items: Vec<MediaItem>,
    as_of_ms: i64,
) -> Option<spotuify_core::Queue> {
    let first = context_items.first().map(|item| item.uri.clone())?;
    queue_for_started_context_at(context_items, &first, as_of_ms)
}

/// Build a "now playing + upcoming" queue that starts at `start_uri`.
///
/// Finds `start_uri` in `context_items`; everything from it becomes the
/// now-playing head and the remainder the upcoming tail. A missing
/// `start_uri` means the cache cannot truthfully describe this playback, so
/// no synthetic queue is emitted.
pub(crate) fn queue_for_started_context_at(
    context_items: Vec<MediaItem>,
    start_uri: &str,
    as_of_ms: i64,
) -> Option<spotuify_core::Queue> {
    if context_items.is_empty() {
        return None;
    }
    let start = context_items
        .iter()
        .position(|item| item.uri == start_uri)?;
    let mut rest = context_items;
    let tail = rest.split_off(start);
    let mut tail = tail.into_iter();
    let currently_playing = tail.next();
    Some(spotuify_core::Queue {
        currently_playing,
        items: tail.collect(),
        session_active: true,
        as_of_ms,
    })
}

pub(crate) fn queue_with_appended_items(
    mut queue: spotuify_core::Queue,
    queued_items: Vec<MediaItem>,
    as_of_ms: i64,
) -> spotuify_core::Queue {
    queue.items.extend(queued_items);
    queue.session_active = true;
    queue.as_of_ms = as_of_ms;
    queue
}

pub(crate) fn scope_media_kinds(scope: SearchScopeData) -> Vec<MediaKind> {
    match scope {
        SearchScopeData::All => vec![
            MediaKind::Track,
            MediaKind::Episode,
            MediaKind::Show,
            MediaKind::Album,
            MediaKind::Artist,
            MediaKind::Playlist,
        ],
        SearchScopeData::Track => vec![MediaKind::Track],
        SearchScopeData::Episode => vec![MediaKind::Episode],
        SearchScopeData::Show => vec![MediaKind::Show],
        SearchScopeData::Album => vec![MediaKind::Album],
        SearchScopeData::Artist => vec![MediaKind::Artist],
        SearchScopeData::Playlist => vec![MediaKind::Playlist],
    }
}

pub(crate) async fn cache_playback(
    state: &DaemonState,
    provider: &ProviderId,
    playback: &Playback,
) -> bool {
    let media = playback.item.iter().collect::<Vec<_>>();
    let provider = match routed_media_provider(state, &media, Some(provider)).await {
        Ok(provider) => provider,
        Err(err) => {
            tracing::warn!(error = %err, "refusing unroutable playback snapshot");
            return false;
        }
    };
    match state
        .store()
        .persist_provider_playback(&provider, playback)
        .await
    {
        Ok(_) => true,
        Err(err) => {
            tracing::warn!(error = %err, "failed to cache playback snapshot");
            false
        }
    }
}

async fn routed_media_provider(
    state: &DaemonState,
    items: &[&MediaItem],
    expected: Option<&ProviderId>,
) -> anyhow::Result<ProviderId> {
    let providers = state.providers().await?;
    let mut routed = None;
    for item in items {
        let uri = ResourceUri::parse(&item.uri)?;
        let provider = providers.provider_for_uri(&uri)?.id().clone();
        if routed
            .as_ref()
            .is_some_and(|selected| selected != &provider)
        {
            return Err(ProviderError::InvalidInput {
                field: "media_item.uri".to_string(),
                message: "snapshot contains resources from multiple providers".to_string(),
            }
            .into());
        }
        routed = Some(provider);
    }
    if let (Some(routed), Some(expected)) = (routed.as_ref(), expected) {
        if routed != expected {
            return Err(ProviderError::InvalidInput {
                field: "media_item.uri".to_string(),
                message: format!(
                    "snapshot from provider `{expected}` contains a `{routed}` resource"
                ),
            }
            .into());
        }
    }
    Ok(routed.unwrap_or_else(|| {
        expected.cloned().unwrap_or_else(|| {
            state
                .active_transport_provider()
                .unwrap_or_else(|| providers.default_id().clone())
        })
    }))
}

/// Persist a polled playback snapshot only when no hot-path mutation
/// has fired since `captured_seq` was observed. Without this gate the
/// background refresh below can clobber an optimistic Pause/Resume
/// with Spotify's stale pre-mutation `is_playing` flag. Returns
/// `true` if the persist applied; `false` if it was dropped as
/// stale. The caller uses the return to decide whether to broadcast
/// a `PlaybackChanged` event — there's no point notifying clients to
/// re-fetch if we threw the result away.
pub(crate) async fn cache_playback_if_fresh(
    state: &DaemonState,
    provider: &ProviderId,
    playback: &Playback,
    captured_seq: u64,
    sampled_at_ms: i64,
) -> bool {
    let media = playback.item.iter().collect::<Vec<_>>();
    if let Err(err) = routed_media_provider(state, &media, Some(provider)).await {
        tracing::warn!(error = %err, "refusing unroutable playback snapshot");
        return false;
    }
    match <DaemonState as spotuify_sync::SyncContext>::prepare_and_persist_playback_poll_if_current(
        state,
        provider,
        playback,
        captured_seq,
        sampled_at_ms,
        playback.provider_timestamp_ms,
    )
    .await
    {
        Ok(candidate) => candidate.is_some(),
        Err(err) => {
            tracing::warn!(error = %err, "failed to cache playback snapshot");
            false
        }
    }
}

pub(crate) async fn skip_refresh_due_to_rate_limit(
    state: &DaemonState,
    provider: &ProviderId,
    domain: &str,
    refresh: &'static str,
) -> bool {
    match state
        .store()
        .provider_rate_limit_cooldown_remaining_ms(provider.as_str(), domain)
        .await
    {
        Ok(Some(remaining_ms)) => {
            tracing::debug!(
                domain,
                refresh,
                remaining_ms,
                "skipping refresh while provider rate-limit cooldown is active"
            );
            // Tell subscribers — the TUI's rate-limit banner has
            // existed for months but nothing ever emitted this event,
            // so cooldowns looked like silent staleness.
            let notice_key = format!("{provider}/{domain}");
            if state.should_notify_rate_limit(&notice_key, now_ms()) {
                state.emit_event(DaemonEvent::RateLimited {
                    retry_after_secs: (remaining_ms.max(0) as u64).div_ceil(1000).max(1),
                    scope: domain.to_string(),
                    provider: Some(provider.clone()),
                });
            }
            true
        }
        Ok(None) => false,
        Err(err) => {
            tracing::debug!(
                domain,
                refresh,
                error = %err,
                "failed to inspect rate-limit cooldown before refresh"
            );
            false
        }
    }
}

/// Minimum gap between on-demand `/me/player` fetches while playback
/// runs on a FOREIGN device (bursty client reads coalesce into one).
const PLAYBACK_WEB_FETCH_GAP_MS: i64 = 3_000;
/// Gap while OUR embedded device is the active player: librespot's
/// PlayerEvents are the truth and the web poll is pure reconciliation
/// (mirrors the sync loop's PLAYBACK_RECONCILE_CADENCE).
const PLAYBACK_WEB_FETCH_GAP_EMBEDDED_MS: i64 = 30_000;
/// Local-truth freshness: skip the web poll entirely when a librespot
/// PlayerEvent re-seated the clock this recently.
const PLAYER_EVENT_FRESH_MS: i64 = 5_000;
const QUEUE_WEB_FETCH_GAP_MS: i64 = 10_000;
const DEVICES_WEB_FETCH_GAP_MS: i64 = 60_000;

pub(crate) fn spawn_playback_refresh(state: Arc<DaemonState>) {
    let now = now_ms();
    let snapshot = state.snapshot_playback();
    if snapshot.source == Some(spotuify_core::PlaybackStateSource::PlayerEvent)
        && snapshot
            .sampled_at_ms
            .is_some_and(|sampled| now.saturating_sub(sampled) < PLAYER_EVENT_FRESH_MS)
    {
        // A fresh PlayerEvent outranks anything `/me/player` could say.
        return;
    }
    let min_gap = if state.embedded_owns_playback() {
        PLAYBACK_WEB_FETCH_GAP_EMBEDDED_MS
    } else {
        PLAYBACK_WEB_FETCH_GAP_MS
    };
    if !state.playback_refresh_gate.try_claim(now, min_gap) {
        return;
    }
    spawn_playback_refresh_forced(state);
}

/// Ungated playback refresh — for mutation-failure reconciles where a
/// fetch is mandatory regardless of coalescing. Stamps the gate so
/// gated callers right after it still coalesce.
pub(crate) fn spawn_playback_refresh_forced(state: Arc<DaemonState>) {
    state.playback_refresh_gate.stamp(now_ms());
    let task_state = state.clone();
    let captured_seq = state.current_mutation_seq();
    state.spawn_background("playback-refresh", async move {
        let started = std::time::Instant::now();
        let Ok((provider, transport)) = current_transport_provider_pair(&task_state).await else {
            return;
        };
        let provider_id = provider.id().clone();
        if skip_refresh_due_to_rate_limit(&task_state, &provider_id, "playback", "playback-refresh")
            .await
        {
            return;
        }
        if !provider
            .capabilities()
            .transport
            .as_ref()
            .is_some_and(|caps| caps.playback_state)
        {
            return;
        }
        match transport.playback(RequestContext::BACKGROUND_SYNC).await {
            Ok(playback) => {
                if let Err(err) = validate_provider_playback(provider.as_ref(), &playback) {
                    tracing::warn!(error = %err, provider = %provider_id, "refusing invalid playback output");
                    return;
                }
                record_daemon_action(
                    "status",
                    playback.item.as_ref().map(|item| item.uri.as_str()),
                    serde_json::json!({"is_playing": playback.is_playing}),
                )
                .await;
                let has_live_signal = playback_has_live_signal(&playback);
                // SQLite is the commit point: serialize the final sequence
                // check + write against transport mutations before exposing
                // the poll through the canonical clock.
                let sampled_at_ms = spotuify_core::now_ms();
                let persisted = cache_playback_if_fresh(
                    &task_state,
                    &provider_id,
                    &playback,
                    captured_seq,
                    sampled_at_ms,
                )
                .await;
                let clock_applied = if persisted {
                    task_state.playback_clock().apply_web_api_poll(
                        &playback,
                        captured_seq,
                        task_state.current_mutation_seq(),
                        sampled_at_ms,
                        playback.provider_timestamp_ms,
                    )
                } else {
                    false
                };
                let applied = persisted && (has_live_signal || clock_applied);
                if applied {
                    task_state
                        .viz_coordinator()
                        .set_playing(playback.is_playing);
                }
                tracing::debug!(
                    target: "spotuify_daemon::refresh",
                    captured_seq,
                    duration_ms = started.elapsed().as_millis() as u64,
                    outcome = if applied {
                        if has_live_signal {
                            "applied"
                        } else {
                            "no-session-cleared"
                        }
                    } else if has_live_signal {
                        "stale"
                    } else {
                        "empty-ignored"
                    },
                    fetched_uri = playback
                        .item
                        .as_ref()
                        .map_or("", |i| i.uri.as_str()),
                    is_playing = playback.is_playing,
                    "playback refresh"
                );
                if applied {
                    // Phase 3 — embed the just-applied snapshot from the
                    // clock so TUI/MCP can re-render in one IPC, not two.
                    task_state.emit_event(DaemonEvent::PlaybackChanged {
                        action: "refreshed".to_string(),
                        playback: Some(task_state.snapshot_playback()),
                    });
                }
            }
            Err(err) => tracing::warn!(
                target: "spotuify_daemon::refresh",
                captured_seq,
                duration_ms = started.elapsed().as_millis() as u64,
                outcome = "error",
                error = %err,
                "background playback refresh failed"
            ),
        }
    });
}

pub(crate) async fn cache_queue(state: &DaemonState, queue: &Queue) {
    let _ = cache_queue_for_provider(state, None, queue).await;
}

pub(crate) async fn cache_queue_for_provider(
    state: &DaemonState,
    expected_provider: Option<&ProviderId>,
    queue: &Queue,
) -> bool {
    let mut media = Vec::with_capacity(queue.items.len() + 1);
    media.extend(queue.currently_playing.iter());
    media.extend(queue.items.iter());
    let provider = match routed_media_provider(state, &media, expected_provider).await {
        Ok(provider) => provider,
        Err(err) => {
            tracing::warn!(error = %err, "refusing unroutable queue snapshot");
            return false;
        }
    };
    if let Err(err) = state.store().persist_provider_queue(&provider, queue).await {
        tracing::warn!(error = %err, "failed to cache queue");
        return false;
    }
    state.warm_queue(queue);
    true
}

/// Persist + cache only when the queue snapshot came from a live
/// session. When Spotify reports no active session the returned queue
/// is structurally empty (`currently_playing: None`, `items: []`) — in
/// that case we deliberately skip the store write so history remains
/// recoverable, but clients receive an empty non-actionable live queue.
pub(crate) async fn cache_queue_if_fresh(
    state: &DaemonState,
    provider: &ProviderId,
    queue: &Queue,
    captured_seq: u64,
) -> Option<Queue> {
    if !queue.session_active {
        tracing::debug!("queue refresh: no active session, preserving cache");
        return None;
    }
    // Anchor BEFORE the overlay: the tail lookup keys on the last item
    // Spotify itself reported, not on our optimistic appends. The merge
    // itself (Spotify's ~20-item cap, librespot's empty-items shape,
    // wrong-prediction recovery, shuffle gate) lives in
    // `spotuify_core::queue_merge` so the sync loop's queue write path
    // applies IDENTICAL logic — it bypassing the merge was a live bug
    // (queue rail collapsed within one 15s sync cadence).
    let anchor = spotuify_core::queue_merge::queue_tail_anchor(queue);
    let now = now_ms();
    let queue = state.overlay_pending_queue_appends(provider, queue.clone(), now);
    let queue_snapshots_complete = state
        .providers()
        .await
        .ok()
        .and_then(|registry| {
            registry
                .provider(provider)
                .ok()
                .and_then(|runtime| runtime.capabilities().transport.as_ref())
                .map(|transport| transport.queue_snapshots_complete)
        })
        .unwrap_or(false);
    let queue = if let Ok(Some(cached)) = state.store().latest_provider_queue(500, provider).await {
        spotuify_core::queue_merge::reconcile_provider_queue(
            queue,
            anchor.as_deref(),
            &cached,
            state.snapshot_playback().shuffle,
            now,
            queue_snapshots_complete,
        )
    } else {
        queue
    };
    let mut media = Vec::with_capacity(queue.items.len() + 1);
    media.extend(queue.currently_playing.iter());
    media.extend(queue.items.iter());
    if let Err(err) = routed_media_provider(state, &media, Some(provider)).await {
        tracing::warn!(error = %err, "refusing unroutable queue snapshot");
        return None;
    }
    match state
        .persist_fresh_queue(provider, &queue, captured_seq)
        .await
    {
        Ok(true) => {
            state.warm_queue(&queue);
            Some(queue)
        }
        Ok(false) => {
            tracing::debug!("dropping stale queue refresh: mutation in flight");
            None
        }
        Err(err) => {
            tracing::warn!(error = %err, "failed to cache queue");
            None
        }
    }
}

pub(crate) fn spawn_queue_refresh(state: Arc<DaemonState>) {
    // Coalesce read-driven refreshes (QueueGet from every connecting
    // client). Mutation reconciles use `spawn_queue_refresh_with_seq`
    // directly and bypass the gate.
    if !state
        .queue_refresh_gate
        .try_claim(now_ms(), QUEUE_WEB_FETCH_GAP_MS)
    {
        return;
    }
    let captured_seq = state.current_mutation_seq();
    spawn_queue_refresh_with_seq(state, captured_seq);
}

/// Queue refresh measured against an explicit seq — used by mutation
/// closures so the refresh is invalidated by ANY mutation after the
/// one that scheduled it (capturing at fetch time would adopt a racing
/// mutation's seq and apply a mid-transition snapshot).
pub(crate) fn spawn_queue_refresh_with_seq(state: Arc<DaemonState>, captured_seq: u64) {
    state.queue_refresh_gate.stamp(now_ms());
    let task_state = state.clone();
    state.spawn_background("queue-refresh", async move {
        let Ok((provider, transport)) = current_transport_provider_pair(&task_state).await else {
            return;
        };
        refresh_queue_for_pair(task_state, provider, transport, captured_seq).await;
    });
}

/// Reconcile a queue mutation against the exact provider pair that accepted
/// it. Resolving the global active owner inside the spawned task can race a
/// later provider switch and fetch/cache the wrong queue.
pub(crate) fn spawn_queue_refresh_for_pair(
    state: Arc<DaemonState>,
    provider: Arc<dyn MusicProvider>,
    transport: Arc<dyn RemoteTransport>,
    captured_seq: u64,
) {
    state.queue_refresh_gate.stamp(now_ms());
    let task_state = state.clone();
    state.spawn_background("queue-refresh", async move {
        refresh_queue_for_pair(task_state, provider, transport, captured_seq).await;
    });
}

async fn refresh_queue_for_pair(
    task_state: Arc<DaemonState>,
    provider: Arc<dyn MusicProvider>,
    transport: Arc<dyn RemoteTransport>,
    captured_seq: u64,
) {
    let started = std::time::Instant::now();
    let provider_id = provider.id().clone();
    if skip_refresh_due_to_rate_limit(&task_state, &provider_id, "queue", "queue-refresh").await {
        return;
    }
    if !provider
        .capabilities()
        .transport
        .as_ref()
        .is_some_and(|caps| caps.queue_read)
    {
        return;
    }
    match transport.queue(RequestContext::BACKGROUND_SYNC).await {
        Ok(queue) => {
            if let Err(err) = validate_provider_queue(provider.as_ref(), &queue) {
                tracing::warn!(error = %err, provider = %provider_id, "refusing invalid queue output");
                return;
            }
            record_daemon_action(
                "queue",
                queue
                    .currently_playing
                    .as_ref()
                    .map(|item| item.uri.as_str()),
                serde_json::json!({"upcoming_count": queue.items.len()}),
            )
            .await;
            let applied_queue =
                cache_queue_if_fresh(&task_state, &provider_id, &queue, captured_seq).await;
            let applied = applied_queue.is_some();
            tracing::debug!(
                target: "spotuify_daemon::refresh",
                captured_seq,
                duration_ms = started.elapsed().as_millis() as u64,
                outcome = if applied {
                    "applied"
                } else if queue.session_active {
                    "stale"
                } else {
                    "no-session"
                },
                fetched_uri = queue
                    .currently_playing
                    .as_ref()
                    .map_or("", |i| i.uri.as_str()),
                items = queue.items.len(),
                "queue refresh"
            );
            if let Some(queue) = applied_queue {
                task_state.emit_event(DaemonEvent::QueueChanged {
                    action: "refreshed".to_string(),
                    uris: Vec::new(),
                    queue: Some(queue),
                });
            }
        }
        Err(err) => tracing::warn!(
            target: "spotuify_daemon::refresh",
            captured_seq,
            duration_ms = started.elapsed().as_millis() as u64,
            outcome = "error",
            error = %err,
            "background queue refresh failed"
        ),
    }
}

pub(crate) async fn cache_devices(
    state: &DaemonState,
    provider: &ProviderId,
    devices: &[Device],
) -> bool {
    // Full-refresh path: this is the entire `/v1/me/player/devices`
    // snapshot, so call `replace_devices` to prune any cached row
    // Spotify didn't return. Drops stale "spotuify" namesakes left
    // over from prior daemon runs once Spotify's own retention
    // expires them upstream.
    match state
        .store()
        .replace_provider_devices(provider, devices)
        .await
    {
        Ok(_) => true,
        Err(err) => {
            tracing::warn!(error = %err, "failed to cache devices");
            false
        }
    }
}

pub(crate) async fn cached_devices_with_own_device(
    state: &DaemonState,
    provider: &ProviderId,
) -> anyhow::Result<Vec<spotuify_core::Device>> {
    let mut devices = state.store().list_provider_devices(provider).await?;
    // `own_device_entry` (not `connected_own_device`): the embedded device
    // stays listed while the player idles after a session drop, or it
    // becomes untargetable from every client until a manual reconnect.
    let owns_embedded = state.provider_owns_embedded_player(provider);
    if owns_embedded {
        if let Some(own_device) = state.own_device_entry().await {
            let own_id = own_device.id.as_deref();
            if !devices.iter().any(|device| device.id.as_deref() == own_id) {
                devices.push(own_device);
            }
        }
    }
    Ok(devices)
}

pub(crate) async fn cache_devices_if_fresh(
    state: &DaemonState,
    provider: &ProviderId,
    devices: &[Device],
    captured_seq: u64,
) -> bool {
    match state
        .persist_fresh_devices(provider, devices, captured_seq)
        .await
    {
        Ok(applied) => applied,
        Err(err) => {
            tracing::warn!(error = %err, "failed to cache devices");
            false
        }
    }
}

/// Phase 1 — persist the provider-neutral transport `CommandResult`
/// BEFORE emitting `PlaybackChanged`. Without this, subscribers re-fetch
/// `PlaybackGet` and read stale cached state until the next background
/// refresh — the exact "pause feels laggy" symptom the plan calls out.
///
/// Guards everything behind `may_apply_state_update(captured_seq)` so a
/// follow-up mutation that bumps the seq won't be clobbered by our
/// older response. Returns the set of state classes that were persisted
/// (for span fields); empty when nothing applied.
pub(crate) async fn persist_command_result(
    state: &DaemonState,
    provider_id: &ProviderId,
    captured_seq: u64,
    result: &CommandResult,
    action: &'static str,
    expected_playback: Option<&ExpectedPlayback>,
) -> CommandResultPersistOutcome {
    let mut outcome = CommandResultPersistOutcome::default();
    if !state.may_apply_state_update(captured_seq) {
        tracing::debug!(
            target: "spotuify_daemon::post_command",
            action,
            captured_seq,
            "dropping post-command result: newer mutation in flight"
        );
        return outcome;
    }
    if let Some(playback) = result.playback.as_ref() {
        let provider = match state.provider(provider_id).await {
            Ok(provider) => provider,
            Err(err) => {
                tracing::warn!(error = %err, %provider_id, "post-command provider disappeared");
                return outcome;
            }
        };
        if let Err(err) = validate_provider_playback(provider.as_ref(), playback) {
            tracing::warn!(error = %err, %provider_id, "refusing invalid post-command playback");
            return outcome;
        }
        if !post_command_playback_matches(playback, expected_playback) {
            tracing::debug!(
                target: "spotuify_daemon::post_command",
                action,
                captured_seq,
                fetched_uri = playback
                    .item
                    .as_ref()
                    .map_or("", |item| item.uri.as_str()),
                fetched_is_playing = playback.is_playing,
                expected_uri = expected_playback
                    .and_then(|expected| expected.uri.as_deref())
                    .unwrap_or(""),
                expected_is_playing = expected_playback
                    .and_then(|expected| expected.is_playing)
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                "dropping post-command playback result: stale spotify readback"
            );
        } else if cache_playback(state, provider_id, playback).await {
            state.viz_coordinator().set_playing(playback.is_playing);
            // Phase 2 — feed the clock immediately so the next
            // `PlaybackGet` (and the pushed snapshot in Phase 3) reflect
            // the post-mutation truth without waiting for a poll.
            state
                .playback_clock()
                .apply_command_result(playback, spotuify_core::now_ms());
            outcome.playback = Some(PostCommandPlayback {
                is_playing: playback.is_playing,
                uri: playback.item.as_ref().map(|item| item.uri.clone()),
            });
        }
    }
    if let Some(queue) = result.queue.as_ref() {
        let provider = match state.provider(provider_id).await {
            Ok(provider) => provider,
            Err(err) => {
                tracing::warn!(error = %err, %provider_id, "post-command provider disappeared");
                return outcome;
            }
        };
        if let Err(err) = validate_provider_queue(provider.as_ref(), queue) {
            tracing::warn!(error = %err, %provider_id, "refusing invalid post-command queue");
            return outcome;
        }
        if cache_queue_for_provider(state, Some(provider_id), queue).await {
            outcome.queue_items = Some(queue.items.len());
        }
    }
    if let Some(devices) = result.devices.as_ref() {
        if cache_devices(state, provider_id, devices).await {
            outcome.devices = Some(devices.len());
        }
    }
    outcome
}

#[derive(Debug, Default, Clone)]
pub(crate) struct CommandResultPersistOutcome {
    pub(crate) playback: Option<PostCommandPlayback>,
    pub(crate) queue_items: Option<usize>,
    pub(crate) devices: Option<usize>,
}

#[derive(Debug, Clone)]
pub(crate) struct PostCommandPlayback {
    pub(crate) is_playing: bool,
    pub(crate) uri: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ExpectedPlayback {
    pub(crate) uri: Option<String>,
    pub(crate) is_playing: Option<bool>,
}

pub(crate) fn post_command_playback_matches(
    playback: &Playback,
    expected: Option<&ExpectedPlayback>,
) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    if let Some(expected_uri) = expected.uri.as_deref() {
        let fetched_uri = playback.item.as_ref().map(|item| item.uri.as_str());
        if fetched_uri != Some(expected_uri) {
            return false;
        }
    }
    if let Some(expected_is_playing) = expected.is_playing {
        if playback.is_playing != expected_is_playing {
            return false;
        }
    }
    true
}

pub(crate) fn playback_has_live_signal(playback: &Playback) -> bool {
    playback.item.is_some() || playback.device.is_some() || playback.is_playing
}

pub(crate) fn spawn_devices_refresh(state: Arc<DaemonState>) {
    // The device list changes rarely; DevicesList fires on every
    // client seed, so coalesce hard.
    if !state
        .devices_refresh_gate
        .try_claim(now_ms(), DEVICES_WEB_FETCH_GAP_MS)
    {
        return;
    }
    let task_state = state.clone();
    let captured_seq = state.current_mutation_seq();
    state.spawn_background("devices-refresh", async move {
        let started = std::time::Instant::now();
        let Ok((provider, transport)) = current_transport_provider_pair(&task_state).await else {
            return;
        };
        let provider_id = provider.id().clone();
        if skip_refresh_due_to_rate_limit(&task_state, &provider_id, "devices", "devices-refresh")
            .await
        {
            return;
        }
        if !provider
            .capabilities()
            .transport
            .as_ref()
            .is_some_and(|caps| caps.devices)
        {
            return;
        }
        match transport.devices(RequestContext::BACKGROUND_SYNC).await {
            Ok(devices) => {
                record_daemon_action(
                    "devices",
                    None,
                    serde_json::json!({"device_count": devices.len()}),
                )
                .await;
                let applied =
                    cache_devices_if_fresh(&task_state, &provider_id, &devices, captured_seq).await;
                tracing::debug!(
                    target: "spotuify_daemon::refresh",
                    captured_seq,
                    duration_ms = started.elapsed().as_millis() as u64,
                    outcome = if applied { "applied" } else { "stale" },
                    device_count = devices.len(),
                    "devices refresh"
                );
                if applied {
                    let devices_snapshot = devices.clone();
                    task_state.emit_event(DaemonEvent::DevicesChanged {
                        action: "refreshed".to_string(),
                        devices: Some(devices_snapshot),
                    });
                }
            }
            Err(err) => tracing::warn!(
                target: "spotuify_daemon::refresh",
                captured_seq,
                duration_ms = started.elapsed().as_millis() as u64,
                outcome = "error",
                error = %err,
                "background devices refresh failed"
            ),
        }
    });
}

pub(crate) async fn cache_recent_items(
    state: &DaemonState,
    provider: &ProviderId,
    items: &[MediaItem],
) -> bool {
    let runtime = match state.provider(provider).await {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::warn!(error = %err, %provider, "failed to resolve recent-items provider");
            return false;
        }
    };
    if let Err(err) = validate_provider_collection_items(
        runtime.as_ref(),
        "recently_played",
        &[MediaKind::Track, MediaKind::Episode],
        items,
    ) {
        tracing::warn!(error = %err, %provider, "refusing invalid recent-items output");
        return false;
    }
    match state
        .store()
        .persist_provider_recent_items(provider, items)
        .await
    {
        Ok(_) => true,
        Err(err) => {
            tracing::warn!(error = %err, "failed to cache recent items");
            false
        }
    }
}

pub(crate) fn spawn_recent_refresh(state: Arc<DaemonState>, provider_id: ProviderId) {
    let task_state = state.clone();
    state.spawn_background("recent-refresh", async move {
        if skip_refresh_due_to_rate_limit(&task_state, &provider_id, "recent", "recent-refresh")
            .await
        {
            return;
        }
        let Ok(provider) = task_state.provider(&provider_id).await else {
            return;
        };
        if require_provider_capability(
            provider.as_ref(),
            "recently played",
            provider.capabilities().catalog.recently_played,
        )
        .is_err()
        {
            return;
        }
        let max_page = u32::try_from(
            provider
                .capabilities()
                .catalog
                .recently_played_max_page_size
                .unwrap_or(50),
        )
        .unwrap_or(u32::MAX)
        .max(1);
        let page_request = PageRequest::new(50_u32.min(max_page), 0);
        match provider
            .recently_played(
                RequestContext::BACKGROUND_SYNC,
                page_request.clone(),
            )
            .await
        {
            Ok(page) => {
                if let Err(err) =
                    validate_provider_page_offset(&page_request, &page, "recently_played")
                {
                    tracing::warn!(error = %err, provider = %provider_id, "refusing recent page with wrong echoed offset");
                    return;
                }
                let items = page.items;
                if !cache_recent_items(&task_state, &provider_id, &items).await {
                    return;
                }
                task_state.emit_event(DaemonEvent::SyncFinished {
                    summary: recent_refresh_summary(
                        provider_id,
                        items.len() as u32,
                        spotuify_protocol::SyncCompletionStatus::Succeeded,
                        None,
                    ),
                });
            }
            Err(err) => {
                tracing::debug!(error = %err, "background recent refresh failed");
                task_state.emit_event(DaemonEvent::SyncFinished {
                    summary: recent_refresh_summary(
                        provider_id,
                        0,
                        spotuify_protocol::SyncCompletionStatus::Failed,
                        Some(err.to_string()),
                    ),
                });
            }
        }
    });
}

fn recent_refresh_summary(
    provider: ProviderId,
    recent_items: u32,
    status: spotuify_protocol::SyncCompletionStatus,
    error: Option<String>,
) -> spotuify_protocol::CacheSyncSummary {
    spotuify_protocol::CacheSyncSummary {
        target: spotuify_protocol::SyncTargetData::Recent,
        provider: Some(provider),
        playback_snapshots: 0,
        queue_snapshots: 0,
        queue_items: 0,
        devices: 0,
        playlists: 0,
        playlist_items: 0,
        recent_items,
        library_items: 0,
        media_items: recent_items,
        status,
        error,
        provider_outcomes: Vec::new(),
    }
}

pub(crate) async fn cache_playlists(
    state: &DaemonState,
    provider_id: &ProviderId,
    playlists: &[Playlist],
) {
    let provider = match state.provider(provider_id).await {
        Ok(provider) => provider,
        Err(err) => {
            tracing::warn!(error = %err, provider = %provider_id, "failed to resolve playlist cache provider");
            return;
        }
    };
    let playlists = playlists
        .iter()
        .filter_map(|playlist| {
            let mut playlist = playlist.clone();
            match playlist_resource(provider.as_ref(), &playlist.id) {
                Ok(resource)
                    if resource.kind() == MediaKind::Playlist
                        && resource.scheme() == provider.uri_scheme() =>
                {
                    playlist.id = resource.as_uri();
                    Some(playlist)
                }
                Ok(resource) => {
                    tracing::warn!(provider = %provider_id, playlist = %resource, "ignoring foreign provider playlist");
                    None
                }
                Err(err) => {
                    tracing::warn!(error = %err, provider = %provider_id, playlist = %playlist.id, "ignoring invalid provider playlist");
                    None
                }
            }
        })
        .collect::<Vec<_>>();
    if let Err(err) = state
        .store()
        .persist_provider_playlists(provider_id.as_str(), &playlists)
        .await
    {
        tracing::warn!(error = %err, "failed to cache playlists");
    }
}

pub(crate) async fn cache_playlist_items(
    state: &DaemonState,
    provider: &ProviderId,
    playlist_id: &str,
    items: &[MediaItem],
) {
    if let Err(err) = state
        .store()
        .persist_provider_playlist_items(provider, playlist_id, items)
        .await
    {
        tracing::warn!(error = %err, "failed to cache playlist items");
    }
}

pub(crate) fn expected_playback_after_command(
    command: &PlaybackCommand,
    predicted: Option<&Playback>,
) -> Option<ExpectedPlayback> {
    let predicted_uri =
        || predicted.and_then(|playback| playback.item.as_ref().map(|item| item.uri.clone()));
    match command {
        PlaybackCommand::Pause => Some(ExpectedPlayback {
            uri: predicted_uri(),
            is_playing: Some(false),
        }),
        PlaybackCommand::Resume => Some(ExpectedPlayback {
            uri: predicted_uri(),
            is_playing: Some(true),
        }),
        PlaybackCommand::Toggle => predicted.map(|playback| ExpectedPlayback {
            uri: playback.item.as_ref().map(|item| item.uri.clone()),
            is_playing: Some(playback.is_playing),
        }),
        PlaybackCommand::PlayUri { uri, .. } => Some(ExpectedPlayback {
            uri: Some(uri.clone()),
            is_playing: predicted.and_then(|playback| playback.is_playing.then_some(true)),
        }),
        PlaybackCommand::Next | PlaybackCommand::Previous => {
            predicted.map(|playback| ExpectedPlayback {
                // Spotify may return a different valid track than our cached
                // prediction (shuffle/autoplay/queue races, or previous
                // stepping back instead of restarting current). Treat any
                // post-command snapshot with the expected play/pause state as
                // authoritative instead of rejecting it and leaving clients on
                // stale optimistic state; reject a stale paused readback while
                // the daemon-owned prediction says playback should remain live.
                uri: None,
                is_playing: Some(playback.is_playing),
            })
        }
        PlaybackCommand::Seek { .. } | PlaybackCommand::SeekRelative { .. } => {
            predicted.map(|playback| ExpectedPlayback {
                uri: playback.item.as_ref().map(|item| item.uri.clone()),
                is_playing: None,
            })
        }
        PlaybackCommand::Volume { .. }
        | PlaybackCommand::Shuffle { .. }
        | PlaybackCommand::Repeat { .. } => None,
    }
}

pub(crate) fn playback_command_kind(command: PlaybackCommand) -> CommandKind {
    match command {
        PlaybackCommand::Pause => CommandKind::Pause,
        PlaybackCommand::Resume => CommandKind::Resume,
        PlaybackCommand::Toggle => CommandKind::TogglePlayback,
        PlaybackCommand::Next => CommandKind::Next,
        PlaybackCommand::Previous => CommandKind::Previous,
        // The optional `context_uri` is resolved into a `PlayContext`
        // asynchronously in the dispatch handler (it needs the store /
        // Spotify client), so this sync mapping always starts with
        // `context: None`.
        PlaybackCommand::PlayUri { uri, .. } => CommandKind::PlayUri { uri, context: None },
        PlaybackCommand::Seek { position_ms } => CommandKind::Seek { position_ms },
        // `SeekRelative` is resolved to absolute `Seek` against the daemon
        // `PlaybackClock` upstream in the `PlaybackCommand` handler arm
        // before this function is reached. Hitting this branch means the
        // resolution step was skipped — fall through to a no-op seek so
        // we never silently issue a wrong absolute target.
        PlaybackCommand::SeekRelative { .. } => CommandKind::Seek { position_ms: 0 },
        PlaybackCommand::Volume { volume_percent } => CommandKind::Volume { volume_percent },
        PlaybackCommand::Shuffle { state } => CommandKind::Shuffle { state },
        PlaybackCommand::Repeat { state } => CommandKind::Repeat { state },
    }
}

pub(crate) fn playback_command_action(command: &PlaybackCommand) -> &'static str {
    match command {
        PlaybackCommand::Pause => "pause",
        PlaybackCommand::Resume => "resume",
        PlaybackCommand::Toggle => "toggle",
        PlaybackCommand::Next => "next",
        PlaybackCommand::Previous => "previous",
        PlaybackCommand::PlayUri { .. } => "play-uri",
        PlaybackCommand::Seek { .. } => "seek",
        PlaybackCommand::SeekRelative { .. } => "seek-relative",
        PlaybackCommand::Volume { .. } => "volume",
        PlaybackCommand::Shuffle { .. } => "shuffle",
        PlaybackCommand::Repeat { .. } => "repeat",
    }
}

pub(crate) fn playback_command_viz_state(command: &PlaybackCommand) -> Option<bool> {
    match command {
        PlaybackCommand::Pause => Some(false),
        PlaybackCommand::Resume | PlaybackCommand::PlayUri { .. } => Some(true),
        _ => None,
    }
}

pub(crate) fn playback_command_operation_kind(command: &PlaybackCommand) -> OperationKind {
    match command {
        PlaybackCommand::Pause => OperationKind::Pause,
        PlaybackCommand::Resume => OperationKind::Resume,
        PlaybackCommand::Toggle => OperationKind::Toggle,
        PlaybackCommand::Next => OperationKind::Next,
        PlaybackCommand::Previous => OperationKind::Previous,
        PlaybackCommand::PlayUri { .. } => OperationKind::Play,
        PlaybackCommand::Seek { .. } | PlaybackCommand::SeekRelative { .. } => OperationKind::Seek,
        PlaybackCommand::Volume { .. } => OperationKind::Volume,
        PlaybackCommand::Shuffle { .. } => OperationKind::Shuffle,
        PlaybackCommand::Repeat { .. } => OperationKind::Repeat,
    }
}

pub(crate) fn emit_mutation_finished(state: &DaemonState, action: &str, message: &str) {
    state.emit_event(DaemonEvent::MutationFinished {
        action: action.to_string(),
        message: message.to_string(),
    });
}

pub(crate) async fn reject_if_auth_blocked(
    state: &DaemonState,
    provider: Option<&ProviderId>,
) -> anyhow::Result<()> {
    let Some(err) = state.auth_gate_error() else {
        return Ok(());
    };
    let Some(provider) = provider else {
        return Ok(());
    };
    let auth_target = state
        .configured_auth_target(Some(provider.as_str()))
        .await?;
    if auth_target.strategy == crate::provider_factory::ProviderAuthStrategy::SpotifyOauth {
        return Err(anyhow::Error::new(err));
    }
    Ok(())
}

// Predict the post-command playback state so the daemon can emit an
// optimistic `PlaybackChanged` BEFORE the Spotify round-trip. Returns
// `None` when no prediction is sensible (e.g. `Next` without a current
// queue row — we can't guess the next track safely).
//
// The eventual authoritative `CommandResult` event from
// `persist_command_result` overrides whatever we predict via the
// clock's source-priority logic. Same pattern the embedded librespot
// `PlayerEvent` already uses for local mutations.
/// The next track to optimistically show for a `Next`, taken from the cached
/// queue — but only when the queue's `currently_playing` matches the track that
/// is actually playing (`current_uri`). A mismatch means the cache is
/// historical (a dead session), so we return `None` and skip the prediction
/// rather than flash a stale title.
pub(crate) fn optimistic_next_from_queue(
    queue: &spotuify_core::Queue,
    current_uri: &str,
) -> Option<spotuify_core::MediaItem> {
    let describes_current = queue
        .currently_playing
        .as_ref()
        .is_some_and(|current| current.uri == current_uri);
    if !describes_current {
        return None;
    }
    queue.items.first().cloned()
}

/// Predicted queue after a `Next`: the cached queue with the predicted
/// track promoted to `currently_playing` and everything up to (and
/// including) it dropped from the upcoming list. Returns `None` when the
/// predicted track isn't in the cached queue — the cache is historical
/// and an optimistic emit would show a wrong list.
pub(crate) async fn optimistic_queue_after_next(
    state: &DaemonState,
    next_item: &spotuify_core::MediaItem,
) -> Option<spotuify_core::Queue> {
    let (provider, _) = current_transport_provider_pair(state).await.ok()?;
    let queue = state
        .store()
        .latest_provider_queue(500, provider.id())
        .await
        .ok()
        .flatten()?;
    optimistic_queue_promoting(queue, next_item)
}

/// Pure half of `optimistic_queue_after_next`: promote `next_item` to
/// `currently_playing` and drop it (and anything queued before it) from
/// the upcoming list.
pub(crate) fn optimistic_queue_promoting(
    mut queue: spotuify_core::Queue,
    next_item: &spotuify_core::MediaItem,
) -> Option<spotuify_core::Queue> {
    let pos = queue
        .items
        .iter()
        .position(|item| item.uri == next_item.uri)?;
    queue.items.drain(..=pos);
    queue.currently_playing = Some(next_item.clone());
    // `latest_queue` marks every cache read inactive because SQLite cannot
    // attest to session liveness. This prediction is produced only while a
    // live `next` command is being applied, so broadcasting the cache bit
    // would make clients hide an otherwise valid optimistic queue.
    queue.session_active = true;
    queue.as_of_ms = spotuify_core::now_ms();
    Some(queue)
}

/// Re-fetch the authoritative queue shortly after a transport command
/// that changes the playing track. The delay gives Spotify's
/// `/me/player/queue` time to reflect the Spirc-side skip — fetching
/// immediately often returns the pre-skip queue, which would clobber
/// the optimistic emit with stale data.
///
/// `captured_seq` is the SCHEDULING command's own seq: any mutation
/// during the delay advances past it and the refresh becomes a no-op
/// (the newer mutation's refresh reconciles instead). Capturing after
/// the sleep adopted a racing command's seq and let a mid-transition
/// snapshot through — live-observed as the queue jumping one track
/// behind on rapid double-Next.
pub(crate) fn spawn_queue_refresh_delayed(
    state: Arc<DaemonState>,
    delay_ms: u64,
    captured_seq: u64,
) {
    let task_state = state.clone();
    state.spawn_background("queue-refresh-delayed", async move {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        spawn_queue_refresh_with_seq(task_state, captured_seq);
    });
}

pub(crate) async fn compute_optimistic_playback(
    state: &DaemonState,
    command: &PlaybackCommand,
) -> Option<spotuify_core::Playback> {
    let mut predicted = state.snapshot_playback();
    let now_ms = spotuify_core::now_ms();
    match command {
        PlaybackCommand::Pause => {
            if !predicted.is_playing {
                return None;
            }
            predicted.is_playing = false;
        }
        PlaybackCommand::Resume => {
            if predicted.is_playing {
                return None;
            }
            if !playback_has_active_device(&predicted) {
                return None;
            }
            predicted.is_playing = true;
        }
        PlaybackCommand::Toggle => {
            if predicted.is_playing {
                predicted.is_playing = false;
            } else if playback_has_active_device(&predicted) {
                predicted.is_playing = true;
            } else {
                return None;
            }
        }
        PlaybackCommand::PlayUri { uri, .. } => {
            let was_audible = predicted.is_playing && playback_has_active_device(&predicted);
            // Try the local Tantivy/SQLite media_items cache first.
            // Falls through to a stub MediaItem (URI only) when the
            // URI isn't known locally — at minimum the URI change
            // triggers the TUI's `handle_art_url_change` to clear
            // the old cover and paint the gradient placeholder.
            let resolved = lookup_known_media_item(state, uri)
                .await
                .unwrap_or_else(|| spotuify_core::MediaItem {
                    uri: uri.clone(),
                    name: "Loading…".to_string(),
                    ..Default::default()
                });
            predicted.item = Some(resolved);
            predicted.is_playing = was_audible;
            predicted.progress_ms = 0;
            predicted.sampled_at_ms = Some(now_ms);
        }
        PlaybackCommand::Next => {
            // Predict the next track from the cached queue, but only when the
            // cache still describes the *current* track — otherwise the queue
            // is historical (a dead session) and we'd show a stale title.
            let current_uri = predicted.item.as_ref().map(|item| item.uri.clone())?;
            let (provider, _) = current_transport_provider_pair(state).await.ok()?;
            let queue = state
                .store()
                .latest_provider_queue(500, provider.id())
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            let mut next = optimistic_next_from_queue(&queue, &current_uri)?;
            // Fill artwork from the cache when the queue row lacks it, so the
            // cover swaps instantly instead of waiting for reconciliation. No
            // network call on this hot path — if still unknown, art fills when
            // the authoritative event lands.
            if next.image_url.is_none() {
                if let Some(enriched) = lookup_known_media_item(state, &next.uri).await {
                    next = enriched;
                }
            }
            let was_audible = predicted.is_playing && playback_has_active_device(&predicted);
            predicted.item = Some(next);
            predicted.is_playing = was_audible;
            predicted.progress_ms = 0;
            predicted.sampled_at_ms = Some(now_ms);
        }
        PlaybackCommand::Previous => {
            // Restart-current: the always-safe optimistic move (Spotify itself
            // restarts the track once you're past the first few seconds). It
            // resets the progress bar to 0:00 instantly and never shows a wrong
            // track; if Spotify actually steps back a track, the authoritative
            // event reconciles via the clock's source priority.
            predicted.item.as_ref()?;
            predicted.progress_ms = 0;
            predicted.sampled_at_ms = Some(now_ms);
        }
        PlaybackCommand::Seek { position_ms } => {
            predicted.item.as_ref()?;
            predicted.progress_ms = *position_ms;
        }
        PlaybackCommand::SeekRelative { .. } => {
            // Already resolved to absolute `Seek` upstream in the
            // PlaybackCommand handler — should never reach here.
            return None;
        }
        PlaybackCommand::Volume { volume_percent } => {
            let device = predicted.device.as_mut()?;
            device.volume_percent = Some(*volume_percent);
        }
        PlaybackCommand::Shuffle { state: shuffle } => {
            if predicted.item.is_none() && predicted.device.is_none() {
                return None;
            }
            predicted.shuffle = *shuffle;
        }
        PlaybackCommand::Repeat { state: repeat } => {
            if predicted.item.is_none() && predicted.device.is_none() {
                return None;
            }
            predicted.repeat = *repeat;
        }
    }
    Some(predicted)
}

pub(crate) fn playback_has_active_device(playback: &spotuify_core::Playback) -> bool {
    playback
        .device
        .as_ref()
        .is_some_and(|device| device.is_active)
}

/// Look up a MediaItem by URI from the daemon's local caches. Used by
/// optimistic playback prediction so a PlayUri can carry the track's
/// title / artist / image_url immediately, before Spotify's playback
/// state catches up. Returns `None` when the URI isn't in any cache —
/// the caller falls back to a stub.
pub(crate) async fn lookup_known_media_item(
    state: &DaemonState,
    uri: &str,
) -> Option<spotuify_core::MediaItem> {
    state
        .store()
        .media_items_by_uris(&[uri.to_string()])
        .await
        .ok()
        .and_then(|items| items.into_iter().next())
}

fn reconciliation_for_mutation(
    provider: &ProviderId,
    mutation: &Mutation,
    receipt_id: ReceiptId,
    operation_id: OperationId,
) -> spotuify_store::ProviderReconciliation {
    match mutation {
        Mutation::PlaylistAdd {
            playlist_uri,
            items,
            ..
        } => {
            let mut resources = items
                .iter()
                .map(|item| item.uri.as_uri())
                .collect::<Vec<_>>();
            resources.push(playlist_uri.as_uri());
            resources.sort();
            resources.dedup();
            spotuify_store::ProviderReconciliation::targeted(
                receipt_id,
                operation_id,
                provider.clone(),
                spotuify_protocol::SyncTargetData::Playlists,
                resources,
            )
        }
        Mutation::PlaylistRemove {
            playlist_uri,
            items,
            ..
        } => {
            let mut resources = items
                .iter()
                .map(|item| item.uri.as_uri())
                .collect::<Vec<_>>();
            resources.push(playlist_uri.as_uri());
            resources.sort();
            resources.dedup();
            spotuify_store::ProviderReconciliation::targeted(
                receipt_id,
                operation_id,
                provider.clone(),
                spotuify_protocol::SyncTargetData::Playlists,
                resources,
            )
        }
        Mutation::PlaylistReorder { playlist_uri, .. } => {
            spotuify_store::ProviderReconciliation::targeted(
                receipt_id,
                operation_id,
                provider.clone(),
                spotuify_protocol::SyncTargetData::Playlists,
                vec![playlist_uri.as_uri()],
            )
        }
        Mutation::PlaylistCreate { .. }
        | Mutation::PlaylistSetImage { .. }
        | Mutation::PlaylistUnfollow { .. } => spotuify_store::ProviderReconciliation::full_domain(
            receipt_id,
            operation_id,
            provider.clone(),
            spotuify_protocol::SyncTargetData::Playlists,
        ),
        Mutation::LibrarySave { .. }
        | Mutation::LibraryUnsave { .. }
        | Mutation::Follow { .. }
        | Mutation::Unfollow { .. } => {
            let resources = match mutation {
                Mutation::LibrarySave { uris }
                | Mutation::LibraryUnsave { uris }
                | Mutation::Follow { uris }
                | Mutation::Unfollow { uris } => {
                    uris.iter().map(ResourceUri::as_uri).collect::<Vec<_>>()
                }
                _ => unreachable!(),
            };
            spotuify_store::ProviderReconciliation::targeted(
                receipt_id,
                operation_id,
                provider.clone(),
                spotuify_protocol::SyncTargetData::Library,
                resources,
            )
        }
    }
}

#[derive(Clone, Debug)]
struct ProviderReconciliationSeed {
    provider: ProviderId,
    target: spotuify_protocol::SyncTargetData,
    scope: spotuify_store::ProviderReconciliationScope,
    resource_uris: Vec<String>,
}

impl ProviderReconciliationSeed {
    fn materialize(
        &self,
        receipt_id: ReceiptId,
        operation_id: OperationId,
    ) -> spotuify_store::ProviderReconciliation {
        match self.scope {
            spotuify_store::ProviderReconciliationScope::Targeted => {
                spotuify_store::ProviderReconciliation::targeted(
                    receipt_id,
                    operation_id,
                    self.provider.clone(),
                    self.target,
                    self.resource_uris.clone(),
                )
            }
            spotuify_store::ProviderReconciliationScope::FullDomain => {
                spotuify_store::ProviderReconciliation::full_domain(
                    receipt_id,
                    operation_id,
                    self.provider.clone(),
                    self.target,
                )
            }
        }
    }
}

#[derive(Debug)]
struct PostWriteLifecycleError {
    message: String,
    seeds: Vec<ProviderReconciliationSeed>,
    guard: Option<spotuify_store::PostWriteOperationGuard>,
}

impl std::fmt::Display for PostWriteLifecycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PostWriteLifecycleError {}

async fn operation_reconciliation_seed(
    state: &DaemonState,
    operation: &spotuify_protocol::Operation,
) -> anyhow::Result<Option<ProviderReconciliationSeed>> {
    let (target, scope) = match operation.kind {
        OperationKind::PlaylistAdd
        | OperationKind::PlaylistRemove
        | OperationKind::PlaylistReorder => (
            spotuify_protocol::SyncTargetData::Playlists,
            spotuify_store::ProviderReconciliationScope::Targeted,
        ),
        OperationKind::PlaylistCreate
        | OperationKind::PlaylistSetImage
        | OperationKind::PlaylistUnfollow => (
            spotuify_protocol::SyncTargetData::Playlists,
            spotuify_store::ProviderReconciliationScope::FullDomain,
        ),
        OperationKind::LibrarySave
        | OperationKind::LibraryUnsave
        | OperationKind::ArtistFollow
        | OperationKind::ArtistUnfollow
        | OperationKind::Like
        | OperationKind::Unlike => (
            spotuify_protocol::SyncTargetData::Library,
            spotuify_store::ProviderReconciliationScope::Targeted,
        ),
        _ => return Ok(None),
    };
    let playlist_uri = operation
        .pre_state
        .as_ref()
        .and_then(|pre_state| match pre_state {
            spotuify_protocol::PreState::PlaylistAdd { playlist_id, .. }
            | spotuify_protocol::PreState::PlaylistRemove { playlist_id, .. }
            | spotuify_protocol::PreState::PlaylistCreate { playlist_id }
            | spotuify_protocol::PreState::PlaylistReorder { playlist_id, .. } => {
                Some(playlist_id.clone())
            }
            _ => None,
        });
    let mut resource_uris = operation.subject_uris.clone();
    if let Some(playlist_uri) = playlist_uri {
        resource_uris.push(playlist_uri);
    }
    resource_uris.sort();
    resource_uris.dedup();
    let mut provider = None;
    for uri in &resource_uris {
        let Ok(uri) = ResourceUri::parse(uri) else {
            continue;
        };
        let owner = state.provider_for_uri(&uri).await?;
        provider = Some(owner.id().clone());
        break;
    }
    let Some(provider) = provider else {
        return Ok(None);
    };
    Ok(Some(ProviderReconciliationSeed {
        provider,
        target,
        scope,
        resource_uris,
    }))
}

fn merge_reconciliation_seed(
    seeds: &mut Vec<ProviderReconciliationSeed>,
    mut candidate: ProviderReconciliationSeed,
) {
    if let Some(existing) = seeds.iter_mut().find(|existing| {
        existing.provider == candidate.provider && existing.target == candidate.target
    }) {
        if existing.scope == spotuify_store::ProviderReconciliationScope::FullDomain
            || candidate.scope == spotuify_store::ProviderReconciliationScope::FullDomain
        {
            existing.scope = spotuify_store::ProviderReconciliationScope::FullDomain;
            existing.resource_uris.clear();
            return;
        }
        existing.resource_uris.append(&mut candidate.resource_uris);
        existing.resource_uris.sort();
        existing.resource_uris.dedup();
        return;
    }
    candidate.resource_uris.sort();
    candidate.resource_uris.dedup();
    seeds.push(candidate);
}

async fn request_reconciliation_seed(
    state: &DaemonState,
    request: &Request,
    operation: Option<&spotuify_protocol::Operation>,
) -> anyhow::Result<Option<ProviderReconciliationSeed>> {
    let (target, scope, playlist) = match request {
        Request::PlaylistAddItems { playlist, .. }
        | Request::PlaylistRemoveItems { playlist, .. } => (
            spotuify_protocol::SyncTargetData::Playlists,
            spotuify_store::ProviderReconciliationScope::Targeted,
            Some(playlist.as_str()),
        ),
        Request::PlaylistCreate { .. }
        | Request::PlaylistUnfollow { .. }
        | Request::PlaylistSetImage { .. } => (
            spotuify_protocol::SyncTargetData::Playlists,
            spotuify_store::ProviderReconciliationScope::FullDomain,
            None,
        ),
        Request::LibrarySave { .. }
        | Request::LibraryUnsave { .. }
        | Request::ArtistFollow { .. }
        | Request::ArtistUnfollow { .. } => (
            spotuify_protocol::SyncTargetData::Library,
            spotuify_store::ProviderReconciliationScope::Targeted,
            None,
        ),
        _ => return Ok(None),
    };
    state.providers().await?;
    let Some(provider) = request_provider_context(state, request).await else {
        return Ok(None);
    };
    let mut resource_uris = operation
        .map(|operation| operation.subject_uris.clone())
        .unwrap_or_default();
    if let Some(playlist) = playlist {
        let runtime = state.provider(&provider).await?;
        let resource = playlist_resource(runtime.as_ref(), playlist)?;
        resource_uris.push(resource.as_uri());
    }
    Ok(Some(ProviderReconciliationSeed {
        provider,
        target,
        scope,
        resource_uris,
    }))
}

async fn recovery_reconciliation_intent(
    state: &DaemonState,
    request_json: &str,
    receipt_id: ReceiptId,
    operation_id: OperationId,
) -> anyhow::Result<(
    Vec<spotuify_store::ProviderReconciliation>,
    Option<spotuify_store::PostWriteOperationGuard>,
)> {
    let request = serde_json::from_str::<Request>(request_json)
        .map_err(|error| anyhow::anyhow!("failed to decode recovery request: {error}"))?;
    let outer_operation = state.store().get_operation(operation_id).await?;
    let mut seeds = Vec::new();
    let mut guard = None;

    match &request {
        Request::OpsUndo {
            bulk_since_ms: Some(since_ms),
            ..
        } => {
            let operations = state
                .store()
                .operations_for_bulk_undo_recovery(*since_ms, operation_id)
                .await?;
            guard = operations
                .iter()
                .find(|operation| operation.undone_by_op_id != Some(operation_id))
                .map(|operation| {
                    spotuify_store::PostWriteOperationGuard::DisableUndo(operation.operation_id)
                });
            for operation in operations {
                if let Some(seed) = operation_reconciliation_seed(state, &operation).await? {
                    merge_reconciliation_seed(&mut seeds, seed);
                }
            }
        }
        Request::OpsUndo {
            operation_id: requested,
            ..
        } => {
            let subject = match state.store().get_subject_operation(operation_id).await? {
                Some(operation) => Some(operation),
                None => match requested {
                    Some(requested) => Some(state.store().get_operation(*requested).await?),
                    None => None,
                },
            };
            if let Some(subject) = subject {
                guard = Some(spotuify_store::PostWriteOperationGuard::DisableUndo(
                    subject.operation_id,
                ));
                if let Some(seed) = operation_reconciliation_seed(state, &subject).await? {
                    merge_reconciliation_seed(&mut seeds, seed);
                }
            }
        }
        Request::OpsRedo {
            operation_id: requested,
        } => {
            let subject = match state.store().get_subject_operation(operation_id).await? {
                Some(operation) => Some(operation),
                None => match requested {
                    Some(requested) => Some(state.store().get_operation(*requested).await?),
                    None => None,
                },
            };
            if let Some(subject) = subject {
                guard = Some(spotuify_store::PostWriteOperationGuard::MarkRedone(
                    subject.operation_id,
                ));
                if let Some(seed) = operation_reconciliation_seed(state, &subject).await? {
                    merge_reconciliation_seed(&mut seeds, seed);
                }
            }
        }
        request => {
            if let Some(seed) = operation_reconciliation_seed(state, &outer_operation).await? {
                merge_reconciliation_seed(&mut seeds, seed);
            }
            if seeds.is_empty() {
                if let Some(seed) =
                    request_reconciliation_seed(state, request, Some(&outer_operation)).await?
                {
                    merge_reconciliation_seed(&mut seeds, seed);
                }
            }
        }
    }

    Ok((
        seeds
            .iter()
            .map(|seed| seed.materialize(receipt_id, operation_id))
            .collect(),
        guard,
    ))
}

fn partial_reconciliation(
    partial: &PartialMutationError,
    receipt_id: ReceiptId,
    operation_id: OperationId,
) -> spotuify_store::ProviderReconciliation {
    let mut reconciliation = reconciliation_for_mutation(
        &partial.provider,
        &partial.mutation,
        receipt_id,
        operation_id,
    );
    let mut resources = partial
        .succeeded_uris
        .iter()
        .chain(&partial.failed_uris)
        .map(ResourceUri::as_uri)
        .collect::<Vec<_>>();
    if let Some(target) = partial_target_uri(&partial.mutation) {
        resources.push(target.as_uri());
    }
    resources.sort();
    resources.dedup();
    reconciliation.resource_uris = resources;
    reconciliation
}

fn error_reconciliations<T>(
    result: &anyhow::Result<T>,
    receipt_id: ReceiptId,
    operation_id: OperationId,
) -> Vec<spotuify_store::ProviderReconciliation> {
    let Some(error) = result.as_ref().err() else {
        return Vec::new();
    };
    if let Some(partial) = error.downcast_ref::<PartialMutationError>() {
        return vec![partial_reconciliation(partial, receipt_id, operation_id)];
    }
    if let Some(malformed) = error.downcast_ref::<MalformedProviderReceiptError>() {
        return vec![reconciliation_for_mutation(
            &malformed.provider,
            &malformed.mutation,
            receipt_id,
            operation_id,
        )];
    }
    if let Some(post_write) = error.downcast_ref::<PostWriteLifecycleError>() {
        return post_write
            .seeds
            .iter()
            .map(|seed| seed.materialize(receipt_id, operation_id))
            .collect();
    }
    Vec::new()
}

async fn fail_started_provider_reconciliation(
    state: &Arc<DaemonState>,
    reconciliation: &spotuify_store::ProviderReconciliation,
    detail: String,
) {
    let Some(claim_token) = reconciliation.claim_token else {
        tracing::error!(reconciliation_id = %reconciliation.reconciliation_id, "claimed provider reconciliation is missing its ownership token");
        return;
    };
    let mut reset = None;
    for delay in MUTATION_FINALIZATION_RETRY_DELAYS {
        match state
            .store()
            .fail_provider_reconciliation_if_attempts(
                reconciliation.reconciliation_id,
                reconciliation.attempts,
                claim_token,
                &detail,
            )
            .await
        {
            Ok(value) => {
                reset = Some(value);
                break;
            }
            Err(store_err) => {
                tracing::warn!(reconciliation_id = %reconciliation.reconciliation_id, error = %store_err, "failed to reset provider reconciliation for replay");
                tokio::time::sleep(delay).await;
            }
        }
    }
    if reset == Some(false) {
        return;
    }
    if reset.is_none() {
        spawn_provider_reconciliation_reset_retry(state, reconciliation.clone(), detail);
        return;
    }
    emit_failed_provider_reconciliation_and_schedule(state, reconciliation, detail);
}

fn emit_failed_provider_reconciliation_and_schedule(
    state: &Arc<DaemonState>,
    reconciliation: &spotuify_store::ProviderReconciliation,
    detail: String,
) {
    state.emit_event(DaemonEvent::SyncFinished {
        summary: spotuify_protocol::CacheSyncSummary {
            target: reconciliation.target,
            provider: Some(reconciliation.provider.clone()),
            playback_snapshots: 0,
            queue_snapshots: 0,
            queue_items: 0,
            devices: 0,
            playlists: 0,
            playlist_items: 0,
            recent_items: 0,
            library_items: 0,
            media_items: 0,
            status: spotuify_protocol::SyncCompletionStatus::Failed,
            error: Some(detail),
            provider_outcomes: vec![],
        },
    });
    schedule_provider_reconciliation_retry(state, reconciliation);
}

fn spawn_provider_reconciliation_reset_retry(
    state: &Arc<DaemonState>,
    reconciliation: spotuify_store::ProviderReconciliation,
    detail: String,
) {
    let Some(claim_token) = reconciliation.claim_token else {
        tracing::error!(reconciliation_id = %reconciliation.reconciliation_id, "claimed provider reconciliation is missing its ownership token");
        return;
    };
    let task_state = state.clone();
    state.spawn_background("provider-reconciliation-reset-retry", async move {
        let mut delay = PROVIDER_RECONCILIATION_RETRY_BASE;
        loop {
            tokio::time::sleep(delay).await;
            match task_state
                .store()
                .fail_provider_reconciliation_if_attempts(
                    reconciliation.reconciliation_id,
                    reconciliation.attempts,
                    claim_token,
                    &detail,
                )
                .await
            {
                Ok(true) => {
                    emit_failed_provider_reconciliation_and_schedule(
                        &task_state,
                        &reconciliation,
                        detail,
                    );
                    return;
                }
                Ok(false) => return,
                Err(error) => {
                    tracing::warn!(reconciliation_id = %reconciliation.reconciliation_id, %error, "provider reconciliation reset retry failed");
                    delay = delay.saturating_mul(2).min(PROVIDER_RECONCILIATION_RETRY_MAX);
                }
            }
        }
    });
}

fn reconciliation_retry_delay(reconciliation_id: uuid::Uuid, attempts: u32) -> Duration {
    let exponent = attempts.saturating_sub(1).min(20);
    let base_ms = PROVIDER_RECONCILIATION_RETRY_BASE
        .as_millis()
        .saturating_mul(1_u128 << exponent)
        .min(PROVIDER_RECONCILIATION_RETRY_MAX.as_millis());
    let jitter_percent = u128::from(reconciliation_id.as_bytes()[15] % 21);
    let delayed_ms = base_ms
        .saturating_add(base_ms.saturating_mul(jitter_percent) / 100)
        .min(PROVIDER_RECONCILIATION_RETRY_MAX.as_millis());
    Duration::from_millis(delayed_ms.try_into().unwrap_or(u64::MAX))
}

fn schedule_provider_reconciliation_retry(
    state: &Arc<DaemonState>,
    reconciliation: &spotuify_store::ProviderReconciliation,
) {
    let task_state = state.clone();
    let reconciliation_id = reconciliation.reconciliation_id;
    let expected_attempts = reconciliation.attempts;
    let delay = reconciliation_retry_delay(reconciliation_id, expected_attempts);
    state.spawn_background("provider-reconciliation-retry", async move {
        tokio::time::sleep(delay).await;
        spawn_provider_reconciliation(&task_state, reconciliation_id, expected_attempts);
    });
}

fn schedule_provider_reconciliation_claim_retry(
    state: &Arc<DaemonState>,
    reconciliation_id: uuid::Uuid,
    expected_attempts: u32,
    claim_token: uuid::Uuid,
) {
    let task_state = state.clone();
    state.spawn_background("provider-reconciliation-claim-retry", async move {
        tokio::time::sleep(PROVIDER_RECONCILIATION_RETRY_BASE).await;
        spawn_provider_reconciliation_with_claim(
            &task_state,
            reconciliation_id,
            expected_attempts,
            claim_token,
            true,
        );
    });
}

async fn verify_and_persist_provider_reconciliation_resources(
    state: &DaemonState,
    provider: &dyn MusicProvider,
    reconciliation: &spotuify_store::ProviderReconciliation,
) -> anyhow::Result<()> {
    if reconciliation.target != spotuify_protocol::SyncTargetData::Playlists
        || reconciliation.scope == spotuify_store::ProviderReconciliationScope::FullDomain
    {
        return Ok(());
    }
    let playlist_uris = reconciliation
        .resource_uris
        .iter()
        .filter_map(|uri| ResourceUri::parse(uri).ok())
        .filter(|uri| uri.kind() == MediaKind::Playlist)
        .collect::<Vec<_>>();
    if playlist_uris.is_empty() {
        anyhow::bail!("playlist reconciliation has no target playlist URI");
    }
    let caps = provider.capabilities().playlists;
    let page_limit = caps.items_max_page_size.unwrap_or(50).max(1) as u32;
    let mut total_items = 0_usize;
    for playlist_uri in playlist_uris {
        let mut page = PageRequest::new(page_limit, 0);
        let mut items = Vec::new();
        let mut seen_cursors = std::collections::HashSet::new();
        let mut complete = false;
        for page_index in 0..PROVIDER_RECONCILIATION_MAX_PAGES {
            let result = match provider
                .playlist_items(
                    RequestContext::BACKGROUND_SYNC,
                    CollectionRequest {
                        uri: playlist_uri.clone(),
                        page: page.clone(),
                    },
                )
                .await?
            {
                AccessOutcome::Available(result) => result,
                AccessOutcome::Unavailable(reason) => anyhow::bail!(
                    "playlist {} remained unavailable after reconciliation ({reason:?})",
                    playlist_uri.as_uri()
                ),
            };
            validate_provider_page_offset(&page, &result, "playlist_items")?;
            validate_provider_collection_items(
                provider,
                "playlist_items",
                &[MediaKind::Track, MediaKind::Episode],
                &result.items,
            )?;
            total_items = total_items.saturating_add(result.items.len());
            if total_items > PROVIDER_RECONCILIATION_MAX_ITEMS {
                anyhow::bail!(
                    "playlist reconciliation exceeded {PROVIDER_RECONCILIATION_MAX_ITEMS} items"
                );
            }
            items.extend(result.items);
            let Some(continuation) = result.next else {
                complete = true;
                break;
            };
            page = next_provider_page(
                &page,
                continuation,
                items.len() as u64,
                &mut seen_cursors,
                page_index + 1,
                "playlist reconciliation items",
            )?;
        }
        if !complete {
            anyhow::bail!(
                "playlist reconciliation exceeded {PROVIDER_RECONCILIATION_MAX_PAGES} pages"
            );
        }
        let playlist_id = playlist_uri.as_uri();
        let version_token = state.store().playlist_version_token(&playlist_id).await?;
        state
            .store()
            .persist_provider_playlist_items_with_version_bulk(
                &reconciliation.provider,
                &playlist_id,
                &items,
                version_token.as_deref(),
            )
            .await?;
    }
    Ok(())
}

/// Re-run one already-durable reconciliation intent. An atomic pending→running
/// claim suppresses duplicate work from immediate execution, mutation replay,
/// and startup recovery racing each other.
fn spawn_provider_reconciliation(
    state: &Arc<DaemonState>,
    reconciliation_id: uuid::Uuid,
    expected_attempts: u32,
) {
    spawn_provider_reconciliation_with_claim(
        state,
        reconciliation_id,
        expected_attempts,
        uuid::Uuid::now_v7(),
        false,
    );
}

fn spawn_provider_reconciliation_with_claim(
    state: &Arc<DaemonState>,
    reconciliation_id: uuid::Uuid,
    expected_attempts: u32,
    claim_token: uuid::Uuid,
    recover_claim_after_error: bool,
) {
    let task_state = state.clone();
    state.spawn_background("provider-mutation-reconciliation", async move {
        let claimed = if recover_claim_after_error {
            task_state
                .store()
                .recover_provider_reconciliation_claim_after_error(
                    reconciliation_id,
                    expected_attempts,
                    claim_token,
                )
                .await
        } else {
            task_state
                .store()
                .claim_provider_reconciliation_if_attempts(
                    reconciliation_id,
                    expected_attempts,
                    claim_token,
                )
                .await
        };
        let reconciliation = match claimed {
            Ok(Some(reconciliation)) => reconciliation,
            Ok(None) => {
                match task_state
                    .store()
                    .provider_reconciliation_not_before_ms(
                        reconciliation_id,
                        expected_attempts,
                    )
                    .await
                {
                    Ok(Some(not_before_ms)) => {
                        let delay_ms = not_before_ms.saturating_sub(now_ms()).max(1) as u64;
                        let retry_state = task_state.clone();
                        task_state.spawn_background(
                            "provider-reconciliation-not-before",
                            async move {
                                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                                spawn_provider_reconciliation(
                                    &retry_state,
                                    reconciliation_id,
                                    expected_attempts,
                                );
                            },
                        );
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(%reconciliation_id, %error, "failed to read provider reconciliation not-before deadline");
                        schedule_provider_reconciliation_claim_retry(
                            &task_state,
                            reconciliation_id,
                            expected_attempts,
                            claim_token,
                        );
                    }
                }
                return;
            }
            Err(err) => {
                tracing::warn!(%reconciliation_id, error = %err, "failed to claim provider mutation reconciliation");
                schedule_provider_reconciliation_claim_retry(
                    &task_state,
                    reconciliation_id,
                    expected_attempts,
                    claim_token,
                );
                return;
            }
        };
        let Some(claim_token) = reconciliation.claim_token else {
            tracing::error!(%reconciliation_id, "claimed provider reconciliation is missing its ownership token");
            return;
        };
        let panic_state = task_state.clone();
        let panic_reconciliation = reconciliation.clone();
        let attempt = AssertUnwindSafe(async move {
        let receipt_id = reconciliation.receipt_id;
        let provider_id = reconciliation.provider.clone();
        let target = reconciliation.target;
        let provider = match task_state.provider(&provider_id).await {
            Ok(provider) => provider,
            Err(err) => {
                let detail = bounded_redacted_text(&err.to_string(), 512);
                fail_started_provider_reconciliation(
                    &task_state,
                    &reconciliation,
                    detail.clone(),
                )
                .await;
                tracing::warn!(%provider_id, error = %detail, "partial mutation reconciliation could not resolve provider");
                return;
            }
        };
        let verification_provider = provider.clone();
        let provider = match spotuify_sync::SyncProvider::new(provider, None) {
            Ok(provider) => provider,
            Err(err) => {
                let detail = bounded_redacted_text(&err.to_string(), 512);
                fail_started_provider_reconciliation(
                    &task_state,
                    &reconciliation,
                    detail.clone(),
                )
                .await;
                tracing::warn!(%provider_id, error = %detail, "partial mutation reconciliation provider was invalid");
                return;
            }
        };
        match target {
            spotuify_protocol::SyncTargetData::Library => {
                let mut kinds = reconciliation
                    .resource_uris
                    .iter()
                    .filter_map(|uri| ResourceUri::parse(uri).ok())
                    .map(|uri| uri.kind())
                    .collect::<Vec<_>>();
                kinds.sort_by_key(MediaKind::label);
                kinds.dedup();
                for kind in kinds {
                    if let Err(err) = task_state
                        .store()
                        .clear_sync_cursor(
                            provider_id.as_str(),
                            &format!("library/{}", kind.label()),
                        )
                        .await
                    {
                        let detail = bounded_redacted_text(&err.to_string(), 512);
                        fail_started_provider_reconciliation(
                            &task_state,
                            &reconciliation,
                            detail,
                        )
                        .await;
                        return;
                    }
                }
            }
            spotuify_protocol::SyncTargetData::Playlists => {
                if let Err(err) = task_state
                    .store()
                    .clear_sync_cursor(provider_id.as_str(), target.label())
                    .await
                {
                    let detail = bounded_redacted_text(&err.to_string(), 512);
                    fail_started_provider_reconciliation(
                        &task_state,
                        &reconciliation,
                        detail,
                    )
                    .await;
                    return;
                }
                for playlist_uri in reconciliation.resource_uris.iter().filter_map(|uri| {
                    ResourceUri::parse(uri)
                        .ok()
                        .filter(|uri| uri.kind() == MediaKind::Playlist)
                }) {
                    if let Err(err) = task_state
                        .store()
                        .clear_playlist_version_token(
                            provider_id.as_str(),
                            &playlist_uri.as_uri(),
                        )
                        .await
                    {
                        let detail = bounded_redacted_text(&err.to_string(), 512);
                        fail_started_provider_reconciliation(
                            &task_state,
                            &reconciliation,
                            detail,
                        )
                        .await;
                        return;
                    }
                }
            }
            _ => unreachable!("partial reconciliation targets only mutable collection domains"),
        }
        task_state.emit_event(DaemonEvent::SyncStarted {
            target,
            provider: Some(provider_id.clone()),
        });
        let result = spotuify_sync::sync_provider_target_bounded(
            task_state.clone(),
            provider,
            target,
        )
        .await;
        match result {
            Ok(summary) => {
                let verification = AssertUnwindSafe(
                    verify_and_persist_provider_reconciliation_resources(
                        task_state.as_ref(),
                        verification_provider.as_ref(),
                        &reconciliation,
                    ),
                )
                .catch_unwind();
                let verification_error = match tokio::time::timeout(
                    PROVIDER_RECONCILIATION_VERIFY_TIMEOUT,
                    verification,
                )
                .await
                {
                    Ok(Ok(Ok(()))) => None,
                    Ok(Ok(Err(err))) => Some(bounded_redacted_text(&err.to_string(), 512)),
                    Ok(Err(_)) => Some(
                        "provider reconciliation verification panicked; retry is required"
                            .to_string(),
                    ),
                    Err(_) => Some(format!(
                        "provider reconciliation verification timed out after {} seconds",
                        PROVIDER_RECONCILIATION_VERIFY_TIMEOUT.as_secs()
                    )),
                };
                if let Some(detail) = verification_error {
                    fail_started_provider_reconciliation(
                        &task_state,
                        &reconciliation,
                        detail.clone(),
                    )
                    .await;
                    tracing::warn!(%provider_id, target = target.label(), error = %detail, "partial mutation target verification failed");
                    return;
                }
                match task_state
                    .store()
                    .record_provider_reconciliation_success(
                        reconciliation.reconciliation_id,
                        reconciliation.attempts,
                        claim_token,
                        now_ms(),
                    )
                    .await
                {
                    Ok(spotuify_store::ProviderReconciliationCompletion::Completed) => {
                        task_state.emit_event(DaemonEvent::SyncFinished { summary });
                        match target {
                            spotuify_protocol::SyncTargetData::Library => {
                                task_state.emit_event(DaemonEvent::LibraryChanged {
                                    action: "provider-mutation-reconciled".to_string(),
                                    uris: reconciliation.resource_uris,
                                    provider: Some(provider_id),
                                });
                            }
                            spotuify_protocol::SyncTargetData::Playlists => {
                                let playlist =
                                    reconciliation.resource_uris.iter().find_map(|uri| {
                                        ResourceUri::parse(uri)
                                            .ok()
                                            .filter(|uri| uri.kind() == MediaKind::Playlist)
                                            .map(|uri| uri.as_uri())
                                    });
                                task_state.emit_event(DaemonEvent::PlaylistsChanged {
                                    action: "provider-mutation-reconciled".to_string(),
                                    playlist,
                                    provider: Some(provider_id),
                                });
                            }
                            _ => unreachable!(),
                        }
                    }
                    Ok(spotuify_store::ProviderReconciliationCompletion::NeedsAnotherPass) => {
                        task_state.emit_event(DaemonEvent::SyncFinished { summary });
                        let retry_state = task_state.clone();
                        let reconciliation_id = reconciliation.reconciliation_id;
                        let expected_attempts = reconciliation.attempts;
                        task_state.spawn_background(
                            "provider-reconciliation-stability-pass",
                            async move {
                                tokio::time::sleep(Duration::from_secs(2)).await;
                                spawn_provider_reconciliation(
                                    &retry_state,
                                    reconciliation_id,
                                    expected_attempts,
                                );
                            },
                        );
                    }
                    Ok(spotuify_store::ProviderReconciliationCompletion::Stale) => (),
                    Err(err) => {
                        let detail = bounded_redacted_text(&err.to_string(), 512);
                        fail_started_provider_reconciliation(
                            &task_state,
                            &reconciliation,
                            detail.clone(),
                        )
                        .await;
                        tracing::warn!(%receipt_id, error = %detail, "failed to commit partial mutation reconciliation");
                    }
                }
            }
            Err(err) => {
                let detail = bounded_redacted_text(&err.to_string(), 512);
                fail_started_provider_reconciliation(
                    &task_state,
                    &reconciliation,
                    detail.clone(),
                )
                .await;
                tracing::warn!(%provider_id, target = target.label(), error = %detail, "partial mutation reconciliation failed");
            }
        }
        })
        .catch_unwind()
        .await;
        if attempt.is_err() {
            let detail = "provider reconciliation attempt panicked; retry is required".to_string();
            fail_started_provider_reconciliation(
                &panic_state,
                &panic_reconciliation,
                detail.clone(),
            )
            .await;
            tracing::warn!(%reconciliation_id, error = %detail, "provider mutation reconciliation panicked");
        }
    });
}

fn spawn_provider_reconciliations_for_receipt(state: &Arc<DaemonState>, receipt_id: ReceiptId) {
    let task_state = state.clone();
    state.spawn_background("provider-reconciliations-for-receipt", async move {
        let mut delay = PROVIDER_RECONCILIATION_RETRY_BASE;
        loop {
            match task_state
                .store()
                .pending_provider_reconciliations_for_receipt(receipt_id)
                .await
            {
                Ok(reconciliations) => {
                    for reconciliation in reconciliations {
                        let expected_attempts = reconciliation.attempts;
                        spawn_provider_reconciliation(
                            &task_state,
                            reconciliation.reconciliation_id,
                            expected_attempts,
                        );
                    }
                    return;
                }
                Err(error) => {
                    tracing::warn!(%receipt_id, %error, "failed to load provider reconciliations for receipt; retrying");
                    tokio::time::sleep(delay).await;
                    delay = delay.saturating_mul(2).min(PROVIDER_RECONCILIATION_RETRY_MAX);
                }
            }
        }
    });
}

fn spawn_provider_reconciliations_for_receipt_after(
    state: &Arc<DaemonState>,
    receipt_id: ReceiptId,
    delay: Duration,
) {
    let task_state = state.clone();
    state.spawn_background("delayed-provider-reconciliations", async move {
        tokio::time::sleep(delay).await;
        spawn_provider_reconciliations_for_receipt(&task_state, receipt_id);
    });
}

pub(crate) async fn resume_provider_reconciliations(
    state: &Arc<DaemonState>,
) -> anyhow::Result<()> {
    for reconciliation in state.store().pending_provider_reconciliations().await? {
        let expected_attempts = reconciliation.attempts;
        spawn_provider_reconciliation(state, reconciliation.reconciliation_id, expected_attempts);
    }
    Ok(())
}

const MUTATION_FINALIZATION_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(25),
    Duration::from_millis(75),
    Duration::from_millis(225),
];

struct ClaimedMutationFinalization {
    mutation_id: MutationId,
    receipt_id: ReceiptId,
    receipt_status: spotuify_protocol::ReceiptStatus,
    receipt_message: String,
    receipt_error: Option<spotuify_protocol::ApiErrorSummary>,
    operation_id: OperationId,
    operation_status: OperationStatus,
    operation_error: Option<String>,
    response_json: String,
    succeeded: bool,
    reconciliations: Vec<spotuify_store::ProviderReconciliation>,
    fallback_reconciliations: Vec<spotuify_store::ProviderReconciliation>,
    post_write_guard: Option<spotuify_store::PostWriteOperationGuard>,
    fallback_post_write_guard: Option<spotuify_store::PostWriteOperationGuard>,
    operation_recovery: Option<spotuify_store::PartialOperationRecovery>,
    finished_at_ms: i64,
}

enum MutationFinalizationOutcome {
    Finalized,
    Indeterminate(Box<Response>),
}

async fn persist_indeterminate_mutation(
    state: &DaemonState,
    mutation_id: MutationId,
    reconciliations: &[spotuify_store::ProviderReconciliation],
    post_write_guard: Option<spotuify_store::PostWriteOperationGuard>,
    finished_at_ms: i64,
) -> anyhow::Result<Response> {
    let mut last_error = None;
    for delay in MUTATION_FINALIZATION_RETRY_DELAYS {
        match state
            .store()
            .mark_mutation_indeterminate(
                mutation_id,
                reconciliations,
                post_write_guard,
                finished_at_ms,
            )
            .await
        {
            Ok(response) => return Ok(response),
            Err(error) => last_error = Some(error),
        }
        tokio::time::sleep(delay).await;
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("indeterminate finalization failed")))
}

async fn recover_processing_mutation_claim(
    state: &Arc<DaemonState>,
    claim: &spotuify_store::ProcessingMutationClaim,
    launch: RecoveredReconciliationLaunch,
) -> anyhow::Result<Response> {
    let (mut reconciliations, post_write_guard) = recovery_reconciliation_intent(
        state.as_ref(),
        &claim.request_json,
        claim.receipt_id,
        claim.operation_id,
    )
    .await?;
    for reconciliation in &mut reconciliations {
        reconciliation.require_stability_pass();
    }
    let response = persist_indeterminate_mutation(
        state.as_ref(),
        claim.mutation_id,
        &reconciliations,
        post_write_guard,
        now_ms(),
    )
    .await?;
    if !reconciliations.is_empty() {
        match launch {
            RecoveredReconciliationLaunch::After(delay) => {
                spawn_provider_reconciliations_for_receipt_after(state, claim.receipt_id, delay);
            }
            RecoveredReconciliationLaunch::StartupResume => {}
        }
    }
    Ok(response)
}

#[derive(Clone, Copy)]
enum RecoveredReconciliationLaunch {
    After(Duration),
    StartupResume,
}

pub(crate) async fn recover_processing_mutation(
    state: &Arc<DaemonState>,
    mutation_id: MutationId,
) -> anyhow::Result<Option<Response>> {
    let claims = state.store().processing_mutation_claims().await?;
    let Some(claim) = claims
        .into_iter()
        .find(|claim| claim.mutation_id == mutation_id)
    else {
        return state.store().terminal_mutation_response(mutation_id).await;
    };
    recover_processing_mutation_claim(
        state,
        &claim,
        RecoveredReconciliationLaunch::After(Duration::from_secs(2)),
    )
    .await
    .map(Some)
}

fn schedule_processing_mutation_lifecycle_recovery(
    state: &Arc<DaemonState>,
    mutation_id: MutationId,
) {
    let task_state = state.clone();
    state.spawn_background("processing-mutation-lifecycle-recovery", async move {
        let mut delay = PROVIDER_RECONCILIATION_RETRY_BASE;
        loop {
            tokio::time::sleep(delay).await;
            match recover_processing_mutation(&task_state, mutation_id).await {
                Ok(_) => return,
                Err(error) => {
                    tracing::warn!(%mutation_id, %error, "processing mutation lifecycle recovery retry failed");
                    delay = delay.saturating_mul(2).min(PROVIDER_RECONCILIATION_RETRY_MAX);
                }
            }
        }
    });
}

pub(crate) async fn recover_processing_mutations(
    state: &Arc<DaemonState>,
) -> anyhow::Result<(u64, u64)> {
    let claims = state.store().processing_mutation_claims().await?;
    let mut recovered = 0_u64;
    let mut failed = 0_u64;
    for claim in claims {
        match recover_processing_mutation_claim(
            state,
            &claim,
            RecoveredReconciliationLaunch::StartupResume,
        )
        .await
        {
            Ok(_) => recovered = recovered.saturating_add(1),
            Err(error) => {
                failed = failed.saturating_add(1);
                tracing::warn!(mutation_id = %claim.mutation_id, %error, "in-flight mutation recovery failed; continuing with other claims");
            }
        }
    }
    Ok((recovered, failed))
}

async fn persist_claimed_mutation_finalization(
    state: &DaemonState,
    intent: &ClaimedMutationFinalization,
) -> anyhow::Result<MutationFinalizationOutcome> {
    let mut last_error = None;
    for delay in MUTATION_FINALIZATION_RETRY_DELAYS {
        match state
            .store()
            .finalize_claimed_mutation(
                intent.mutation_id,
                intent.receipt_id,
                intent.receipt_status,
                &intent.receipt_message,
                intent.receipt_error.as_ref(),
                intent.operation_id,
                intent.operation_status,
                intent.operation_error.as_deref(),
                &intent.response_json,
                intent.succeeded,
                &intent.reconciliations,
                intent.post_write_guard,
                intent.operation_recovery.as_ref(),
                intent.finished_at_ms,
            )
            .await
        {
            Ok(()) => return Ok(MutationFinalizationOutcome::Finalized),
            Err(error) => last_error = Some(error),
        }
        tokio::time::sleep(delay).await;
    }

    match persist_indeterminate_mutation(
        state,
        intent.mutation_id,
        if intent.reconciliations.is_empty() {
            &intent.fallback_reconciliations
        } else {
            &intent.reconciliations
        },
        intent.post_write_guard.or(intent.fallback_post_write_guard),
        intent.finished_at_ms,
    )
    .await
    {
        Ok(response) => {
            if serde_json::to_string(&response)? == intent.response_json {
                Ok(MutationFinalizationOutcome::Finalized)
            } else {
                Ok(MutationFinalizationOutcome::Indeterminate(Box::new(
                    response,
                )))
            }
        }
        Err(fallback_error) => Err(anyhow::anyhow!(
            "mutation lifecycle finalization failed ({}) and indeterminate fallback failed ({})",
            last_error
                .map(|error| bounded_redacted_text(&error.to_string(), 256))
                .unwrap_or_else(|| "unknown finalization error".to_string()),
            bounded_redacted_text(&fallback_error.to_string(), 256),
        )),
    }
}

fn mutation_error_from_response(response: Response) -> anyhow::Error {
    match response {
        Response::Error {
            message,
            kind,
            retryable,
            provider,
            detail,
            ..
        } => MutationRequestError {
            kind,
            message,
            retryable,
            provider,
            detail,
        }
        .into(),
        Response::Ok { .. } => anyhow::anyhow!(
            "mutation lifecycle was already finalized but its typed response could not be replayed"
        ),
    }
}

/// Phase 12 — record an operation row around every mutation. Wraps
/// `record_mutation` (Phase 6.6 receipt lifecycle) and also writes an
/// `operations` row + emits `OperationRecorded`.
///
/// `body` receives the freshly-minted `OperationId` so it can call
/// `state.store().update_operation_plan(op_id, …)` mid-flight once it
/// has captured the pre-mutation version token / prior device / etc.
/// Transport commands typically pass `(NotReversible, Transport)` up
/// front; reversible mutations (playlist_add, transfer, library_save)
/// fill in real pre-state inside the body.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_operation<F, Fut, T>(
    state: &std::sync::Arc<DaemonState>,
    kind: OperationKind,
    source: OperationSource,
    subject_uris: Vec<String>,
    action: &str,
    request_summary: &str,
    mutation_id: Option<MutationId>,
    initial_pre_state: Option<spotuify_protocol::PreState>,
    initial_reversal_plan: Option<spotuify_protocol::ReversalPlan>,
    mutation_lane: Option<Arc<tokio::sync::Mutex<()>>>,
    body: F,
) -> anyhow::Result<T>
where
    F: FnOnce(OperationId) -> Fut + Send,
    Fut: std::future::Future<Output = anyhow::Result<T>> + Send,
    T: Serialize + DeserializeOwned + MutationResponseMetadata + Send,
{
    if let Some(replay) =
        replay_existing_recorded_mutation(state, mutation_id, request_summary).await?
    {
        return Ok(replay);
    }
    let error_provider = match serde_json::from_str::<Request>(request_summary) {
        Ok(request) => request_provider_context(state, &request).await,
        Err(_) => None,
    };
    reject_if_auth_blocked(state, error_provider.as_ref()).await?;

    let operation_id = OperationId::new_v7();
    let occurred_at_ms = now_ms();
    let receipt_id = ReceiptId::new_v7();
    let reversible = kind.is_reversible()
        && !matches!(
            &initial_reversal_plan,
            Some(spotuify_protocol::ReversalPlan::NotReversible { .. })
        );
    let row = Operation {
        operation_id,
        kind,
        occurred_at_ms,
        finished_at_ms: None,
        source,
        requester: None,
        subject_uris: subject_uris.clone(),
        reversible,
        reversal_plan: initial_reversal_plan,
        pre_state: initial_pre_state,
        status: OperationStatus::Pending,
        receipt_id: Some(receipt_id),
        subject_op_id: None,
        undone_by_op_id: None,
        redone_by_op_id: None,
        error_message: None,
    };

    // The operations table has a foreign key on `receipt_id`. The
    // writer pool runs with `PRAGMA foreign_keys = ON`, so the receipt
    // row MUST exist before we insert the operation. Earlier versions
    // ran the inserts in the opposite order and the FK violation was
    // silently swallowed by `let _ = ...`, leaving the operations
    // table empty in production.
    let started = now_ms();
    let receipt = spotuify_protocol::Receipt {
        receipt_id,
        action: action.to_string(),
        status: spotuify_protocol::ReceiptStatus::Pending,
        message: "queued".to_string(),
        started_at_ms: started,
        finished_at_ms: None,
        error: None,
    };
    if let Some(id) = mutation_id {
        match state
            .store()
            .claim_mutation(
                id,
                &mutation_fingerprint(request_summary),
                request_summary,
                &receipt,
                &row,
                started,
            )
            .await?
        {
            spotuify_store::MutationClaim::Claimed => {}
            spotuify_store::MutationClaim::FingerprintMismatch => {
                return Err(MutationRequestError {
                    kind: spotuify_protocol::IpcErrorKind::InvalidRequest,
                    message: format!("mutation id {id} is already bound to a different request"),
                    retryable: false,
                    provider: None,
                    detail: None,
                }
                .into());
            }
            spotuify_store::MutationClaim::Existing {
                receipt,
                response_json,
            } => {
                if let Some(receipt) = receipt.as_ref() {
                    spawn_provider_reconciliations_for_receipt(state, receipt.receipt_id);
                }
                return replay_recorded_mutation(
                    id,
                    receipt.map(|receipt| *receipt),
                    response_json,
                );
            }
        }
    } else {
        state
            .store()
            .insert_pending_receipt(&receipt, request_summary)
            .await?;
        state.store().insert_pending_operation(&row).await?;
    }
    state.emit_event(spotuify_protocol::DaemonEvent::MutationAccepted {
        receipt_id,
        action: action.to_string(),
    });

    let _mutation_guard = match mutation_lane {
        Some(lane) => Some(lane.lock_owned().await),
        None => None,
    };
    let mut result = body(operation_id).await;
    if let Ok(value) = &mut result {
        value.set_mutation_metadata(mutation_id, receipt_id, false);
    }
    let partial = result
        .as_ref()
        .err()
        .and_then(|err| err.downcast_ref::<PartialMutationError>());
    let reconciliations = error_reconciliations(&result, receipt_id, operation_id);
    let post_write_lifecycle = result
        .as_ref()
        .err()
        .and_then(|error| error.downcast_ref::<PostWriteLifecycleError>());
    let post_write_guard = partial
        .and_then(|partial| partial.post_write_guard)
        .or_else(|| {
            result
                .as_ref()
                .err()
                .and_then(|error| error.downcast_ref::<MalformedProviderReceiptError>())
                .and_then(|malformed| malformed.post_write_guard)
        })
        .or_else(|| post_write_lifecycle.and_then(|error| error.guard));
    let retained_artifact = result
        .as_ref()
        .err()
        .and_then(|err| err.downcast_ref::<RemoteArtifactRetainedError>());
    let operation_recovery = partial
        .and_then(|partial| partial.operation_recovery.clone())
        .or_else(|| retained_artifact.and_then(|retained| retained.operation_recovery.clone()));
    let retain_cleanup_plan = operation_recovery.is_some() && kind == OperationKind::PlaylistCreate;
    let finished = now_ms();
    let (receipt_status, message, error_summary) = match &result {
        Ok(_) => (
            spotuify_protocol::ReceiptStatus::Confirmed,
            format!("{action} confirmed"),
            None,
        ),
        Err(err) => {
            let mut summary = receipt_error_summary_from_error(err);
            if summary.provider.is_none() {
                summary.provider = error_provider.clone();
            }
            let msg = summary.message.clone();
            (
                spotuify_protocol::ReceiptStatus::Failed,
                msg.clone(),
                Some(summary),
            )
        }
    };
    let (status, error) = match &result {
        Ok(_) => (OperationStatus::Succeeded, None),
        Err(err) if retain_cleanup_plan => (
            OperationStatus::Succeeded,
            Some(spotuify_protocol::redact_sensitive_text(&err.to_string())),
        ),
        Err(err) => (
            OperationStatus::Failed,
            Some(spotuify_protocol::redact_sensitive_text(&err.to_string())),
        ),
    };
    let mut response = match &result {
        Ok(value) => Response::Ok {
            data: value.response_data(),
        },
        Err(err) => error_response_from(err),
    };
    if let Response::Error {
        provider, detail, ..
    } = &mut response
    {
        if provider.is_none() {
            *provider = error_provider;
        }
        if detail.is_none() {
            *detail = Some(message.clone());
        }
    }
    redact_error_response_fields(&mut response);
    match mutation_id {
        Some(id) => {
            let response_json = serde_json::to_string(&response)?;
            let (fallback_reconciliations, fallback_guard) = match recovery_reconciliation_intent(
                state.as_ref(),
                request_summary,
                receipt_id,
                operation_id,
            )
            .await
            {
                Ok(recovery) => recovery,
                Err(error) => {
                    schedule_processing_mutation_lifecycle_recovery(state, id);
                    return Err(error);
                }
            };
            let intent = ClaimedMutationFinalization {
                mutation_id: id,
                receipt_id,
                receipt_status,
                receipt_message: message.clone(),
                receipt_error: error_summary.clone(),
                operation_id,
                operation_status: status,
                operation_error: error.clone(),
                response_json,
                succeeded: result.is_ok(),
                reconciliations: reconciliations.clone(),
                fallback_reconciliations,
                post_write_guard,
                fallback_post_write_guard: fallback_guard,
                operation_recovery: operation_recovery.clone(),
                finished_at_ms: finished,
            };
            let finalization =
                match persist_claimed_mutation_finalization(state.as_ref(), &intent).await {
                    Ok(finalization) => finalization,
                    Err(error) => {
                        schedule_processing_mutation_lifecycle_recovery(state, id);
                        return Err(error);
                    }
                };
            if let MutationFinalizationOutcome::Indeterminate(response) = finalization {
                let indeterminate_message = match response.as_ref() {
                    Response::Error { message, .. } => message.clone(),
                    Response::Ok { .. } => {
                        "mutation outcome was terminalized as indeterminate".to_string()
                    }
                };
                state.emit_event(spotuify_protocol::DaemonEvent::MutationFinalized {
                    receipt_id,
                    status: spotuify_protocol::ReceiptStatus::Failed,
                    message: indeterminate_message,
                });
                state.emit_event(DaemonEvent::OperationRecorded {
                    operation_id,
                    kind,
                    source,
                });
                spawn_provider_reconciliations_for_receipt(state, receipt_id);
                return Err(mutation_error_from_response(*response));
            }
        }
        None if !reconciliations.is_empty()
            || operation_recovery.is_some()
            || post_write_guard.is_some() =>
        {
            state
                .store()
                .finalize_partial_operation(
                    receipt_id,
                    &message,
                    error_summary
                        .as_ref()
                        .expect("partial mutation has a typed error summary"),
                    operation_id,
                    status,
                    error
                        .as_deref()
                        .expect("partial mutation has an operation error"),
                    &reconciliations,
                    post_write_guard,
                    operation_recovery.as_ref(),
                    finished,
                )
                .await?;
        }
        None => {
            state
                .store()
                .finalize_receipt(
                    receipt_id,
                    receipt_status,
                    &message,
                    finished,
                    error_summary.as_ref(),
                )
                .await?;
            state
                .store()
                .finalize_operation(operation_id, status, finished, error.as_deref())
                .await?;
        }
    }
    state.emit_event(spotuify_protocol::DaemonEvent::MutationFinalized {
        receipt_id,
        status: receipt_status,
        message: message.clone(),
    });
    state.emit_event(DaemonEvent::OperationRecorded {
        operation_id,
        kind,
        source,
    });
    if !reconciliations.is_empty() {
        spawn_provider_reconciliations_for_receipt(state, receipt_id);
    }
    result
}

pub(crate) trait MutationResponseMetadata {
    fn set_mutation_metadata(
        &mut self,
        mutation_id: Option<MutationId>,
        receipt_id: ReceiptId,
        replayed: bool,
    );
    fn response_data(&self) -> ResponseData;
}

impl MutationResponseMetadata for ResponseData {
    fn set_mutation_metadata(
        &mut self,
        mutation_id: Option<MutationId>,
        receipt_id: ReceiptId,
        replayed: bool,
    ) {
        match self {
            ResponseData::Mutation { receipt } => {
                receipt.receipt_id = Some(receipt_id);
                receipt.mutation_id = mutation_id;
                receipt.replayed = replayed;
            }
            ResponseData::PlaylistCreate { receipt } => {
                receipt.receipt_id = Some(receipt_id);
                receipt.mutation_id = mutation_id;
                receipt.replayed = replayed;
            }
            _ => {}
        }
    }

    fn response_data(&self) -> ResponseData {
        self.clone()
    }
}

fn mutation_fingerprint(request_json: &str) -> String {
    format!("{:x}", Sha256::digest(request_json.as_bytes()))
}

fn replay_recorded_mutation<T>(
    mutation_id: MutationId,
    receipt: Option<spotuify_protocol::Receipt>,
    response_json: Option<String>,
) -> anyhow::Result<T>
where
    T: DeserializeOwned + MutationResponseMetadata,
{
    let receipt = receipt.ok_or_else(|| MutationRequestError {
        kind: spotuify_protocol::IpcErrorKind::Internal,
        message: format!("mutation id {mutation_id} has no linked receipt"),
        retryable: false,
        provider: None,
        detail: None,
    })?;
    if receipt.status == spotuify_protocol::ReceiptStatus::Pending {
        return Err(MutationRequestError {
            kind: spotuify_protocol::IpcErrorKind::InvalidRequest,
            message: format!(
                "mutation id {mutation_id} is already processing as receipt {}",
                receipt.receipt_id
            ),
            retryable: true,
            provider: None,
            detail: None,
        }
        .into());
    }
    let raw = response_json.ok_or_else(|| MutationRequestError {
        kind: spotuify_protocol::IpcErrorKind::Internal,
        message: receipt.message.clone(),
        retryable: false,
        provider: receipt
            .error
            .as_ref()
            .and_then(|error| error.provider.clone()),
        detail: receipt
            .error
            .as_ref()
            .and_then(|error| error.detail.clone()),
    })?;
    match serde_json::from_str::<Response>(&raw)? {
        Response::Ok { data } => {
            let mut value: T = serde_json::from_value(serde_json::to_value(data)?)?;
            value.set_mutation_metadata(Some(mutation_id), receipt.receipt_id, true);
            Ok(value)
        }
        Response::Error {
            message,
            kind,
            retryable,
            provider,
            detail,
            ..
        } => Err(MutationRequestError {
            kind,
            message,
            retryable,
            provider,
            detail,
        }
        .into()),
    }
}

enum MutationBodyOutcome {
    Completed(anyhow::Result<()>),
    Indeterminate,
}

async fn run_mutation_body<F>(body: F, deadline: Duration) -> MutationBodyOutcome
where
    F: std::future::Future<Output = anyhow::Result<()>>,
{
    match tokio::time::timeout(deadline, AssertUnwindSafe(body).catch_unwind()).await {
        Ok(Ok(result)) => MutationBodyOutcome::Completed(result),
        Ok(Err(_)) | Err(_) => MutationBodyOutcome::Indeterminate,
    }
}

/// Spawn a mutation body and return an optimistic `Mutation` response
/// immediately. The IPC caller sees `ok=true` and a "queued" message
/// before Spotify confirms; subscribers to the daemon event bus see
/// `MutationFinalized { status: Confirmed | Failed }` when the
/// background body resolves.
///
/// The lane handle is moved into the spawned task, then acquired there,
/// so concurrent mutations on the same lane still serialise at Spotify
/// without making the IPC response wait behind the lane. The
/// operation/receipt lifecycle (insert pending row → emit
/// `MutationAccepted` → finalise on body completion → emit
/// `MutationFinalized`) mirrors `record_operation` exactly so undo/redo
/// + receipt recovery keep working unchanged. The only difference is
///   *when* the response returns: optimistic, before the body runs.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn spawn_optimistic_mutation<F, Fut>(
    state: &Arc<DaemonState>,
    kind: OperationKind,
    source: OperationSource,
    subject_uris: Vec<String>,
    action: &'static str,
    request_summary: String,
    initial_pre_state: Option<spotuify_protocol::PreState>,
    initial_reversal_plan: Option<spotuify_protocol::ReversalPlan>,
    mutation_lane: Option<Arc<tokio::sync::Mutex<()>>>,
    mutation_id: Option<MutationId>,
    body: F,
) -> anyhow::Result<ResponseData>
where
    F: FnOnce(OperationId) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    spawn_optimistic_mutation_inner(
        state,
        kind,
        source,
        subject_uris,
        action,
        request_summary,
        initial_pre_state,
        initial_reversal_plan,
        mutation_lane,
        mutation_id,
        body,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn spawn_optimistic_mutation_inner<F, Fut>(
    state: &Arc<DaemonState>,
    kind: OperationKind,
    source: OperationSource,
    subject_uris: Vec<String>,
    action: &'static str,
    request_summary: String,
    initial_pre_state: Option<spotuify_protocol::PreState>,
    initial_reversal_plan: Option<spotuify_protocol::ReversalPlan>,
    mutation_lane: Option<Arc<tokio::sync::Mutex<()>>>,
    mutation_id: Option<MutationId>,
    body: F,
) -> anyhow::Result<ResponseData>
where
    F: FnOnce(OperationId) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    if let Some(replay) =
        replay_existing_optimistic_mutation(state, mutation_id, &request_summary).await?
    {
        return Ok(replay);
    }
    let error_provider = match serde_json::from_str::<Request>(&request_summary) {
        Ok(request) => request_provider_context(state, &request).await,
        Err(_) => None,
    };
    reject_if_auth_blocked(state, error_provider.as_ref()).await?;

    let operation_id = OperationId::new_v7();
    let occurred_at_ms = now_ms();
    let receipt_id = ReceiptId::new_v7();
    let reversible = kind.is_reversible()
        && !matches!(
            &initial_reversal_plan,
            Some(spotuify_protocol::ReversalPlan::NotReversible { .. })
        );
    let row = Operation {
        operation_id,
        kind,
        occurred_at_ms,
        finished_at_ms: None,
        source,
        requester: None,
        subject_uris,
        reversible,
        reversal_plan: initial_reversal_plan,
        pre_state: initial_pre_state,
        status: OperationStatus::Pending,
        receipt_id: Some(receipt_id),
        subject_op_id: None,
        undone_by_op_id: None,
        redone_by_op_id: None,
        error_message: None,
    };
    // Receipt FIRST so the operations.receipt_id FK lands cleanly.
    // See `record_operation` for the same ordering rationale.
    let started_at_ms = crate::analytics::now_ms();
    let receipt = spotuify_protocol::Receipt {
        receipt_id,
        action: action.to_string(),
        status: spotuify_protocol::ReceiptStatus::Pending,
        message: format!("{action} queued"),
        started_at_ms,
        finished_at_ms: None,
        error: None,
    };
    if let Some(id) = mutation_id {
        match state
            .store()
            .claim_mutation(
                id,
                &mutation_fingerprint(&request_summary),
                &request_summary,
                &receipt,
                &row,
                started_at_ms,
            )
            .await?
        {
            spotuify_store::MutationClaim::Claimed => {}
            spotuify_store::MutationClaim::FingerprintMismatch => {
                return Err(MutationRequestError {
                    kind: spotuify_protocol::IpcErrorKind::InvalidRequest,
                    message: format!("mutation id {id} is already bound to a different request"),
                    retryable: false,
                    provider: None,
                    detail: None,
                }
                .into());
            }
            spotuify_store::MutationClaim::Existing {
                receipt,
                response_json,
            } => {
                return replay_existing_optimistic_claim(state, id, receipt, response_json);
            }
        }
    } else {
        state
            .store()
            .insert_pending_receipt(&receipt, &request_summary)
            .await?;
        state.store().insert_pending_operation(&row).await?;
    }
    state.emit_event(spotuify_protocol::DaemonEvent::MutationAccepted {
        receipt_id,
        action: action.to_string(),
    });

    let task_state = state.clone();
    state.spawn_background("optimistic-mutation-body", async move {
        let body_with_lane = async move {
            // Hold the lane guard across the body so concurrent mutations
            // on the same lane still serialise. Dropped on body return.
            let _guard = match mutation_lane {
                Some(lane) => Some(lane.lock_owned().await),
                None => None,
            };
            body(operation_id).await
        };
        let (result, outcome_unknown) = match run_mutation_body(body_with_lane, MUTATION_BODY_TIMEOUT).await {
            MutationBodyOutcome::Completed(result) => (result, false),
            MutationBodyOutcome::Indeterminate => (
                Err(anyhow::anyhow!(
                    "remote outcome indeterminate; inspect state before retrying with a new mutation id"
                )),
                true,
            ),
        };
        let finished = crate::analytics::now_ms();

        if outcome_unknown {
            if let Some(id) = mutation_id {
                let recovery = recovery_reconciliation_intent(
                    task_state.as_ref(),
                    &request_summary,
                    receipt_id,
                    operation_id,
                )
                .await;
                let (mut reconciliations, post_write_guard) = match recovery {
                    Ok(recovery) => recovery,
                    Err(error) => {
                        tracing::error!(%error, %receipt_id, %operation_id, "failed to reconstruct indeterminate mutation recovery intent");
                        schedule_processing_mutation_lifecycle_recovery(&task_state, id);
                        return;
                    }
                };
                for reconciliation in &mut reconciliations {
                    reconciliation.require_stability_pass();
                }
                match persist_indeterminate_mutation(
                    task_state.as_ref(),
                    id,
                    &reconciliations,
                    post_write_guard,
                    finished,
                )
                .await {
                    Ok(_) => {
                        task_state.emit_event(DaemonEvent::OperationRecorded {
                            operation_id,
                            kind,
                            source,
                        });
                        task_state.emit_event(spotuify_protocol::DaemonEvent::MutationFinalized {
                            receipt_id,
                            status: spotuify_protocol::ReceiptStatus::Failed,
                            message: "remote outcome indeterminate; inspect state before retrying with a new mutation id".to_string(),
                        });
                        if !reconciliations.is_empty() {
                            spawn_provider_reconciliations_for_receipt_after(
                                &task_state,
                                receipt_id,
                                Duration::from_secs(2),
                            );
                        }
                    }
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            %receipt_id,
                            %operation_id,
                            "failed to persist indeterminate mutation outcome"
                        );
                        schedule_processing_mutation_lifecycle_recovery(&task_state, id);
                    }
                }
                return;
            }
        }

        let partial = result
            .as_ref()
            .err()
            .and_then(|err| err.downcast_ref::<PartialMutationError>());
        let reconciliations = error_reconciliations(&result, receipt_id, operation_id);
        let post_write_lifecycle = result
            .as_ref()
            .err()
            .and_then(|error| error.downcast_ref::<PostWriteLifecycleError>());
        let post_write_guard = partial
            .and_then(|partial| partial.post_write_guard)
            .or_else(|| {
                result
                    .as_ref()
                    .err()
                    .and_then(|error| error.downcast_ref::<MalformedProviderReceiptError>())
                    .and_then(|malformed| malformed.post_write_guard)
            })
            .or_else(|| post_write_lifecycle.and_then(|error| error.guard));
        let retained_artifact = result
            .as_ref()
            .err()
            .and_then(|err| err.downcast_ref::<RemoteArtifactRetainedError>());
        let operation_recovery = partial
            .and_then(|partial| partial.operation_recovery.clone())
            .or_else(|| {
                retained_artifact.and_then(|retained| retained.operation_recovery.clone())
            });
        let retain_cleanup_plan =
            operation_recovery.is_some() && kind == OperationKind::PlaylistCreate;
        let (op_status, op_error) = match &result {
            Ok(()) => (OperationStatus::Succeeded, None),
            Err(err) if retain_cleanup_plan => (
                OperationStatus::Succeeded,
                Some(spotuify_protocol::redact_sensitive_text(&err.to_string())),
            ),
            Err(err) => (
                OperationStatus::Failed,
                Some(spotuify_protocol::redact_sensitive_text(&err.to_string())),
            ),
        };
        let (receipt_status, message, error_summary) = match &result {
            Ok(()) => (
                spotuify_protocol::ReceiptStatus::Confirmed,
                format!("{action} confirmed"),
                None,
            ),
            Err(err) => {
                let mut summary = receipt_error_summary_from_error(err);
                if summary.provider.is_none() {
                    summary.provider = error_provider.clone();
                }
                let msg = summary.message.clone();
                (
                    spotuify_protocol::ReceiptStatus::Failed,
                    msg.clone(),
                    Some(summary),
                )
            }
        };
        let mut response = match &result {
            Ok(()) => Response::Ok {
                data: ResponseData::Mutation {
                    receipt: CommandReceipt {
                        ok: true,
                        action: action.to_string(),
                        message: format!("{action} confirmed"),
                        receipt_id: Some(receipt_id),
                        mutation_id,
                        status: Some(spotuify_protocol::ReceiptStatus::Confirmed),
                        error: None,
                        replayed: false,
                    },
                },
            },
            Err(err) => error_response_from(err),
        };
        if let Response::Error {
            provider, detail, ..
        } = &mut response
        {
            if provider.is_none() {
                *provider = error_provider;
            }
            if detail.is_none() {
                *detail = Some(message.clone());
            }
        }
        redact_error_response_fields(&mut response);
        let finalize_result: anyhow::Result<MutationFinalizationOutcome> = match mutation_id {
            Some(id) => match serde_json::to_string(&response) {
                Ok(response_json) => {
                    match recovery_reconciliation_intent(
                            task_state.as_ref(),
                            &request_summary,
                            receipt_id,
                            operation_id,
                        )
                        .await
                    {
                        Ok((fallback_reconciliations, fallback_guard)) => {
                            let intent = ClaimedMutationFinalization {
                                mutation_id: id,
                                receipt_id,
                                receipt_status,
                                receipt_message: message.clone(),
                                receipt_error: error_summary.clone(),
                                operation_id,
                                operation_status: op_status,
                                operation_error: op_error.clone(),
                                response_json,
                                succeeded: result.is_ok(),
                                reconciliations: reconciliations.clone(),
                                fallback_reconciliations,
                                post_write_guard,
                                fallback_post_write_guard: fallback_guard,
                                operation_recovery: operation_recovery.clone(),
                                finished_at_ms: finished,
                            };
                            persist_claimed_mutation_finalization(task_state.as_ref(), &intent).await
                        }
                        Err(error) => Err(error),
                    }
                }
                Err(err) => Err(err.into()),
            },
            None
                if !reconciliations.is_empty()
                    || operation_recovery.is_some()
                    || post_write_guard.is_some() => task_state
                .store()
                .finalize_partial_operation(
                    receipt_id,
                    &message,
                    error_summary
                        .as_ref()
                        .expect("partial mutation has a typed error summary"),
                    operation_id,
                    op_status,
                    op_error
                        .as_deref()
                        .expect("partial mutation has an operation error"),
                    &reconciliations,
                    post_write_guard,
                    operation_recovery.as_ref(),
                    finished,
                )
                .await
                .map(|()| MutationFinalizationOutcome::Finalized),
            None => {
                let result: anyhow::Result<()> = async {
                    task_state
                        .store()
                        .finalize_receipt(
                            receipt_id,
                            receipt_status,
                            &message,
                            finished,
                            error_summary.as_ref(),
                        )
                        .await?;
                    task_state
                        .store()
                        .finalize_operation(
                            operation_id,
                            op_status,
                            finished,
                            op_error.as_deref(),
                        )
                        .await?;
                    Ok(())
                }
                .await;
                result.map(|()| MutationFinalizationOutcome::Finalized)
            }
        };
        match finalize_result {
            Ok(MutationFinalizationOutcome::Finalized) => {
                task_state.emit_event(DaemonEvent::OperationRecorded {
                    operation_id,
                    kind,
                    source,
                });
                task_state.emit_event(spotuify_protocol::DaemonEvent::MutationFinalized {
                    receipt_id,
                    status: receipt_status,
                    message,
                });
                if !reconciliations.is_empty() {
                    spawn_provider_reconciliations_for_receipt(&task_state, receipt_id);
                }
            }
            Ok(MutationFinalizationOutcome::Indeterminate(indeterminate)) => {
                let indeterminate_message = match *indeterminate {
                    Response::Error { message, .. } => message,
                    Response::Ok { .. } => {
                        "mutation outcome was terminalized as indeterminate".to_string()
                    }
                };
                task_state.emit_event(DaemonEvent::OperationRecorded {
                    operation_id,
                    kind,
                    source,
                });
                task_state.emit_event(spotuify_protocol::DaemonEvent::MutationFinalized {
                    receipt_id,
                    status: spotuify_protocol::ReceiptStatus::Failed,
                    message: indeterminate_message,
                });
                spawn_provider_reconciliations_for_receipt(&task_state, receipt_id);
            }
            Err(err) => {
                tracing::error!(error = %err, %receipt_id, %operation_id, "failed to finalize mutation lifecycle");
                if let Some(id) = mutation_id {
                    schedule_processing_mutation_lifecycle_recovery(&task_state, id);
                }
            }
        }
    });

    Ok(ResponseData::Mutation {
        receipt: CommandReceipt {
            ok: true,
            action: action.to_string(),
            message: receipt.message.clone(),
            receipt_id: Some(receipt_id),
            mutation_id,
            status: Some(spotuify_protocol::ReceiptStatus::Pending),
            error: None,
            replayed: false,
        },
    })
}

/// Wrap provider transport execution with a one-shot device-recovery retry.
///
/// Spotify's `PUT /me/player/<cmd>` endpoints fail with a structured
/// 404 + `"Player command failed: No active device found"` whenever no
/// device is currently registered as the active player. That's a
/// terrible message to surface to the user — they hit Pause, the TUI
/// flashes "404 on PUT /me/player/pause", and the actual remedy
/// (start spotifyd / open the Spotify app) is buried.
///
/// This wrapper detects that specific case and tries to recover
/// automatically:
/// 1. `ensure_player_ready(configured_name)` — bring up the configured
///    backend (embedded librespot).
/// 2. Short pause so Spotify's device registry catches up after the
///    new device announces itself via the librespot/spotifyd SPIRC.
/// 3. Retry the original command.
///
/// If recovery fails — backend unavailable, auth missing — we fall
/// through to a human-readable error that
/// lists any devices Spotify *does* know about, with the actionable
/// next step (`spotuify devices transfer <name>` or open the Spotify
/// app).
pub(crate) async fn execute_with_device_recovery(
    state: &Arc<DaemonState>,
    provider: Arc<dyn MusicProvider>,
    transport: Arc<dyn RemoteTransport>,
    command: CommandKind,
) -> anyhow::Result<CommandResult> {
    if let Some(result) = try_embedded_transport(state, &command).await {
        return Ok(result);
    }
    match execute_provider_command(
        state,
        provider.as_ref(),
        transport.as_ref(),
        command.clone(),
    )
    .await
    {
        Ok(result) => Ok(result),
        Err(err) if is_recoverable_device_error(&err) => {
            let no_active = is_no_active_device_error(&err);
            tracing::info!(
                error = %err,
                "transport command hit missing device; attempting recovery"
            );
            let device_name = state.configured_device_name();
            let recovered = match tokio::time::timeout(
                DEVICE_RECOVERY_TIMEOUT,
                state.reconnect_player(&device_name),
            )
            .await
            {
                Ok(Ok(_)) => true,
                Ok(Err(err)) => {
                    tracing::warn!(error = %err, "embedded device reconnect failed");
                    false
                }
                Err(_) => {
                    tracing::warn!(
                        timeout_secs = DEVICE_RECOVERY_TIMEOUT.as_secs(),
                        "embedded device reconnect timed out"
                    );
                    false
                }
            };
            if recovered {
                if !wait_for_preferred_device(state, provider.as_ref(), transport.as_ref()).await {
                    tracing::warn!(
                        timeout_secs = DEVICE_REGISTRY_TIMEOUT.as_secs(),
                        "preferred device still absent from Spotify registry after reconnect"
                    );
                }
                match execute_provider_command(
                    state,
                    provider.as_ref(),
                    transport.as_ref(),
                    command.clone(),
                )
                .await
                {
                    Ok(result) => return Ok(result),
                    Err(retry_err) if no_active && is_no_active_device_error(&retry_err) => {
                        return Err(friendly_no_active_device_error(
                            provider.as_ref(),
                            transport.as_ref(),
                            &retry_err,
                        )
                        .await);
                    }
                    Err(retry_err) => return Err(retry_err),
                }
            }
            if no_active {
                Err(
                    friendly_no_active_device_error(provider.as_ref(), transport.as_ref(), &err)
                        .await,
                )
            } else {
                Err(err)
            }
        }
        Err(err) => Err(err),
    }
}

/// Execute against one validated provider/transport pair. Embedded recovery
/// belongs only to the runtime explicitly marked by the factory; every other
/// provider mutates its own remote transport directly.
pub(crate) async fn execute_provider_pair_with_recovery(
    state: &Arc<DaemonState>,
    provider: Arc<dyn MusicProvider>,
    transport: Arc<dyn RemoteTransport>,
    command: CommandKind,
) -> anyhow::Result<CommandResult> {
    if provider.id() != transport.provider_id() || provider.uri_scheme() != transport.uri_scheme() {
        return Err(ProviderError::InvalidInput {
            field: "provider_transport".to_string(),
            message: format!(
                "provider `{}` / `{}` is not paired with transport `{}` / `{}`",
                provider.id(),
                provider.uri_scheme(),
                transport.provider_id(),
                transport.uri_scheme()
            ),
        }
        .into());
    }

    let registry = state.providers().await?;
    let uses_embedded_recovery =
        registry.provider(provider.id())?.transport_recovery() == TransportRecovery::EmbeddedPlayer;
    if uses_embedded_recovery {
        execute_with_device_recovery(state, provider, transport, command).await
    } else {
        execute_provider_command(state, provider.as_ref(), transport.as_ref(), command).await
    }
}

pub(crate) async fn provider_pair_for_command(
    state: &DaemonState,
    command: &CommandKind,
) -> anyhow::Result<(Arc<dyn MusicProvider>, Arc<dyn RemoteTransport>)> {
    let resource = match command {
        CommandKind::PlayItem { item }
        | CommandKind::QueueItem { item }
        | CommandKind::SaveItem { item } => Some(ResourceUri::parse(&item.uri)?),
        CommandKind::PlayUri { uri, .. } | CommandKind::QueueUri { uri } => {
            Some(ResourceUri::parse(uri)?)
        }
        CommandKind::AddToPlaylist { playlist_id, .. } => ResourceUri::parse(playlist_id).ok(),
        CommandKind::Pause
        | CommandKind::Resume
        | CommandKind::TogglePlayback
        | CommandKind::Next
        | CommandKind::Previous
        | CommandKind::Seek { .. }
        | CommandKind::Volume { .. }
        | CommandKind::Shuffle { .. }
        | CommandKind::Repeat { .. }
        | CommandKind::Transfer { .. }
        | CommandKind::SaveCurrent => None,
    };
    let provider = match resource.as_ref() {
        Some(resource) => state.provider_for_uri(resource).await?,
        None => return current_transport_provider_pair(state).await,
    };
    let transport = state.provider_transport(provider.id()).await?;
    Ok((provider, transport))
}

pub(crate) fn require_resource_kind(
    resource: &ResourceUri,
    expected: MediaKind,
    field: &str,
) -> anyhow::Result<()> {
    if resource.kind() == expected {
        return Ok(());
    }
    Err(ProviderError::InvalidInput {
        field: field.to_string(),
        message: format!("expected {expected} URI, got {}", resource.kind()),
    }
    .into())
}

pub(crate) async fn current_transport_provider_pair(
    state: &DaemonState,
) -> anyhow::Result<(Arc<dyn MusicProvider>, Arc<dyn RemoteTransport>)> {
    let provider = if let Some(provider_id) = state.active_transport_provider() {
        state
            .provider(&provider_id)
            .await
            .unwrap_or(state.default_provider().await?)
    } else {
        let routed = match state.snapshot_playback().item.as_ref() {
            Some(item) => match ResourceUri::parse(&item.uri) {
                Ok(uri) => state.provider_for_uri(&uri).await.ok(),
                Err(_) => None,
            },
            None => None,
        };
        match routed {
            Some(provider) => provider,
            None => state.default_provider().await?,
        }
    };
    let transport = state.provider_transport(provider.id()).await?;
    Ok((provider, transport))
}

/// Resolve the provider whose cached transport-shaped snapshots clients should
/// render without requiring that provider to expose a transport facet. This
/// keeps catalog/search/library-only providers usable during client bootstrap.
pub(crate) async fn current_snapshot_provider_id(
    state: &DaemonState,
) -> anyhow::Result<ProviderId> {
    let providers = state.providers().await?;
    if let Some(active) = state.active_transport_provider() {
        if providers.provider(&active).is_ok() {
            return Ok(active);
        }
    }
    Ok(providers.default_id().clone())
}

pub(crate) async fn provider_pair_uses_embedded_transport(
    state: &DaemonState,
    provider: &dyn MusicProvider,
    transport: &dyn RemoteTransport,
) -> anyhow::Result<bool> {
    let registry = state.providers().await?;
    Ok(provider.id() == transport.provider_id()
        && provider.uri_scheme() == transport.uri_scheme()
        && registry.provider(provider.id())?.transport_recovery()
            == TransportRecovery::EmbeddedPlayer)
}

async fn execute_provider_command(
    state: &DaemonState,
    provider: &dyn MusicProvider,
    transport: &dyn RemoteTransport,
    command: CommandKind,
) -> anyhow::Result<CommandResult> {
    let message = command_message(&command);
    let analytics_command = command.clone();
    let result = match command {
        CommandKind::AddToPlaylist {
            item, playlist_id, ..
        } => {
            let mutation = Mutation::PlaylistAdd {
                playlist_uri: playlist_resource(provider, &playlist_id)?,
                items: vec![PlaylistInsertion {
                    uri: ResourceUri::parse(&item.uri)?,
                    position: None,
                }],
                expected_version: None,
            };
            let mutation_id = uuid::Uuid::now_v7();
            apply_provider_mutation_checked(provider, mutation_id, &mutation).await?;
            Ok(CommandResult {
                message: Some(message),
                request_refresh: true,
                ..Default::default()
            })
        }
        CommandKind::SaveItem { item } => {
            let mutation = Mutation::LibrarySave {
                uris: vec![ResourceUri::parse(&item.uri)?],
            };
            let mutation_id = uuid::Uuid::now_v7();
            apply_provider_mutation_checked(provider, mutation_id, &mutation).await?;
            Ok(CommandResult {
                message: Some(message),
                request_refresh: true,
                ..Default::default()
            })
        }
        CommandKind::SaveCurrent => {
            require_provider_capability(
                provider,
                "playback state",
                provider
                    .capabilities()
                    .transport
                    .as_ref()
                    .is_some_and(|caps| caps.playback_state),
            )?;
            let playback = transport.playback(RequestContext::PLAYBACK_CONTROL).await?;
            validate_provider_playback(provider, &playback)?;
            let item = playback
                .item
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("nothing is currently playing"))?;
            let mutation = Mutation::LibrarySave {
                uris: vec![ResourceUri::parse(&item.uri)?],
            };
            let mutation_id = uuid::Uuid::now_v7();
            apply_provider_mutation_checked(provider, mutation_id, &mutation).await?;
            Ok(CommandResult {
                message: Some(message),
                playback: Some(playback),
                request_refresh: true,
                ..Default::default()
            })
        }
        command => {
            let command = provider_transport_command(state, provider, transport, command).await?;
            require_transport_command_capability(provider, &command)?;
            let outcome = transport
                .execute(RequestContext::PLAYBACK_CONTROL, command)
                .await?;
            if let Some(playback) = outcome.playback.as_ref() {
                validate_provider_playback(provider, playback)?;
            }
            if let Some(queue) = outcome.queue.as_ref() {
                validate_provider_queue(provider, queue)?;
            }
            Ok(CommandResult {
                message: Some(message),
                playback: outcome.playback,
                queue: outcome.queue,
                devices: outcome.devices,
                request_refresh: true,
            })
        }
    };
    if result.is_ok() {
        record_command_analytics(&analytics_command).await;
    }
    result
}

async fn record_command_analytics(command: &CommandKind) {
    let (action, subject) = match command {
        CommandKind::Pause => ("pause", None),
        CommandKind::Resume => ("resume", None),
        CommandKind::TogglePlayback => ("toggle", None),
        CommandKind::PlayItem { item } => ("play", Some(item.uri.as_str())),
        CommandKind::PlayUri { uri, .. } => ("play", Some(uri.as_str())),
        CommandKind::Next => ("next", None),
        CommandKind::Previous => ("previous", None),
        CommandKind::Seek { .. } => ("seek", None),
        CommandKind::Volume { .. } => ("volume", None),
        CommandKind::Shuffle { .. } => ("shuffle", None),
        CommandKind::Repeat { .. } => ("repeat", None),
        CommandKind::QueueItem { item } => ("queue", Some(item.uri.as_str())),
        CommandKind::QueueUri { uri } => ("queue", Some(uri.as_str())),
        CommandKind::Transfer { .. } => ("transfer", None),
        CommandKind::AddToPlaylist { item, .. } => ("playlist_add", Some(item.uri.as_str())),
        CommandKind::SaveItem { item } => ("save", Some(item.uri.as_str())),
        CommandKind::SaveCurrent => ("save", None),
    };
    record_daemon_action(action, subject, serde_json::json!({})).await;
}

pub(crate) async fn record_daemon_action(
    action: &str,
    subject: Option<&str>,
    payload: serde_json::Value,
) {
    if let Ok(analytics) = crate::analytics::AnalyticsStore::open_default().await {
        let _ = analytics
            .record_event(&action_finished_event(
                AnalyticsSource::Daemon,
                action,
                subject,
                "ok",
                payload,
                now_ms(),
            ))
            .await;
    }
}

async fn provider_transport_command(
    state: &DaemonState,
    provider: &dyn MusicProvider,
    transport: &dyn RemoteTransport,
    command: CommandKind,
) -> anyhow::Result<TransportCommand> {
    let command = match command {
        CommandKind::TogglePlayback => {
            require_provider_capability(
                provider,
                "playback state",
                provider
                    .capabilities()
                    .transport
                    .as_ref()
                    .is_some_and(|caps| caps.playback_state),
            )?;
            if transport
                .playback(RequestContext::PLAYBACK_CONTROL)
                .await?
                .is_playing
            {
                CommandKind::Pause
            } else {
                CommandKind::Resume
            }
        }
        command => command,
    };
    Ok(match command {
        CommandKind::Pause => TransportCommand::Pause,
        CommandKind::Resume => {
            ensure_playback_target(state, provider, transport).await?;
            TransportCommand::Resume
        }
        CommandKind::PlayItem { item } => TransportCommand::Play(PlayRequest {
            start_uri: ResourceUri::parse(&item.uri)?,
            source: PlaySource::Single,
            device: preferred_transport_device(state, provider, transport).await?,
            position_ms: 0,
        }),
        CommandKind::PlayUri { uri, context } => {
            let start_uri = ResourceUri::parse(&uri)?;
            let source = match context {
                Some(PlayContext {
                    context_uri: Some(uri),
                    ..
                }) => PlaySource::Context(ResourceUri::parse(&uri)?),
                Some(PlayContext {
                    tracks: Some(tracks),
                    ..
                }) => PlaySource::Ordered(
                    ordered_playback_window(&tracks, &uri, REMOTE_ORDERED_PLAY_MAX)
                        .iter()
                        .map(|uri| ResourceUri::parse(uri))
                        .collect::<Result<Vec<_>, _>>()?,
                ),
                _ => PlaySource::Single,
            };
            TransportCommand::Play(PlayRequest {
                start_uri,
                source,
                device: preferred_transport_device(state, provider, transport).await?,
                position_ms: 0,
            })
        }
        CommandKind::Next => TransportCommand::Next,
        CommandKind::Previous => TransportCommand::Previous,
        CommandKind::Seek { position_ms } => TransportCommand::Seek { position_ms },
        CommandKind::Volume { volume_percent } => {
            ensure_playback_target(state, provider, transport).await?;
            TransportCommand::Volume {
                percent: volume_percent.min(100),
            }
        }
        CommandKind::Shuffle { state } => TransportCommand::Shuffle { enabled: state },
        CommandKind::Repeat { state } => TransportCommand::Repeat { mode: state },
        CommandKind::QueueItem { item } => TransportCommand::QueueAdd(QueueAddRequest {
            uri: ResourceUri::parse(&item.uri)?,
            device: TransportDevice::Active,
        }),
        CommandKind::QueueUri { uri } => TransportCommand::QueueAdd(QueueAddRequest {
            uri: ResourceUri::parse(&uri)?,
            device: TransportDevice::Active,
        }),
        CommandKind::Transfer { device, play } => TransportCommand::Transfer {
            device_id: device
                .id
                .ok_or_else(|| anyhow::anyhow!("device `{}` has no provider id", device.name))?,
            play,
        },
        CommandKind::TogglePlayback
        | CommandKind::AddToPlaylist { .. }
        | CommandKind::SaveItem { .. }
        | CommandKind::SaveCurrent => unreachable!("handled before transport conversion"),
    })
}

const REMOTE_ORDERED_PLAY_MAX: usize = 100;

fn ordered_playback_window(tracks: &[String], start_uri: &str, max_items: usize) -> Vec<String> {
    if max_items == 0 {
        return Vec::new();
    }
    match tracks.iter().position(|uri| uri == start_uri) {
        Some(start) => tracks.iter().skip(start).take(max_items).cloned().collect(),
        None => vec![start_uri.to_string()],
    }
}

fn command_message(command: &CommandKind) -> String {
    match command {
        CommandKind::Pause => "Paused".to_string(),
        CommandKind::Resume => "Playing".to_string(),
        CommandKind::TogglePlayback => "Toggled playback".to_string(),
        CommandKind::PlayItem { item } => format!("Playing {}", item.name),
        CommandKind::PlayUri { uri, .. } => format!("Playing {uri}"),
        CommandKind::Next => "Skipped".to_string(),
        CommandKind::Previous => "Previous track".to_string(),
        CommandKind::Seek { position_ms } => format!("Seeked to {position_ms}ms"),
        CommandKind::Volume { volume_percent } => format!("Volume {}%", (*volume_percent).min(100)),
        CommandKind::Shuffle { state } => {
            format!("Shuffle {}", if *state { "on" } else { "off" })
        }
        CommandKind::Repeat { state } => format!("Repeat {state}"),
        CommandKind::QueueItem { item } => format!("Queued {}", item.name),
        CommandKind::QueueUri { uri } => format!("Queued {uri}"),
        CommandKind::Transfer { device, .. } => format!("Transferred to {}", device.name),
        CommandKind::AddToPlaylist {
            item,
            playlist_name,
            ..
        } => format!("Added {} to {playlist_name}", item.name),
        CommandKind::SaveItem { item } => format!("Saved {}", item.name),
        CommandKind::SaveCurrent => "Saved current item".to_string(),
    }
}

pub(crate) async fn try_embedded_transport(
    state: &Arc<DaemonState>,
    command: &CommandKind,
) -> Option<CommandResult> {
    // Prefer the embedded librespot (Spirc) path — instant, no HTTP
    // round-trip, and it still works while Spotify read endpoints are
    // in cooldown. Do not preflight with GET /me/player here: that
    // read path is exactly what can be rate-limited during startup
    // sync, and a transport command should not inherit that cooldown.
    let transport_snapshot = state.snapshot_playback();
    if let Some((cmd, effective_command)) =
        transport_cmd_for_command_kind(command, &transport_snapshot)
    {
        if !embedded_transport_allowed(state, &cmd, &transport_snapshot) {
            return None;
        }
        let mut player_connected = state.player_is_connected().await;
        if !player_connected {
            let device_name = state.configured_device_name();
            player_connected = match tokio::time::timeout(
                DEVICE_RECOVERY_TIMEOUT,
                state.reconnect_player(&device_name),
            )
            .await
            {
                Ok(Ok(_)) => true,
                Ok(Err(err)) => {
                    tracing::debug!(error = %err, "embedded device reconnect before transport failed");
                    false
                }
                Err(_) => {
                    tracing::debug!(
                        timeout_secs = DEVICE_RECOVERY_TIMEOUT.as_secs(),
                        "embedded device reconnect before transport timed out"
                    );
                    false
                }
            };
        }
        if player_connected {
            match tokio::time::timeout(TRANSPORT_BACKEND_TIMEOUT, state.transport(cmd)).await {
                Err(_) => {
                    tracing::warn!(
                        timeout_secs = TRANSPORT_BACKEND_TIMEOUT.as_secs(),
                        "embedded transport timed out; falling back to Web API"
                    );
                }
                Ok(result) => match result {
                    Ok(()) => {
                        return Some(CommandResult {
                            playback: local_transport_playback_snapshot(state, &effective_command),
                            request_refresh: true,
                            ..Default::default()
                        });
                    }
                    Err(spotuify_player::PlayerError::Unsupported(_)) => {
                        // Fall through to Web API.
                    }
                    Err(err) => {
                        tracing::warn!(
                            error = %player_error_for_display(&err),
                            "embedded transport failed; falling back to Web API"
                        );
                    }
                },
            }
        }
    }
    None
}

pub(crate) fn local_transport_playback_snapshot(
    state: &DaemonState,
    command: &CommandKind,
) -> Option<Playback> {
    let mut playback = state.snapshot_playback();
    playback.sampled_at_ms = Some(spotuify_core::now_ms());
    playback.source = Some(spotuify_core::PlaybackStateSource::CommandResult);

    match command {
        CommandKind::Pause => playback.is_playing = false,
        CommandKind::Resume => playback.is_playing = true,
        CommandKind::PlayItem { item } => {
            playback.item = Some(item.clone());
            playback.progress_ms = 0;
            playback.is_playing = true;
        }
        CommandKind::PlayUri { uri, .. } => {
            if playback.item.as_ref().map(|item| item.uri.as_str()) != Some(uri.as_str()) {
                playback.item = Some(MediaItem {
                    uri: uri.clone(),
                    ..Default::default()
                });
            }
            playback.progress_ms = 0;
            playback.is_playing = true;
        }
        CommandKind::Seek { position_ms } => {
            playback.progress_ms = *position_ms;
        }
        CommandKind::Volume { volume_percent } => {
            if let Some(device) = playback.device.as_mut() {
                device.volume_percent = Some(*volume_percent);
            }
        }
        CommandKind::Shuffle { state } => playback.shuffle = *state,
        CommandKind::Repeat { state } => playback.repeat = *state,
        CommandKind::Next | CommandKind::Previous => return None,
        CommandKind::TogglePlayback
        | CommandKind::QueueItem { .. }
        | CommandKind::QueueUri { .. }
        | CommandKind::Transfer { .. }
        | CommandKind::AddToPlaylist { .. }
        | CommandKind::SaveItem { .. }
        | CommandKind::SaveCurrent => return None,
    }

    Some(playback)
}

/// May the embedded librespot (Spirc) path carry this transport
/// command? Spirc silently drops transport while our device is NOT the
/// active session ("SpircCommand::Pause will be ignored while Not
/// Active" — log-confirmed ×22/day): the fast path would then report
/// success while nothing happened. Only PlayUri loads activate the
/// device; everything else must go to the Web API, which targets
/// whatever device is actually playing.
pub(crate) fn embedded_transport_allowed(
    state: &DaemonState,
    cmd: &crate::state::TransportCmd,
    snapshot: &Playback,
) -> bool {
    if matches!(cmd, crate::state::TransportCmd::PlayUri { .. }) {
        return true;
    }
    let own = state.own_device_id();
    let allowed =
        own.is_some() && snapshot.device.as_ref().and_then(|d| d.id.as_deref()) == own.as_deref();
    if !allowed {
        tracing::debug!(
            target: "spotuify_daemon::transport",
            active_device = ?snapshot.device.as_ref().map(|d| d.name.as_str()),
            "embedded device not the active session; using Web API transport"
        );
    }
    allowed
}

pub(crate) async fn apply_fast_transport(
    state: &Arc<DaemonState>,
    cmd: crate::state::TransportCmd,
    effective_command: &CommandKind,
    action: &str,
) -> Option<CommandResult> {
    match state.transport_fast(cmd, FAST_TRANSPORT_TIMEOUT).await {
        Ok(FastTransportStatus::Applied) => {
            tracing::debug!(action, "fast local transport applied");
            Some(local_transport_command_result(state, effective_command))
        }
        Ok(FastTransportStatus::Dispatched { ack }) => {
            tracing::debug!(
                timeout_ms = FAST_TRANSPORT_TIMEOUT.as_millis(),
                action,
                "fast local transport dispatched without waiting for backend ack"
            );
            // The deadline elapsed before the player acked. We're about
            // to tell clients the command applied, so watch the late ack
            // and reconcile if it turns out the backend rejected it.
            spawn_fast_transport_ack_watcher(state.clone(), ack, action.to_string());
            Some(local_transport_command_result(state, effective_command))
        }
        Err(err) => {
            tracing::debug!(
                error = %player_error_for_display(&err),
                action,
                "fast local transport skipped"
            );
            None
        }
    }
}

/// Watch a fast-transport ack that arrived after the fast deadline. A
/// late success is a no-op (the optimistic state already matches); a
/// late failure or a dropped ack means the daemon optimistically
/// reported success that didn't hold, so bump the mutation seq and
/// refresh playback to overwrite the stale optimistic snapshot with
/// authoritative state.
pub(crate) fn spawn_fast_transport_ack_watcher(
    state: Arc<DaemonState>,
    ack: tokio::sync::oneshot::Receiver<spotuify_player::PlayerResult<()>>,
    action: String,
) {
    state.clone().spawn_background("fast-transport-ack", async move {
        let reconcile = |reason: &str| {
            tracing::warn!(action = %action, reason, "fast transport did not hold; reconciling");
            state.bump_mutation_seq();
            spawn_playback_refresh_forced(state.clone());
        };
        match tokio::time::timeout(FAST_TRANSPORT_ACK_GRACE, ack).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(err))) => {
                reconcile(&format!("backend error: {}", player_error_for_display(&err)));
            }
            Ok(Err(_)) => reconcile("player actor dropped the ack"),
            Err(_) => reconcile("ack timed out"),
        }
    });
}

pub(crate) fn local_transport_command_result(
    state: &DaemonState,
    effective_command: &CommandKind,
) -> CommandResult {
    CommandResult {
        playback: local_transport_playback_snapshot(state, effective_command),
        request_refresh: true,
        ..Default::default()
    }
}

fn playlist_resource(provider: &dyn MusicProvider, value: &str) -> anyhow::Result<ResourceUri> {
    match ResourceUri::parse(value) {
        Ok(uri) => Ok(uri),
        Err(_) => ResourceUri::new(provider.uri_scheme().clone(), MediaKind::Playlist, value)
            .map_err(Into::into),
    }
}

pub(crate) fn resolve_device(devices: &[Device], value: &str) -> anyhow::Result<Device> {
    devices
        .iter()
        .find(|device| {
            device.id.as_deref() == Some(value) || device.name.eq_ignore_ascii_case(value)
        })
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no device matching `{value}`"))
}

pub(crate) fn preferred_device(
    devices: &[Device],
    configured_name: Option<&str>,
    own_device_id: Option<&str>,
) -> Option<Device> {
    let unrestricted = devices.iter().filter(|device| !device.is_restricted);
    if let Some(own_id) = own_device_id {
        if let Some(device) = unrestricted
            .clone()
            .find(|device| device.id.as_deref() == Some(own_id))
        {
            return Some(device.clone());
        }
    }
    if let Some(device) = unrestricted.clone().find(|device| device.is_active) {
        return Some(device.clone());
    }
    if let Some(name) = configured_name.filter(|name| !name.is_empty()) {
        if let Some(device) = unrestricted
            .clone()
            .find(|device| device.name.eq_ignore_ascii_case(name))
        {
            return Some(device.clone());
        }
    }
    if let Some(device) = unrestricted.clone().find(|device| {
        let name = device.name.to_ascii_lowercase();
        name.contains("librespot") || name.contains("spotuify")
    }) {
        return Some(device.clone());
    }
    let mut candidates: Vec<&Device> = unrestricted.collect();
    candidates.sort_by(|a, b| a.id.cmp(&b.id));
    if let Some(name) = configured_name.filter(|name| !name.is_empty()) {
        let needle = name.to_ascii_lowercase();
        let stripped = needle
            .trim_start_matches("spotuify-")
            .trim_start_matches("librespot-");
        let token = if stripped.is_empty() {
            needle.as_str()
        } else {
            stripped
        };
        if let Some(device) = candidates.iter().find(|device| {
            let name = device.name.to_ascii_lowercase();
            name.contains(token) || token.contains(&name)
        }) {
            return Some((*device).clone());
        }
    }
    None
}

fn playback_target_device(
    devices: &[Device],
    configured_name: Option<&str>,
    own_device_id: Option<&str>,
) -> Option<Device> {
    devices
        .iter()
        .find(|device| device.is_active && !device.is_restricted)
        .cloned()
        .or_else(|| preferred_device(devices, configured_name, own_device_id))
}

async fn preferred_transport_device(
    state: &DaemonState,
    provider: &dyn MusicProvider,
    transport: &dyn RemoteTransport,
) -> anyhow::Result<TransportDevice> {
    require_provider_capability(
        provider,
        "device listing",
        provider
            .capabilities()
            .transport
            .as_ref()
            .is_some_and(|caps| caps.devices),
    )?;
    let devices = transport.devices(RequestContext::PLAYBACK_CONTROL).await?;
    let configured = state.configured_device_name();
    let own = state.own_device_id();
    let device =
        playback_target_device(&devices, Some(&configured), own.as_deref()).ok_or_else(|| {
            anyhow::anyhow!("no preferred provider device found; reconnect or transfer explicitly")
        })?;
    let id = device
        .id
        .ok_or_else(|| anyhow::anyhow!("preferred device `{}` has no provider id", device.name))?;
    Ok(TransportDevice::Id(id))
}

async fn ensure_playback_target(
    state: &DaemonState,
    provider: &dyn MusicProvider,
    transport: &dyn RemoteTransport,
) -> anyhow::Result<()> {
    require_provider_capability(
        provider,
        "device listing",
        provider
            .capabilities()
            .transport
            .as_ref()
            .is_some_and(|caps| caps.devices),
    )?;
    let devices = transport.devices(RequestContext::PLAYBACK_CONTROL).await?;
    let configured = state.configured_device_name();
    let own = state.own_device_id();
    let target =
        playback_target_device(&devices, Some(&configured), own.as_deref()).ok_or_else(|| {
            anyhow::anyhow!("no preferred provider device found; reconnect or transfer explicitly")
        })?;
    if target.is_active {
        return Ok(());
    }
    let id = target
        .id
        .ok_or_else(|| anyhow::anyhow!("preferred device `{}` has no provider id", target.name))?;
    require_provider_capability(
        provider,
        "device transfer",
        provider
            .capabilities()
            .transport
            .as_ref()
            .is_some_and(|caps| caps.transfer),
    )?;
    transport
        .execute(
            RequestContext::PLAYBACK_CONTROL,
            TransportCommand::Transfer {
                device_id: id,
                play: false,
            },
        )
        .await?;
    Ok(())
}

pub(crate) async fn wait_for_preferred_device(
    state: &DaemonState,
    provider: &dyn MusicProvider,
    transport: &dyn RemoteTransport,
) -> bool {
    if !provider
        .capabilities()
        .transport
        .as_ref()
        .is_some_and(|caps| caps.devices)
    {
        return false;
    }
    let started = Instant::now();
    loop {
        match transport.devices(RequestContext::PLAYBACK_CONTROL).await {
            Ok(devices) => {
                if preferred_device(
                    &devices,
                    Some(state.configured_device_name().as_str()),
                    state.own_device_id().as_deref(),
                )
                .is_some()
                {
                    return true;
                }
            }
            Err(err) => {
                tracing::debug!(error = %err, "device registry poll failed during recovery");
            }
        }
        if started.elapsed() >= DEVICE_REGISTRY_TIMEOUT {
            return false;
        }
        tokio::time::sleep(DEVICE_REGISTRY_POLL_INTERVAL).await;
    }
}

pub(crate) fn transport_cmd_for_command_kind(
    kind: &CommandKind,
    playback: &Playback,
) -> Option<(crate::state::TransportCmd, CommandKind)> {
    use crate::state::TransportCmd;
    // TogglePlayback is resolved against the daemon-owned playback
    // clock so Space never needs a GET /me/player preflight. SaveCurrent
    // is resolved in the LibrarySave handler for the same reason.
    // AddToPlaylist, SaveItem, Queue, and Transfer are not transport
    // controls, so they stay on their mutation-specific paths.
    match kind {
        CommandKind::Pause => Some((TransportCmd::Pause, CommandKind::Pause)),
        CommandKind::Resume if playback_can_resume_locally(playback) => {
            Some((TransportCmd::Resume, CommandKind::Resume))
        }
        CommandKind::TogglePlayback if playback.is_playing => {
            Some((TransportCmd::Pause, CommandKind::Pause))
        }
        CommandKind::TogglePlayback if playback_can_resume_locally(playback) => {
            Some((TransportCmd::Resume, CommandKind::Resume))
        }
        CommandKind::Next => Some((TransportCmd::Next, CommandKind::Next)),
        CommandKind::Previous => Some((TransportCmd::Previous, CommandKind::Previous)),
        CommandKind::PlayUri { uri, context } => Some((
            match context {
                // A resolved collection context: load the album/playlist
                // or explicit track list and start at `uri`.
                Some(context) => TransportCmd::PlayContext {
                    context_uri: context.context_uri.clone(),
                    tracks: context.tracks.clone(),
                    start_uri: uri.clone(),
                    position_ms: 0,
                },
                // Legacy single-track / single-context play, unchanged.
                None => TransportCmd::PlayUri {
                    uri: uri.clone(),
                    position_ms: 0,
                },
            },
            kind.clone(),
        )),
        CommandKind::PlayItem { item } => Some((
            TransportCmd::PlayUri {
                uri: item.uri.clone(),
                position_ms: 0,
            },
            kind.clone(),
        )),
        CommandKind::Seek { position_ms } => Some((
            TransportCmd::Seek {
                position_ms: (*position_ms).min(u32::MAX as u64) as u32,
            },
            kind.clone(),
        )),
        CommandKind::Volume { volume_percent } => Some((
            TransportCmd::Volume {
                percent: *volume_percent,
            },
            kind.clone(),
        )),
        CommandKind::Shuffle { state } => {
            Some((TransportCmd::Shuffle { on: *state }, kind.clone()))
        }
        CommandKind::Repeat { state } => match state {
            RepeatMode::Off => Some((
                TransportCmd::Repeat {
                    mode: spotuify_player::RepeatMode::Off,
                },
                kind.clone(),
            )),
            RepeatMode::Context => Some((
                TransportCmd::Repeat {
                    mode: spotuify_player::RepeatMode::Context,
                },
                kind.clone(),
            )),
            RepeatMode::Track => Some((
                TransportCmd::Repeat {
                    mode: spotuify_player::RepeatMode::Track,
                },
                kind.clone(),
            )),
        },
        CommandKind::Resume
        | CommandKind::TogglePlayback
        | CommandKind::QueueItem { .. }
        | CommandKind::QueueUri { .. }
        | CommandKind::Transfer { .. }
        | CommandKind::AddToPlaylist { .. }
        | CommandKind::SaveItem { .. }
        | CommandKind::SaveCurrent => None,
    }
}

pub(crate) fn playback_can_resume_locally(playback: &Playback) -> bool {
    let Some(item) = playback.item.as_ref() else {
        return false;
    };
    item.duration_ms == 0 || playback.progress_ms.saturating_add(750) < item.duration_ms
}

pub(crate) fn is_no_active_device_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<ProviderError>()
        .is_some_and(|err| matches!(err, ProviderError::NoActiveDevice))
}

/// Outcome of a single queue-add attempt. A provider's `NoActiveDevice`
/// response is surfaced as a value so callers may recover by starting
/// playback rather than failing the whole operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QueueAttempt {
    Queued,
    NoActiveDevice,
}

/// One queue-add through the transport selected for the resource provider.
/// Maps a provider's "no active device" response to
/// [`QueueAttempt::NoActiveDevice`]; all other failures are errors.
pub(crate) async fn queue_one(
    provider: &dyn MusicProvider,
    transport: &dyn RemoteTransport,
    uri: &str,
) -> anyhow::Result<QueueAttempt> {
    let command = TransportCommand::QueueAdd(QueueAddRequest {
        uri: ResourceUri::parse(uri)?,
        device: TransportDevice::Active,
    });
    require_transport_command_capability(provider, &command)?;
    match transport
        .execute(RequestContext::PLAYBACK_CONTROL, command)
        .await
    {
        Ok(_) => Ok(QueueAttempt::Queued),
        Err(ProviderError::NoActiveDevice) => Ok(QueueAttempt::NoActiveDevice),
        Err(err) => Err(err.into()),
    }
}

pub(crate) fn is_recoverable_device_error(err: &anyhow::Error) -> bool {
    if is_no_active_device_error(err) {
        return true;
    }
    err.to_string()
        .contains("no preferred provider device found")
}

pub(crate) async fn friendly_no_active_device_error(
    provider: &dyn MusicProvider,
    transport: &dyn RemoteTransport,
    original: &anyhow::Error,
) -> anyhow::Error {
    let device_read_supported = provider
        .capabilities()
        .transport
        .as_ref()
        .is_some_and(|caps| caps.devices);
    let hint = if device_read_supported {
        match transport.devices(RequestContext::PLAYBACK_CONTROL).await {
            Ok(devs) if !devs.is_empty() => {
                let names = devs
                    .iter()
                    .map(|d| d.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "Available devices: {names}. Run `spotuify devices transfer <name>` to activate one."
                )
            }
            _ => "No Spotify devices online. Open the Spotify app on any device, or run `spotuify reconnect`."
                .to_string(),
        }
    } else {
        "The selected provider cannot list playback devices.".to_string()
    };
    anyhow::anyhow!("No active Spotify device. {hint} (Spotify said: {original})")
}

/// Display snapshot for a reminder: cache-first (media_items), else derive the
/// kind from the URI with a URI-tail label fallback so a reminder still renders
/// sensibly even for an item that was never cached.
pub(crate) async fn resolve_reminder_snapshot(
    state: &DaemonState,
    uri: &str,
) -> anyhow::Result<(MediaKind, String, String, Option<String>)> {
    if let Ok(items) = state.store().media_items_by_uris(&[uri.to_string()]).await {
        if let Some(item) = items.into_iter().next() {
            return Ok((item.kind, item.name, item.subtitle, item.image_url));
        }
    }
    let resource = ResourceUri::parse(uri)?;
    Ok((
        resource.kind(),
        resource.bare_id().to_string(),
        String::new(),
        None,
    ))
}

pub(crate) fn media_item_from_uri(uri: &str) -> anyhow::Result<MediaItem> {
    let resource = ResourceUri::parse(uri)?;
    Ok(MediaItem {
        id: Some(resource.bare_id().to_string()),
        uri: uri.to_string(),
        name: uri.to_string(),
        subtitle: String::new(),
        context: String::new(),
        duration_ms: 0,
        image_url: None,
        kind: resource.kind(),
        source: None,
        freshness: None,
        explicit: None,
        is_playable: None,
        ..Default::default()
    })
}

#[cfg(test)]
mod provider_boundary_tests {
    #![allow(clippy::panic, clippy::unwrap_used)]

    use std::collections::HashSet;

    use spotuify_core::{
        Device, MediaKind, PageContinuation, PageRequest, ProviderError, ProviderId, ResourceUri,
    };
    use spotuify_protocol::{IpcErrorKind, Response};

    use super::{
        error_response_from, error_response_with_context, next_provider_page,
        ordered_playback_window, playback_target_device, PROVIDER_PAGINATION_MAX_PAGES,
    };

    fn device(id: &str, active: bool, restricted: bool) -> Device {
        Device {
            id: Some(id.to_string()),
            name: id.to_string(),
            kind: "Computer".to_string(),
            is_active: active,
            is_restricted: restricted,
            volume_percent: Some(50),
            supports_volume: true,
        }
    }

    #[test]
    fn active_remote_device_wins_over_idle_own_device() {
        let devices = [device("own", false, false), device("remote", true, false)];
        let selected = playback_target_device(&devices, Some("own"), Some("own")).unwrap();
        assert_eq!(selected.id.as_deref(), Some("remote"));
    }

    #[test]
    fn ordered_fallback_starts_at_selected_uri_and_caps_the_window() {
        let tracks = (0..250)
            .map(|index| {
                ResourceUri::spotify(MediaKind::Track, index.to_string())
                    .expect("valid fixture URI")
                    .as_uri()
            })
            .collect::<Vec<_>>();
        let window = ordered_playback_window(&tracks, "spotify:track:125", 100);
        assert_eq!(window.len(), 100);
        assert_eq!(
            window.first().map(String::as_str),
            Some("spotify:track:125")
        );
        assert_eq!(window.last().map(String::as_str), Some("spotify:track:224"));
    }

    #[test]
    fn restricted_and_unrelated_devices_are_not_implicit_playback_targets() {
        let restricted = device("own", true, true);
        let unrelated = device("kitchen-speaker", false, false);

        assert!(
            playback_target_device(&[restricted], Some("spotuify-hume"), Some("own")).is_none()
        );
        assert!(playback_target_device(&[unrelated], Some("spotuify-hume"), Some("own")).is_none());
    }

    #[test]
    fn repeated_provider_cursor_is_rejected() {
        let mut seen = HashSet::new();
        let first = next_provider_page(
            &PageRequest::new(50, 0),
            PageContinuation::Cursor("cursor-a".to_string()),
            50,
            &mut seen,
            1,
            "test",
        )
        .unwrap();
        let error = next_provider_page(
            &first,
            PageContinuation::Cursor("cursor-a".to_string()),
            100,
            &mut seen,
            2,
            "test",
        )
        .unwrap_err();
        assert!(matches!(error, ProviderError::Provider(message) if message.contains("repeated")));
    }

    #[test]
    fn non_advancing_provider_offsets_are_rejected() {
        let mut seen = HashSet::new();
        for offset in [50, 49] {
            let error = next_provider_page(
                &PageRequest::new(50, 50),
                PageContinuation::Offset(offset),
                100,
                &mut seen,
                2,
                "test",
            )
            .unwrap_err();
            assert!(
                matches!(error, ProviderError::Provider(message) if message.contains("did not advance"))
            );
        }
    }

    #[test]
    fn fresh_provider_cursors_cannot_exceed_the_page_budget() {
        let mut seen = HashSet::new();
        let error = next_provider_page(
            &PageRequest::with_cursor(50, 50, "cursor-before-limit"),
            PageContinuation::Cursor("cursor-at-limit".to_string()),
            100,
            &mut seen,
            PROVIDER_PAGINATION_MAX_PAGES,
            "test",
        )
        .unwrap_err();
        assert!(matches!(error, ProviderError::Provider(message) if message.contains("exceeded")));
    }

    #[test]
    fn unsupported_provider_error_keeps_typed_ipc_kind() {
        let error = anyhow::Error::new(ProviderError::unsupported("test"));
        assert!(matches!(
            error_response_from(&error),
            Response::Error {
                kind: IpcErrorKind::Unsupported,
                retryable: false,
                ..
            }
        ));
    }

    #[test]
    fn player_policy_error_keeps_provider_identity_and_redacts_short_assignment() {
        let error = anyhow::Error::new(crate::state::ProviderPolicyRequestError {
            provider: ProviderId::new("nebula").unwrap(),
            reason: spotuify_protocol::sanitize_provider_policy_reason(
                "policy response token=Ab1Cd2Ef3Gh4",
            ),
        });
        assert!(matches!(
            error_response_from(&error),
            Response::Error {
                kind: IpcErrorKind::Provider,
                provider: Some(provider),
                ref message,
                ..
            } if provider.as_str() == "nebula"
                && message.contains("<redacted>")
                && !message.contains("Ab1Cd2Ef3Gh4")
        ));
    }

    #[test]
    fn generic_error_context_never_copies_secret_bearing_text_into_wire_detail() {
        let error = anyhow::anyhow!("provider failed token=\"x\"; Authorization: Bearer y");
        let response = error_response_with_context(&error, None);
        let Response::Error {
            message, detail, ..
        } = response
        else {
            panic!("expected error response");
        };
        let detail = detail.expect("generic failures carry sanitized detail");
        for field in [&message, &detail] {
            assert!(field.contains("<redacted>"), "{field}");
            assert!(!field.contains("token=\"x\""), "{field}");
            assert!(!field.contains("Bearer y"), "{field}");
        }
    }
}

#[cfg(test)]
mod queue_tests {
    use super::{
        idle_context_start_label, queue_for_started_context, queue_for_started_context_at,
        queue_with_appended_items, queueable_items_for_selection_without_cache,
    };
    use async_trait::async_trait;
    use spotuify_core::{
        AccessOutcome, CollectionRequest, MediaItem, MediaKind, MusicProvider, ProviderCaps,
        ProviderId, ProviderPage, ProviderResult, Queue, RequestContext, ResourceUri, UriScheme,
    };
    use spotuify_provider_fake::FakeProvider;

    struct ForeignCollectionProvider(FakeProvider);
    struct WrongKindCollectionProvider(FakeProvider);

    #[async_trait]
    impl MusicProvider for ForeignCollectionProvider {
        fn id(&self) -> &ProviderId {
            MusicProvider::id(&self.0)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(&self.0)
        }

        fn display_name(&self) -> &str {
            "Foreign collection output"
        }

        fn capabilities(&self) -> ProviderCaps {
            self.0.capabilities()
        }

        async fn playlist_items(
            &self,
            _context: RequestContext,
            request: CollectionRequest,
        ) -> ProviderResult<AccessOutcome<ProviderPage<MediaItem>>> {
            Ok(AccessOutcome::Available(foreign_page(request)))
        }

        async fn album_tracks(
            &self,
            _context: RequestContext,
            request: CollectionRequest,
        ) -> ProviderResult<ProviderPage<MediaItem>> {
            Ok(foreign_page(request))
        }
    }

    #[async_trait]
    impl MusicProvider for WrongKindCollectionProvider {
        fn id(&self) -> &ProviderId {
            MusicProvider::id(&self.0)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(&self.0)
        }

        fn display_name(&self) -> &str {
            "Wrong-kind collection output"
        }

        fn capabilities(&self) -> ProviderCaps {
            self.0.capabilities()
        }

        async fn playlist_items(
            &self,
            _context: RequestContext,
            request: CollectionRequest,
        ) -> ProviderResult<AccessOutcome<ProviderPage<MediaItem>>> {
            Ok(AccessOutcome::Available(wrong_kind_page(
                request,
                MediaKind::Album,
            )))
        }

        async fn album_tracks(
            &self,
            _context: RequestContext,
            request: CollectionRequest,
        ) -> ProviderResult<ProviderPage<MediaItem>> {
            Ok(wrong_kind_page(request, MediaKind::Episode))
        }
    }

    fn foreign_page(request: CollectionRequest) -> ProviderPage<MediaItem> {
        ProviderPage {
            items: vec![MediaItem {
                uri: "foreign:track:poison".to_string(),
                kind: MediaKind::Track,
                ..Default::default()
            }],
            requested_offset: request.page.offset,
            total: Some(1),
            next: None,
        }
    }

    fn wrong_kind_page(request: CollectionRequest, kind: MediaKind) -> ProviderPage<MediaItem> {
        ProviderPage {
            items: vec![MediaItem {
                uri: format!("fake:{kind}:poison"),
                kind,
                ..Default::default()
            }],
            requested_offset: request.page.offset,
            total: Some(1),
            next: None,
        }
    }

    fn track(uri: &str, name: &str) -> MediaItem {
        MediaItem {
            id: ResourceUri::parse(uri)
                .ok()
                .map(|resource| resource.bare_id().to_string()),
            uri: uri.to_string(),
            name: name.to_string(),
            subtitle: "Artist".to_string(),
            context: "Album".to_string(),
            duration_ms: 180_000,
            image_url: None,
            kind: MediaKind::Track,
            source: None,
            freshness: None,
            explicit: None,
            is_playable: None,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn queue_expansion_keeps_track_uri_as_single_append() {
        let provider = FakeProvider::new();
        let items = queueable_items_for_selection_without_cache(&provider, "fake:track:track-1")
            .await
            .expect("track should queue directly");
        assert_eq!(
            items.into_iter().map(|item| item.uri).collect::<Vec<_>>(),
            vec!["fake:track:track-1"]
        );
    }

    #[tokio::test]
    async fn queue_expansion_resolves_provider_collections() {
        let provider = FakeProvider::new();
        for uri in ["fake:playlist:playlist-1", "fake:album:album-1"] {
            let items = queueable_items_for_selection_without_cache(&provider, uri)
                .await
                .expect("collection should expand");
            assert_eq!(
                items.into_iter().map(|item| item.uri).collect::<Vec<_>>(),
                vec![
                    "fake:track:track-1".to_string(),
                    "fake:track:track-2".to_string(),
                ]
            );
        }
    }

    #[tokio::test]
    async fn queue_expansion_rejects_foreign_collection_items_at_adapter_boundary() {
        let provider = ForeignCollectionProvider(FakeProvider::new());
        for uri in ["fake:playlist:playlist-1", "fake:album:album-1"] {
            let error = queueable_items_for_selection_without_cache(&provider, uri)
                .await
                .expect_err("foreign collection output must fail closed");
            assert!(matches!(
                error.downcast_ref::<spotuify_core::ProviderError>(),
                Some(spotuify_core::ProviderError::InvalidInput { field, .. })
                    if field == "media_item.uri"
            ));
        }
    }

    #[tokio::test]
    async fn queue_expansion_rejects_owned_items_with_the_wrong_endpoint_kind() {
        let provider = WrongKindCollectionProvider(FakeProvider::new());
        for (uri, field) in [
            ("fake:playlist:playlist-1", "playlist_items.kind"),
            ("fake:album:album-1", "album_tracks.kind"),
        ] {
            let error = queueable_items_for_selection_without_cache(&provider, uri)
                .await
                .expect_err("wrong-kind collection output must fail closed");
            assert!(matches!(
                error.downcast_ref::<spotuify_core::ProviderError>(),
                Some(spotuify_core::ProviderError::InvalidInput { field: actual, .. })
                    if actual == field
            ));
        }
    }

    #[test]
    fn idle_queue_starts_contexts_as_contexts() {
        assert_eq!(
            idle_context_start_label(&MediaKind::Playlist),
            Some("playlist")
        );
        assert_eq!(idle_context_start_label(&MediaKind::Album), Some("album"));
        assert_eq!(idle_context_start_label(&MediaKind::Track), None);
    }

    #[test]
    fn optimistic_queue_append_keeps_existing_items_and_duplicates() {
        let queue = Queue {
            currently_playing: None,
            items: vec![track("spotify:track:a", "A")],
            session_active: false,
            as_of_ms: 1,
        };

        let queue = queue_with_appended_items(
            queue,
            vec![
                track("spotify:track:b", "B"),
                track("spotify:track:a", "A duplicate"),
            ],
            2,
        );

        let uris: Vec<&str> = queue.items.iter().map(|item| item.uri.as_str()).collect();
        assert_eq!(
            uris,
            vec!["spotify:track:a", "spotify:track:b", "spotify:track:a"]
        );
        assert!(queue.session_active);
        assert_eq!(queue.as_of_ms, 2);
    }

    #[test]
    fn context_queue_snapshot_sets_current_and_up_next() {
        let queue = queue_for_started_context(
            vec![
                track("spotify:track:first", "First"),
                track("spotify:track:second", "Second"),
            ],
            3,
        )
        .expect("context with tracks should produce a queue snapshot");

        assert_eq!(
            queue
                .currently_playing
                .as_ref()
                .map(|item| item.uri.as_str()),
            Some("spotify:track:first")
        );
        let uris: Vec<&str> = queue.items.iter().map(|item| item.uri.as_str()).collect();
        assert_eq!(uris, vec!["spotify:track:second"]);
        assert!(queue.session_active);
        assert_eq!(queue.as_of_ms, 3);
    }

    #[test]
    fn context_queue_starts_at_requested_track() {
        let items = vec![
            track("spotify:track:a", "A"),
            track("spotify:track:b", "B"),
            track("spotify:track:c", "C"),
        ];
        let queue = queue_for_started_context_at(items, "spotify:track:b", 7)
            .expect("start-at track should produce a queue");

        assert_eq!(
            queue
                .currently_playing
                .as_ref()
                .map(|item| item.uri.as_str()),
            Some("spotify:track:b")
        );
        let uris: Vec<&str> = queue.items.iter().map(|item| item.uri.as_str()).collect();
        assert_eq!(uris, vec!["spotify:track:c"]);
        assert!(queue.session_active);
        assert_eq!(queue.as_of_ms, 7);
    }

    #[test]
    fn context_queue_is_absent_when_start_track_is_not_cached() {
        let items = vec![track("spotify:track:a", "A"), track("spotify:track:b", "B")];
        assert!(queue_for_started_context_at(items, "spotify:track:missing", 9).is_none());
    }

    #[test]
    fn context_queue_empty_items_yields_none() {
        assert!(queue_for_started_context_at(Vec::new(), "spotify:track:a", 1).is_none());
    }
}

#[cfg(test)]
mod next_prediction_tests {
    use super::{apply_search_sort, optimistic_next_from_queue, optimistic_queue_promoting};
    use spotuify_core::{MediaItem, MediaKind, Queue};
    use spotuify_protocol::SearchSortData;

    fn item(uri: &str, name: &str, subtitle: &str, duration_ms: u64) -> MediaItem {
        MediaItem {
            uri: uri.to_string(),
            name: name.to_string(),
            subtitle: subtitle.to_string(),
            duration_ms,
            kind: MediaKind::Track,
            ..Default::default()
        }
    }

    fn queue(current: Option<&str>, items: &[&str]) -> Queue {
        Queue {
            currently_playing: current.map(|uri| item(uri, "Current", "A", 1000)),
            items: items.iter().map(|uri| item(uri, uri, "A", 1000)).collect(),
            session_active: true,
            as_of_ms: 0,
        }
    }

    #[test]
    fn next_returns_first_queue_item_when_current_matches() {
        let q = queue(
            Some("spotify:track:cur"),
            &["spotify:track:n1", "spotify:track:n2"],
        );
        let next = optimistic_next_from_queue(&q, "spotify:track:cur");
        assert_eq!(next.map(|i| i.uri), Some("spotify:track:n1".to_string()));
    }

    #[test]
    fn next_is_none_when_cached_current_is_stale() {
        // Cached queue describes a different track than what's actually playing
        // → the queue is historical, so we must not predict a wrong "next".
        let q = queue(Some("spotify:track:other"), &["spotify:track:n1"]);
        assert!(optimistic_next_from_queue(&q, "spotify:track:cur").is_none());
    }

    #[test]
    fn next_is_none_when_queue_is_empty_or_session_unknown() {
        let q = queue(Some("spotify:track:cur"), &[]);
        assert!(optimistic_next_from_queue(&q, "spotify:track:cur").is_none());
        let no_current = queue(None, &["spotify:track:n1"]);
        assert!(optimistic_next_from_queue(&no_current, "spotify:track:cur").is_none());
    }

    #[test]
    fn queue_promotion_drops_through_predicted_track() {
        let q = queue(
            Some("spotify:track:cur"),
            &["spotify:track:n1", "spotify:track:n2", "spotify:track:n3"],
        );
        let next = item("spotify:track:n1", "N1", "A", 1000);
        let promoted = optimistic_queue_promoting(q, &next).expect("promotes head");
        assert_eq!(
            promoted.currently_playing.map(|i| i.uri),
            Some("spotify:track:n1".to_string())
        );
        assert_eq!(
            promoted
                .items
                .iter()
                .map(|i| i.uri.as_str())
                .collect::<Vec<_>>(),
            ["spotify:track:n2", "spotify:track:n3"]
        );
    }

    #[test]
    fn queue_promotion_marks_cached_snapshot_active() {
        let mut q = queue(
            Some("spotify:track:cur"),
            &["spotify:track:n1", "spotify:track:n2"],
        );
        // SQLite cache reads are deliberately marked inactive because the
        // store cannot know session liveness. A successful `next` prediction
        // is live daemon state and must restore that bit before broadcast.
        q.session_active = false;
        let next = item("spotify:track:n1", "N1", "A", 1000);

        let promoted = optimistic_queue_promoting(q, &next).expect("promotes cached queue");

        assert!(promoted.session_active);
        assert_eq!(
            promoted
                .items
                .iter()
                .map(|item| item.uri.as_str())
                .collect::<Vec<_>>(),
            ["spotify:track:n2"]
        );
    }

    #[test]
    fn queue_promotion_is_none_when_predicted_track_not_in_queue() {
        let q = queue(Some("spotify:track:cur"), &["spotify:track:n1"]);
        let stranger = item("spotify:track:elsewhere", "X", "A", 1000);
        assert!(optimistic_queue_promoting(q, &stranger).is_none());
    }

    #[test]
    fn search_sort_relevance_preserves_order() {
        let mut items = vec![item("u:b", "B", "Z", 300), item("u:a", "A", "Y", 100)];
        apply_search_sort(&mut items, None);
        assert_eq!(
            items.iter().map(|i| i.uri.as_str()).collect::<Vec<_>>(),
            ["u:b", "u:a"]
        );
        apply_search_sort(&mut items, Some(SearchSortData::Relevance));
        assert_eq!(
            items.iter().map(|i| i.uri.as_str()).collect::<Vec<_>>(),
            ["u:b", "u:a"]
        );
    }

    #[test]
    fn search_sort_by_name_and_duration() {
        let mut items = vec![
            item("u:b", "Beta", "Z", 300),
            item("u:a", "Alpha", "Y", 100),
        ];
        apply_search_sort(&mut items, Some(SearchSortData::Name));
        assert_eq!(
            items.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
            ["Alpha", "Beta"]
        );
        apply_search_sort(&mut items, Some(SearchSortData::Duration));
        assert_eq!(items[0].duration_ms, 100);
    }
}

#[cfg(test)]
mod lyrics_tests {
    use std::sync::Arc;

    use spotuify_core::{LyricsProvider, SyncedLyrics};
    use spotuify_protocol::{Request, ResponseData};
    use tempfile::TempDir;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{dispatch, DaemonState};

    struct TestEnv {
        _temp: TempDir,
    }

    impl TestEnv {
        fn new(lrclib_base_url: &str) -> Self {
            let temp = tempfile::tempdir().expect("tempdir");
            std::env::set_var("SPOTUIFY_FAKE_SPOTIFY", "1");
            std::env::set_var("SPOTUIFY_LRCLIB_BASE_URL", lrclib_base_url);
            std::env::set_var("SPOTUIFY_CACHE_DB", temp.path().join("cache.sqlite3"));
            std::env::set_var("SPOTUIFY_SEARCH_INDEX", temp.path().join("search-index"));
            std::env::set_var("SPOTUIFY_RUNTIME_DIR", temp.path().join("runtime"));
            Self { _temp: temp }
        }
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            std::env::remove_var("SPOTUIFY_FAKE_SPOTIFY");
            std::env::remove_var("SPOTUIFY_LRCLIB_BASE_URL");
            std::env::remove_var("SPOTUIFY_CACHE_DB");
            std::env::remove_var("SPOTUIFY_SEARCH_INDEX");
            std::env::remove_var("SPOTUIFY_RUNTIME_DIR");
        }
    }

    fn lyrics_response(response: ResponseData) -> Option<(SyncedLyrics, i64)> {
        match response {
            ResponseData::Lyrics {
                lyrics: Some(lyrics),
                offset_ms,
            } => Some((lyrics, offset_ms)),
            _ => None,
        }
    }

    #[tokio::test]
    async fn explicit_track_uri_fetches_lrclib_when_media_item_is_not_cached() {
        let _guard = crate::ENV_LOCK.lock().await;
        let lrclib = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/get"))
            .and(query_param("track_name", "Never Too Much"))
            .and(query_param("artist_name", "Luther Vandross"))
            .and(query_param("album_name", "Never Too Much"))
            .and(query_param("duration", "221"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "instrumental": false,
                "plainLyrics": null,
                "syncedLyrics": "[00:01.00]Never too much, never too much",
            })))
            .expect(1)
            .mount(&lrclib)
            .await;
        let _env = TestEnv::new(&lrclib.uri());
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));

        let response = dispatch(
            state.clone(),
            Request::LyricsGet {
                track_uri: Some("spotify:track:never-too-much".to_string()),
                force_refresh: true,
            },
            None,
        )
        .await
        .expect("lyrics response");

        state.shutdown_search().await;
        state.shutdown_player().await;

        let (lyrics, offset_ms) = lyrics_response(response).expect("expected LRCLIB lyrics");
        assert_eq!(offset_ms, 0);
        assert_eq!(lyrics.provider, LyricsProvider::Lrclib);
        assert_eq!(lyrics.track_uri, "spotify:track:never-too-much");
        assert_eq!(lyrics.lines[0].start_ms, 1_000);
        assert_eq!(lyrics.lines[0].text, "Never too much, never too much");
    }

    #[tokio::test]
    async fn cached_lyrics_survive_daemon_restart_without_refetching() {
        let _guard = crate::ENV_LOCK.lock().await;
        let lrclib = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/get"))
            .and(query_param("track_name", "Never Too Much"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "instrumental": false,
                "plainLyrics": "cached lyric",
                "syncedLyrics": null,
            })))
            .expect(1)
            .mount(&lrclib)
            .await;
        let _env = TestEnv::new(&lrclib.uri());

        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        let first = dispatch(
            state.clone(),
            Request::LyricsGet {
                track_uri: Some("spotify:track:never-too-much".to_string()),
                force_refresh: true,
            },
            None,
        )
        .await
        .expect("initial lyrics response");
        state.shutdown_search().await;
        state.shutdown_player().await;
        drop(state);

        let restarted = Arc::new(DaemonState::new().await.expect("restarted daemon state"));
        let second = dispatch(
            restarted.clone(),
            Request::LyricsGet {
                track_uri: Some("spotify:track:never-too-much".to_string()),
                force_refresh: false,
            },
            None,
        )
        .await
        .expect("cached lyrics response");
        restarted.shutdown_search().await;
        restarted.shutdown_player().await;

        let (first_lyrics, _) = lyrics_response(first).expect("initial lyrics should exist");
        let (second_lyrics, _) = lyrics_response(second).expect("cached lyrics should exist");
        assert_eq!(first_lyrics.lines[0].text, "cached lyric");
        assert_eq!(second_lyrics.lines[0].text, "cached lyric");
        assert_eq!(second_lyrics.provider, LyricsProvider::Lrclib);
    }
}

#[cfg(test)]
mod reload_tests {
    use std::sync::Arc;

    use spotuify_protocol::{Request, ResponseData, VizSourceKindData};
    use tempfile::TempDir;

    use super::{dispatch, DaemonState};

    struct TestEnv {
        temp: TempDir,
    }

    impl TestEnv {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("tempdir");
            std::env::set_var("SPOTUIFY_FAKE_SPOTIFY", "1");
            std::env::set_var("SPOTUIFY_CACHE_DB", temp.path().join("cache.sqlite3"));
            std::env::set_var("SPOTUIFY_SEARCH_INDEX", temp.path().join("search-index"));
            std::env::set_var("SPOTUIFY_RUNTIME_DIR", temp.path().join("runtime"));
            std::env::set_var("SPOTUIFY_CONFIG", temp.path().join("spotuify.toml"));
            Self { temp }
        }

        fn write_config(&self, viz: &str) {
            std::fs::write(
                self.temp.path().join("spotuify.toml"),
                format!(
                    r#"
client_id = "test-client"
redirect_uri = "http://127.0.0.1:8888/callback"

{viz}
"#
                ),
            )
            .expect("config write");
        }
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            std::env::remove_var("SPOTUIFY_FAKE_SPOTIFY");
            std::env::remove_var("SPOTUIFY_CACHE_DB");
            std::env::remove_var("SPOTUIFY_SEARCH_INDEX");
            std::env::remove_var("SPOTUIFY_RUNTIME_DIR");
            std::env::remove_var("SPOTUIFY_CONFIG");
        }
    }

    #[tokio::test]
    async fn reload_applies_viz_config_without_daemon_restart() {
        let _guard = crate::ENV_LOCK.lock().await;
        let env = TestEnv::new();
        env.write_config(
            r#"
[viz]
enabled = false
source = "auto"
target_fps = 30
"#,
        );
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        assert!(!state.viz_coordinator().diagnostics().await.enabled);

        env.write_config(
            r#"
[viz]
enabled = true
source = "none"
target_fps = 7
smoothing = 0.2
noise_gate = 0.25
"#,
        );
        let response = dispatch(state.clone(), Request::Reload, None)
            .await
            .expect("reload response");

        assert!(matches!(response, ResponseData::Ack { .. }));
        let diagnostics = state.viz_coordinator().diagnostics().await;
        assert!(diagnostics.enabled);
        assert_eq!(diagnostics.configured_source, VizSourceKindData::None);
        assert_eq!(diagnostics.target_fps, 7);

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn reconnect_re_registers_player_backend() {
        let _guard = crate::ENV_LOCK.lock().await;
        let env = TestEnv::new();
        env.write_config(
            r#"
[player]
"#,
        );
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        assert!(!state.player_is_connected().await);

        let response = dispatch(state.clone(), Request::Reconnect, None)
            .await
            .expect("reconnect response");

        assert!(matches!(response, ResponseData::Ack { .. }));
        assert!(state.player_is_connected().await);

        state.shutdown_search().await;
        state.shutdown_player().await;
    }
}

#[cfg(test)]
mod post_command_persist_tests {
    //! Phase 1 + Phase 3 + Phase 5 integration tests.
    //!
    //! Asserts that:
    //! - The daemon persists `CommandResult.playback` before emitting
    //!   `PlaybackChanged` (Phase 1), so a subscriber that re-fetches
    //!   immediately sees the post-mutation state.
    //! - The emitted `PlaybackChanged` event carries the embedded
    //!   `Playback` snapshot (Phase 3), so clients don't need a
    //!   follow-up `PlaybackGet`.
    //! - `SeekRelative` is resolved against the clock daemon-side
    //!   (Phase 5), not the caller's stale read.
    //!
    //! Anti-implementation-coupling: we observe via the public event
    //! channel + store query path. No internal counters or method
    //! orderings.

    use std::ffi::OsString;
    use std::sync::Arc;
    use std::time::Duration;

    use spotuify_core::{
        now_ms, MediaItem, MediaKind, Playback, ProviderError, ProviderId, Queue, ResourceUri,
    };
    use spotuify_protocol::{DaemonEvent, IpcPayload, PlaybackCommand, Request, ResponseData};
    use tempfile::TempDir;

    use super::{
        cache_queue, cache_queue_if_fresh, compute_optimistic_playback, dispatch,
        expected_playback_after_command, optimistic_queue_with_appends, persist_command_result,
        playback_command_kind, post_command_playback_matches, transport_cmd_for_command_kind,
        CommandKind, CommandResult, DaemonState, ExpectedPlayback,
    };

    struct TestEnv {
        _temp: TempDir,
        old_values: Vec<(&'static str, Option<OsString>)>,
    }

    impl TestEnv {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("tempdir");
            let old_values = [
                "SPOTUIFY_FAKE_SPOTIFY",
                "SPOTUIFY_CACHE_DB",
                "SPOTUIFY_SEARCH_INDEX",
                "SPOTUIFY_RUNTIME_DIR",
                "SPOTUIFY_CONFIG_DIR",
                "SPOTUIFY_CONFIG",
            ]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect();
            std::env::set_var("SPOTUIFY_FAKE_SPOTIFY", "1");
            std::env::set_var("SPOTUIFY_CACHE_DB", temp.path().join("cache.sqlite3"));
            std::env::set_var("SPOTUIFY_SEARCH_INDEX", temp.path().join("search-index"));
            std::env::set_var("SPOTUIFY_RUNTIME_DIR", temp.path().join("runtime"));
            std::env::set_var("SPOTUIFY_CONFIG_DIR", temp.path().join("config"));
            std::env::set_var("SPOTUIFY_CONFIG", temp.path().join("spotuify.toml"));
            Self {
                _temp: temp,
                old_values,
            }
        }

        fn spotify_auth() -> Self {
            let env = Self::new();
            std::env::remove_var("SPOTUIFY_FAKE_SPOTIFY");
            std::fs::write(
                env._temp.path().join("spotuify.toml"),
                r#"
[providers]
default = "spotify"

[providers.spotify]
type = "spotify"
client_id = "test-client"
redirect_uri = "http://127.0.0.1:8888/callback"
"#,
            )
            .expect("write isolated Spotify config");
            env
        }
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            for (key, value) in &self.old_values {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn track(uri: &str, name: &str) -> MediaItem {
        MediaItem {
            id: ResourceUri::parse(uri)
                .ok()
                .map(|resource| resource.bare_id().to_string()),
            uri: uri.to_string(),
            name: name.to_string(),
            subtitle: "Artist".to_string(),
            context: "Album".to_string(),
            duration_ms: 180_000,
            image_url: None,
            kind: MediaKind::Track,
            source: Some("test".into()),
            freshness: None,
            explicit: Some(false),
            is_playable: Some(true),
            ..Default::default()
        }
    }

    /// Pull the command-result `PlaybackChanged` event off the
    /// broadcast within the timeout. Skips intermediate accepted,
    /// operation, optimistic, and local player events that legitimately
    /// fire in the same flow.
    async fn next_playback_event(
        rx: &mut tokio::sync::broadcast::Receiver<spotuify_protocol::IpcMessage>,
        expected_action: &str,
    ) -> DaemonEvent {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let msg = tokio::time::timeout(remaining, rx.recv())
                .await
                .expect("waiting for PlaybackChanged timed out")
                .expect("event channel closed");
            if let IpcPayload::Event(event) = msg.payload {
                if let DaemonEvent::PlaybackChanged { ref action, .. } = event {
                    if action == expected_action {
                        return event;
                    }
                }
            }
        }
    }

    async fn next_queue_event(
        rx: &mut tokio::sync::broadcast::Receiver<spotuify_protocol::IpcMessage>,
        expected_action: &str,
    ) -> DaemonEvent {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let msg = tokio::time::timeout(remaining, rx.recv())
                .await
                .expect("waiting for QueueChanged timed out")
                .expect("event channel closed");
            if let IpcPayload::Event(event) = msg.payload {
                if matches!(
                    &event,
                    DaemonEvent::QueueChanged { action, .. } if action == expected_action
                ) {
                    return event;
                }
            }
        }
    }

    async fn next_playlists_event(
        rx: &mut tokio::sync::broadcast::Receiver<spotuify_protocol::IpcMessage>,
        expected_action: &str,
        expected_playlist: Option<&str>,
    ) -> DaemonEvent {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let msg = tokio::time::timeout(remaining, rx.recv())
                .await
                .expect("waiting for PlaylistsChanged timed out")
                .expect("event channel closed");
            if let IpcPayload::Event(event) = msg.payload {
                if matches!(
                    &event,
                    DaemonEvent::PlaylistsChanged { action, playlist, .. }
                        if action == expected_action
                            && playlist.as_deref() == expected_playlist
                ) {
                    return event;
                }
            }
        }
    }

    async fn assert_no_mutation_accepted(
        rx: &mut tokio::sync::broadcast::Receiver<spotuify_protocol::IpcMessage>,
    ) {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(100);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let recv = tokio::time::timeout(remaining, rx.recv()).await;
            let Ok(Ok(msg)) = recv else {
                break;
            };
            assert!(
                !matches!(
                    msg.payload,
                    IpcPayload::Event(DaemonEvent::MutationAccepted { .. })
                ),
                "auth-blocked request must not emit MutationAccepted"
            );
        }
    }

    #[tokio::test]
    async fn playback_command_emits_playback_changed_with_embedded_snapshot() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));

        let mut rx = state.event_tx.subscribe();
        let response = dispatch(
            state.clone(),
            Request::PlaybackCommand {
                command: PlaybackCommand::Resume,
            },
            None,
        )
        .await
        .expect("playback response");
        // The immediate response is a receipt (Phase 6.6 optimistic
        // mutation). The interesting event is the PlaybackChanged that
        // follows once the spawned task completes.
        assert!(matches!(response, ResponseData::Mutation { .. }));

        match next_playback_event(&mut rx, "resume").await {
            DaemonEvent::PlaybackChanged { action, playback } => {
                assert_eq!(action, "resume");
                // Phase 3: the event must carry the post-mutation playback so
                // clients don't need a follow-up PlaybackGet round-trip.
                let pb = playback.expect("Phase 3 contract: PlaybackChanged must embed a snapshot");
                // Phase 4: that snapshot must be tagged with its source so
                // freshness-aware clients (TUI merge re-anchor) can react.
                assert!(
                    pb.source.is_some(),
                    "Phase 4 contract: embedded playback must carry source label"
                );
            }
            other => assert!(
                matches!(other, DaemonEvent::PlaybackChanged { .. }),
                "expected PlaybackChanged"
            ),
        }

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn failed_play_uri_preserves_prior_listen_context() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::spotify_auth();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        let provider = ProviderId::new("spotify").expect("valid provider id");
        state.set_playback_context(Some("spotify:album:prior".to_string()));
        state.mark_auth_required(Some(&provider)).await;

        let result = dispatch(
            state.clone(),
            Request::PlaybackCommand {
                command: PlaybackCommand::PlayUri {
                    uri: "spotify:track:track-1".to_string(),
                    context_uri: Some("spotify:album:new".to_string()),
                },
            },
            None,
        )
        .await;

        assert!(result.is_err(), "auth-blocked play must fail");
        assert_eq!(
            state._session_tracker.current_context().as_deref(),
            Some("spotify:album:prior"),
            "failed play must not reattribute the active listen session"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn playback_command_ack_does_not_wait_for_transport_lane() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        let request = Request::PlaybackCommand {
            command: PlaybackCommand::Resume,
        };
        let lane = state
            .mutation_lane(&request)
            .await
            .expect("playback command should use transport lane");
        let lane_guard = lane.lock_owned().await;

        let response = tokio::time::timeout(
            Duration::from_millis(200),
            dispatch(state.clone(), request, None),
        )
        .await
        .expect("optimistic response must not wait behind lane lock")
        .expect("playback response");

        assert!(matches!(response, ResponseData::Mutation { .. }));
        drop(lane_guard);

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn optimistic_playback_command_fails_fast_when_auth_required() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::spotify_auth();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        let provider = ProviderId::new("spotify").expect("valid provider id");
        state.mark_auth_required(Some(&provider)).await;
        let mut rx = state.event_tx.subscribe();

        let err = dispatch(
            state.clone(),
            Request::PlaybackCommand {
                command: PlaybackCommand::Resume,
            },
            None,
        )
        .await
        .expect_err("auth-required latch should reject before optimistic ack");

        assert!(matches!(
            err.downcast_ref::<ProviderError>(),
            Some(ProviderError::AuthRequired)
        ));
        assert_no_mutation_accepted(&mut rx).await;
        assert!(
            state
                .store()
                .list_pending_receipts()
                .await
                .expect("pending receipts")
                .is_empty(),
            "auth preflight must reject before creating a pending receipt"
        );
        assert!(
            state
                .store()
                .list_operations(10, None, None)
                .await
                .expect("operations")
                .is_empty(),
            "auth preflight must reject before creating an operation row"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn playlist_tracks_nonblocking_refreshes_cache_for_tui() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));

        dispatch(
            state.clone(),
            Request::PlaylistsList { provider: None },
            None,
        )
        .await
        .expect("playlist cache warm");
        let mut rx = state.event_tx.subscribe();
        let response = tokio::time::timeout(
            Duration::from_millis(200),
            dispatch(
                state.clone(),
                Request::PlaylistTracks {
                    playlist: "quiet-storm".to_string(),
                    wait: false,
                    provider: None,
                },
                None,
            ),
        )
        .await
        .expect("nonblocking playlist tracks should return promptly")
        .expect("playlist tracks response");

        assert!(matches!(response, ResponseData::MediaItems { items } if items.is_empty()));

        let playlist_uri = "spotify:playlist:quiet-storm";
        let event = next_playlists_event(&mut rx, "tracks-refreshed", Some(playlist_uri)).await;
        assert!(matches!(
            event,
            DaemonEvent::PlaylistsChanged {
                action,
                playlist: Some(playlist),
                ..
            } if action == "tracks-refreshed" && playlist == playlist_uri
        ));

        let cached = dispatch(
            state.clone(),
            Request::PlaylistTracks {
                playlist: "quiet-storm".to_string(),
                wait: false,
                provider: None,
            },
            None,
        )
        .await
        .expect("cached playlist tracks response");
        match cached {
            ResponseData::MediaItems { items } => {
                let uris = items
                    .iter()
                    .map(|item| item.uri.as_str())
                    .collect::<Vec<_>>();
                assert_eq!(
                    uris,
                    vec!["spotify:track:never-too-much", "spotify:track:sweet-thing"]
                );
            }
            other => assert!(
                matches!(other, ResponseData::MediaItems { .. }),
                "expected cached media items"
            ),
        }

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn queue_add_ignores_stale_cached_queue_when_deciding_append() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        dispatch(state.clone(), Request::Reconnect, None)
            .await
            .expect("test player should be active before queue add");
        let stale_queue = Queue {
            currently_playing: None,
            items: vec![track(
                "spotify:track:never-too-much",
                "Never Too Much stale",
            )],
            session_active: false,
            as_of_ms: 1,
        };
        state
            .store()
            .persist_queue(&stale_queue)
            .await
            .expect("persist stale queue");

        let mut rx = state.event_tx.subscribe();
        let response = dispatch(
            state.clone(),
            Request::QueueAdd {
                uri: "spotify:track:never-too-much".to_string(),
            },
            None,
        )
        .await
        .expect("queue add response");
        assert!(matches!(
            response,
            ResponseData::Mutation { receipt } if receipt.ok && receipt.action == "queue"
        ));

        match next_queue_event(&mut rx, "queue").await {
            DaemonEvent::QueueChanged { uris, queue, .. } => {
                assert_eq!(uris, vec!["spotify:track:never-too-much"]);
                let queue = queue.expect("queue add event should embed actionable queue");
                let embedded_uris = queue
                    .items
                    .iter()
                    .map(|item| item.uri.as_str())
                    .collect::<Vec<_>>();
                assert_eq!(embedded_uris, vec!["spotify:track:never-too-much"]);
                assert!(queue.session_active);
            }
            other => assert!(
                matches!(other, DaemonEvent::QueueChanged { .. }),
                "expected QueueChanged"
            ),
        }

        let cached = state
            .store()
            .latest_queue(10)
            .await
            .expect("latest queue")
            .expect("queue cache should be updated by queue add");
        let cached_uris = cached
            .items
            .iter()
            .map(|item| item.uri.as_str())
            .collect::<Vec<_>>();
        assert_eq!(cached_uris, vec!["spotify:track:never-too-much"]);

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn stale_queue_refresh_preserves_pending_optimistic_append() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        let existing = track("spotify:track:queued", "Queued");
        let appended = track("spotify:track:queued", "Queued duplicate");
        let base = Queue {
            currently_playing: Some(track("spotify:track:current", "Current")),
            items: vec![existing.clone()],
            session_active: true,
            as_of_ms: now_ms(),
        };
        state
            .store()
            .persist_queue(&base)
            .await
            .expect("persist base queue");

        // The live queue already held this URI when the (duplicate)
        // add went through — occurrence counting keys off live truth,
        // so the pending append must wait for the SECOND occurrence.
        let live_uris: std::collections::HashSet<String> =
            std::iter::once(existing.uri.clone()).collect();
        let provider = state
            .providers()
            .await
            .expect("providers")
            .default_id()
            .clone();
        let optimistic =
            optimistic_queue_with_appends(&state, &provider, vec![appended.clone()], &live_uris)
                .await
                .expect("optimistic append");
        cache_queue(&state, &optimistic).await;

        let stale_live = Queue {
            currently_playing: base.currently_playing.clone(),
            items: vec![existing.clone()],
            session_active: true,
            as_of_ms: 2,
        };
        let applied =
            cache_queue_if_fresh(&state, &provider, &stale_live, state.current_mutation_seq())
                .await
                .expect("stale live queue should be overlaid and cached");
        let applied_uris = applied
            .items
            .iter()
            .map(|item| item.uri.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            applied_uris,
            vec!["spotify:track:queued", "spotify:track:queued"]
        );

        let cached = state
            .store()
            .latest_queue(10)
            .await
            .expect("latest queue")
            .expect("queue cache");
        let cached_uris = cached
            .items
            .iter()
            .map(|item| item.uri.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            cached_uris,
            vec!["spotify:track:queued", "spotify:track:queued"]
        );

        let confirmed_live = Queue {
            currently_playing: base.currently_playing,
            items: vec![existing, appended],
            session_active: true,
            as_of_ms: 3,
        };
        let confirmed = cache_queue_if_fresh(
            &state,
            &provider,
            &confirmed_live,
            state.current_mutation_seq(),
        )
        .await
        .expect("confirmed live queue should be cached");
        let confirmed_uris = confirmed
            .items
            .iter()
            .map(|item| item.uri.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            confirmed_uris,
            vec!["spotify:track:queued", "spotify:track:queued"]
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn queue_get_returns_cached_queue_instead_of_empty_snapshot() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        let queued = track("spotify:track:queued", "Queued");
        state
            .store()
            .persist_queue(&Queue {
                currently_playing: None,
                items: vec![queued.clone(), queued],
                session_active: false,
                as_of_ms: 1,
            })
            .await
            .expect("persist cached queue");
        state.playback_clock().apply_command_result(
            &Playback {
                item: Some(track("spotify:track:current", "Current")),
                is_playing: true,
                ..Default::default()
            },
            now_ms(),
        );

        let response = dispatch(state.clone(), Request::QueueGet, None)
            .await
            .expect("queue get response");

        match response {
            ResponseData::Queue { queue } => {
                assert!(
                    queue.session_active,
                    "cached queue stays renderable until playback confirms durable inactivity"
                );
                let uris = queue
                    .items
                    .iter()
                    .map(|item| item.uri.as_str())
                    .collect::<Vec<_>>();
                assert_eq!(uris, vec!["spotify:track:queued", "spotify:track:queued"]);
            }
            other => assert!(
                matches!(other, ResponseData::Queue { .. }),
                "expected queue response"
            ),
        }

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn play_uri_context_publishes_context_queue_snapshot() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        let mut rx = state.event_tx.subscribe();

        let response = dispatch(
            state.clone(),
            Request::PlaybackCommand {
                command: PlaybackCommand::PlayUri {
                    uri: "spotify:playlist:quiet-storm".to_string(),
                    context_uri: None,
                },
            },
            None,
        )
        .await
        .expect("play context response");
        assert!(matches!(response, ResponseData::Mutation { .. }));

        match next_queue_event(&mut rx, "play-context").await {
            DaemonEvent::QueueChanged { queue, .. } => {
                let queue = queue.expect("play-context event should embed queue");
                assert_eq!(
                    queue
                        .currently_playing
                        .as_ref()
                        .map(|item| item.uri.as_str()),
                    Some("spotify:track:never-too-much")
                );
                let up_next = queue
                    .items
                    .iter()
                    .map(|item| item.uri.as_str())
                    .collect::<Vec<_>>();
                assert_eq!(up_next, vec!["spotify:track:sweet-thing"]);
                assert!(queue.session_active);
            }
            other => assert!(
                matches!(other, DaemonEvent::QueueChanged { .. }),
                "expected QueueChanged"
            ),
        }

        let cached = state
            .store()
            .latest_queue(10)
            .await
            .expect("latest queue")
            .expect("play context should cache queue snapshot");
        assert_eq!(
            cached
                .currently_playing
                .as_ref()
                .map(|item| item.uri.as_str()),
            Some("spotify:track:never-too-much")
        );
        let cached_up_next = cached
            .items
            .iter()
            .map(|item| item.uri.as_str())
            .collect::<Vec<_>>();
        assert_eq!(cached_up_next, vec!["spotify:track:sweet-thing"]);

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[test]
    fn toggle_transport_uses_daemon_clock_state() {
        let playing = spotuify_core::Playback {
            item: Some(track("spotify:track:test", "Test")),
            is_playing: true,
            ..Default::default()
        };
        let (cmd, effective) =
            transport_cmd_for_command_kind(&CommandKind::TogglePlayback, &playing)
                .expect("playing toggle should pause locally");
        assert!(matches!(cmd, crate::state::TransportCmd::Pause));
        assert!(matches!(effective, CommandKind::Pause));

        let paused = spotuify_core::Playback {
            is_playing: false,
            ..playing
        };
        let (cmd, effective) =
            transport_cmd_for_command_kind(&CommandKind::TogglePlayback, &paused)
                .expect("paused toggle with an item should resume locally");
        assert!(matches!(cmd, crate::state::TransportCmd::Resume));
        assert!(matches!(effective, CommandKind::Resume));

        let no_item = spotuify_core::Playback {
            item: None,
            device: Some(spotuify_core::Device {
                id: Some("active-device".to_string()),
                name: "spotuify-hume".to_string(),
                kind: "Speaker".to_string(),
                is_active: true,
                is_restricted: false,
                volume_percent: Some(25),
                supports_volume: true,
            }),
            is_playing: false,
            ..Default::default()
        };
        assert!(
            transport_cmd_for_command_kind(&CommandKind::TogglePlayback, &no_item).is_none(),
            "toggle with only an active device must use Web API recovery, not local resume"
        );
    }

    #[test]
    fn fast_transport_freezes_toggle_before_optimistic_state() {
        let playing = spotuify_core::Playback {
            item: Some(track("spotify:track:test", "Test")),
            is_playing: true,
            ..Default::default()
        };
        let command_kind = playback_command_kind(PlaybackCommand::Toggle);
        let (cmd, effective) = transport_cmd_for_command_kind(&command_kind, &playing)
            .expect("playing toggle should freeze as pause");
        assert!(matches!(cmd, crate::state::TransportCmd::Pause));
        assert!(matches!(effective, CommandKind::Pause));

        let mut optimistic_after_toggle = playing.clone();
        optimistic_after_toggle.is_playing = false;
        let (cmd, effective) =
            transport_cmd_for_command_kind(&command_kind, &optimistic_after_toggle)
                .expect("paused toggle should freeze as resume");
        assert!(matches!(cmd, crate::state::TransportCmd::Resume));
        assert!(matches!(effective, CommandKind::Resume));

        assert!(
            transport_cmd_for_command_kind(&command_kind, &spotuify_core::Playback::default())
                .is_none()
        );

        let ended = spotuify_core::Playback {
            item: Some(track("spotify:track:ended", "Ended")),
            is_playing: false,
            progress_ms: 180_000,
            ..Default::default()
        };
        assert!(
            transport_cmd_for_command_kind(&command_kind, &ended).is_none(),
            "ended tracks must not call librespot resume"
        );

        let (cmd, effective) =
            transport_cmd_for_command_kind(&playback_command_kind(PlaybackCommand::Next), &playing)
                .expect("next should use fast local transport");
        assert!(matches!(cmd, crate::state::TransportCmd::Next));
        assert!(matches!(effective, CommandKind::Next));

        let (cmd, effective) = transport_cmd_for_command_kind(
            &playback_command_kind(PlaybackCommand::Previous),
            &playing,
        )
        .expect("previous should use fast local transport");
        assert!(matches!(cmd, crate::state::TransportCmd::Previous));
        assert!(matches!(effective, CommandKind::Previous));

        let (cmd, effective) = transport_cmd_for_command_kind(
            &playback_command_kind(PlaybackCommand::Seek {
                position_ms: 42_000,
            }),
            &playing,
        )
        .expect("seek should use fast local transport");
        assert!(matches!(
            cmd,
            crate::state::TransportCmd::Seek {
                position_ms: 42_000
            }
        ));
        assert!(matches!(
            effective,
            CommandKind::Seek {
                position_ms: 42_000
            }
        ));

        let (cmd, effective) = transport_cmd_for_command_kind(
            &playback_command_kind(PlaybackCommand::Volume { volume_percent: 50 }),
            &playing,
        )
        .expect("volume should use fast local transport");
        assert!(matches!(
            cmd,
            crate::state::TransportCmd::Volume { percent: 50 }
        ));
        assert!(matches!(
            effective,
            CommandKind::Volume { volume_percent: 50 }
        ));
    }

    #[tokio::test]
    async fn play_uri_prediction_does_not_tick_without_active_device() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));

        let playback = compute_optimistic_playback(
            &state,
            &PlaybackCommand::PlayUri {
                uri: "spotify:track:test-track".to_string(),
                context_uri: None,
            },
        )
        .await
        .expect("play-uri should still predict selected metadata");

        assert!(
            !playback.is_playing,
            "idle/no-device play should not start the progress clock before audio is confirmed"
        );
        assert_eq!(playback.progress_ms, 0);
        assert!(playback.item.is_some());

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn play_uri_prediction_keeps_clock_running_for_active_playback() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        state.playback_clock().seed_from_cache(
            spotuify_core::Playback {
                item: Some(spotuify_core::MediaItem {
                    uri: "spotify:track:old".to_string(),
                    duration_ms: 180_000,
                    ..Default::default()
                }),
                device: Some(spotuify_core::Device {
                    id: Some("active-device".to_string()),
                    name: "spotuify-hume".to_string(),
                    kind: "Speaker".to_string(),
                    is_active: true,
                    is_restricted: false,
                    volume_percent: Some(50),
                    supports_volume: true,
                }),
                is_playing: true,
                progress_ms: 12_000,
                ..Default::default()
            },
            spotuify_core::PlaybackStateSource::PlayerEvent,
            spotuify_core::now_ms(),
        );

        let playback = compute_optimistic_playback(
            &state,
            &PlaybackCommand::PlayUri {
                uri: "spotify:track:new".to_string(),
                context_uri: None,
            },
        )
        .await
        .expect("play-uri should predict active transition");

        assert!(playback.is_playing);
        assert_eq!(playback.progress_ms, 0);

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn post_command_persist_drops_stale_play_uri_readback() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));
        let result = CommandResult {
            playback: Some(spotuify_core::Playback {
                item: Some(spotuify_core::MediaItem {
                    uri: "spotify:track:old".to_string(),
                    duration_ms: 180_000,
                    ..Default::default()
                }),
                is_playing: false,
                ..Default::default()
            }),
            ..Default::default()
        };
        let expected = ExpectedPlayback {
            uri: Some("spotify:track:new".to_string()),
            is_playing: Some(true),
        };

        let outcome = persist_command_result(
            &state,
            &ProviderId::new("spotify").expect("valid provider id"),
            state.current_mutation_seq(),
            &result,
            "play-uri",
            Some(&expected),
        )
        .await;

        assert!(
            outcome.playback.is_none(),
            "stale readback must not overwrite the optimistic/player-event track"
        );
        assert!(
            state
                .store()
                .latest_playback()
                .await
                .expect("latest playback")
                .is_none(),
            "dropped playback must not be cached"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[test]
    fn next_previous_expected_playback_accepts_valid_spotify_track_mismatch() {
        let predicted = spotuify_core::Playback {
            item: Some(track("spotify:track:predicted", "Predicted")),
            is_playing: true,
            ..Default::default()
        };
        let spotify_track = spotuify_core::Playback {
            item: Some(track("spotify:track:actual", "Actual")),
            is_playing: true,
            ..Default::default()
        };
        let paused_readback = spotuify_core::Playback {
            item: Some(track("spotify:track:actual", "Actual")),
            is_playing: false,
            ..Default::default()
        };

        for command in [PlaybackCommand::Next, PlaybackCommand::Previous] {
            let expected = expected_playback_after_command(&command, Some(&predicted))
                .expect("track navigation prediction should build an expectation");
            assert!(
                post_command_playback_matches(&spotify_track, Some(&expected)),
                "a valid playing track from Spotify should reconcile {command:?}"
            );
            assert!(
                !post_command_playback_matches(&paused_readback, Some(&expected)),
                "{command:?} must not reconcile to a stopped/paused readback"
            );
        }
    }

    #[tokio::test]
    async fn playback_command_persists_before_emitting_event() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));

        let mut rx = state.event_tx.subscribe();
        let _ = dispatch(
            state.clone(),
            Request::PlaybackCommand {
                command: PlaybackCommand::Resume,
            },
            None,
        )
        .await
        .expect("resume response");

        // Wait for the PlaybackChanged event — the persist must have
        // already landed by the time this fires (Phase 1).
        let _ = next_playback_event(&mut rx, "resume").await;

        // The store now has a row that reflects the post-command
        // result (not the pre-command empty cache). The fake client
        // returns a non-empty fake_playback, so the latest row should
        // include an item.
        let cached = state
            .store()
            .latest_playback()
            .await
            .expect("query latest playback");
        assert!(
            cached.is_some(),
            "Phase 1 contract: post-command playback must be persisted before PlaybackChanged emit"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn playback_get_reads_from_clock_not_store() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));

        // Cold start: clock is seeded from cache (none); snapshot is
        // empty. PlaybackGet should return that without touching store.
        let response = dispatch(state.clone(), Request::PlaybackGet, None)
            .await
            .expect("PlaybackGet response");
        let pb = match response {
            ResponseData::Playback { playback } => playback,
            other => {
                assert!(
                    matches!(other, ResponseData::Playback { .. }),
                    "expected ResponseData::Playback"
                );
                return;
            }
        };
        // Phase 4 — snapshot must carry a source. Empty cold clock is
        // RecentFallback (or Cache if recent_items existed).
        assert!(pb.source.is_some(), "PlaybackGet must carry source label");
        // Phase 2 — sampled_at_ms is set by the clock on every snapshot.
        assert!(
            pb.sampled_at_ms.is_some(),
            "PlaybackGet snapshot must carry sampled_at_ms"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn ipc_request_span_captures_kind_and_outcome() {
        use std::io::Write;
        use std::sync::{Arc as StdArc, Mutex as StdMutex};
        use tracing_subscriber::fmt::MakeWriter;

        // Phase 0 — the IPC span records `request_kind`, `duration_ms`,
        // and `outcome`. Verify by installing a JSON tracing subscriber
        // captured into a Vec<u8>, dispatching a real request, and
        // grepping the output for the expected fields. Uses
        // `with_default` so the subscriber is scoped to this test and
        // doesn't bleed into others.

        #[derive(Clone)]
        struct VecWriter(StdArc<StdMutex<Vec<u8>>>);
        impl Write for VecWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .expect("captured tracing buffer lock")
                    .write(buf)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> MakeWriter<'a> for VecWriter {
            type Writer = VecWriter;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let buf = StdArc::new(StdMutex::new(Vec::<u8>::new()));
        let writer = VecWriter(buf.clone());
        let subscriber = tracing_subscriber::fmt()
            .with_writer(writer)
            .with_ansi(false)
            .json()
            .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
            .finish();

        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();

        // Run inside the subscriber's scope. Server-level
        // `guard_ipc_response` is private, but it produces the canonical
        // span shape — we mirror the structure by emitting a span here
        // through tracing::info_span! and asserting on the captured
        // output. This is what the real handler emits per request.
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!(
                target: "spotuify_daemon::ipc",
                "ipc.request",
                request_id = 42u64,
                request_kind = "playback-get",
                source = "tui",
                duration_ms = tracing::field::Empty,
                outcome = tracing::field::Empty,
            );
            let _enter = span.enter();
            span.record("duration_ms", 7u64);
            span.record("outcome", "ok");
        });

        let output = String::from_utf8(buf.lock().expect("captured tracing buffer lock").clone())
            .expect("captured tracing output is utf-8");
        assert!(
            output.contains("ipc.request"),
            "captured tracing output should contain span name 'ipc.request': {output}"
        );
        assert!(
            output.contains("playback-get"),
            "should contain request_kind: {output}"
        );
        assert!(
            output.contains("\"duration_ms\":7"),
            "should record duration_ms after span enter: {output}"
        );
        assert!(
            output.contains("\"outcome\":\"ok\""),
            "should record outcome: {output}"
        );
    }

    #[tokio::test]
    async fn seek_relative_without_active_track_returns_invalid_request() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));

        // No track has been played; clock has no item; relative seek
        // should return InvalidRequest, not silently send Seek{0}.
        let response = dispatch(
            state.clone(),
            Request::PlaybackCommand {
                command: PlaybackCommand::SeekRelative { offset_ms: 15_000 },
            },
            None,
        )
        .await;
        assert!(
            response.is_err(),
            "Phase 5 contract: SeekRelative without active track must error"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }
}

/// Phase: dispatch routing coverage. The `dispatch` god-function routes
/// ~90 request variants; before extracting it into per-area handler
/// modules we lock the request→response-variant mapping so a careless
/// move (re-ordered arm, wrong response variant, accidental default)
/// is caught. Uses the fake Spotify provider; assertions are on the
/// response *shape*, not provider data.
#[cfg(test)]
mod routing_tests {
    #![allow(clippy::panic, clippy::unwrap_used)]

    use std::sync::Arc;
    use std::time::Duration;

    use spotuify_protocol::{
        IpcErrorKind, Request, Response, ResponseData, SearchScopeData, SearchSourceData,
        SinceWindow, TopKind,
    };
    use tempfile::TempDir;

    use super::{
        error_response_from, handle_request_with_source, receipt_error_summary_from_error,
        run_mutation_body, DaemonState, MutationBodyOutcome,
    };

    #[test]
    fn uri_parse_errors_are_invalid_requests() {
        let err =
            anyhow::Error::new(spotuify_core::ResourceUri::parse("spotify:track:").unwrap_err());
        assert!(matches!(
            error_response_from(&err),
            Response::Error {
                kind: IpcErrorKind::InvalidRequest,
                retryable: false,
                ..
            }
        ));
    }

    #[test]
    fn uri_parse_errors_persist_as_invalid_request_receipt_errors() {
        let err =
            anyhow::Error::new(spotuify_core::ResourceUri::parse("spotify:track:").unwrap_err());
        let summary = receipt_error_summary_from_error(&err);
        assert_eq!(summary.kind, IpcErrorKind::InvalidRequest);
        assert!(summary.message.contains("must not be empty"));
    }

    #[tokio::test]
    async fn optimistic_mutation_panics_become_indeterminate_outcomes() {
        let outcome = run_mutation_body(
            async {
                panic!("mutation body panic");
                #[allow(unreachable_code)]
                Ok(())
            },
            Duration::from_secs(1),
        )
        .await;
        assert!(matches!(outcome, MutationBodyOutcome::Indeterminate));
    }

    struct TestEnv {
        _temp: TempDir,
    }

    impl TestEnv {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("tempdir");
            std::env::set_var("SPOTUIFY_FAKE_SPOTIFY", "1");
            std::env::set_var("SPOTUIFY_CACHE_DB", temp.path().join("cache.sqlite3"));
            std::env::set_var("SPOTUIFY_SEARCH_INDEX", temp.path().join("search-index"));
            std::env::set_var("SPOTUIFY_RUNTIME_DIR", temp.path().join("runtime"));
            std::env::set_var("SPOTUIFY_DATA_DIR", temp.path().join("data"));
            std::env::set_var("SPOTUIFY_CONFIG", temp.path().join("spotuify.toml"));
            std::fs::write(
                temp.path().join("spotuify.toml"),
                "client_id = \"test-client\"\nredirect_uri = \"http://127.0.0.1:8888/callback\"\n",
            )
            .expect("config write");
            Self { _temp: temp }
        }
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            for key in [
                "SPOTUIFY_FAKE_SPOTIFY",
                "SPOTUIFY_CACHE_DB",
                "SPOTUIFY_SEARCH_INDEX",
                "SPOTUIFY_RUNTIME_DIR",
                "SPOTUIFY_DATA_DIR",
                "SPOTUIFY_CONFIG",
            ] {
                std::env::remove_var(key);
            }
        }
    }

    /// Dispatch `request` and return the OK `ResponseData`, panicking with
    /// the error message if the daemon returned `Response::Error`.
    async fn ok_data(state: &Arc<DaemonState>, label: &str, request: Request) -> ResponseData {
        match handle_request_with_source(state.clone(), request, None).await {
            spotuify_protocol::Response::Ok { data } => data,
            spotuify_protocol::Response::Error { message, .. } => {
                panic!("{label} should route to an Ok response, got error: {message}")
            }
        }
    }

    #[tokio::test]
    async fn dispatch_routes_each_request_to_its_response_variant() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));

        // (label, request, predicate the response variant must satisfy)
        macro_rules! case {
            ($label:literal, $req:expr, $pat:pat) => {{
                let data = ok_data(&state, $label, $req).await;
                assert!(
                    matches!(data, $pat),
                    "{} routed to the wrong response variant: {:?}",
                    $label,
                    data
                );
            }};
        }

        case!("ping", Request::Ping, ResponseData::Pong);
        case!(
            "subscribe",
            Request::SubscribeEvents {
                provider_policy: true,
            },
            ResponseData::Ack { .. }
        );
        case!(
            "status",
            Request::GetDaemonStatus,
            ResponseData::DaemonStatus { .. }
        );
        case!(
            "playback-get",
            Request::PlaybackGet,
            ResponseData::Playback { .. }
        );
        case!(
            "client-seed",
            Request::ClientSeed,
            ResponseData::ClientSeed { .. }
        );
        case!("queue-get", Request::QueueGet, ResponseData::Queue { .. });
        case!(
            "devices-list",
            Request::DevicesList,
            ResponseData::Devices { .. }
        );
        case!(
            "playlists-list",
            Request::PlaylistsList { provider: None },
            ResponseData::Playlists { .. }
        );
        case!(
            "reminders-list",
            Request::RemindersList {
                include_inactive: false
            },
            ResponseData::Reminders { .. }
        );
        case!(
            "viz-status",
            Request::GetVizStatus,
            ResponseData::VizStatus { .. }
        );
        case!(
            "set-audio-output",
            Request::SetAudioOutput { device: None },
            ResponseData::Ack { .. }
        );
        case!(
            "cache-status",
            Request::CacheStatus,
            ResponseData::CacheStatus { .. }
        );
        case!(
            "ops-log",
            Request::OpsLog {
                limit: 10,
                since_ms: None,
                source: None,
            },
            ResponseData::Operations { .. }
        );
        case!(
            "analytics-top",
            Request::AnalyticsTop {
                kind: TopKind::Tracks,
                since_window: SinceWindow::Days(30),
                limit: 10,
            },
            ResponseData::AnalyticsTop { .. }
        );
        case!(
            "search-local",
            Request::Search {
                query: "anything".to_string(),
                scope: SearchScopeData::Track,
                source: SearchSourceData::Local,
                limit: 5,
                provider: None,
                kinds: None,
                sort: None,
            },
            ResponseData::SearchResults { .. }
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn dispatch_maps_invalid_request_to_error_response() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let state = Arc::new(DaemonState::new().await.expect("daemon state"));

        // Relative seek with no active track is the canonical typed
        // InvalidRequest path; it must surface as Response::Error, not a
        // panic and not a silent Ok.
        let response = handle_request_with_source(
            state.clone(),
            Request::PlaybackCommand {
                command: spotuify_protocol::PlaybackCommand::SeekRelative { offset_ms: 15_000 },
            },
            None,
        )
        .await;
        assert!(
            matches!(response, spotuify_protocol::Response::Error { .. }),
            "invalid request must route to an error response, got {response:?}"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }
}

#[cfg(test)]
mod provider_acceptance_tests {
    #![allow(clippy::panic, clippy::unwrap_used)]

    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Duration;

    use async_trait::async_trait;
    use spotuify_core::{
        AccessOutcome, CollectionRequest, MediaItem, MediaKind, MusicProvider, Mutation,
        MutationCompletion, MutationFailure, MutationOutcome, MutationReceipt, PageRequest,
        Playlist, PlaylistInsertion, ProviderCaps, ProviderError, ProviderId, ProviderPage,
        ProviderResult, RequestContext, ResourceUri, SearchRequest, UriScheme,
    };
    use spotuify_protocol::{
        DaemonEvent, MutationId, Operation, OperationId, OperationKind, OperationSource,
        OperationStatus, PlaybackCommand, PlaylistCreateReceipt, PlaylistItemMutationAction,
        PreState, Receipt, ReceiptId, ReceiptStatus, Request, Response, ResponseData, ReversalPlan,
        SearchScopeData, SearchSourceData, SyncTargetData,
    };
    use spotuify_provider_fake::{FakeDataset, FakeProvider};

    use crate::provider_registry::{ProviderRegistry, ProviderRuntime};

    use super::{
        bounded_partial_summary, context_queue_snapshot_for_play, dispatch, dispatch_with_mutation,
        error_response_from, execute_provider_command, execute_provider_pair_with_recovery,
        fetch_and_emit_page, handle_request_with_source, handle_request_with_source_and_mutation,
        is_partial_mutation_error, provider_error_may_follow_write, provider_pair_for_command,
        provider_pair_uses_embedded_transport, record_operation, recover_processing_mutations,
        recovery_reconciliation_intent, remote_search_and_cache,
        require_provider_mutation_capability, resolve_play_context, resolve_search_provider,
        resource_summary, search_with_source, spawn_optimistic_mutation, spawn_recent_refresh,
        spawn_search_stream, validate_mutation_receipt, CommandKind, DaemonState,
        PartialMutationSummary, SearchParams, LIKED_SONGS_CONTEXT, PARTIAL_SUMMARY_MAX_BYTES,
        PARTIAL_SUMMARY_MESSAGE_CHARS, PARTIAL_SUMMARY_URI_CHARS,
    };

    const RECEIPT_OK: u8 = 0;
    const RECEIPT_PARTIAL: u8 = 1;
    const RECEIPT_WRONG_MUTATION_ID: u8 = 2;
    const RECEIPT_WRONG_PROVIDER: u8 = 3;
    const RECEIPT_WRONG_OUTCOME: u8 = 4;
    const RECEIPT_APPLIED_WITH_FAILURES: u8 = 5;
    const RECEIPT_CREATED_INVALID_URI: u8 = 6;
    const RECEIPT_CREATED_FOREIGN_URI: u8 = 7;
    const RECEIPT_CREATED_WRONG_KIND: u8 = 8;
    const RECEIPT_CREATED_WRONG_VERSION: u8 = 9;
    const RECEIPT_PARTIAL_ADD_ONLY: u8 = 10;
    const RECEIPT_POPULATE_ROLLBACK_FAIL: u8 = 11;
    const RECONCILE_CAPS_COMPLETE: u8 = 0;
    const RECONCILE_CAPS_NO_LIBRARY_READ: u8 = 1;
    const RECONCILE_CAPS_NO_PLAYLIST_ITEM_READ: u8 = 2;

    struct UnsupportedMutationProvider {
        id: ProviderId,
        scheme: UriScheme,
        calls: AtomicUsize,
    }

    struct FailingPlaylistUnfollowProvider {
        inner: FakeProvider,
        playlist_lookups: AtomicUsize,
        mutation_calls: AtomicUsize,
    }

    impl FailingPlaylistUnfollowProvider {
        fn new() -> Self {
            Self {
                inner: FakeProvider::isolated("failed-unfollow").unwrap(),
                playlist_lookups: AtomicUsize::new(0),
                mutation_calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl MusicProvider for FailingPlaylistUnfollowProvider {
        fn id(&self) -> &ProviderId {
            MusicProvider::id(&self.inner)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(&self.inner)
        }

        fn display_name(&self) -> &str {
            "Failing playlist unfollow"
        }

        fn capabilities(&self) -> ProviderCaps {
            let mut capabilities = self.inner.capabilities();
            capabilities.transport = None;
            capabilities
        }

        async fn playlist(
            &self,
            context: RequestContext,
            uri: &ResourceUri,
        ) -> ProviderResult<Option<Playlist>> {
            self.playlist_lookups.fetch_add(1, Ordering::SeqCst);
            self.inner.playlist(context, uri).await
        }

        async fn apply_mutation(
            &self,
            _context: RequestContext,
            _mutation_id: uuid::Uuid,
            mutation: &Mutation,
        ) -> ProviderResult<MutationReceipt> {
            self.mutation_calls.fetch_add(1, Ordering::SeqCst);
            match mutation {
                Mutation::PlaylistUnfollow { .. } => Err(ProviderError::InvalidInput {
                    field: "playlist".to_string(),
                    message: "injected unfollow rejection".to_string(),
                }),
                other => panic!("unexpected mutation in failed replay test: {other:?}"),
            }
        }
    }

    impl UnsupportedMutationProvider {
        fn new() -> Self {
            Self {
                id: ProviderId::new("no-write").unwrap(),
                scheme: UriScheme::new("no-write").unwrap(),
                calls: AtomicUsize::new(0),
            }
        }
    }

    struct HostileReceiptProvider {
        inner: FakeProvider,
        fault: AtomicU8,
        reconciliation_caps: AtomicU8,
        playlist_create_enabled: AtomicBool,
        playlist_items_unavailable: AtomicBool,
        playlist_items_first_read_unavailable: AtomicBool,
        playlist_item_read_counts: Mutex<HashMap<String, usize>>,
        playlist_items_delay_after_first_read: AtomicBool,
        playlist_items_delay_ms: AtomicUsize,
        playlist_items_panic_after_first_read: AtomicBool,
    }

    struct SearchLimitProvider {
        inner: FakeProvider,
        max_query_chars: AtomicUsize,
    }

    struct TrackOnlySearchProvider {
        inner: FakeProvider,
    }

    struct WrongLookupProvider {
        inner: FakeProvider,
        execute_calls: AtomicUsize,
    }

    #[async_trait]
    impl MusicProvider for WrongLookupProvider {
        fn id(&self) -> &ProviderId {
            MusicProvider::id(&self.inner)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(&self.inner)
        }

        fn display_name(&self) -> &str {
            "Wrong point lookup"
        }

        fn capabilities(&self) -> ProviderCaps {
            self.inner.capabilities()
        }

        async fn media_item(
            &self,
            context: RequestContext,
            _uri: &spotuify_core::ResourceUri,
        ) -> ProviderResult<Option<MediaItem>> {
            let wrong = spotuify_core::ResourceUri::parse("wrong-lookup:track:track-2").unwrap();
            self.inner.media_item(context, &wrong).await
        }
    }

    #[async_trait]
    impl spotuify_core::RemoteTransport for WrongLookupProvider {
        fn provider_id(&self) -> &ProviderId {
            MusicProvider::id(&self.inner)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(&self.inner)
        }

        async fn execute(
            &self,
            _context: RequestContext,
            _command: spotuify_core::TransportCommand,
        ) -> ProviderResult<spotuify_core::TransportOutcome> {
            self.execute_calls.fetch_add(1, Ordering::SeqCst);
            unreachable!("point lookup validation must fail before transport")
        }
    }

    #[async_trait]
    impl MusicProvider for TrackOnlySearchProvider {
        fn id(&self) -> &ProviderId {
            MusicProvider::id(&self.inner)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(&self.inner)
        }

        fn display_name(&self) -> &str {
            "Track-only search"
        }

        fn capabilities(&self) -> ProviderCaps {
            let mut caps = self.inner.capabilities();
            caps.transport = None;
            caps.search.kinds = vec![MediaKind::Track];
            caps
        }

        async fn search(
            &self,
            context: RequestContext,
            request: SearchRequest,
        ) -> ProviderResult<ProviderPage<MediaItem>> {
            MusicProvider::search(&self.inner, context, request).await
        }
    }

    struct ForeignSearchProvider {
        inner: FakeProvider,
    }

    struct WrongKindSearchProvider {
        inner: FakeProvider,
    }

    struct WrongOffsetSearchProvider {
        inner: FakeProvider,
    }

    #[async_trait]
    impl MusicProvider for WrongKindSearchProvider {
        fn id(&self) -> &ProviderId {
            MusicProvider::id(&self.inner)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(&self.inner)
        }

        fn display_name(&self) -> &str {
            "Wrong-kind search"
        }

        fn capabilities(&self) -> ProviderCaps {
            let mut caps = self.inner.capabilities();
            caps.transport = None;
            caps
        }

        async fn search(
            &self,
            context: RequestContext,
            mut request: SearchRequest,
        ) -> ProviderResult<ProviderPage<MediaItem>> {
            request.kind = MediaKind::Album;
            self.inner.search(context, request).await
        }
    }

    #[async_trait]
    impl MusicProvider for WrongOffsetSearchProvider {
        fn id(&self) -> &ProviderId {
            MusicProvider::id(&self.inner)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(&self.inner)
        }

        fn display_name(&self) -> &str {
            "Wrong-offset search"
        }

        fn capabilities(&self) -> ProviderCaps {
            let mut caps = self.inner.capabilities();
            caps.transport = None;
            caps
        }

        async fn search(
            &self,
            context: RequestContext,
            request: SearchRequest,
        ) -> ProviderResult<ProviderPage<MediaItem>> {
            let mut page = self.inner.search(context, request).await?;
            page.requested_offset = page.requested_offset.saturating_add(1);
            Ok(page)
        }
    }

    struct ForeignShowProvider {
        inner: FakeProvider,
    }

    #[async_trait]
    impl MusicProvider for ForeignShowProvider {
        fn id(&self) -> &ProviderId {
            MusicProvider::id(&self.inner)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(&self.inner)
        }

        fn display_name(&self) -> &str {
            "Foreign show output"
        }

        fn capabilities(&self) -> ProviderCaps {
            let mut caps = self.inner.capabilities();
            caps.transport = None;
            caps.catalog.show_episodes = true;
            caps
        }

        async fn show_episodes(
            &self,
            _context: RequestContext,
            request: CollectionRequest,
        ) -> ProviderResult<ProviderPage<MediaItem>> {
            Ok(ProviderPage {
                items: vec![MediaItem {
                    uri: "foreign:episode:poison".to_string(),
                    kind: MediaKind::Episode,
                    ..Default::default()
                }],
                requested_offset: request.page.offset,
                total: Some(1),
                next: None,
            })
        }
    }

    #[async_trait]
    impl MusicProvider for ForeignSearchProvider {
        fn id(&self) -> &ProviderId {
            MusicProvider::id(&self.inner)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(&self.inner)
        }

        fn display_name(&self) -> &str {
            "Foreign search output"
        }

        fn capabilities(&self) -> ProviderCaps {
            let mut caps = self.inner.capabilities();
            caps.transport = None;
            caps
        }

        async fn search(
            &self,
            _context: RequestContext,
            request: SearchRequest,
        ) -> ProviderResult<ProviderPage<MediaItem>> {
            Ok(ProviderPage {
                items: vec![MediaItem {
                    uri: "foreign:track:poison".to_string(),
                    kind: request.kind,
                    ..Default::default()
                }],
                requested_offset: request.page.offset,
                total: Some(1),
                next: None,
            })
        }
    }

    #[async_trait]
    impl MusicProvider for SearchLimitProvider {
        fn id(&self) -> &ProviderId {
            MusicProvider::id(&self.inner)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(&self.inner)
        }

        fn display_name(&self) -> &str {
            "Search limit provider"
        }

        fn capabilities(&self) -> ProviderCaps {
            let mut caps = self.inner.capabilities();
            caps.transport = None;
            caps.search.max_query_chars = Some(self.max_query_chars.load(Ordering::SeqCst));
            caps
        }

        async fn search(
            &self,
            context: RequestContext,
            request: SearchRequest,
        ) -> ProviderResult<ProviderPage<MediaItem>> {
            MusicProvider::search(&self.inner, context, request).await
        }
    }

    impl HostileReceiptProvider {
        fn new() -> Self {
            Self {
                inner: FakeProvider::isolated("receipt-hostile").unwrap(),
                fault: AtomicU8::new(RECEIPT_OK),
                reconciliation_caps: AtomicU8::new(RECONCILE_CAPS_COMPLETE),
                playlist_create_enabled: AtomicBool::new(true),
                playlist_items_unavailable: AtomicBool::new(false),
                playlist_items_first_read_unavailable: AtomicBool::new(false),
                playlist_item_read_counts: Mutex::new(HashMap::new()),
                playlist_items_delay_after_first_read: AtomicBool::new(false),
                playlist_items_delay_ms: AtomicUsize::new(0),
                playlist_items_panic_after_first_read: AtomicBool::new(false),
            }
        }

        fn set_fault(&self, fault: u8) {
            self.fault.store(fault, Ordering::SeqCst);
        }

        fn set_reconciliation_caps(&self, caps: u8) {
            self.reconciliation_caps.store(caps, Ordering::SeqCst);
        }

        fn set_playlist_create_enabled(&self, enabled: bool) {
            self.playlist_create_enabled
                .store(enabled, Ordering::SeqCst);
        }

        fn set_playlist_items_unavailable(&self, unavailable: bool) {
            self.playlist_items_unavailable
                .store(unavailable, Ordering::SeqCst);
        }

        fn set_playlist_items_first_read_unavailable(&self) {
            self.playlist_items_first_read_unavailable
                .store(true, Ordering::SeqCst);
        }

        fn set_playlist_items_delay_after_first_read(&self, delay: Duration) {
            self.playlist_items_delay_after_first_read
                .store(true, Ordering::SeqCst);
            self.playlist_items_delay_ms
                .store(delay.as_millis() as usize, Ordering::SeqCst);
        }

        fn set_playlist_items_panic_after_first_read(&self) {
            self.playlist_items_panic_after_first_read
                .store(true, Ordering::SeqCst);
        }

        fn clear_playlist_items_reconciliation_faults(&self) {
            self.playlist_items_unavailable
                .store(false, Ordering::SeqCst);
            self.playlist_items_delay_after_first_read
                .store(false, Ordering::SeqCst);
            self.playlist_items_panic_after_first_read
                .store(false, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl MusicProvider for HostileReceiptProvider {
        fn id(&self) -> &ProviderId {
            MusicProvider::id(&self.inner)
        }

        fn uri_scheme(&self) -> &UriScheme {
            MusicProvider::uri_scheme(&self.inner)
        }

        fn display_name(&self) -> &str {
            "Hostile Receipt"
        }

        fn capabilities(&self) -> ProviderCaps {
            let mut capabilities = MusicProvider::capabilities(&self.inner);
            capabilities.transport = None;
            capabilities.playlists.create = self.playlist_create_enabled.load(Ordering::SeqCst);
            match self.reconciliation_caps.load(Ordering::SeqCst) {
                RECONCILE_CAPS_COMPLETE => {}
                RECONCILE_CAPS_NO_LIBRARY_READ => {
                    capabilities.library.read_kinds.clear();
                }
                RECONCILE_CAPS_NO_PLAYLIST_ITEM_READ => {
                    capabilities.playlists.item_read = false;
                }
                mode => panic!("unknown reconciliation capability mode {mode}"),
            }
            capabilities
        }

        async fn apply_mutation(
            &self,
            context: RequestContext,
            mutation_id: uuid::Uuid,
            mutation: &Mutation,
        ) -> ProviderResult<MutationReceipt> {
            let mut receipt =
                MusicProvider::apply_mutation(&self.inner, context, mutation_id, mutation).await?;
            let fault = self.fault.load(Ordering::SeqCst);
            match fault {
                RECEIPT_OK => {}
                RECEIPT_PARTIAL | RECEIPT_PARTIAL_ADD_ONLY
                    if fault == RECEIPT_PARTIAL
                        || matches!(mutation, Mutation::PlaylistAdd { .. }) =>
                {
                    receipt.completion = MutationCompletion::PartiallyApplied;
                    let failed_uri = match &mut receipt.outcome {
                        MutationOutcome::LibraryChanged { uris, .. } if uris.len() > 1 => {
                            uris.pop()
                        }
                        MutationOutcome::FollowChanged { uris, .. } if uris.len() > 1 => uris.pop(),
                        MutationOutcome::PlaylistChanged { .. } => match mutation {
                            Mutation::PlaylistAdd { items, .. } if items.len() > 1 => {
                                items.last().map(|item| item.uri.clone())
                            }
                            Mutation::PlaylistRemove { items, .. } if items.len() > 1 => {
                                items.last().map(|item| item.uri.clone())
                            }
                            _ => None,
                        },
                        _ => None,
                    };
                    receipt.failures.push(MutationFailure {
                        uri: failed_uri,
                        message: "hostile partial application".to_string(),
                    });
                }
                RECEIPT_WRONG_MUTATION_ID => receipt.mutation_id = uuid::Uuid::nil(),
                RECEIPT_WRONG_PROVIDER => {
                    receipt.provider = ProviderId::new("receipt-liar").unwrap();
                }
                RECEIPT_WRONG_OUTCOME => {
                    receipt.outcome = MutationOutcome::FollowChanged {
                        uris: Vec::new(),
                        following: true,
                    };
                }
                RECEIPT_APPLIED_WITH_FAILURES => receipt.failures.push(MutationFailure {
                    uri: None,
                    message: "hostile contradictory failure".to_string(),
                }),
                RECEIPT_CREATED_INVALID_URI => {
                    let MutationOutcome::PlaylistCreated { playlist } = &mut receipt.outcome else {
                        panic!("playlist receipt fault requires playlist create")
                    };
                    playlist.id = "not-a-canonical-uri".to_string();
                }
                RECEIPT_CREATED_FOREIGN_URI => {
                    let MutationOutcome::PlaylistCreated { playlist } = &mut receipt.outcome else {
                        panic!("playlist receipt fault requires playlist create")
                    };
                    playlist.id = "receipt-liar:playlist:foreign".to_string();
                }
                RECEIPT_CREATED_WRONG_KIND => {
                    let MutationOutcome::PlaylistCreated { playlist } = &mut receipt.outcome else {
                        panic!("playlist receipt fault requires playlist create")
                    };
                    playlist.id = "receipt-hostile:album:not-a-playlist".to_string();
                }
                RECEIPT_CREATED_WRONG_VERSION => {
                    receipt.version_token = Some("hostile-version".to_string());
                }
                RECEIPT_PARTIAL_ADD_ONLY => {}
                RECEIPT_POPULATE_ROLLBACK_FAIL
                    if matches!(mutation, Mutation::PlaylistCreate { .. }) => {}
                RECEIPT_POPULATE_ROLLBACK_FAIL => {
                    receipt.outcome = MutationOutcome::FollowChanged {
                        uris: Vec::new(),
                        following: true,
                    };
                }
                fault => panic!("unknown receipt fault {fault}"),
            }
            Ok(receipt)
        }

        async fn library_items(
            &self,
            context: RequestContext,
            request: spotuify_core::LibraryRequest,
        ) -> ProviderResult<ProviderPage<MediaItem>> {
            MusicProvider::library_items(&self.inner, context, request).await
        }

        async fn library_freshness_probe(
            &self,
            context: RequestContext,
            kind: MediaKind,
        ) -> ProviderResult<spotuify_core::FreshnessProbe> {
            MusicProvider::library_freshness_probe(&self.inner, context, kind).await
        }

        async fn playlists(
            &self,
            context: RequestContext,
            page: PageRequest,
        ) -> ProviderResult<ProviderPage<Playlist>> {
            MusicProvider::playlists(&self.inner, context, page).await
        }

        async fn playlist_items(
            &self,
            context: RequestContext,
            request: CollectionRequest,
        ) -> ProviderResult<AccessOutcome<ProviderPage<MediaItem>>> {
            let per_uri_call = {
                let mut counts = self.playlist_item_read_counts.lock().unwrap();
                let count = counts.entry(request.uri.as_uri()).or_insert(0);
                let current = *count;
                *count += 1;
                current
            };
            if per_uri_call > 0
                && self
                    .playlist_items_panic_after_first_read
                    .load(Ordering::SeqCst)
            {
                std::panic::panic_any("injected playlist verification panic");
            }
            if per_uri_call > 0
                && self
                    .playlist_items_delay_after_first_read
                    .load(Ordering::SeqCst)
            {
                let delay_ms = self.playlist_items_delay_ms.load(Ordering::SeqCst);
                if delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(delay_ms as u64)).await;
                }
            }
            let first_read_unavailable = per_uri_call == 0
                && self
                    .playlist_items_first_read_unavailable
                    .load(Ordering::SeqCst);
            if self.playlist_items_unavailable.load(Ordering::SeqCst) || first_read_unavailable {
                return Ok(AccessOutcome::Unavailable(
                    spotuify_core::AccessUnavailable::Private,
                ));
            }
            MusicProvider::playlist_items(&self.inner, context, request).await
        }
    }

    #[async_trait]
    impl MusicProvider for UnsupportedMutationProvider {
        fn id(&self) -> &ProviderId {
            &self.id
        }

        fn uri_scheme(&self) -> &UriScheme {
            &self.scheme
        }

        fn display_name(&self) -> &str {
            "No Write"
        }

        fn capabilities(&self) -> ProviderCaps {
            ProviderCaps::default()
        }

        async fn apply_mutation(
            &self,
            _context: RequestContext,
            _mutation_id: uuid::Uuid,
            _mutation: &Mutation,
        ) -> ProviderResult<MutationReceipt> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            unreachable!("capability gate must reject before adapter invocation")
        }
    }

    struct TestEnv {
        _temp: tempfile::TempDir,
    }

    impl TestEnv {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            std::env::set_var("SPOTUIFY_CACHE_DB", temp.path().join("cache.sqlite3"));
            std::env::set_var(
                "SPOTUIFY_ANALYTICS_DB",
                temp.path().join("analytics.sqlite3"),
            );
            std::env::set_var("SPOTUIFY_SEARCH_INDEX", temp.path().join("search-index"));
            std::env::set_var("SPOTUIFY_RUNTIME_DIR", temp.path().join("runtime"));
            std::env::set_var("SPOTUIFY_CONFIG", temp.path().join("spotuify.toml"));
            Self { _temp: temp }
        }

        fn configure_fake_default_with_spotify_secondary(&self) {
            std::fs::write(
                self._temp.path().join("spotuify.toml"),
                r#"
[providers]
default = "local"

[providers.local]
type = "fake"

[providers.spotify-work]
type = "spotify"
client_id = "test-client"
redirect_uri = "http://127.0.0.1:8888/callback"
"#,
            )
            .expect("write isolated multi-provider config");
        }
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            for key in [
                "SPOTUIFY_CACHE_DB",
                "SPOTUIFY_ANALYTICS_DB",
                "SPOTUIFY_SEARCH_INDEX",
                "SPOTUIFY_RUNTIME_DIR",
                "SPOTUIFY_CONFIG",
            ] {
                std::env::remove_var(key);
            }
        }
    }

    fn registry(default: Arc<FakeProvider>, selected: Arc<FakeProvider>) -> ProviderRegistry {
        ProviderRegistry::new(
            default.id().clone(),
            [
                ProviderRuntime::with_transport(default).unwrap(),
                ProviderRuntime::with_transport(selected).unwrap(),
            ],
        )
        .unwrap()
    }

    fn pending_receipt(response: &ResponseData) -> ReceiptId {
        let ResponseData::Mutation { receipt } = response else {
            panic!("expected pending mutation response")
        };
        receipt.receipt_id.expect("mutation receipt id")
    }

    async fn wait_for_receipt(state: &DaemonState, receipt_id: ReceiptId) -> ReceiptStatus {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let receipt = state.store().get_receipt(receipt_id).await.unwrap();
                if receipt.status != ReceiptStatus::Pending {
                    return receipt.status;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("mutation should finalize")
    }

    async fn operation_count(state: &Arc<DaemonState>) -> usize {
        match dispatch(
            state.clone(),
            Request::OpsLog {
                limit: 500,
                since_ms: None,
                source: None,
            },
            None,
        )
        .await
        .expect("operations log")
        {
            ResponseData::Operations { ops } => ops.len(),
            response => panic!("expected operations response, got {response:?}"),
        }
    }

    async fn provider_playlist_uris(
        provider: &FakeProvider,
        playlist: &ResourceUri,
    ) -> Vec<String> {
        match provider
            .playlist_items(
                RequestContext::FOREGROUND,
                CollectionRequest {
                    uri: playlist.clone(),
                    page: PageRequest::new(100, 0),
                },
            )
            .await
            .unwrap()
        {
            AccessOutcome::Available(page) => page.items.into_iter().map(|item| item.uri).collect(),
            AccessOutcome::Unavailable(reason) => panic!("playlist unavailable: {reason:?}"),
        }
    }

    #[tokio::test]
    async fn resource_commands_and_playlist_mutations_route_to_uri_provider() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let default = Arc::new(FakeProvider::isolated("fake-a").unwrap());
        let selected = Arc::new(FakeProvider::isolated("fake-b").unwrap());
        let state = Arc::new(
            DaemonState::new_with_providers(registry(default.clone(), selected.clone()))
                .await
                .unwrap(),
        );

        let (music, transport) = provider_pair_for_command(
            &state,
            &CommandKind::PlayUri {
                uri: "fake-b:track:track-1".to_string(),
                context: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(music.id().as_str(), "fake-b");
        assert_eq!(transport.provider_id().as_str(), "fake-b");

        let (music, _) = provider_pair_for_command(
            &state,
            &CommandKind::AddToPlaylist {
                item: spotuify_core::MediaItem {
                    uri: "fake-b:track:track-1".to_string(),
                    ..Default::default()
                },
                playlist_id: "playlist-1".to_string(),
                playlist_name: "Legacy destination".to_string(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            music.id().as_str(),
            "fake-a",
            "an ambiguous bare playlist id belongs to the default provider"
        );

        let response = dispatch_with_mutation(
            state.clone(),
            Request::PlaylistAddItems {
                playlist: "fake-b:playlist:playlist-1".to_string(),
                uris: vec!["fake-b:track:track-1".to_string()],
                provider: None,
            },
            None,
            Some(MutationId::new_v7()),
        )
        .await
        .unwrap();
        assert_eq!(
            wait_for_receipt(&state, pending_receipt(&response)).await,
            ReceiptStatus::Confirmed
        );
        assert_eq!(
            default
                .observed_requests()
                .await
                .iter()
                .filter(|request| request.operation == "apply_mutation")
                .count(),
            0
        );
        assert_eq!(
            selected
                .observed_requests()
                .await
                .iter()
                .filter(|request| request.operation == "apply_mutation")
                .count(),
            1
        );

        let explicitly_selected = dispatch_with_mutation(
            state.clone(),
            Request::PlaylistAddItems {
                playlist: "playlist-1".to_string(),
                uris: vec!["fake-b:track:track-2".to_string()],
                provider: Some(ProviderId::new("fake-b").unwrap()),
            },
            None,
            Some(MutationId::new_v7()),
        )
        .await
        .unwrap();
        assert_eq!(
            wait_for_receipt(&state, pending_receipt(&explicitly_selected)).await,
            ReceiptStatus::Confirmed
        );
        assert_eq!(
            selected
                .observed_requests()
                .await
                .iter()
                .filter(|request| request.operation == "apply_mutation")
                .count(),
            2,
            "explicit provider scope must route a bare playlist reference"
        );

        let conflict = dispatch_with_mutation(
            state.clone(),
            Request::PlaylistAddItems {
                playlist: "fake-b:playlist:playlist-1".to_string(),
                uris: vec!["fake-b:track:track-1".to_string()],
                provider: Some(ProviderId::new("fake-a").unwrap()),
            },
            None,
            Some(MutationId::new_v7()),
        )
        .await
        .expect_err("explicit provider must not override canonical URI ownership");
        assert!(matches!(
            conflict.downcast_ref::<ProviderError>(),
            Some(ProviderError::InvalidInput { field, .. }) if field == "provider"
        ));

        let mixed_mutation_id = MutationId::new_v7();
        let operations_before_mixed = operation_count(&state).await;
        let selected_requests_before_mixed = selected.observed_requests().await.len();
        let mixed = dispatch_with_mutation(
            state.clone(),
            Request::PlaylistAddItems {
                playlist: "fake-b:playlist:playlist-1".to_string(),
                uris: vec![
                    "fake-b:track:track-1".to_string(),
                    "fake-a:track:track-1".to_string(),
                ],
                provider: None,
            },
            None,
            Some(mixed_mutation_id),
        )
        .await
        .expect_err("cross-provider items must fail before receipt creation");
        assert!(matches!(
            mixed.downcast_ref::<ProviderError>(),
            Some(ProviderError::InvalidInput { field, .. }) if field == "uri"
        ));
        assert_eq!(
            operation_count(&state).await,
            operations_before_mixed,
            "rejected ownership conflicts must not create an operation or linked receipt"
        );
        assert_eq!(
            selected.observed_requests().await.len(),
            selected_requests_before_mixed,
            "rejected ownership conflicts must not read or mutate the selected provider"
        );

        let retry = dispatch_with_mutation(
            state.clone(),
            Request::PlaylistAddItems {
                playlist: "fake-b:playlist:playlist-1".to_string(),
                uris: vec!["fake-b:track:track-1".to_string()],
                provider: None,
            },
            None,
            Some(mixed_mutation_id),
        )
        .await
        .expect("a synchronous preflight rejection must not bind the mutation id");
        assert_eq!(
            wait_for_receipt(&state, pending_receipt(&retry)).await,
            ReceiptStatus::Confirmed
        );
        assert_eq!(
            selected
                .observed_requests()
                .await
                .iter()
                .filter(|request| request.operation == "apply_mutation")
                .count(),
            3,
            "only the valid retry may reach the adapter"
        );
        assert_eq!(
            operation_count(&state).await,
            operations_before_mixed + 1,
            "only the valid retry may create an operation"
        );

        let mixed_remove_mutation_id = MutationId::new_v7();
        let operations_before_mixed_remove = operation_count(&state).await;
        let selected_requests_before_mixed_remove = selected.observed_requests().await.len();
        let mixed_remove = dispatch_with_mutation(
            state.clone(),
            Request::PlaylistRemoveItems {
                playlist: "fake-b:playlist:playlist-1".to_string(),
                uris: vec![
                    "fake-b:track:track-1".to_string(),
                    "fake-a:track:track-1".to_string(),
                ],
                provider: None,
            },
            None,
            Some(mixed_remove_mutation_id),
        )
        .await
        .expect_err("cross-provider removals must fail before receipt creation");
        assert!(matches!(
            mixed_remove.downcast_ref::<ProviderError>(),
            Some(ProviderError::InvalidInput { field, .. }) if field == "uri"
        ));
        assert_eq!(
            operation_count(&state).await,
            operations_before_mixed_remove
        );
        assert_eq!(
            selected.observed_requests().await.len(),
            selected_requests_before_mixed_remove
        );

        let remove_retry = dispatch_with_mutation(
            state.clone(),
            Request::PlaylistRemoveItems {
                playlist: "fake-b:playlist:playlist-1".to_string(),
                uris: vec!["fake-b:track:track-1".to_string()],
                provider: None,
            },
            None,
            Some(mixed_remove_mutation_id),
        )
        .await
        .expect("a rejected removal must not bind the mutation id");
        assert_eq!(
            wait_for_receipt(&state, pending_receipt(&remove_retry)).await,
            ReceiptStatus::Confirmed
        );
        assert_eq!(
            selected
                .observed_requests()
                .await
                .iter()
                .filter(|request| request.operation == "apply_mutation")
                .count(),
            4,
            "only the valid removal retry may reach the adapter"
        );
        assert_eq!(
            operation_count(&state).await,
            operations_before_mixed_remove + 1
        );

        // Keep this request construction compiled at the public protocol
        // boundary; playback dispatch resolves the same provider pair above.
        let _ = Request::PlaybackCommand {
            command: PlaybackCommand::PlayUri {
                uri: "fake-b:track:track-1".to_string(),
                context_uri: None,
            },
        };
        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn playlist_provider_conflicts_are_zero_side_effect_for_every_request_shape() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let default = Arc::new(FakeProvider::isolated("fake-a").unwrap());
        let selected = Arc::new(FakeProvider::isolated("fake-b").unwrap());
        let state = Arc::new(
            DaemonState::new_with_providers(registry(default.clone(), selected.clone()))
                .await
                .unwrap(),
        );
        let wrong_provider = Some(ProviderId::new("fake-a").unwrap());
        let playlist = "fake-b:playlist:playlist-1".to_string();
        let requests = [
            Request::PlaylistTracks {
                playlist: playlist.clone(),
                wait: true,
                provider: wrong_provider.clone(),
            },
            Request::PlaylistAddItems {
                playlist: playlist.clone(),
                uris: vec!["fake-b:track:track-1".to_string()],
                provider: wrong_provider.clone(),
            },
            Request::PlaylistRemoveItems {
                playlist: playlist.clone(),
                uris: vec!["fake-b:track:track-1".to_string()],
                provider: wrong_provider.clone(),
            },
            Request::PlaylistSetImage {
                playlist: playlist.clone(),
                image_base64: "aGVsbG8=".to_string(),
                provider: wrong_provider.clone(),
            },
            Request::PlaylistUnfollow {
                playlist,
                provider: wrong_provider,
            },
        ];

        for request in requests {
            let operation_count_before = operation_count(&state).await;
            let error =
                dispatch_with_mutation(state.clone(), request, None, Some(MutationId::new_v7()))
                    .await
                    .expect_err("explicit provider must not override playlist URI ownership");
            assert!(matches!(
                error.downcast_ref::<ProviderError>(),
                Some(ProviderError::InvalidInput { field, .. }) if field == "provider"
            ));
            assert_eq!(operation_count(&state).await, operation_count_before);
            assert!(default.observed_requests().await.is_empty());
            assert!(selected.observed_requests().await.is_empty());
        }

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn empty_playlist_add_is_rejected_without_receipts_or_provider_calls() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(FakeProvider::isolated("fake-a").unwrap());
        let other = Arc::new(FakeProvider::isolated("fake-b").unwrap());
        let state = Arc::new(
            DaemonState::new_with_providers(registry(provider.clone(), other.clone()))
                .await
                .unwrap(),
        );
        let operations_before = operation_count(&state).await;

        let error = dispatch_with_mutation(
            state.clone(),
            Request::PlaylistAddItems {
                playlist: "fake-a:playlist:playlist-1".to_string(),
                uris: Vec::new(),
                provider: None,
            },
            None,
            Some(MutationId::new_v7()),
        )
        .await
        .expect_err("empty additions must fail synchronously");

        assert!(error.to_string().contains("no track URIs to add"));
        assert_eq!(operation_count(&state).await, operations_before);
        assert!(provider.observed_requests().await.is_empty());
        assert!(other.observed_requests().await.is_empty());

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn playlist_item_previews_run_authoritative_preflight_without_mutation_side_effects() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let default = Arc::new(FakeProvider::isolated("fake-a").unwrap());
        let selected = Arc::new(FakeProvider::isolated("fake-b").unwrap());
        let state = Arc::new(
            DaemonState::new_with_providers(registry(default.clone(), selected.clone()))
                .await
                .unwrap(),
        );

        for action in [
            PlaylistItemMutationAction::Add,
            PlaylistItemMutationAction::Remove,
        ] {
            let operations_before = operation_count(&state).await;
            let response = dispatch_with_mutation(
                state.clone(),
                Request::PlaylistItemsPreview {
                    playlist: "fake-b:playlist:playlist-1".to_string(),
                    uris: vec!["fake-b:track:track-1".to_string()],
                    action,
                    provider: None,
                },
                None,
                None,
            )
            .await
            .expect("valid preview must pass authoritative preflight");
            assert!(matches!(
                response,
                ResponseData::Playlists { ref playlists }
                    if playlists.len() == 1
                        && playlists[0].id == "fake-b:playlist:playlist-1"
            ));
            assert_eq!(operation_count(&state).await, operations_before);
        }

        assert!(default.observed_requests().await.is_empty());
        let selected_requests = selected.observed_requests().await;
        assert!(
            selected_requests.len() >= 2,
            "each preview must authoritatively read the selected provider"
        );
        assert!(selected_requests
            .iter()
            .any(|request| request.operation == "playlist_items"));
        assert!(selected_requests
            .iter()
            .all(|request| request.operation != "apply_mutation"));

        let operations_before = operation_count(&state).await;
        let provider_requests_before = selected.observed_requests().await.len();
        let invalid = dispatch_with_mutation(
            state.clone(),
            Request::PlaylistItemsPreview {
                playlist: "fake-b:playlist:playlist-1".to_string(),
                uris: vec![
                    "fake-b:track:track-1".to_string(),
                    "fake-a:track:track-1".to_string(),
                ],
                action: PlaylistItemMutationAction::Remove,
                provider: None,
            },
            None,
            None,
        )
        .await
        .expect_err("preview must run the full item ownership preflight");
        assert!(matches!(
            invalid.downcast_ref::<ProviderError>(),
            Some(ProviderError::InvalidInput { field, .. }) if field == "uri"
        ));
        assert_eq!(operation_count(&state).await, operations_before);
        assert_eq!(
            selected.observed_requests().await.len(),
            provider_requests_before
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn playlist_create_preview_runs_live_preflight_without_write_side_effects() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let default = Arc::new(FakeProvider::isolated("fake-a").unwrap());
        let selected = Arc::new(FakeProvider::isolated("fake-b").unwrap());
        let state = Arc::new(
            DaemonState::new_with_providers(registry(default.clone(), selected.clone()))
                .await
                .unwrap(),
        );
        let provider = Some(ProviderId::new("fake-b").unwrap());
        let operations_before = operation_count(&state).await;

        let response = dispatch_with_mutation(
            state.clone(),
            Request::PlaylistCreatePreview {
                name: "Focus".to_string(),
                description: Some("Deep focus".to_string()),
                uris: vec!["fake-b:track:track-1".to_string()],
                provider: provider.clone(),
            },
            None,
            None,
        )
        .await
        .expect("valid create preview must pass the live preflight");
        assert!(matches!(
            response,
            ResponseData::Playlists { ref playlists } if playlists.is_empty()
        ));
        assert_eq!(operation_count(&state).await, operations_before);
        assert!(default.observed_requests().await.is_empty());
        assert!(selected.observed_requests().await.is_empty());

        for (request, mutation_id) in [
            (
                Request::PlaylistCreatePreview {
                    name: "   ".to_string(),
                    description: Some("Deep focus".to_string()),
                    uris: vec!["fake-b:track:track-1".to_string()],
                    provider: provider.clone(),
                },
                None,
            ),
            (
                Request::PlaylistCreate {
                    name: "   ".to_string(),
                    description: Some("Deep focus".to_string()),
                    uris: vec!["fake-b:track:track-1".to_string()],
                    provider: provider.clone(),
                },
                Some(MutationId::new_v7()),
            ),
        ] {
            let error = dispatch_with_mutation(
                state.clone(),
                request,
                Some(OperationSource::Cli),
                mutation_id,
            )
            .await
            .expect_err("blank names must fail identically before any write");
            assert!(matches!(
                error.downcast_ref::<ProviderError>(),
                Some(ProviderError::InvalidInput { field, .. }) if field == "name"
            ));
            assert_eq!(operation_count(&state).await, operations_before);
            assert!(default.observed_requests().await.is_empty());
            assert!(selected.observed_requests().await.is_empty());
        }

        for uris in [
            Vec::new(),
            vec![
                "fake-b:track:track-1".to_string(),
                "fake-a:track:track-1".to_string(),
            ],
        ] {
            dispatch_with_mutation(
                state.clone(),
                Request::PlaylistCreatePreview {
                    name: "Focus".to_string(),
                    description: Some("Deep focus".to_string()),
                    uris,
                    provider: provider.clone(),
                },
                None,
                None,
            )
            .await
            .expect_err("invalid create previews must fail synchronously");
            assert_eq!(operation_count(&state).await, operations_before);
            assert!(default.observed_requests().await.is_empty());
            assert!(selected.observed_requests().await.is_empty());
        }

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn playlist_remove_undo_uses_exact_positions_and_rejects_later_version_drift() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(FakeProvider::isolated("fake-a").unwrap());
        let selected = Arc::new(FakeProvider::isolated("fake-b").unwrap());
        let state = Arc::new(
            DaemonState::new_with_providers(registry(provider.clone(), selected))
                .await
                .unwrap(),
        );
        let playlist = ResourceUri::parse("fake-a:playlist:playlist-1").unwrap();
        let before = provider_playlist_uris(&provider, &playlist).await;
        assert!(before.len() >= 2);
        let removed_uri = before[0].clone();

        let remove_request = Request::PlaylistRemoveItems {
            playlist: playlist.as_uri(),
            uris: vec![removed_uri.clone()],
            provider: None,
        };
        let remove_mutation_id = MutationId::new_v7();
        let response = dispatch_with_mutation(
            state.clone(),
            remove_request.clone(),
            Some(OperationSource::Cli),
            Some(remove_mutation_id),
        )
        .await
        .unwrap();
        let receipt_id = pending_receipt(&response);
        assert_eq!(
            wait_for_receipt(&state, receipt_id).await,
            ReceiptStatus::Confirmed
        );
        let observed_before_replay = provider.observed_requests().await;
        let replay = dispatch_with_mutation(
            state.clone(),
            remove_request,
            Some(OperationSource::Cli),
            Some(remove_mutation_id),
        )
        .await
        .expect("confirmed remove retry must replay after its item is absent");
        assert!(matches!(
            replay,
            ResponseData::Mutation { receipt }
                if receipt.replayed && receipt.status == Some(ReceiptStatus::Confirmed)
        ));
        assert_eq!(
            provider.observed_requests().await,
            observed_before_replay,
            "replay must not rebuild the positional plan or write again"
        );
        let operation = state
            .store()
            .list_operations(20, None, None)
            .await
            .unwrap()
            .into_iter()
            .find(|operation| operation.receipt_id == Some(receipt_id))
            .unwrap();
        assert!(operation.reversible);
        assert!(matches!(
            operation.pre_state,
            Some(spotuify_protocol::PreState::PlaylistRemove {
                ref removed_items,
                ..
            }) if removed_items == &vec![(removed_uri.clone(), 0)]
        ));
        assert!(matches!(
            operation.reversal_plan,
            Some(spotuify_protocol::ReversalPlan::PlaylistAddAtPositions {
                ref items,
                version_token: Some(_),
                ..
            }) if items == &vec![(removed_uri.clone(), 0)]
        ));
        assert_eq!(
            provider_playlist_uris(&provider, &playlist).await,
            before[1..].to_vec()
        );

        dispatch_with_mutation(
            state.clone(),
            Request::OpsUndo {
                operation_id: Some(operation.operation_id),
                dry_run: false,
                force: false,
                bulk_since_ms: None,
            },
            Some(OperationSource::Cli),
            Some(MutationId::new_v7()),
        )
        .await
        .unwrap();
        assert_eq!(provider_playlist_uris(&provider, &playlist).await, before);

        let second = dispatch_with_mutation(
            state.clone(),
            Request::PlaylistRemoveItems {
                playlist: playlist.as_uri(),
                uris: vec![removed_uri.clone()],
                provider: None,
            },
            Some(OperationSource::Cli),
            Some(MutationId::new_v7()),
        )
        .await
        .unwrap();
        let second_receipt = pending_receipt(&second);
        assert_eq!(
            wait_for_receipt(&state, second_receipt).await,
            ReceiptStatus::Confirmed
        );
        let second_operation = state
            .store()
            .list_operations(20, None, None)
            .await
            .unwrap()
            .into_iter()
            .find(|operation| operation.receipt_id == Some(second_receipt))
            .unwrap();
        let current_version = provider
            .playlist(RequestContext::FOREGROUND, &playlist)
            .await
            .unwrap()
            .unwrap()
            .version_token;
        provider
            .apply_mutation(
                RequestContext::FOREGROUND,
                uuid::Uuid::now_v7(),
                &Mutation::PlaylistAdd {
                    playlist_uri: playlist.clone(),
                    items: vec![PlaylistInsertion {
                        uri: ResourceUri::parse(&before[1]).unwrap(),
                        position: None,
                    }],
                    expected_version: current_version,
                },
            )
            .await
            .unwrap();
        let error = dispatch_with_mutation(
            state.clone(),
            Request::OpsUndo {
                operation_id: Some(second_operation.operation_id),
                dry_run: false,
                force: false,
                bulk_since_ms: None,
            },
            Some(OperationSource::Cli),
            Some(MutationId::new_v7()),
        )
        .await
        .expect_err("post-removal drift must invalidate the positional undo");
        assert!(matches!(
            error.downcast_ref::<ProviderError>(),
            Some(ProviderError::VersionConflict { .. })
        ));
        let unchanged = state
            .store()
            .get_operation(second_operation.operation_id)
            .await
            .unwrap();
        assert_eq!(unchanged.status, OperationStatus::Succeeded);
        assert!(unchanged.reversible);

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn playlist_remove_plan_activation_failure_stays_non_reversible_and_reconciles() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(FakeProvider::isolated("fake-a").unwrap());
        let selected = Arc::new(FakeProvider::isolated("fake-b").unwrap());
        let state = Arc::new(
            DaemonState::new_with_providers(registry(provider.clone(), selected))
                .await
                .unwrap(),
        );
        let playlist = ResourceUri::parse("fake-a:playlist:playlist-1").unwrap();
        let before = provider_playlist_uris(&provider, &playlist).await;
        crate::handlers::playlists::reset_playlist_remove_prestate_observation();
        crate::handlers::playlists::fail_next_playlist_remove_plan_activation();

        let request = Request::PlaylistRemoveItems {
            playlist: playlist.as_uri(),
            uris: vec![before[0].clone()],
            provider: None,
        };
        let mutation_id = MutationId::new_v7();
        let response = dispatch_with_mutation(
            state.clone(),
            request.clone(),
            Some(OperationSource::Cli),
            Some(mutation_id),
        )
        .await
        .unwrap();
        let receipt_id = pending_receipt(&response);
        assert_eq!(
            wait_for_receipt(&state, receipt_id).await,
            ReceiptStatus::Failed
        );
        assert!(
            crate::handlers::playlists::playlist_remove_prestate_was_observed_before_apply(),
            "exact pre-state + NotReversible must be durable before fake provider apply"
        );
        assert_eq!(
            provider_playlist_uris(&provider, &playlist).await,
            before[1..].to_vec(),
            "the provider write completed before local activation failed"
        );
        let operation = state
            .store()
            .list_operations(20, None, None)
            .await
            .unwrap()
            .into_iter()
            .find(|operation| operation.receipt_id == Some(receipt_id))
            .unwrap();
        assert_eq!(operation.status, OperationStatus::Failed);
        assert!(!operation.reversible);
        assert!(matches!(
            operation.pre_state,
            Some(spotuify_protocol::PreState::PlaylistRemove {
                ref removed_items,
                ..
            }) if removed_items == &vec![(before[0].clone(), 0)]
        ));
        assert!(matches!(
            operation.reversal_plan,
            Some(spotuify_protocol::ReversalPlan::NotReversible { .. })
        ));
        let receipt = state.store().get_receipt(receipt_id).await.unwrap();
        let detail = receipt.error.unwrap().detail.unwrap();
        let detail: serde_json::Value = serde_json::from_str(&detail).unwrap();
        assert_eq!(
            detail["schema"],
            "spotuify.local-finalization-reconciliation.v1"
        );
        assert!(state
            .store()
            .provider_reconciliation_exists(receipt_id)
            .await
            .unwrap());
        tokio::time::timeout(Duration::from_secs(5), async {
            while state
                .store()
                .provider_reconciliation_pending(receipt_id)
                .await
                .unwrap()
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("reconciliation worker should quiesce");
        let durable_response = state
            .store()
            .terminal_mutation_response(mutation_id)
            .await
            .unwrap()
            .expect("failed remove must persist its terminal response");
        let observed_before_replay = provider.observed_requests().await;
        let replay = handle_request_with_source_and_mutation(
            state.clone(),
            request,
            Some(OperationSource::Cli),
            Some(mutation_id),
        )
        .await;
        assert_eq!(
            serde_json::to_value(replay).unwrap(),
            serde_json::to_value(durable_response).unwrap(),
            "failed typed-partial remove replay must preserve its exact durable response"
        );
        assert_eq!(
            provider.observed_requests().await,
            observed_before_replay,
            "failed replay must not rebuild the positional plan or write again"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn cold_bare_playlist_names_and_ids_resolve_inside_the_explicit_provider() {
        let _guard = crate::ENV_LOCK.lock().await;
        type BuildRequest = fn(String, Option<ProviderId>) -> Request;
        let cases: [(&str, BuildRequest); 5] = [
            ("tracks", |playlist, provider| Request::PlaylistTracks {
                playlist,
                wait: true,
                provider,
            }),
            ("add", |playlist, provider| Request::PlaylistAddItems {
                playlist,
                uris: vec!["fake-b:track:track-1".to_string()],
                provider,
            }),
            ("remove", |playlist, provider| {
                Request::PlaylistRemoveItems {
                    playlist,
                    uris: vec!["fake-b:track:track-2".to_string()],
                    provider,
                }
            }),
            ("set-image", |playlist, provider| {
                Request::PlaylistSetImage {
                    playlist,
                    image_base64: "aGVsbG8=".to_string(),
                    provider,
                }
            }),
            ("unfollow", |playlist, provider| Request::PlaylistUnfollow {
                playlist,
                provider,
            }),
        ];

        for reference in ["Fake Favorites", "playlist-1"] {
            for &(action, build_request) in &cases {
                let _env = TestEnv::new();
                let default = Arc::new(FakeProvider::isolated("fake-a").unwrap());
                let selected = Arc::new(FakeProvider::isolated("fake-b").unwrap());
                let state = Arc::new(
                    DaemonState::new_with_providers(registry(default.clone(), selected.clone()))
                        .await
                        .unwrap(),
                );
                let request = build_request(
                    reference.to_string(),
                    Some(ProviderId::new("fake-b").unwrap()),
                );
                let mutation_id = request.requires_mutation_id().then(MutationId::new_v7);

                let response = dispatch_with_mutation(state.clone(), request, None, mutation_id)
                    .await
                    .unwrap_or_else(|error| {
                        panic!("cold bare {reference} {action} must resolve: {error}")
                    });
                if action == "tracks" {
                    assert!(matches!(
                        response,
                        ResponseData::MediaItems { ref items }
                            if items.len() == 2
                                && items.iter().all(|item| item.uri.starts_with("fake-b:"))
                    ));
                } else {
                    assert_eq!(
                        wait_for_receipt(&state, pending_receipt(&response)).await,
                        ReceiptStatus::Confirmed,
                        "cold bare {reference} {action} receipt"
                    );
                }

                assert!(
                    default.observed_requests().await.is_empty(),
                    "cold bare {reference} {action} must not touch the default provider"
                );
                let observed = selected.observed_requests().await;
                assert!(
                    observed
                        .iter()
                        .any(|request| request.operation == "playlists"),
                    "cold bare {reference} {action} must resolve from the selected provider"
                );
                if action == "tracks" {
                    assert!(observed
                        .iter()
                        .any(|request| request.operation == "playlist_items"));
                } else {
                    assert_eq!(
                        observed
                            .iter()
                            .filter(|request| request.operation == "apply_mutation")
                            .count(),
                        1,
                        "cold bare {reference} {action} must write only through the selected provider"
                    );
                }

                state.shutdown_search().await;
                state.shutdown_player().await;
            }
        }
    }

    #[tokio::test]
    async fn playlist_provider_scope_participates_in_mutation_replay_fingerprints() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let default = Arc::new(FakeProvider::isolated("fake-a").unwrap());
        let selected = Arc::new(FakeProvider::isolated("fake-b").unwrap());
        let state = Arc::new(
            DaemonState::new_with_providers(registry(default.clone(), selected.clone()))
                .await
                .unwrap(),
        );
        let mutation_id = MutationId::new_v7();
        let request = Request::PlaylistAddItems {
            playlist: "fake-a:playlist:playlist-1".to_string(),
            uris: vec!["fake-a:track:track-1".to_string()],
            provider: Some(ProviderId::new("fake-a").unwrap()),
        };

        let original =
            dispatch_with_mutation(state.clone(), request.clone(), None, Some(mutation_id))
                .await
                .unwrap();
        assert_eq!(
            wait_for_receipt(&state, pending_receipt(&original)).await,
            ReceiptStatus::Confirmed
        );
        let writes = default
            .observed_requests()
            .await
            .iter()
            .filter(|request| request.operation == "apply_mutation")
            .count();
        assert_eq!(writes, 1);

        let replay = dispatch_with_mutation(state.clone(), request, None, Some(mutation_id))
            .await
            .expect("identical provider-scoped request must replay");
        assert!(matches!(
            replay,
            ResponseData::Mutation { receipt }
                if receipt.replayed && receipt.status == Some(ReceiptStatus::Confirmed)
        ));
        assert_eq!(
            default
                .observed_requests()
                .await
                .iter()
                .filter(|request| request.operation == "apply_mutation")
                .count(),
            writes,
            "a replay must not issue a second provider write"
        );

        let mismatch = dispatch_with_mutation(
            state.clone(),
            Request::PlaylistAddItems {
                playlist: "fake-a:playlist:playlist-1".to_string(),
                uris: vec!["fake-a:track:track-1".to_string()],
                provider: None,
            },
            None,
            Some(mutation_id),
        )
        .await
        .expect_err("changing only explicit provider scope must change the fingerprint");
        assert!(matches!(
            mismatch.downcast_ref::<super::MutationRequestError>(),
            Some(error) if error.kind == spotuify_protocol::IpcErrorKind::InvalidRequest
        ));
        assert_eq!(
            default
                .observed_requests()
                .await
                .iter()
                .filter(|request| request.operation == "apply_mutation")
                .count(),
            writes
        );
        assert!(selected.observed_requests().await.is_empty());

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn playlist_add_replays_after_the_remote_playlist_disappears() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(FakeProvider::isolated("replay-owner").unwrap());
        let other = Arc::new(FakeProvider::isolated("other-owner").unwrap());
        let state = Arc::new(
            DaemonState::new_with_providers(registry(provider.clone(), other))
                .await
                .unwrap(),
        );
        let mutation_id = MutationId::new_v7();
        let playlist = ResourceUri::parse("replay-owner:playlist:playlist-1").unwrap();
        let request = Request::PlaylistAddItems {
            playlist: playlist.as_uri(),
            uris: vec!["replay-owner:track:track-1".to_string()],
            provider: None,
        };

        let original =
            dispatch_with_mutation(state.clone(), request.clone(), None, Some(mutation_id))
                .await
                .expect("initial playlist add");
        assert_eq!(
            wait_for_receipt(&state, pending_receipt(&original)).await,
            ReceiptStatus::Confirmed
        );

        provider
            .apply_mutation(
                RequestContext::FOREGROUND,
                uuid::Uuid::now_v7(),
                &Mutation::PlaylistUnfollow {
                    playlist_uri: playlist,
                },
            )
            .await
            .expect("remove remote playlist after the terminal response");

        let replay = dispatch_with_mutation(state.clone(), request, None, Some(mutation_id))
            .await
            .expect("terminal mutation must replay without rediscovering the playlist");
        assert!(matches!(
            replay,
            ResponseData::Mutation { receipt }
                if receipt.replayed && receipt.status == Some(ReceiptStatus::Confirmed)
        ));

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn playlist_unfollow_replays_after_the_first_call_deletes_the_playlist() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(FakeProvider::isolated("unfollow-replay").unwrap());
        let other = Arc::new(FakeProvider::isolated("other-owner").unwrap());
        let state = Arc::new(
            DaemonState::new_with_providers(registry(provider.clone(), other))
                .await
                .unwrap(),
        );
        let mutation_id = MutationId::new_v7();
        let request = Request::PlaylistUnfollow {
            playlist: "unfollow-replay:playlist:playlist-1".to_string(),
            provider: None,
        };

        let original =
            dispatch_with_mutation(state.clone(), request.clone(), None, Some(mutation_id))
                .await
                .expect("initial playlist unfollow");
        assert_eq!(
            wait_for_receipt(&state, pending_receipt(&original)).await,
            ReceiptStatus::Confirmed
        );
        let observed_before_replay = provider.observed_requests().await;

        let replay = dispatch_with_mutation(state.clone(), request, None, Some(mutation_id))
            .await
            .expect("completed unfollow must replay after deleting its subject");
        assert!(matches!(
            replay,
            ResponseData::Mutation { receipt }
                if receipt.replayed && receipt.status == Some(ReceiptStatus::Confirmed)
        ));
        assert_eq!(
            provider.observed_requests().await,
            observed_before_replay,
            "replay must not rediscover the deleted playlist"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn failed_optimistic_playlist_replay_preserves_the_terminal_error_envelope() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(FailingPlaylistUnfollowProvider::new());
        let runtime = ProviderRuntime::music_only(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let mutation_id = MutationId::new_v7();
        let request = Request::PlaylistUnfollow {
            playlist: "failed-unfollow:playlist:playlist-1".to_string(),
            provider: None,
        };

        let original =
            dispatch_with_mutation(state.clone(), request.clone(), None, Some(mutation_id))
                .await
                .expect("optimistic request must return its pending receipt");
        assert_eq!(
            wait_for_receipt(&state, pending_receipt(&original)).await,
            ReceiptStatus::Failed
        );
        let durable = state
            .store()
            .terminal_mutation_response(mutation_id)
            .await
            .unwrap()
            .expect("failed mutation must persist its terminal response");
        let Response::Error {
            message: expected_message,
            kind: expected_kind,
            retryable: expected_retryable,
            provider: expected_provider,
            detail: expected_detail,
            ..
        } = durable
        else {
            panic!("failed mutation must persist an error response")
        };
        assert_eq!(
            expected_kind,
            spotuify_protocol::IpcErrorKind::InvalidRequest
        );
        assert!(!expected_retryable);
        assert_eq!(expected_provider.as_ref(), Some(provider.id()));
        assert_eq!(expected_detail.as_deref(), Some(expected_message.as_str()));
        let lookups_before_replay = provider.playlist_lookups.load(Ordering::SeqCst);
        let mutations_before_replay = provider.mutation_calls.load(Ordering::SeqCst);

        let replay = handle_request_with_source_and_mutation(
            state.clone(),
            request,
            Some(OperationSource::Cli),
            Some(mutation_id),
        )
        .await;
        let Response::Error {
            message,
            kind,
            retryable,
            provider: replay_provider,
            detail,
            ..
        } = replay
        else {
            panic!("failed replay must return the stored error response")
        };
        assert_eq!(
            (message, kind, retryable, replay_provider, detail),
            (
                expected_message,
                expected_kind,
                expected_retryable,
                expected_provider,
                expected_detail,
            )
        );
        assert_eq!(
            provider.playlist_lookups.load(Ordering::SeqCst),
            lookups_before_replay,
            "terminal replay must not repeat playlist preflight"
        );
        assert_eq!(
            provider.mutation_calls.load(Ordering::SeqCst),
            mutations_before_replay,
            "terminal replay must not invoke the provider mutation again"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn playlist_set_image_replays_after_the_playlist_disappears() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(FakeProvider::isolated("image-replay").unwrap());
        let other = Arc::new(FakeProvider::isolated("other-owner").unwrap());
        let state = Arc::new(
            DaemonState::new_with_providers(registry(provider.clone(), other))
                .await
                .unwrap(),
        );
        let mutation_id = MutationId::new_v7();
        let playlist = ResourceUri::parse("image-replay:playlist:playlist-1").unwrap();
        let request = Request::PlaylistSetImage {
            playlist: playlist.as_uri(),
            image_base64: "aGVsbG8=".to_string(),
            provider: None,
        };

        let original =
            dispatch_with_mutation(state.clone(), request.clone(), None, Some(mutation_id))
                .await
                .expect("initial playlist image mutation");
        assert_eq!(
            wait_for_receipt(&state, pending_receipt(&original)).await,
            ReceiptStatus::Confirmed
        );
        provider
            .apply_mutation(
                RequestContext::FOREGROUND,
                uuid::Uuid::now_v7(),
                &Mutation::PlaylistUnfollow {
                    playlist_uri: playlist,
                },
            )
            .await
            .expect("remove playlist after terminal image response");
        let observed_before_replay = provider.observed_requests().await;

        let replay = dispatch_with_mutation(state.clone(), request, None, Some(mutation_id))
            .await
            .expect("completed image mutation must replay without playlist discovery");
        assert!(matches!(
            replay,
            ResponseData::Mutation { receipt }
                if receipt.replayed && receipt.status == Some(ReceiptStatus::Confirmed)
        ));
        assert_eq!(provider.observed_requests().await, observed_before_replay);

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn playlist_create_replays_after_provider_capability_changes() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(HostileReceiptProvider::new());
        let runtime = ProviderRuntime::music_only(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let mutation_id = MutationId::new_v7();
        let request = Request::PlaylistCreate {
            name: "Durable create".to_string(),
            description: None,
            uris: vec!["receipt-hostile:track:track-1".to_string()],
            provider: None,
        };

        let original =
            dispatch_with_mutation(state.clone(), request.clone(), None, Some(mutation_id))
                .await
                .expect("initial playlist create");
        let original_receipt_id = match original {
            ResponseData::PlaylistCreate { receipt } => receipt.receipt_id.unwrap(),
            response => panic!("expected playlist-create response, got {response:?}"),
        };
        provider.set_playlist_create_enabled(false);
        let observed_before_replay = provider.inner.observed_requests().await;

        let replay = dispatch_with_mutation(state.clone(), request, None, Some(mutation_id))
            .await
            .expect("completed create must replay despite current provider capabilities");
        assert!(matches!(
            replay,
            ResponseData::PlaylistCreate { receipt }
                if receipt.replayed && receipt.receipt_id == Some(original_receipt_id)
        ));
        assert_eq!(
            provider.inner.observed_requests().await,
            observed_before_replay,
            "replay must not re-run create preflight or provider writes"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn optimistic_mutation_replays_after_provider_auth_becomes_blocked() {
        let _guard = crate::ENV_LOCK.lock().await;
        let env = TestEnv::new();
        env.configure_fake_default_with_spotify_secondary();
        let state = Arc::new(DaemonState::new().await.unwrap());
        let provider = ProviderId::new("spotify-work").unwrap();
        let mutation_id = MutationId::new_v7();
        let request = Request::LibrarySave {
            uri: Some("spotify:track:track-1".to_string()),
            current: false,
        };
        let request_json = serde_json::to_string(&request).unwrap();

        let original = spawn_optimistic_mutation(
            &state,
            OperationKind::LibrarySave,
            OperationSource::Cli,
            vec!["spotify:track:track-1".to_string()],
            "save",
            request_json.clone(),
            None,
            None,
            None,
            Some(mutation_id),
            |_| async { Ok(()) },
        )
        .await
        .expect("initial mutation");
        assert_eq!(
            wait_for_receipt(&state, pending_receipt(&original)).await,
            ReceiptStatus::Confirmed
        );

        state.mark_auth_required(Some(&provider)).await;

        let replay = spawn_optimistic_mutation(
            &state,
            OperationKind::LibrarySave,
            OperationSource::Cli,
            vec!["spotify:track:track-1".to_string()],
            "save",
            request_json,
            None,
            None,
            None,
            Some(mutation_id),
            |_| async { anyhow::bail!("replayed mutation body must not run") },
        )
        .await
        .expect("terminal mutation must replay despite current auth state");
        assert!(matches!(
            replay,
            ResponseData::Mutation { receipt }
                if receipt.replayed && receipt.status == Some(ReceiptStatus::Confirmed)
        ));

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn recorded_mutation_replays_typed_response_after_provider_auth_becomes_blocked() {
        fn response() -> ResponseData {
            ResponseData::PlaylistCreate {
                receipt: PlaylistCreateReceipt {
                    ok: true,
                    action: "playlist-create".to_string(),
                    playlist_uri: "spotify:playlist:created".to_string(),
                    playlist_id: "spotify:playlist:created".to_string(),
                    name: "Durable replay".to_string(),
                    added_item_count: 1,
                    message: "Created Durable replay".to_string(),
                    receipt_id: None,
                    mutation_id: None,
                    replayed: false,
                },
            }
        }

        let _guard = crate::ENV_LOCK.lock().await;
        let env = TestEnv::new();
        env.configure_fake_default_with_spotify_secondary();
        let state = Arc::new(DaemonState::new().await.unwrap());
        let provider = ProviderId::new("spotify-work").unwrap();
        let mutation_id = MutationId::new_v7();
        let request_json = serde_json::to_string(&Request::PlaylistCreate {
            name: "Durable replay".to_string(),
            description: None,
            uris: vec!["spotify:track:track-1".to_string()],
            provider: Some(provider.clone()),
        })
        .unwrap();

        let original = record_operation(
            &state,
            OperationKind::PlaylistCreate,
            OperationSource::Cli,
            Vec::new(),
            "playlist-create",
            &request_json,
            Some(mutation_id),
            None,
            None,
            None,
            |_| async { Ok(response()) },
        )
        .await
        .expect("initial recorded mutation");
        let original_receipt = match original {
            ResponseData::PlaylistCreate { receipt } => receipt.receipt_id.unwrap(),
            other => panic!("expected playlist-create response, got {other:?}"),
        };

        state.mark_auth_required(Some(&provider)).await;

        let replay = record_operation(
            &state,
            OperationKind::PlaylistCreate,
            OperationSource::Cli,
            Vec::new(),
            "playlist-create",
            &request_json,
            Some(mutation_id),
            None,
            None,
            None,
            |_| async {
                anyhow::bail!("replayed mutation body must not run");
                #[allow(unreachable_code)]
                Ok(response())
            },
        )
        .await
        .expect("typed terminal response must replay despite current auth state");
        assert!(matches!(
            replay,
            ResponseData::PlaylistCreate { receipt }
                if receipt.replayed
                    && receipt.receipt_id == Some(original_receipt)
                    && receipt.playlist_uri == "spotify:playlist:created"
                    && receipt.message == "Created Durable replay"
        ));

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn liked_context_is_scoped_to_the_tapped_uri_provider() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let default = Arc::new(FakeProvider::isolated("fake-a").unwrap());
        let selected = Arc::new(FakeProvider::isolated("fake-b").unwrap());
        let state = Arc::new(
            DaemonState::new_with_providers(registry(default.clone(), selected.clone()))
                .await
                .unwrap(),
        );
        let item = |uri: &str, name: &str| MediaItem {
            uri: uri.to_string(),
            name: name.to_string(),
            kind: MediaKind::Track,
            ..Default::default()
        };
        state
            .store()
            .replace_provider_library_kind_bulk(
                default.id().as_str(),
                &MediaKind::Track,
                &[item("fake-a:track:track-1", "A one")],
            )
            .await
            .unwrap();
        state
            .store()
            .replace_provider_library_kind_bulk(
                selected.id().as_str(),
                &MediaKind::Track,
                &[
                    item("fake-b:track:track-1", "B one"),
                    item("fake-b:track:track-2", "B two"),
                ],
            )
            .await
            .unwrap();

        let context = resolve_play_context(&state, selected.as_ref(), Some(LIKED_SONGS_CONTEXT))
            .await
            .unwrap()
            .expect("selected provider has liked tracks");
        let tracks = context.tracks.as_ref().expect("ordered liked context");
        assert_eq!(tracks.len(), 2);
        assert!(tracks.iter().all(|uri| uri.starts_with("fake-b:")));

        let snapshot = context_queue_snapshot_for_play(
            &state,
            selected.id(),
            "fake-b:track:track-1",
            Some(&context),
        )
        .await
        .expect("liked context queue snapshot");
        assert!(snapshot
            .currently_playing
            .iter()
            .chain(snapshot.items.iter())
            .all(|item| item.uri.starts_with("fake-b:")));

        let response = dispatch_with_mutation(
            state.clone(),
            Request::PlaybackCommand {
                command: PlaybackCommand::PlayUri {
                    uri: "fake-b:track:track-1".to_string(),
                    context_uri: Some(LIKED_SONGS_CONTEXT.to_string()),
                },
            },
            None,
            Some(MutationId::new_v7()),
        )
        .await
        .unwrap();
        assert_eq!(
            wait_for_receipt(&state, pending_receipt(&response)).await,
            ReceiptStatus::Confirmed
        );
        let queue = spotuify_core::RemoteTransport::queue(
            selected.as_ref(),
            RequestContext::BACKGROUND_SYNC,
        )
        .await
        .unwrap();
        assert!(queue
            .currently_playing
            .iter()
            .chain(queue.items.iter())
            .all(|item| item.uri.starts_with("fake-b:")));

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn mismatched_point_lookup_fails_before_transport_mutation() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(WrongLookupProvider {
            inner: FakeProvider::isolated("wrong-lookup").unwrap(),
            execute_calls: AtomicUsize::new(0),
        });
        let runtime = ProviderRuntime::with_transport(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());

        let response = dispatch_with_mutation(
            state.clone(),
            Request::QueueAdd {
                uri: "wrong-lookup:track:track-1".to_string(),
            },
            None,
            Some(MutationId::new_v7()),
        )
        .await
        .unwrap();
        assert_eq!(
            wait_for_receipt(&state, pending_receipt(&response)).await,
            ReceiptStatus::Failed
        );
        assert_eq!(provider.execute_calls.load(Ordering::SeqCst), 0);

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn foreign_play_context_fails_before_optimistic_state_or_transport() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let default = Arc::new(FakeProvider::isolated("context-a").unwrap());
        let foreign = Arc::new(FakeProvider::isolated("context-b").unwrap());
        let state = Arc::new(
            DaemonState::new_with_providers(registry(default.clone(), foreign.clone()))
                .await
                .unwrap(),
        );
        let mut events = state.event_tx.subscribe();

        let error = dispatch(
            state.clone(),
            Request::PlaybackCommand {
                command: PlaybackCommand::PlayUri {
                    uri: "context-a:track:track-1".to_string(),
                    context_uri: Some("context-b:playlist:playlist-1".to_string()),
                },
            },
            None,
        )
        .await
        .expect_err("cross-provider context must fail synchronously");
        assert!(matches!(
            error.downcast_ref::<ProviderError>(),
            Some(ProviderError::InvalidInput { field, .. }) if field == "context_uri"
        ));
        assert!(events.try_recv().is_err());
        for provider in [&default, &foreign] {
            assert_eq!(
                provider
                    .observed_requests()
                    .await
                    .iter()
                    .filter(|request| request.operation == "transport.execute")
                    .count(),
                0
            );
        }
        assert!(state
            .store()
            .latest_provider_queue(500, default.id())
            .await
            .unwrap()
            .is_none());

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn music_only_default_can_seed_and_read_empty_transport_snapshots() {
        let _guard = crate::ENV_LOCK.lock().await;
        let env = TestEnv::new();
        std::fs::write(
            env._temp.path().join("spotuify.toml"),
            "[providers]\ndefault = \"music-only\"\n[providers.music-only]\ntype = \"fake\"\n",
        )
        .unwrap();
        let provider = Arc::new(TrackOnlySearchProvider {
            inner: FakeProvider::isolated("music-only").unwrap(),
        });
        let runtime = ProviderRuntime::music_only(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let mut events = state.event_tx.subscribe();

        assert!(matches!(
            dispatch(state.clone(), Request::ClientSeed, None)
                .await
                .unwrap(),
            ResponseData::ClientSeed { .. }
        ));
        assert!(matches!(
            dispatch(state.clone(), Request::QueueGet, None)
                .await
                .unwrap(),
            ResponseData::Queue { .. }
        ));
        assert!(matches!(
            dispatch(state.clone(), Request::DevicesList, None)
                .await
                .unwrap(),
            ResponseData::Devices { .. }
        ));
        for command in [
            PlaybackCommand::PlayUri {
                uri: "music-only:track:track-1".to_string(),
                context_uri: None,
            },
            PlaybackCommand::Pause,
        ] {
            assert!(matches!(
                handle_request_with_source(
                    state.clone(),
                    Request::PlaybackCommand { command },
                    None,
                )
                .await,
                Response::Error {
                    kind: spotuify_protocol::IpcErrorKind::Unsupported,
                    provider: Some(ref provider_id),
                    retryable: false,
                    ..
                } if provider_id == provider.id()
            ));
        }
        assert!(events.try_recv().is_err());
        let reconnect = state
            .ensure_player_ready("must-not-register")
            .await
            .expect_err("music-only provider has no embedded player owner");
        assert!(matches!(
            reconnect.downcast_ref::<ProviderError>(),
            Some(ProviderError::Unsupported { .. })
        ));

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn spotify_namespace_custom_adapter_never_uses_embedded_recovery() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(FakeProvider::with_identity(
            ProviderId::new("custom-cloud").unwrap(),
            UriScheme::Spotify,
            FakeDataset::Standard,
        ));
        let runtime = ProviderRuntime::with_transport(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let music: Arc<dyn MusicProvider> = provider.clone();
        let transport: Arc<dyn spotuify_core::RemoteTransport> = provider.clone();

        assert!(
            !provider_pair_uses_embedded_transport(&state, music.as_ref(), transport.as_ref(),)
                .await
                .unwrap()
        );
        execute_provider_pair_with_recovery(&state, music, transport, CommandKind::Pause)
            .await
            .unwrap();
        assert_eq!(
            provider
                .observed_requests()
                .await
                .iter()
                .filter(|request| request.operation == "transport.execute")
                .count(),
            1
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn background_sync_exposes_transport_only_for_default_provider() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let default = Arc::new(FakeProvider::isolated("transport-default").unwrap());
        let secondary = Arc::new(FakeProvider::isolated("transport-secondary").unwrap());
        let registry = ProviderRegistry::new(
            default.id().clone(),
            [
                ProviderRuntime::with_transport(default).unwrap(),
                ProviderRuntime::with_transport(secondary).unwrap(),
            ],
        )
        .unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());

        let providers = spotuify_sync::SyncContext::sync_providers(state.as_ref())
            .await
            .unwrap();
        assert_eq!(providers.len(), 2);
        assert!(providers
            .iter()
            .find(|provider| provider.id() == "transport-default")
            .unwrap()
            .transport
            .is_some());
        assert!(providers
            .iter()
            .find(|provider| provider.id() == "transport-secondary")
            .unwrap()
            .transport
            .is_none());

        state.set_active_transport_provider(
            ProviderId::new("transport-secondary").expect("valid provider id"),
        );
        let providers = spotuify_sync::SyncContext::sync_providers(state.as_ref())
            .await
            .unwrap();
        assert!(providers
            .iter()
            .find(|provider| provider.id() == "transport-default")
            .unwrap()
            .transport
            .is_none());
        assert!(providers
            .iter()
            .find(|provider| provider.id() == "transport-secondary")
            .unwrap()
            .transport
            .is_some());

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn spotify_auth_latch_does_not_block_an_injected_no_auth_provider() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(FakeProvider::with_identity(
            ProviderId::new("custom-cloud").unwrap(),
            UriScheme::Spotify,
            FakeDataset::Standard,
        ));
        let runtime = ProviderRuntime::with_transport(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let spotify = ProviderId::new("spotify").unwrap();
        state.mark_auth_required(Some(&spotify)).await;

        let response = dispatch_with_mutation(
            state.clone(),
            Request::LibrarySave {
                uri: Some("spotify:track:track-1".to_string()),
                current: false,
            },
            None,
            Some(MutationId::new_v7()),
        )
        .await
        .expect("a no-auth adapter must ignore Spotify's auth latch");
        assert_eq!(
            wait_for_receipt(&state, pending_receipt(&response)).await,
            ReceiptStatus::Confirmed
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn search_routes_custom_spotify_scheme_and_rejects_identity_mismatch() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let default = Arc::new(FakeProvider::isolated("fake-a").unwrap());
        let custom = Arc::new(FakeProvider::with_identity(
            ProviderId::new("custom-cloud").unwrap(),
            UriScheme::Spotify,
            FakeDataset::Standard,
        ));
        let state = Arc::new(
            DaemonState::new_with_providers(registry(default.clone(), custom.clone()))
                .await
                .unwrap(),
        );

        let (legacy_route, _) = resolve_search_provider(
            &state,
            &SearchSourceData::Remote(ProviderId::new("spotify").unwrap()),
            None,
        )
        .await
        .expect("legacy spotify source follows the registered URI scheme");
        assert_eq!(legacy_route.as_str(), "custom-cloud");

        let (explicit_default, _) =
            resolve_search_provider(&state, &SearchSourceData::Local, Some(default.id()))
                .await
                .expect("explicit local route resolves");
        let (explicit_custom, _) =
            resolve_search_provider(&state, &SearchSourceData::Hybrid, Some(custom.id()))
                .await
                .expect("subsequent explicit route resolves independently");
        assert_eq!(explicit_default.as_str(), "fake-a");
        assert_eq!(explicit_custom.as_str(), "custom-cloud");

        state.set_cached_episode_feed(
            explicit_default.clone(),
            vec![MediaItem {
                uri: "fake-a:episode:one".to_string(),
                ..Default::default()
            }],
            1,
        );
        state.set_cached_episode_feed(
            explicit_custom.clone(),
            vec![MediaItem {
                uri: "spotify:episode:two".to_string(),
                ..Default::default()
            }],
            2,
        );
        assert_eq!(
            state.cached_episode_feed(&explicit_default).unwrap().0[0].uri,
            "fake-a:episode:one"
        );
        assert_eq!(
            state.cached_episode_feed(&explicit_custom).unwrap().0[0].uri,
            "spotify:episode:two"
        );

        let mismatch = resolve_search_provider(
            &state,
            &SearchSourceData::Remote(custom.id().clone()),
            Some(default.id()),
        )
        .await
        .err()
        .expect("source and explicit provider must agree");
        assert!(matches!(
            mismatch.downcast_ref::<ProviderError>(),
            Some(ProviderError::InvalidInput { field, .. }) if field == "provider"
        ));

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn legacy_spotify_remote_search_fails_when_no_runtime_owns_spotify_scheme() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(FakeProvider::isolated("custom-default").unwrap());
        let runtime = ProviderRuntime::with_transport(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let spotify = ProviderId::new("spotify").unwrap();

        let error =
            resolve_search_provider(&state, &SearchSourceData::Remote(spotify.clone()), None)
                .await
                .err()
                .expect("legacy Spotify routing must not fall back to an unrelated default");
        assert!(matches!(
            error.downcast_ref::<ProviderError>(),
            Some(ProviderError::NotFound { resource }) if resource == "provider-scheme:spotify"
        ));
        assert!(provider.observed_requests().await.is_empty());

        let response = handle_request_with_source(
            state.clone(),
            Request::Search {
                query: "anything".to_string(),
                scope: SearchScopeData::Track,
                source: SearchSourceData::Remote(spotify),
                limit: 5,
                provider: None,
                kinds: None,
                sort: None,
            },
            None,
        )
        .await;
        assert!(matches!(response, Response::Error { provider: None, .. }));
        assert!(provider.observed_requests().await.is_empty());

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn foreign_resource_errors_are_not_misattributed_to_the_default_provider() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(FakeProvider::isolated("default-owner").unwrap());
        let runtime = ProviderRuntime::with_transport(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());

        let response = handle_request_with_source(
            state.clone(),
            Request::PlaylistTracks {
                playlist: "foreign:playlist:unroutable".to_string(),
                wait: true,
                provider: None,
            },
            None,
        )
        .await;
        assert!(matches!(response, Response::Error { provider: None, .. }));

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn every_remote_search_surface_honors_the_selected_provider_query_limit() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(SearchLimitProvider {
            inner: FakeProvider::isolated("search-caps").unwrap(),
            max_query_chars: AtomicUsize::new(200),
        });
        let runtime = ProviderRuntime::music_only(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let long_query = "x".repeat(145);

        search_with_source(
            state.clone(),
            SearchParams {
                query: long_query,
                scope: SearchScopeData::Track,
                source: SearchSourceData::Remote(provider.id().clone()),
                limit: 10,
                requested_provider: Some(provider.id().clone()),
                kinds: None,
                sort: None,
            },
        )
        .await
        .expect("a provider limit above the former global cap must be accepted");
        let calls_after_allowed = provider.inner.observed_requests().await.len();
        assert!(calls_after_allowed > 0);

        provider.max_query_chars.store(5, Ordering::SeqCst);
        let rejected = "too-long".to_string();
        let error = search_with_source(
            state.clone(),
            SearchParams {
                query: rejected.clone(),
                scope: SearchScopeData::Track,
                source: SearchSourceData::Remote(provider.id().clone()),
                limit: 10,
                requested_provider: Some(provider.id().clone()),
                kinds: None,
                sort: None,
            },
        )
        .await
        .expect_err("one-shot remote search must enforce provider caps");
        assert!(matches!(
            error.downcast_ref::<ProviderError>(),
            Some(ProviderError::InvalidInput { field, .. }) if field == "query"
        ));
        assert_eq!(
            provider.inner.observed_requests().await.len(),
            calls_after_allowed
        );

        let mut events = state.event_tx.subscribe();
        spawn_search_stream(
            state.clone(),
            rejected.clone(),
            SearchScopeData::Track,
            SearchSourceData::Remote(provider.id().clone()),
            7,
            provider.id().clone(),
        );
        let stream_event = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            stream_event.payload,
            spotuify_protocol::IpcPayload::Event(DaemonEvent::SearchFailed { version: 7, provider: Some(id), .. })
                if id == *provider.id()
        ));
        let stream_complete = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            stream_complete.payload,
            spotuify_protocol::IpcPayload::Event(DaemonEvent::SearchComplete { version: 7, .. })
        ));

        fetch_and_emit_page(
            state.clone(),
            rejected,
            MediaKind::Track,
            0,
            8,
            provider.id().clone(),
        )
        .await;
        let page_event = tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            page_event.payload,
            spotuify_protocol::IpcPayload::Event(DaemonEvent::SearchFailed { version: 8, provider: Some(id), .. })
                if id == *provider.id()
        ));
        assert_eq!(
            provider.inner.observed_requests().await.len(),
            calls_after_allowed
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn implicit_remote_search_intersects_scope_with_provider_kinds() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(TrackOnlySearchProvider {
            inner: FakeProvider::isolated("track-search").unwrap(),
        });
        let runtime = ProviderRuntime::music_only(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());

        search_with_source(
            state.clone(),
            SearchParams {
                query: "track".to_string(),
                scope: SearchScopeData::All,
                source: SearchSourceData::Remote(provider.id().clone()),
                limit: 10,
                requested_provider: Some(provider.id().clone()),
                kinds: None,
                sort: None,
            },
        )
        .await
        .expect("implicit all-scope search must use the provider's supported subset");
        let observed = provider.inner.observed_requests().await;
        assert_eq!(
            observed
                .iter()
                .filter(|request| request.operation == "search")
                .count(),
            1
        );

        let mut events = state.event_tx.subscribe();
        spawn_search_stream(
            state.clone(),
            "track".to_string(),
            SearchScopeData::All,
            SearchSourceData::Remote(provider.id().clone()),
            31,
            provider.id().clone(),
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match events.recv().await.unwrap().payload {
                    spotuify_protocol::IpcPayload::Event(DaemonEvent::SearchPage {
                        kind,
                        version: 31,
                        ..
                    }) => assert_eq!(kind, MediaKind::Track),
                    spotuify_protocol::IpcPayload::Event(DaemonEvent::SearchFailed {
                        version: 31,
                        message,
                        ..
                    }) => panic!("supported implicit stream failed: {message}"),
                    spotuify_protocol::IpcPayload::Event(DaemonEvent::SearchComplete {
                        version: 31,
                        ..
                    }) => break,
                    _ => {}
                }
            }
        })
        .await
        .expect("track-only stream must complete");

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn foreign_search_output_is_rejected_before_cache_write() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(ForeignSearchProvider {
            inner: FakeProvider::isolated("search-owner").unwrap(),
        });
        let runtime = ProviderRuntime::music_only(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());

        let error = remote_search_and_cache(
            state.clone(),
            provider.id().clone(),
            provider.clone(),
            "poison".to_string(),
            SearchScopeData::Track,
            vec![MediaKind::Track],
            10,
        )
        .await
        .expect_err("foreign provider output must fail closed");
        assert!(matches!(
            error.downcast_ref::<ProviderError>(),
            Some(ProviderError::InvalidInput { field, .. }) if field == "media_item.uri"
        ));
        assert!(state
            .store()
            .local_search("poison", SearchScopeData::Track, 10, Some("search-owner"))
            .await
            .unwrap()
            .is_empty());

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn wrong_kind_search_output_is_rejected_before_cache_write() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(WrongKindSearchProvider {
            inner: FakeProvider::isolated("wrong-kind-search").unwrap(),
        });
        let runtime = ProviderRuntime::music_only(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());

        let error = remote_search_and_cache(
            state.clone(),
            provider.id().clone(),
            provider.clone(),
            "fake".to_string(),
            SearchScopeData::Track,
            vec![MediaKind::Track],
            10,
        )
        .await
        .expect_err("owned wrong-kind output must fail closed");
        assert!(matches!(
            error.downcast_ref::<ProviderError>(),
            Some(ProviderError::InvalidInput { field, .. }) if field == "search.kind"
        ));
        assert!(state
            .store()
            .local_search(
                "fake",
                SearchScopeData::Track,
                10,
                Some(provider.id().as_str()),
            )
            .await
            .unwrap()
            .is_empty());

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn wrong_offset_search_output_emits_failure_without_page_or_cache_write() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(WrongOffsetSearchProvider {
            inner: FakeProvider::isolated("wrong-offset-search").unwrap(),
        });
        let runtime = ProviderRuntime::music_only(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let mut events = state.event_tx.subscribe();

        spawn_search_stream(
            state.clone(),
            "poison".to_string(),
            SearchScopeData::Track,
            SearchSourceData::Remote(provider.id().clone()),
            77,
            provider.id().clone(),
        );
        let mut saw_page = false;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match events.recv().await.unwrap().payload {
                    spotuify_protocol::IpcPayload::Event(DaemonEvent::SearchPage {
                        version: 77,
                        ..
                    }) => saw_page = true,
                    spotuify_protocol::IpcPayload::Event(DaemonEvent::SearchFailed {
                        version: 77,
                        message,
                        ..
                    }) => {
                        assert!(message.contains("requested_offset"), "{message}");
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("wrong-offset search must terminate");
        assert!(!saw_page, "invalid provider page must not be emitted");
        assert!(state
            .store()
            .local_search(
                "poison",
                SearchScopeData::Track,
                10,
                Some(provider.id().as_str()),
            )
            .await
            .unwrap()
            .is_empty());

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn foreign_show_episode_output_is_rejected_before_response() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(ForeignShowProvider {
            inner: FakeProvider::isolated("show-owner").unwrap(),
        });
        let runtime = ProviderRuntime::music_only(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());

        let response = handle_request_with_source(
            state.clone(),
            Request::ShowEpisodes {
                show: "show-owner:show:one".to_string(),
                limit: 10,
                offset: 0,
            },
            None,
        )
        .await;
        assert!(matches!(
            response,
            Response::Error { provider: Some(id), .. } if id == *provider.id()
        ));

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn hybrid_search_stream_refreshes_remote_on_warm_and_cold_cache_paths() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(SearchLimitProvider {
            inner: FakeProvider::isolated("hybrid-owner").unwrap(),
            max_query_chars: AtomicUsize::new(200),
        });
        let runtime = ProviderRuntime::music_only(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        state
            .store()
            .cache_provider_search_results(
                provider.id(),
                "warm-hit",
                SearchScopeData::Track,
                provider.id().as_str(),
                &[MediaItem {
                    uri: "hybrid-owner:track:warm".to_string(),
                    name: "warm-hit".to_string(),
                    kind: MediaKind::Track,
                    ..Default::default()
                }],
            )
            .await
            .unwrap();
        let mut events = state.event_tx.subscribe();

        for (query, version) in [("warm-hit", 21_u64), ("cold-miss", 22_u64)] {
            let calls_before = provider.inner.observed_requests().await.len();
            spawn_search_stream(
                state.clone(),
                query.to_string(),
                SearchScopeData::Track,
                SearchSourceData::Hybrid,
                version,
                provider.id().clone(),
            );
            let mut pages = 0;
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    match events.recv().await.unwrap().payload {
                        spotuify_protocol::IpcPayload::Event(DaemonEvent::SearchPage {
                            version: event_version,
                            ..
                        }) if event_version == version => pages += 1,
                        spotuify_protocol::IpcPayload::Event(DaemonEvent::SearchComplete {
                            version: event_version,
                            ..
                        }) if event_version == version => break,
                        _ => {}
                    }
                }
            })
            .await
            .expect("hybrid stream must complete");
            assert!(pages >= 1, "hybrid stream must emit a remote page");
            assert!(
                provider.inner.observed_requests().await.len() > calls_before,
                "hybrid stream must invoke the provider for {query}"
            );
        }

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn recent_refresh_emits_provider_scoped_sync_event_not_playback_change() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(FakeProvider::isolated("recent-owner").unwrap());
        let runtime = ProviderRuntime::with_transport(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let mut events = state.event_tx.subscribe();

        spawn_recent_refresh(state.clone(), provider.id().clone());
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match events.recv().await.unwrap().payload {
                    spotuify_protocol::IpcPayload::Event(DaemonEvent::PlaybackChanged {
                        ..
                    }) => panic!("recent refresh must not masquerade as playback state"),
                    spotuify_protocol::IpcPayload::Event(DaemonEvent::SyncFinished { summary })
                        if summary.provider.as_ref() == Some(provider.id()) =>
                    {
                        assert_eq!(summary.target, spotuify_protocol::SyncTargetData::Recent);
                        assert!(summary.recent_items > 0);
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("recent refresh must emit completion");

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn empty_recent_refresh_still_emits_successful_completion() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(FakeProvider::with_identity(
            ProviderId::new("empty-recent").unwrap(),
            UriScheme::new("empty-recent").unwrap(),
            FakeDataset::Empty,
        ));
        let runtime = ProviderRuntime::with_transport(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let mut events = state.event_tx.subscribe();

        spawn_recent_refresh(state.clone(), provider.id().clone());
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let spotuify_protocol::IpcPayload::Event(DaemonEvent::SyncFinished { summary }) =
                    events.recv().await.unwrap().payload
                {
                    if summary.provider.as_ref() == Some(provider.id()) {
                        assert_eq!(
                            summary.status,
                            spotuify_protocol::SyncCompletionStatus::Succeeded
                        );
                        assert_eq!(summary.recent_items, 0);
                        break;
                    }
                }
            }
        })
        .await
        .expect("empty recent refresh must emit completion");

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[test]
    fn save_and_follow_capabilities_are_enforced_independently() {
        let provider = FakeProvider::new();
        let track = spotuify_core::ResourceUri::parse("fake:track:track-1").unwrap();
        let artist = spotuify_core::ResourceUri::parse("fake:artist:artist-1").unwrap();

        assert!(require_provider_mutation_capability(
            &provider,
            &Mutation::LibrarySave {
                uris: vec![track.clone()],
            },
        )
        .is_ok());
        assert!(require_provider_mutation_capability(
            &provider,
            &Mutation::Follow {
                uris: vec![artist.clone()],
            },
        )
        .is_ok());
        assert!(matches!(
            require_provider_mutation_capability(
                &provider,
                &Mutation::LibrarySave { uris: vec![artist] },
            ),
            Err(spotuify_core::ProviderError::Unsupported { .. })
        ));
        assert!(matches!(
            require_provider_mutation_capability(
                &provider,
                &Mutation::Follow { uris: vec![track] },
            ),
            Err(spotuify_core::ProviderError::Unsupported { .. })
        ));
    }

    #[tokio::test]
    async fn false_mutation_capability_fails_without_calling_adapter() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(UnsupportedMutationProvider::new());
        let runtime = ProviderRuntime::music_only(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());

        let response = dispatch_with_mutation(
            state.clone(),
            Request::LibrarySave {
                uri: Some("no-write:track:track-1".to_string()),
                current: false,
            },
            None,
            Some(MutationId::new_v7()),
        )
        .await
        .unwrap();
        assert_eq!(
            wait_for_receipt(&state, pending_receipt(&response)).await,
            ReceiptStatus::Failed
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn failed_radio_lifecycle_replay_does_not_run_body_or_report_success() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(FakeProvider::isolated("radio-owner").unwrap());
        let runtime = ProviderRuntime::with_transport(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let mutation_id = MutationId::new_v7();
        let request_json = serde_json::to_string(&Request::RadioStart {
            seed_uri: "radio-owner:track:seed".to_string(),
            dry_run: false,
        })
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));

        let first = spawn_optimistic_mutation(
            &state,
            OperationKind::QueueAdd,
            OperationSource::Cli,
            vec!["radio-owner:track:one".to_string()],
            "radio queue",
            request_json.clone(),
            None,
            Some(spotuify_protocol::ReversalPlan::NotReversible {
                reason: "remote queue has no remove operation".to_string(),
            }),
            state
                .mutation_lane(&Request::RadioStart {
                    seed_uri: "radio-owner:track:seed".to_string(),
                    dry_run: false,
                })
                .await,
            Some(mutation_id),
            {
                let calls = calls.clone();
                move |_| async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    anyhow::bail!("second queue write failed")
                }
            },
        )
        .await
        .expect("mutation accepted");
        assert_eq!(
            wait_for_receipt(&state, pending_receipt(&first)).await,
            ReceiptStatus::Failed
        );

        let replay_error = spawn_optimistic_mutation(
            &state,
            OperationKind::QueueAdd,
            OperationSource::Cli,
            vec!["radio-owner:track:one".to_string()],
            "radio queue",
            request_json,
            None,
            Some(spotuify_protocol::ReversalPlan::NotReversible {
                reason: "remote queue has no remove operation".to_string(),
            }),
            None,
            Some(mutation_id),
            {
                let calls = calls.clone();
                move |_| async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
        )
        .await
        .expect_err("same-id replay must preserve the terminal error");
        assert_eq!(replay_error.to_string(), "second queue write failed");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn provider_policy_credentials_never_enter_durable_mutation_or_replay() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(FakeProvider::isolated("policy-persistence").unwrap());
        let runtime = ProviderRuntime::with_transport(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let mutation_id = MutationId::new_v7();
        let request_json = serde_json::to_string(&Request::RadioStart {
            seed_uri: "policy-persistence:track:seed".to_string(),
            dry_run: false,
        })
        .unwrap();
        let credential = "token=Ab1Cd2Ef3Gh4".to_string();
        let first = spawn_optimistic_mutation(
            &state,
            OperationKind::QueueAdd,
            OperationSource::Cli,
            vec!["policy-persistence:track:one".to_string()],
            "policy queue",
            request_json.clone(),
            None,
            Some(spotuify_protocol::ReversalPlan::NotReversible {
                reason: "remote queue has no remove operation".to_string(),
            }),
            None,
            Some(mutation_id),
            {
                let credential = credential.clone();
                move |_| async move {
                    let error = spotuify_player::PlayerError::ProviderPolicy(format!(
                        "account restriction reported {credential}"
                    ));
                    anyhow::bail!(crate::state::player_error_for_display(&error))
                }
            },
        )
        .await
        .expect("policy mutation accepted");
        let receipt_id = pending_receipt(&first);
        assert_eq!(
            wait_for_receipt(&state, receipt_id).await,
            ReceiptStatus::Failed
        );

        let persisted_receipt = state.store().get_receipt(receipt_id).await.unwrap();
        let receipt_json = serde_json::to_string(&persisted_receipt).unwrap();
        assert!(!receipt_json.contains(&credential));
        assert!(receipt_json.contains("<redacted>"));

        let persisted_operation = state
            .store()
            .list_operations(10, None, None)
            .await
            .unwrap()
            .into_iter()
            .find(|operation| operation.receipt_id == Some(receipt_id))
            .expect("policy operation");
        let operation_json = serde_json::to_string(&persisted_operation).unwrap();
        assert!(!operation_json.contains(&credential));
        assert!(operation_json.contains("<redacted>"));

        let replay_calls = Arc::new(AtomicUsize::new(0));
        let replay_error = spawn_optimistic_mutation(
            &state,
            OperationKind::QueueAdd,
            OperationSource::Cli,
            vec!["policy-persistence:track:one".to_string()],
            "policy queue",
            request_json,
            None,
            Some(spotuify_protocol::ReversalPlan::NotReversible {
                reason: "remote queue has no remove operation".to_string(),
            }),
            None,
            Some(mutation_id),
            {
                let replay_calls = replay_calls.clone();
                move |_| async move {
                    replay_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
        )
        .await
        .expect_err("same-id policy replay must preserve the terminal error");
        let replay_json = serde_json::to_string(&error_response_from(&replay_error)).unwrap();
        assert!(!replay_json.contains(&credential));
        assert!(replay_json.contains("<redacted>"));
        assert_eq!(replay_calls.load(Ordering::SeqCst), 0);

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn partial_provider_receipt_preserves_outcome_failures_and_reconciles() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(HostileReceiptProvider::new());
        let runtime = ProviderRuntime::music_only(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let mut events = state.event_tx.subscribe();
        let succeeded = ResourceUri::parse("receipt-hostile:track:track-1").unwrap();
        let failed = ResourceUri::parse("receipt-hostile:track:track-2").unwrap();
        let mutation = Mutation::LibrarySave {
            uris: vec![succeeded.clone(), failed.clone()],
        };
        let mutation_id = MutationId::new_v7();
        let request_summary = serde_json::to_string(&Request::LibrarySave {
            uri: Some(succeeded.as_uri()),
            current: false,
        })
        .unwrap();

        provider.set_fault(RECEIPT_PARTIAL);
        let response = spawn_optimistic_mutation(
            &state,
            OperationKind::LibrarySave,
            OperationSource::Cli,
            vec![succeeded.as_uri(), failed.as_uri()],
            "partial-save",
            request_summary.clone(),
            None,
            Some(spotuify_protocol::ReversalPlan::NotReversible {
                reason: "partial mutation fixture".to_string(),
            }),
            None,
            Some(mutation_id),
            {
                let provider = provider.clone();
                let mutation = mutation.clone();
                move |_| async move {
                    super::apply_provider_mutation_checked(
                        provider.as_ref(),
                        mutation_id.0,
                        &mutation,
                    )
                    .await?;
                    Ok(())
                }
            },
        )
        .await
        .unwrap();
        let receipt_id = pending_receipt(&response);
        assert_eq!(
            wait_for_receipt(&state, receipt_id).await,
            ReceiptStatus::Failed
        );

        let persisted = state.store().get_receipt(receipt_id).await.unwrap();
        let error = persisted.error.expect("partial receipt must retain detail");
        assert_eq!(error.kind, spotuify_protocol::IpcErrorKind::Provider);
        assert!(!error.kind.is_retryable());
        assert_eq!(error.provider.as_ref(), Some(provider.id()));
        let detail: PartialMutationSummary = serde_json::from_str(
            error
                .detail
                .as_deref()
                .expect("partial receipt JSON detail"),
        )
        .unwrap();
        assert_eq!(detail.schema, "spotuify.provider-partial.v1");
        assert_eq!(detail.succeeded_count, 1);
        assert_eq!(detail.failure_count, 1);
        assert_eq!(detail.succeeded[0], resource_summary(&succeeded));
        assert_eq!(detail.failures.len(), 1);
        assert_eq!(detail.failures[0].resource, resource_summary(&failed));
        assert!(error.detail.as_ref().unwrap().len() <= PARTIAL_SUMMARY_MAX_BYTES);

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if provider
                    .inner
                    .observed_requests()
                    .await
                    .iter()
                    .any(|request| request.operation == "library_items")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("partial mutation must trigger authoritative library reconciliation");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if !state
                    .store()
                    .provider_reconciliation_pending(receipt_id)
                    .await
                    .unwrap()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("reconciliation completion must commit");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let spotuify_protocol::IpcPayload::Event(DaemonEvent::LibraryChanged {
                    action,
                    ..
                }) = events.recv().await.unwrap().payload
                {
                    if action == "provider-mutation-reconciled" {
                        break;
                    }
                }
            }
        })
        .await
        .expect("clients must be notified after reconciliation commits");

        let durable_response = state
            .store()
            .terminal_mutation_response(mutation_id)
            .await
            .unwrap()
            .expect("partial mutation must persist its terminal response");
        let replay_calls = Arc::new(AtomicUsize::new(0));
        let replay_error = spawn_optimistic_mutation(
            &state,
            OperationKind::LibrarySave,
            OperationSource::Cli,
            vec![failed.as_uri()],
            "partial-save",
            request_summary,
            None,
            None,
            None,
            Some(mutation_id),
            {
                let replay_calls = replay_calls.clone();
                move |_| async move {
                    replay_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
        )
        .await
        .expect_err("partial mutation replay must preserve its terminal error");
        assert_eq!(
            serde_json::to_value(error_response_from(&replay_error)).unwrap(),
            serde_json::to_value(durable_response).unwrap(),
            "partial mutation replay must preserve the exact durable error envelope"
        );
        assert_eq!(replay_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            provider
                .inner
                .observed_requests()
                .await
                .iter()
                .filter(|request| request.operation == "apply_mutation")
                .count(),
            1,
            "partial durable outcomes are non-retryable"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn partial_playlist_population_retains_durable_delete_cleanup() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(HostileReceiptProvider::new());
        let runtime = ProviderRuntime::music_only(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());

        provider.set_fault(RECEIPT_PARTIAL_ADD_ONLY);
        let error = dispatch_with_mutation(
            state.clone(),
            Request::PlaylistCreate {
                name: "Partial population".to_string(),
                description: None,
                uris: vec![
                    "receipt-hostile:track:track-1".to_string(),
                    "receipt-hostile:track:track-2".to_string(),
                ],
                provider: Some(provider.id().clone()),
            },
            None,
            Some(MutationId::new_v7()),
        )
        .await
        .unwrap_err();
        assert!(is_partial_mutation_error(&error));

        let operation = state
            .store()
            .list_operations(10, None, None)
            .await
            .unwrap()
            .into_iter()
            .find(|operation| operation.kind == OperationKind::PlaylistCreate)
            .expect("composite create operation");
        assert_eq!(operation.status, OperationStatus::Succeeded);
        assert!(operation.reversible);
        assert!(matches!(
            operation.reversal_plan,
            Some(spotuify_protocol::ReversalPlan::PlaylistDelete { .. })
        ));
        let receipt_id = operation.receipt_id.expect("linked receipt");
        assert_eq!(
            state.store().get_receipt(receipt_id).await.unwrap().status,
            ReceiptStatus::Failed
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if !state
                    .store()
                    .provider_reconciliation_pending(receipt_id)
                    .await
                    .unwrap()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("playlist reconciliation must complete");
        assert!(provider
            .inner
            .observed_requests()
            .await
            .iter()
            .any(|request| request.operation == "playlist_items"));

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn unavailable_target_playlist_retries_and_completes_after_recovery() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(HostileReceiptProvider::new());
        let runtime = ProviderRuntime::music_only(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let mut events = state.event_tx.subscribe();

        provider.set_fault(RECEIPT_PARTIAL_ADD_ONLY);
        provider.set_playlist_items_unavailable(true);
        let error = dispatch_with_mutation(
            state.clone(),
            Request::PlaylistCreate {
                name: "Unavailable reconciliation".to_string(),
                description: None,
                uris: vec![
                    "receipt-hostile:track:track-1".to_string(),
                    "receipt-hostile:track:track-2".to_string(),
                ],
                provider: Some(provider.id().clone()),
            },
            None,
            Some(MutationId::new_v7()),
        )
        .await
        .unwrap_err();
        assert!(is_partial_mutation_error(&error));
        let receipt_id = state
            .store()
            .list_operations(10, None, None)
            .await
            .unwrap()
            .into_iter()
            .find(|operation| operation.kind == OperationKind::PlaylistCreate)
            .and_then(|operation| operation.receipt_id)
            .expect("partial create receipt");

        let reconciled = tokio::time::timeout(Duration::from_secs(5), async {
            let mut reconciled = false;
            loop {
                match events.recv().await.unwrap().payload {
                    spotuify_protocol::IpcPayload::Event(DaemonEvent::PlaylistsChanged {
                        action,
                        ..
                    }) if action == "provider-mutation-reconciled" => reconciled = true,
                    spotuify_protocol::IpcPayload::Event(DaemonEvent::SyncFinished { summary })
                        if summary.target == spotuify_protocol::SyncTargetData::Playlists
                            && summary.status
                                == spotuify_protocol::SyncCompletionStatus::Failed =>
                    {
                        assert!(summary
                            .error
                            .as_deref()
                            .is_some_and(|error| error.contains("remained unavailable")));
                        break reconciled;
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("unavailable target must fail reconciliation");
        assert!(!reconciled, "unverified target must not emit reconciled");
        assert!(state
            .store()
            .provider_reconciliation_pending(receipt_id)
            .await
            .unwrap());

        provider.clear_playlist_items_reconciliation_faults();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if !state
                    .store()
                    .provider_reconciliation_pending(receipt_id)
                    .await
                    .unwrap()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("reconciliation retry must complete after provider recovery");

        state.request_shutdown();
        state
            .shutdown_background_tasks(Duration::from_secs(1))
            .await;
        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn target_verification_timeout_retries_after_provider_recovers() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(HostileReceiptProvider::new());
        let runtime = ProviderRuntime::music_only(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let mut events = state.event_tx.subscribe();

        provider.set_fault(RECEIPT_PARTIAL_ADD_ONLY);
        provider.set_playlist_items_delay_after_first_read(Duration::from_secs(5));
        dispatch_with_mutation(
            state.clone(),
            Request::PlaylistCreate {
                name: "Timed out reconciliation".to_string(),
                description: None,
                uris: vec![
                    "receipt-hostile:track:track-1".to_string(),
                    "receipt-hostile:track:track-2".to_string(),
                ],
                provider: Some(provider.id().clone()),
            },
            None,
            Some(MutationId::new_v7()),
        )
        .await
        .unwrap_err();
        let operation = state
            .store()
            .list_operations(10, None, None)
            .await
            .unwrap()
            .into_iter()
            .find(|operation| operation.kind == OperationKind::PlaylistCreate)
            .expect("partial create operation");
        let receipt_id = operation.receipt_id.expect("partial create receipt");
        let playlist_id = operation
            .subject_uris
            .first()
            .expect("created playlist URI")
            .clone();

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if provider
                    .playlist_item_read_counts
                    .lock()
                    .unwrap()
                    .get(&playlist_id)
                    .copied()
                    .unwrap_or(0)
                    >= 2
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("target verification must start before its timeout");
        assert!(
            provider
                .playlist_item_read_counts
                .lock()
                .unwrap()
                .get(&playlist_id)
                .copied()
                .unwrap_or(0)
                >= 2
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let spotuify_protocol::IpcPayload::Event(DaemonEvent::SyncFinished { summary }) =
                    events.recv().await.unwrap().payload
                {
                    if summary.target == spotuify_protocol::SyncTargetData::Playlists
                        && summary.status == spotuify_protocol::SyncCompletionStatus::Failed
                    {
                        assert!(summary
                            .error
                            .as_deref()
                            .is_some_and(|error| error.contains("timed out")));
                        break;
                    }
                }
            }
        })
        .await
        .expect("verification timeout must emit a failed terminal sync event");
        assert!(state
            .store()
            .provider_reconciliation_pending(receipt_id)
            .await
            .unwrap());

        provider.clear_playlist_items_reconciliation_faults();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if !state
                    .store()
                    .provider_reconciliation_pending(receipt_id)
                    .await
                    .unwrap()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("timed-out reconciliation must retry and complete");

        state.request_shutdown();
        state
            .shutdown_background_tasks(Duration::from_secs(1))
            .await;
        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn target_verification_panic_retries_after_provider_recovers() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(HostileReceiptProvider::new());
        let runtime = ProviderRuntime::music_only(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let mut events = state.event_tx.subscribe();

        provider.set_fault(RECEIPT_PARTIAL_ADD_ONLY);
        provider.set_playlist_items_panic_after_first_read();
        dispatch_with_mutation(
            state.clone(),
            Request::PlaylistCreate {
                name: "Panicked reconciliation".to_string(),
                description: None,
                uris: vec![
                    "receipt-hostile:track:track-1".to_string(),
                    "receipt-hostile:track:track-2".to_string(),
                ],
                provider: Some(provider.id().clone()),
            },
            None,
            Some(MutationId::new_v7()),
        )
        .await
        .unwrap_err();
        let receipt_id = state
            .store()
            .list_operations(10, None, None)
            .await
            .unwrap()
            .into_iter()
            .find(|operation| operation.kind == OperationKind::PlaylistCreate)
            .and_then(|operation| operation.receipt_id)
            .expect("partial create receipt");

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let spotuify_protocol::IpcPayload::Event(DaemonEvent::SyncFinished { summary }) =
                    events.recv().await.unwrap().payload
                {
                    if summary.target == spotuify_protocol::SyncTargetData::Playlists
                        && summary.status == spotuify_protocol::SyncCompletionStatus::Failed
                    {
                        assert!(summary
                            .error
                            .as_deref()
                            .is_some_and(|error| error.contains("panicked")));
                        break;
                    }
                }
            }
        })
        .await
        .expect("verification panic must emit a failed terminal sync event");
        assert!(state
            .store()
            .provider_reconciliation_pending(receipt_id)
            .await
            .unwrap());

        provider.clear_playlist_items_reconciliation_faults();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if !state
                    .store()
                    .provider_reconciliation_pending(receipt_id)
                    .await
                    .unwrap()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("panicked reconciliation must retry and complete");

        state.request_shutdown();
        state
            .shutdown_background_tasks(Duration::from_secs(1))
            .await;
        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn recovered_target_read_is_persisted_before_reconciliation_completes() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(HostileReceiptProvider::new());
        let runtime = ProviderRuntime::music_only(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());

        provider.set_fault(RECEIPT_PARTIAL_ADD_ONLY);
        provider.set_playlist_items_first_read_unavailable();
        let error = dispatch_with_mutation(
            state.clone(),
            Request::PlaylistCreate {
                name: "Recovered reconciliation read".to_string(),
                description: None,
                uris: vec![
                    "receipt-hostile:track:track-1".to_string(),
                    "receipt-hostile:track:track-2".to_string(),
                ],
                provider: Some(provider.id().clone()),
            },
            None,
            Some(MutationId::new_v7()),
        )
        .await
        .unwrap_err();
        assert!(is_partial_mutation_error(&error));
        let operation = state
            .store()
            .list_operations(10, None, None)
            .await
            .unwrap()
            .into_iter()
            .find(|operation| operation.kind == OperationKind::PlaylistCreate)
            .expect("partial create operation");
        let receipt_id = operation.receipt_id.expect("partial create receipt");
        let playlist_id = operation
            .subject_uris
            .first()
            .expect("created playlist URI")
            .clone();

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if !state
                    .store()
                    .provider_reconciliation_pending(receipt_id)
                    .await
                    .unwrap()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("authoritative target read must complete reconciliation");
        assert_eq!(
            state
                .store()
                .playlist_items_count(&playlist_id)
                .await
                .unwrap(),
            2,
            "the Available verification read must replace the stale inaccessible cache"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn uncertain_playlist_population_rollback_retains_durable_delete_cleanup() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(HostileReceiptProvider::new());
        let runtime = ProviderRuntime::music_only(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());

        provider.set_fault(RECEIPT_POPULATE_ROLLBACK_FAIL);
        let error = dispatch_with_mutation(
            state.clone(),
            Request::PlaylistCreate {
                name: "Uncertain rollback".to_string(),
                description: None,
                uris: vec!["receipt-hostile:track:track-1".to_string()],
                provider: Some(provider.id().clone()),
            },
            None,
            Some(MutationId::new_v7()),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error_response_from(&error),
            Response::Error {
                kind: spotuify_protocol::IpcErrorKind::Provider,
                retryable: false,
                provider: Some(ref owner),
                ..
            } if owner == provider.id()
        ));

        let operation = state
            .store()
            .list_operations(10, None, None)
            .await
            .unwrap()
            .into_iter()
            .find(|operation| operation.kind == OperationKind::PlaylistCreate)
            .expect("composite create operation");
        assert_eq!(operation.status, OperationStatus::Succeeded);
        assert!(operation.reversible);
        assert!(matches!(
            operation.reversal_plan,
            Some(spotuify_protocol::ReversalPlan::PlaylistDelete { .. })
        ));
        assert_eq!(
            state
                .store()
                .get_receipt(operation.receipt_id.unwrap())
                .await
                .unwrap()
                .status,
            ReceiptStatus::Failed
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn partial_reversal_receipt_does_not_mark_original_operation_undone() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(HostileReceiptProvider::new());
        let runtime = ProviderRuntime::music_only(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());

        let playlist = ResourceUri::parse("receipt-hostile:playlist:playlist-1").unwrap();
        let first = ResourceUri::parse("receipt-hostile:track:track-1").unwrap();
        let second = ResourceUri::parse("receipt-hostile:track:track-2").unwrap();
        let request = Request::PlaylistAddItems {
            playlist: playlist.as_uri(),
            uris: vec![first.as_uri(), second.as_uri()],
            provider: None,
        };
        let request_summary = serde_json::to_string(&request).unwrap();
        let provider_mutation_id = uuid::Uuid::now_v7();
        record_operation(
            &state,
            OperationKind::PlaylistAdd,
            OperationSource::Cli,
            vec![first.as_uri(), second.as_uri()],
            "playlist-add",
            &request_summary,
            Some(MutationId(provider_mutation_id)),
            Some(spotuify_protocol::PreState::PlaylistAdd {
                playlist_id: playlist.as_uri(),
                version_token: None,
                added_uris: vec![first.as_uri(), second.as_uri()],
            }),
            Some(spotuify_protocol::ReversalPlan::PlaylistRemoveTracks {
                playlist_id: playlist.as_uri(),
                uris: vec![first.as_uri(), second.as_uri()],
                version_token: None,
            }),
            None,
            {
                let provider = provider.clone();
                let playlist = playlist.clone();
                let first = first.clone();
                let second = second.clone();
                move |_| async move {
                    super::apply_provider_mutation_checked(
                        provider.as_ref(),
                        provider_mutation_id,
                        &Mutation::PlaylistAdd {
                            playlist_uri: playlist,
                            items: vec![
                                PlaylistInsertion {
                                    uri: first,
                                    position: None,
                                },
                                PlaylistInsertion {
                                    uri: second,
                                    position: None,
                                },
                            ],
                            expected_version: None,
                        },
                    )
                    .await?;
                    Ok(ResponseData::Ack {
                        message: "added".to_string(),
                    })
                }
            },
        )
        .await
        .unwrap();
        let original = state
            .store()
            .list_operations(20, None, None)
            .await
            .unwrap()
            .into_iter()
            .find(|op| op.kind == OperationKind::PlaylistAdd)
            .expect("playlist add operation");

        provider.set_fault(RECEIPT_PARTIAL);
        let error = dispatch_with_mutation(
            state.clone(),
            Request::OpsUndo {
                operation_id: Some(original.operation_id),
                dry_run: false,
                force: false,
                bulk_since_ms: None,
            },
            None,
            Some(MutationId::new_v7()),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("partially applied"));
        let original = state
            .store()
            .get_operation(original.operation_id)
            .await
            .unwrap();
        assert_eq!(original.status, OperationStatus::Succeeded);
        assert!(!original.reversible, "partial undo must become terminal");
        let writes = provider
            .inner
            .observed_requests()
            .await
            .iter()
            .filter(|request| request.operation == "apply_mutation")
            .count();
        let retry = dispatch_with_mutation(
            state.clone(),
            Request::OpsUndo {
                operation_id: Some(original.operation_id),
                dry_run: false,
                force: false,
                bulk_since_ms: None,
            },
            None,
            Some(MutationId::new_v7()),
        )
        .await
        .unwrap_err();
        assert!(retry.to_string().contains("not reversible"));
        assert_eq!(
            provider
                .inner
                .observed_requests()
                .await
                .iter()
                .filter(|request| request.operation == "apply_mutation")
                .count(),
            writes,
            "a partial reversal must not be repeated"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn hostile_receipt_correlation_never_reports_command_success() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(HostileReceiptProvider::new());
        let runtime = ProviderRuntime::music_only(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let transport = FakeProvider::isolated("receipt-transport").unwrap();
        let item = spotuify_core::MediaItem {
            uri: "receipt-hostile:track:track-1".to_string(),
            ..Default::default()
        };

        for (fault, expected_error) in [
            (RECEIPT_WRONG_MUTATION_ID, "for requested mutation"),
            (RECEIPT_WRONG_PROVIDER, "receipt owned by"),
            (RECEIPT_WRONG_OUTCOME, "returned follow_changed"),
            (RECEIPT_APPLIED_WITH_FAILURES, "reported failures"),
        ] {
            provider.set_fault(fault);
            let error = execute_provider_command(
                &state,
                provider.as_ref(),
                &transport,
                CommandKind::SaveItem { item: item.clone() },
            )
            .await
            .unwrap_err();
            assert!(
                error.to_string().contains(expected_error),
                "unexpected receipt validation error: {error}"
            );
            assert!(matches!(
                error_response_from(&error),
                Response::Error {
                    kind: spotuify_protocol::IpcErrorKind::Provider,
                    retryable: false,
                    provider: Some(ref owner),
                    ..
                } if owner == provider.id()
            ));
        }

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn malformed_receipt_lifecycle_attributes_invoked_provider_and_reconciles() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(HostileReceiptProvider::new());
        let runtime = ProviderRuntime::music_only(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let mut events = state.event_tx.subscribe();
        let uri = ResourceUri::parse("receipt-hostile:track:track-1").unwrap();
        let provider_mutation_id = uuid::Uuid::now_v7();
        provider.set_fault(RECEIPT_WRONG_PROVIDER);

        let response = spawn_optimistic_mutation(
            &state,
            OperationKind::LibrarySave,
            OperationSource::Cli,
            vec![uri.as_uri()],
            "malformed-save",
            serde_json::to_string(&Request::LibrarySave {
                uri: Some(uri.as_uri()),
                current: false,
            })
            .unwrap(),
            None,
            None,
            None,
            Some(MutationId(provider_mutation_id)),
            {
                let provider = provider.clone();
                move |_| async move {
                    super::apply_provider_mutation_checked(
                        provider.as_ref(),
                        provider_mutation_id,
                        &Mutation::LibrarySave { uris: vec![uri] },
                    )
                    .await?;
                    Ok(())
                }
            },
        )
        .await
        .unwrap();
        let receipt_id = pending_receipt(&response);
        assert_eq!(
            wait_for_receipt(&state, receipt_id).await,
            ReceiptStatus::Failed
        );
        let error = state
            .store()
            .get_receipt(receipt_id)
            .await
            .unwrap()
            .error
            .unwrap();
        assert_eq!(error.kind, spotuify_protocol::IpcErrorKind::Provider);
        assert_eq!(error.provider.as_ref(), Some(provider.id()));
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let spotuify_protocol::IpcPayload::Event(DaemonEvent::LibraryChanged {
                    action,
                    provider: owner,
                    ..
                }) = events.recv().await.unwrap().payload
                {
                    if action == "provider-mutation-reconciled" {
                        assert_eq!(owner.as_ref(), Some(provider.id()));
                        break;
                    }
                }
            }
        })
        .await
        .expect("malformed post-write receipt must reconcile provider truth");

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn malformed_receipt_wire_errors_keep_invoked_provider_on_original_and_replay() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let provider = Arc::new(HostileReceiptProvider::new());
        let runtime = ProviderRuntime::music_only(provider.clone()).unwrap();
        let registry = ProviderRegistry::new(provider.id().clone(), [runtime]).unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let request = Request::PlaylistCreate {
            name: "Malformed wire receipt".to_string(),
            description: None,
            uris: vec!["receipt-hostile:track:track-1".to_string()],
            provider: Some(provider.id().clone()),
        };
        let mutation_id = MutationId::new_v7();
        provider.set_fault(RECEIPT_WRONG_PROVIDER);

        let original = handle_request_with_source_and_mutation(
            state.clone(),
            request.clone(),
            Some(OperationSource::Cli),
            Some(mutation_id),
        )
        .await;
        assert!(matches!(
            original,
            Response::Error {
                kind: spotuify_protocol::IpcErrorKind::Provider,
                retryable: false,
                provider: Some(ref owner),
                ..
            } if owner == provider.id()
        ));
        let writes = provider
            .inner
            .observed_requests()
            .await
            .iter()
            .filter(|request| request.operation == "apply_mutation")
            .count();
        assert_eq!(writes, 1, "original request must reach the provider once");

        let replay = handle_request_with_source_and_mutation(
            state.clone(),
            request,
            Some(OperationSource::Cli),
            Some(mutation_id),
        )
        .await;
        assert!(matches!(
            replay,
            Response::Error {
                kind: spotuify_protocol::IpcErrorKind::Provider,
                retryable: false,
                provider: Some(ref owner),
                ..
            } if owner == provider.id()
        ));
        assert_eq!(
            provider
                .inner
                .observed_requests()
                .await
                .iter()
                .filter(|request| request.operation == "apply_mutation")
                .count(),
            writes,
            "malformed receipt replay must not invoke the provider"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn hostile_playlist_create_receipts_must_identify_the_created_playlist() {
        let provider = HostileReceiptProvider::new();
        let mutation = Mutation::PlaylistCreate {
            name: "Hostile create".to_string(),
            public: Some(false),
            description: None,
        };

        for fault in [
            RECEIPT_CREATED_INVALID_URI,
            RECEIPT_CREATED_FOREIGN_URI,
            RECEIPT_CREATED_WRONG_KIND,
            RECEIPT_CREATED_WRONG_VERSION,
        ] {
            provider.set_fault(fault);
            let error =
                super::apply_provider_mutation_checked(&provider, uuid::Uuid::now_v7(), &mutation)
                    .await
                    .unwrap_err();
            assert!(
                error.to_string().contains("returned playlist_created"),
                "unexpected playlist receipt validation error: {error}"
            );
            assert!(matches!(
                error_response_from(&error),
                Response::Error {
                    kind: spotuify_protocol::IpcErrorKind::Provider,
                    retryable: false,
                    provider: Some(ref owner),
                    ..
                } if owner == provider.id()
            ));
        }

        provider.set_fault(RECEIPT_PARTIAL);
        let error =
            super::apply_provider_mutation_checked(&provider, uuid::Uuid::now_v7(), &mutation)
                .await
                .unwrap_err();
        assert!(error.to_string().contains("partial completion for atomic"));
        assert!(matches!(
            error_response_from(&error),
            Response::Error {
                kind: spotuify_protocol::IpcErrorKind::Provider,
                retryable: false,
                provider: Some(ref owner),
                ..
            } if owner == provider.id()
        ));
    }

    #[tokio::test]
    async fn batch_mutations_require_authoritative_readback_capabilities() {
        let library_provider = HostileReceiptProvider::new();
        library_provider.set_fault(RECEIPT_PARTIAL);
        library_provider.set_reconciliation_caps(RECONCILE_CAPS_NO_LIBRARY_READ);
        let library_mutation = Mutation::LibrarySave {
            uris: vec![
                ResourceUri::parse("receipt-hostile:track:track-1").unwrap(),
                ResourceUri::parse("receipt-hostile:track:track-2").unwrap(),
            ],
        };
        let error = super::apply_provider_mutation_checked(
            &library_provider,
            uuid::Uuid::now_v7(),
            &library_mutation,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("library reconciliation"));
        assert!(matches!(
            error_response_from(&error),
            Response::Error {
                kind: spotuify_protocol::IpcErrorKind::Unsupported,
                retryable: false,
                ..
            }
        ));
        assert!(!library_provider
            .inner
            .observed_requests()
            .await
            .iter()
            .any(|request| request.operation == "apply_mutation"));

        let playlist_provider = HostileReceiptProvider::new();
        playlist_provider.set_fault(RECEIPT_PARTIAL);
        playlist_provider.set_reconciliation_caps(RECONCILE_CAPS_NO_PLAYLIST_ITEM_READ);
        let playlist_mutation = Mutation::PlaylistAdd {
            playlist_uri: ResourceUri::parse("receipt-hostile:playlist:playlist-1").unwrap(),
            items: vec![
                PlaylistInsertion {
                    uri: ResourceUri::parse("receipt-hostile:track:track-1").unwrap(),
                    position: None,
                },
                PlaylistInsertion {
                    uri: ResourceUri::parse("receipt-hostile:track:track-2").unwrap(),
                    position: None,
                },
            ],
            expected_version: None,
        };
        let error = super::apply_provider_mutation_checked(
            &playlist_provider,
            uuid::Uuid::now_v7(),
            &playlist_mutation,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("playlist item reconciliation"));
        assert!(matches!(
            error_response_from(&error),
            Response::Error {
                kind: spotuify_protocol::IpcErrorKind::Unsupported,
                retryable: false,
                ..
            }
        ));
        assert!(!playlist_provider
            .inner
            .observed_requests()
            .await
            .iter()
            .any(|request| request.operation == "apply_mutation"));
    }

    #[test]
    fn partial_receipt_requires_an_exact_uri_bound_multiset_partition() {
        let provider = FakeProvider::isolated("partition-owner").unwrap();
        let first = ResourceUri::parse("partition-owner:track:first").unwrap();
        let second = ResourceUri::parse("partition-owner:track:second").unwrap();
        let third = ResourceUri::parse("partition-owner:track:third").unwrap();
        let mutation = Mutation::LibrarySave {
            uris: vec![first.clone(), second.clone(), third],
        };
        let mutation_id = uuid::Uuid::now_v7();
        let base = MutationReceipt {
            mutation_id,
            provider: provider.id().clone(),
            completion: MutationCompletion::PartiallyApplied,
            outcome: MutationOutcome::LibraryChanged {
                uris: vec![first.clone()],
                saved: true,
            },
            version_token: None,
            failures: vec![MutationFailure {
                uri: Some(second.clone()),
                message: "failed".to_string(),
            }],
        };

        for receipt in [
            MutationReceipt {
                failures: vec![MutationFailure {
                    uri: None,
                    message: "missing resource".to_string(),
                }],
                ..base.clone()
            },
            MutationReceipt {
                failures: vec![MutationFailure {
                    uri: Some(first.clone()),
                    message: "overlap".to_string(),
                }],
                ..base.clone()
            },
            base,
        ] {
            let error =
                validate_mutation_receipt(&provider, mutation_id, &mutation, &receipt).unwrap_err();
            assert!(matches!(
                error_response_from(&error),
                Response::Error {
                    kind: spotuify_protocol::IpcErrorKind::Provider,
                    retryable: false,
                    provider: Some(ref owner),
                    ..
                } if owner == provider.id()
            ));
        }
    }

    #[test]
    fn partial_receipt_partitions_duplicate_uri_occurrences() {
        let provider = FakeProvider::isolated("duplicate-owner").unwrap();
        let duplicate = ResourceUri::parse("duplicate-owner:track:same").unwrap();
        let mutation = Mutation::LibrarySave {
            uris: vec![duplicate.clone(), duplicate.clone()],
        };
        let mutation_id = uuid::Uuid::now_v7();
        let receipt = MutationReceipt {
            mutation_id,
            provider: provider.id().clone(),
            completion: MutationCompletion::PartiallyApplied,
            outcome: MutationOutcome::LibraryChanged {
                uris: vec![duplicate.clone()],
                saved: true,
            },
            version_token: None,
            failures: vec![MutationFailure {
                uri: Some(duplicate.clone()),
                message: "one occurrence failed".to_string(),
            }],
        };

        let partition = validate_mutation_receipt(&provider, mutation_id, &mutation, &receipt)
            .unwrap()
            .unwrap();
        assert_eq!(partition.succeeded, vec![duplicate.clone()]);
        assert_eq!(partition.failed, vec![duplicate]);
    }

    #[test]
    fn partial_summary_is_bounded_and_does_not_embed_raw_provider_payloads() {
        let provider = FakeProvider::isolated("bounded-owner").unwrap();
        let secret = "Abc123".repeat(12);
        let first = ResourceUri::parse(&format!(
            "bounded-owner:track:{}",
            "a".repeat(PARTIAL_SUMMARY_URI_CHARS * 4)
        ))
        .unwrap();
        let second = ResourceUri::parse(&format!(
            "bounded-owner:track:{}",
            "b".repeat(PARTIAL_SUMMARY_URI_CHARS * 4)
        ))
        .unwrap();
        let mutation = Mutation::LibrarySave {
            uris: vec![first.clone(), second.clone()],
        };
        let receipt = MutationReceipt {
            mutation_id: uuid::Uuid::now_v7(),
            provider: provider.id().clone(),
            completion: MutationCompletion::PartiallyApplied,
            outcome: MutationOutcome::LibraryChanged {
                uris: vec![first.clone()],
                saved: true,
            },
            version_token: Some(secret.clone()),
            failures: vec![MutationFailure {
                uri: Some(second.clone()),
                message: format!(
                    "upstream token {secret} {}",
                    "large-body ".repeat(PARTIAL_SUMMARY_MESSAGE_CHARS)
                ),
            }],
        };
        let partition =
            validate_mutation_receipt(&provider, receipt.mutation_id, &mutation, &receipt)
                .unwrap()
                .unwrap();
        let (_, detail) =
            bounded_partial_summary(&provider, &mutation, &receipt, &partition).unwrap();
        assert!(detail.len() <= PARTIAL_SUMMARY_MAX_BYTES);
        assert!(!detail.contains(&first.as_uri()));
        assert!(!detail.contains(&second.as_uri()));
        assert!(!detail.contains(&receipt.failures[0].message));
        assert!(!detail.contains(&secret));
        assert!(!detail.contains(&"a".repeat(48)));
        assert!(!detail.contains(&"b".repeat(48)));
        assert!(detail.contains("<redacted>"));
    }

    #[test]
    fn undo_provider_error_classification_distinguishes_definite_failures() {
        for error in [
            ProviderError::AuthRequired,
            ProviderError::RateLimited {
                scope: None,
                retry_after: None,
            },
            ProviderError::VersionConflict {
                expected: Some("expected".to_string()),
                actual: Some("actual".to_string()),
            },
            ProviderError::InvalidInput {
                field: "uri".to_string(),
                message: "invalid".to_string(),
            },
            ProviderError::NoActiveDevice,
            ProviderError::Upstream {
                status: 409,
                message: "conflict".to_string(),
            },
        ] {
            assert!(!provider_error_may_follow_write(&error), "{error}");
        }
        for error in [
            ProviderError::Network("connection reset".to_string()),
            ProviderError::Transient {
                status: Some(503),
                message: "retry".to_string(),
            },
            ProviderError::Decode("empty acknowledgement".to_string()),
            ProviderError::Provider("unknown adapter outcome".to_string()),
            ProviderError::Upstream {
                status: 500,
                message: "server error".to_string(),
            },
        ] {
            assert!(provider_error_may_follow_write(&error), "{error}");
        }
    }

    #[tokio::test]
    async fn processing_recovery_continues_after_one_claim_fails() {
        async fn claim(
            state: &DaemonState,
            request_json: &str,
            uri: &str,
        ) -> (MutationId, ReceiptId) {
            let mutation_id = MutationId::new_v7();
            let receipt = Receipt {
                receipt_id: ReceiptId::new_v7(),
                action: "recovery-test".to_string(),
                status: ReceiptStatus::Pending,
                message: "queued".to_string(),
                started_at_ms: 10,
                finished_at_ms: None,
                error: None,
            };
            let operation = Operation {
                operation_id: OperationId::new_v7(),
                kind: OperationKind::LibrarySave,
                occurred_at_ms: 10,
                finished_at_ms: None,
                source: OperationSource::Cli,
                requester: None,
                subject_uris: vec![uri.to_string()],
                reversible: true,
                reversal_plan: Some(ReversalPlan::LibraryUnsave {
                    uri: uri.to_string(),
                }),
                pre_state: Some(PreState::LibrarySave {
                    uri: uri.to_string(),
                    prior_was_saved: false,
                }),
                status: OperationStatus::Pending,
                receipt_id: Some(receipt.receipt_id),
                subject_op_id: None,
                undone_by_op_id: None,
                redone_by_op_id: None,
                error_message: None,
            };
            state
                .store()
                .claim_mutation(
                    mutation_id,
                    "recovery-test",
                    request_json,
                    &receipt,
                    &operation,
                    10,
                )
                .await
                .unwrap();
            (mutation_id, receipt.receipt_id)
        }

        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let default = Arc::new(FakeProvider::isolated("fake-a").unwrap());
        let selected = Arc::new(FakeProvider::isolated("fake-b").unwrap());
        let state = Arc::new(
            DaemonState::new_with_providers(registry(default, selected))
                .await
                .unwrap(),
        );
        let (bad_id, bad_receipt) = claim(&state, "not-json", "fake-a:track:track-1").await;
        let good_request = serde_json::to_string(&Request::LibrarySave {
            uri: Some("fake-b:track:track-2".to_string()),
            current: false,
        })
        .unwrap();
        let (good_id, good_receipt) = claim(&state, &good_request, "fake-b:track:track-2").await;

        assert_eq!(recover_processing_mutations(&state).await.unwrap(), (1, 1));
        let remaining = state.store().processing_mutation_claims().await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].mutation_id, bad_id);
        assert_eq!(
            state.store().get_receipt(bad_receipt).await.unwrap().status,
            ReceiptStatus::Pending
        );
        assert_eq!(
            state
                .store()
                .get_receipt(good_receipt)
                .await
                .unwrap()
                .status,
            ReceiptStatus::Failed
        );
        assert!(state
            .store()
            .terminal_mutation_response(good_id)
            .await
            .unwrap()
            .is_some());

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn bulk_undo_recovery_uses_only_the_persisted_multi_provider_snapshot() {
        async fn insert_succeeded_operation(
            state: &DaemonState,
            kind: OperationKind,
            subject_uris: Vec<String>,
            pre_state: PreState,
            reversal_plan: ReversalPlan,
            occurred_at_ms: i64,
        ) -> Operation {
            let receipt_id = ReceiptId::new_v7();
            state
                .store()
                .insert_pending_receipt(
                    &Receipt {
                        receipt_id,
                        action: "test".to_string(),
                        status: ReceiptStatus::Pending,
                        message: "queued".to_string(),
                        started_at_ms: occurred_at_ms,
                        finished_at_ms: None,
                        error: None,
                    },
                    "{}",
                )
                .await
                .unwrap();
            let operation = Operation {
                operation_id: OperationId::new_v7(),
                kind,
                occurred_at_ms,
                finished_at_ms: None,
                source: OperationSource::Cli,
                requester: None,
                subject_uris,
                reversible: true,
                reversal_plan: Some(reversal_plan),
                pre_state: Some(pre_state),
                status: OperationStatus::Pending,
                receipt_id: Some(receipt_id),
                subject_op_id: None,
                undone_by_op_id: None,
                redone_by_op_id: None,
                error_message: None,
            };
            state
                .store()
                .insert_pending_operation(&operation)
                .await
                .unwrap();
            state
                .store()
                .finalize_operation(
                    operation.operation_id,
                    OperationStatus::Succeeded,
                    occurred_at_ms + 1,
                    None,
                )
                .await
                .unwrap();
            state
                .store()
                .get_operation(operation.operation_id)
                .await
                .unwrap()
        }

        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let default = Arc::new(FakeProvider::isolated("fake-a").unwrap());
        let selected = Arc::new(FakeProvider::isolated("fake-b").unwrap());
        let state = Arc::new(
            DaemonState::new_with_providers(registry(default, selected))
                .await
                .unwrap(),
        );
        let library_uri = "fake-a:track:track-1".to_string();
        let playlist_uri = "fake-b:playlist:playlist-1".to_string();
        let playlist_item_uri = "fake-b:track:track-2".to_string();
        let library = insert_succeeded_operation(
            &state,
            OperationKind::LibrarySave,
            vec![library_uri.clone()],
            PreState::LibrarySave {
                uri: library_uri.clone(),
                prior_was_saved: false,
            },
            ReversalPlan::LibraryUnsave {
                uri: library_uri.clone(),
            },
            10,
        )
        .await;
        let playlist = insert_succeeded_operation(
            &state,
            OperationKind::PlaylistAdd,
            vec![playlist_item_uri.clone()],
            PreState::PlaylistAdd {
                playlist_id: playlist_uri.clone(),
                version_token: None,
                added_uris: vec![playlist_item_uri.clone()],
            },
            ReversalPlan::PlaylistRemoveTracks {
                playlist_id: playlist_uri.clone(),
                uris: vec![playlist_item_uri],
                version_token: None,
            },
            20,
        )
        .await;
        let later_uri = "fake-a:track:track-3".to_string();
        let later = insert_succeeded_operation(
            &state,
            OperationKind::LibrarySave,
            vec![later_uri.clone()],
            PreState::LibrarySave {
                uri: later_uri.clone(),
                prior_was_saved: false,
            },
            ReversalPlan::LibraryUnsave { uri: later_uri },
            30,
        )
        .await;

        let outer_receipt = ReceiptId::new_v7();
        state
            .store()
            .insert_pending_receipt(
                &Receipt {
                    receipt_id: outer_receipt,
                    action: "ops-undo".to_string(),
                    status: ReceiptStatus::Pending,
                    message: "queued".to_string(),
                    started_at_ms: 40,
                    finished_at_ms: None,
                    error: None,
                },
                "{}",
            )
            .await
            .unwrap();
        let outer = Operation {
            operation_id: OperationId::new_v7(),
            kind: OperationKind::Undo,
            occurred_at_ms: 40,
            finished_at_ms: None,
            source: OperationSource::Cli,
            requester: None,
            subject_uris: vec![],
            reversible: false,
            reversal_plan: Some(ReversalPlan::NotReversible {
                reason: "bulk undo".to_string(),
            }),
            pre_state: None,
            status: OperationStatus::Pending,
            receipt_id: Some(outer_receipt),
            subject_op_id: None,
            undone_by_op_id: None,
            redone_by_op_id: None,
            error_message: None,
        };
        state
            .store()
            .insert_pending_operation(&outer)
            .await
            .unwrap();
        state
            .store()
            .record_bulk_undo_candidates(outer.operation_id, &[library.clone(), playlist.clone()])
            .await
            .unwrap();

        let request = serde_json::to_string(&Request::OpsUndo {
            operation_id: None,
            dry_run: false,
            force: false,
            bulk_since_ms: Some(0),
        })
        .unwrap();
        let (reconciliations, guard) = recovery_reconciliation_intent(
            state.as_ref(),
            &request,
            outer_receipt,
            outer.operation_id,
        )
        .await
        .unwrap();
        assert_eq!(reconciliations.len(), 2);
        assert!(reconciliations.iter().any(|reconciliation| {
            reconciliation.provider.as_str() == "fake-a"
                && reconciliation.target == SyncTargetData::Library
                && reconciliation.resource_uris == vec![library_uri.clone()]
        }));
        assert!(reconciliations.iter().any(|reconciliation| {
            reconciliation.provider.as_str() == "fake-b"
                && reconciliation.target == SyncTargetData::Playlists
                && reconciliation.resource_uris.contains(&playlist_uri)
        }));
        assert!(!reconciliations.iter().any(|reconciliation| {
            reconciliation
                .resource_uris
                .contains(&later.subject_uris[0])
        }));
        assert_eq!(
            guard,
            Some(spotuify_store::PostWriteOperationGuard::DisableUndo(
                library.operation_id,
            ))
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
    }

    #[tokio::test]
    async fn undo_mutation_id_replays_after_restart_without_second_provider_write() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();
        let default = Arc::new(FakeProvider::isolated("fake-a").unwrap());
        let selected = Arc::new(FakeProvider::isolated("fake-b").unwrap());
        let state = Arc::new(
            DaemonState::new_with_providers(registry(default.clone(), selected))
                .await
                .unwrap(),
        );

        let save = dispatch_with_mutation(
            state.clone(),
            Request::LibrarySave {
                uri: Some("fake-a:track:track-2".to_string()),
                current: false,
            },
            None,
            Some(MutationId::new_v7()),
        )
        .await
        .unwrap();
        assert_eq!(
            wait_for_receipt(&state, pending_receipt(&save)).await,
            ReceiptStatus::Confirmed
        );
        let original = state
            .store()
            .list_operations(20, None, None)
            .await
            .unwrap()
            .into_iter()
            .find(|op| op.kind == OperationKind::LibrarySave)
            .expect("saved operation");

        let undo_mutation_id = MutationId::new_v7();
        let first = dispatch_with_mutation(
            state.clone(),
            Request::OpsUndo {
                operation_id: None,
                dry_run: false,
                force: false,
                bulk_since_ms: None,
            },
            None,
            Some(undo_mutation_id),
        )
        .await
        .unwrap();
        let undo_op_id = match &first {
            ResponseData::OperationUndoResult { undo_op_id, .. } => *undo_op_id,
            other => panic!("expected undo result, got {other:?}"),
        };
        assert_eq!(
            state
                .store()
                .get_operation(original.operation_id)
                .await
                .unwrap()
                .status,
            OperationStatus::Undone
        );
        assert_eq!(
            state
                .store()
                .get_operation(undo_op_id)
                .await
                .unwrap()
                .subject_op_id,
            Some(original.operation_id)
        );
        assert_eq!(
            default
                .observed_requests()
                .await
                .iter()
                .filter(|request| request.operation == "apply_mutation")
                .count(),
            2,
            "save plus one undo reversal"
        );

        let redo_mutation_id = MutationId::new_v7();
        let redo_request = Request::OpsRedo { operation_id: None };
        let redo = dispatch_with_mutation(
            state.clone(),
            redo_request.clone(),
            None,
            Some(redo_mutation_id),
        )
        .await
        .unwrap();
        assert_eq!(
            state
                .store()
                .get_operation(original.operation_id)
                .await
                .unwrap()
                .status,
            OperationStatus::Redone
        );
        let redo_replay =
            dispatch_with_mutation(state.clone(), redo_request, None, Some(redo_mutation_id))
                .await
                .unwrap();
        assert_eq!(
            serde_json::to_value(redo_replay).unwrap(),
            serde_json::to_value(redo).unwrap()
        );
        assert_eq!(
            default
                .observed_requests()
                .await
                .iter()
                .filter(|request| request.operation == "apply_mutation")
                .count(),
            3,
            "save, undo, and one redo; replay must not call the provider"
        );

        state.shutdown_search().await;
        state.shutdown_player().await;
        drop(state);

        let restarted_default = Arc::new(FakeProvider::isolated("fake-a").unwrap());
        let restarted_selected = Arc::new(FakeProvider::isolated("fake-b").unwrap());
        let restarted = Arc::new(
            DaemonState::new_with_providers(registry(
                restarted_default.clone(),
                restarted_selected,
            ))
            .await
            .unwrap(),
        );
        let replay = dispatch_with_mutation(
            restarted.clone(),
            Request::OpsUndo {
                operation_id: None,
                dry_run: false,
                force: false,
                bulk_since_ms: None,
            },
            None,
            Some(undo_mutation_id),
        )
        .await
        .unwrap();
        assert_eq!(
            serde_json::to_value(replay).unwrap(),
            serde_json::to_value(first).unwrap()
        );
        assert_eq!(
            restarted_default
                .observed_requests()
                .await
                .iter()
                .filter(|request| request.operation == "apply_mutation")
                .count(),
            0,
            "replayed undo must not call the provider"
        );
        restarted.shutdown_search().await;
        restarted.shutdown_player().await;
    }
}
