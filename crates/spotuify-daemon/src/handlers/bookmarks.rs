//! `bookmarks` request handlers: saved positions inside a media item.

use std::sync::Arc;

use spotuify_core::{now_ms, Bookmark, MediaKind, RequestContext, ResourceUri};
use spotuify_protocol::{DaemonEvent, OperationSource, Request, ResponseData};

use crate::handler::*;
use crate::state::DaemonState;

pub(crate) async fn dispatch(
    state: Arc<DaemonState>,
    request: Request,
    _source: Option<OperationSource>,
) -> anyhow::Result<ResponseData> {
    match request {
        Request::BookmarkCreate {
            media_uri,
            position_ms,
            note,
        } => {
            let (media_uri, position_ms) = resolve_bookmark_target(&state, media_uri, position_ms)?;
            let (media_kind, name, subtitle, image_url) =
                resolve_bookmark_snapshot(&state, &media_uri).await?;
            let bookmark = Bookmark {
                id: uuid::Uuid::now_v7().to_string(),
                media_uri,
                media_kind,
                name,
                subtitle,
                image_url,
                position_ms,
                note: normalize_note(note),
                created_at_ms: now_ms(),
            };
            state.store().create_bookmark(&bookmark).await?;
            state.emit_event(DaemonEvent::BookmarksChanged {
                action: "created".to_string(),
            });
            Ok(ResponseData::BookmarkCreated { bookmark })
        }
        Request::BookmarksList { media_uri } => Ok(ResponseData::Bookmarks {
            bookmarks: state.store().list_bookmarks(media_uri.as_deref()).await?,
        }),
        Request::BookmarkUpdate { id, note } => {
            let note = normalize_note(note);
            if !state
                .store()
                .set_bookmark_note(&id, note.as_deref())
                .await?
            {
                anyhow::bail!("bookmark {id} not found");
            }
            state.emit_event(DaemonEvent::BookmarksChanged {
                action: "updated".to_string(),
            });
            Ok(ResponseData::Ack {
                message: "bookmark updated".to_string(),
            })
        }
        Request::BookmarkDelete { id } => {
            if !state.store().delete_bookmark(&id).await? {
                anyhow::bail!("bookmark {id} not found");
            }
            state.emit_event(DaemonEvent::BookmarksChanged {
                action: "deleted".to_string(),
            });
            Ok(ResponseData::Ack {
                message: "bookmark deleted".to_string(),
            })
        }
        Request::BookmarkPlay { id } => {
            let Some(bookmark) = state.store().get_bookmark(&id).await? else {
                anyhow::bail!("bookmark {id} not found");
            };
            let captured_seq = state.bump_mutation_seq();
            let resource = ResourceUri::parse(&bookmark.media_uri)?;
            let provider = state.provider_for_uri(&resource).await?;
            let transport = state.provider_transport(provider.id()).await?;
            let provider_id = provider.id().clone();
            let result = execute_provider_pair_with_recovery(
                &state,
                provider,
                transport,
                CommandKind::PlayUri {
                    uri: bookmark.media_uri.clone(),
                    context: Some(PlayContext {
                        position_ms: bookmark.position_ms,
                        ..PlayContext::default()
                    }),
                },
            )
            .await?;
            let applied_seq = if state.set_active_transport_provider(provider_id.clone()) {
                state.bump_mutation_seq()
            } else {
                captured_seq
            };
            persist_command_result(
                &state,
                &provider_id,
                applied_seq,
                &result,
                "bookmark-play",
                None,
            )
            .await;
            state.emit_event(DaemonEvent::PlaybackChanged {
                action: "bookmark-play".to_string(),
                playback: Some(state.snapshot_playback()),
            });
            if result.request_refresh {
                spawn_playback_refresh(state.clone());
            }
            Ok(ResponseData::Ack {
                message: format!(
                    "playing {} from {}",
                    bookmark.name,
                    clock_label(bookmark.position_ms)
                ),
            })
        }
        _ => unreachable!("non-bookmark request routed to bookmarks dispatcher"),
    }
}

/// Display snapshot for a bookmark: cache first, then one bounded provider
/// lookup (an explicitly bookmarked episode is rarely in the cache yet), and
/// only then the URI-tail fallback the reminders path uses.
async fn resolve_bookmark_snapshot(
    state: &DaemonState,
    uri: &str,
) -> anyhow::Result<(MediaKind, String, String, Option<String>)> {
    let (kind, name, subtitle, image_url) = resolve_reminder_snapshot(state, uri).await?;
    let resource = ResourceUri::parse(uri)?;
    let unresolved = name == resource.bare_id();
    if !unresolved || !matches!(kind, MediaKind::Track | MediaKind::Episode) {
        return Ok((kind, name, subtitle, image_url));
    }
    let Ok(provider) = state.provider_for_uri(&resource).await else {
        return Ok((kind, name, subtitle, image_url));
    };
    if !provider.capabilities().catalog.lookup_kinds.contains(&kind) {
        return Ok((kind, name, subtitle, image_url));
    }
    match provider
        .media_item(RequestContext::FOREGROUND, &resource)
        .await
    {
        Ok(Some(item)) => Ok((item.kind, item.name, item.subtitle, item.image_url)),
        Ok(None) => Ok((kind, name, subtitle, image_url)),
        Err(error) => {
            tracing::debug!(%uri, error = %error, "bookmark snapshot lookup failed; using URI fallback");
            Ok((kind, name, subtitle, image_url))
        }
    }
}

/// Fill in whatever the caller left blank from the daemon's playback clock:
/// a bare `bookmark add` means "the current item at its current position".
/// An explicit `media_uri` with no position starts at 0 (the caller is
/// pinning an item that may not be playing).
fn resolve_bookmark_target(
    state: &DaemonState,
    media_uri: Option<String>,
    position_ms: Option<u64>,
) -> anyhow::Result<(String, u64)> {
    match media_uri {
        Some(uri) => {
            ResourceUri::parse(&uri)?;
            Ok((uri, position_ms.unwrap_or(0)))
        }
        None => {
            let snapshot = state.snapshot_playback();
            let Some(item) = snapshot.item else {
                anyhow::bail!("invalid request: nothing is playing; pass a media URI to bookmark");
            };
            Ok((item.uri, position_ms.unwrap_or(snapshot.progress_ms)))
        }
    }
}

/// `h:mm:ss` when an hour or more in, else `m:ss` — podcast positions are
/// routinely past the hour mark.
fn clock_label(position_ms: u64) -> String {
    let total_secs = position_ms / 1000;
    let (hours, minutes, seconds) = (total_secs / 3600, (total_secs % 3600) / 60, total_secs % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

fn normalize_note(note: Option<String>) -> Option<String> {
    note.map(|note| note.trim().to_string())
        .filter(|note| !note.is_empty())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn clock_label_switches_to_hours_past_sixty_minutes() {
        assert_eq!(clock_label(0), "0:00");
        assert_eq!(clock_label(61_000), "1:01");
        assert_eq!(clock_label(3_600_000 + 5_000), "1:00:05");
    }

    #[test]
    fn blank_notes_are_stored_as_none() {
        assert_eq!(normalize_note(None), None);
        assert_eq!(normalize_note(Some("   ".to_string())), None);
        assert_eq!(
            normalize_note(Some("  keep me ".to_string())),
            Some("keep me".to_string())
        );
    }
}
