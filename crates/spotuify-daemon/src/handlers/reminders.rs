//! `reminders` request handlers (split out of the dispatch god-function).

use std::sync::Arc;

use spotuify_core::{
    now_ms, AccessOutcome, CollectionRequest, MediaItem, MediaKind, PageRequest, ProviderError,
    RequestContext, ResourceUri,
};
use spotuify_protocol::{DaemonEvent, OperationSource, Request, ResponseData};

use crate::handler::*;
use crate::handlers::playback::{
    dedup_queue_items, live_queue_uris, report_partial_queue_application,
    start_embedded_queue_session,
};
use crate::state::DaemonState;

pub(crate) async fn dispatch(
    state: Arc<DaemonState>,
    request: Request,
    _source: Option<OperationSource>,
) -> anyhow::Result<ResponseData> {
    match request {
        Request::ReminderCreate {
            media_uri,
            anchor_at_ms,
            recurrence,
            tz,
            message,
        } => {
            let (media_kind, name, subtitle, image_url) =
                resolve_reminder_snapshot(&state, &media_uri).await?;
            let reminder = spotuify_core::Reminder {
                id: uuid::Uuid::now_v7().to_string(),
                media_uri,
                media_kind,
                name,
                subtitle,
                image_url,
                anchor_at_ms,
                recurrence,
                tz,
                next_due_at_ms: crate::reminders::initial_next_due(anchor_at_ms, recurrence),
                state: spotuify_core::ReminderState::Active,
                message,
                created_at_ms: now_ms(),
            };
            state.store().create_reminder(&reminder).await?;
            crate::reminders::wake_scheduler();
            state.emit_event(DaemonEvent::RemindersChanged {
                action: "created".to_string(),
            });
            Ok(ResponseData::ReminderCreated { reminder })
        }
        Request::RemindersList { include_inactive } => Ok(ResponseData::Reminders {
            reminders: state.store().list_reminders(include_inactive).await?,
        }),
        Request::ReminderCancel { id } => {
            state.store().cancel_reminder(&id).await?;
            crate::reminders::wake_scheduler();
            state.emit_event(DaemonEvent::RemindersChanged {
                action: "cancelled".to_string(),
            });
            Ok(ResponseData::Ack {
                message: "reminder cancelled".to_string(),
            })
        }
        Request::NotificationsList { include_archived } => Ok(ResponseData::Notifications {
            notifications: state.store().list_notifications(include_archived).await?,
        }),
        Request::NotificationAct {
            id,
            action,
            snooze_until_ms,
        } => {
            use spotuify_core::NotificationState as NS;
            use spotuify_protocol::NotificationAction as NA;
            let Some(notification) = state.store().get_notification(&id).await? else {
                anyhow::bail!("notification {id} not found");
            };
            match action {
                NA::Seen => {
                    state
                        .store()
                        .set_notification_state(&id, NS::Seen, None, None)
                        .await?;
                }
                NA::Dismiss => {
                    state
                        .store()
                        .set_notification_state(&id, NS::Dismissed, None, None)
                        .await?;
                }
                NA::Snooze => {
                    let until = snooze_until_ms.unwrap_or_else(|| now_ms() + 3_600_000);
                    state
                        .store()
                        .set_notification_state(&id, NS::Snoozed, Some(until), None)
                        .await?;
                    crate::reminders::wake_scheduler();
                }
                NA::Play => {
                    let captured_seq = state.bump_mutation_seq();
                    let resource = ResourceUri::parse(&notification.media_uri)?;
                    let provider = state.provider_for_uri(&resource).await?;
                    let transport = state.provider_transport(provider.id()).await?;
                    let provider_id = provider.id().clone();
                    let result = execute_provider_pair_with_recovery(
                        &state,
                        provider,
                        transport,
                        CommandKind::PlayUri {
                            uri: notification.media_uri.clone(),
                            context: None,
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
                        "notification-play",
                        None,
                    )
                    .await;
                    state.emit_event(DaemonEvent::PlaybackChanged {
                        action: "notification-play".to_string(),
                        playback: Some(state.snapshot_playback()),
                    });
                    if result.request_refresh {
                        spawn_playback_refresh(state.clone());
                    }
                    state
                        .store()
                        .set_notification_state(&id, NS::Done, None, Some("played"))
                        .await?;
                }
                NA::Queue => {
                    state.bump_mutation_seq();
                    let resource = ResourceUri::parse(&notification.media_uri)?;
                    let provider = state.provider_for_uri(&resource).await?;
                    let provider_id = provider.id().clone();
                    let transport = state.provider_transport(&provider_id).await?;
                    let resolved_items =
                        queueable_items_for_provider_selection(&state, &notification.media_uri)
                            .await?;
                    let queue_read = provider
                        .capabilities()
                        .transport
                        .as_ref()
                        .is_some_and(|caps| caps.queue_read);
                    let already_queued = live_queue_uris(transport.as_ref(), queue_read).await;
                    let (items, _) = dedup_queue_items(resolved_items, &already_queued);
                    let mut played_first = false;
                    let mut owns_transport = false;
                    let mut applied_items = Vec::new();
                    let mut queued_uris = Vec::new();
                    for (index, item) in items.iter().enumerate() {
                        let attempt =
                            match queue_one(provider.as_ref(), transport.as_ref(), &item.uri).await
                            {
                                Ok(attempt) => attempt,
                                Err(error) => {
                                    if owns_transport {
                                        return Err(report_partial_queue_application(
                                            state.clone(),
                                            provider.clone(),
                                            transport.clone(),
                                            applied_items,
                                            queued_uris,
                                            &already_queued,
                                            played_first.then(|| items[0].clone()),
                                            state.current_mutation_seq(),
                                            error.context(format!(
                                                "notification queue add for {} failed",
                                                item.uri
                                            )),
                                        )
                                        .await);
                                    }
                                    return Err(error);
                                }
                            };
                        match attempt {
                            QueueAttempt::Queued => {
                                if !owns_transport {
                                    state.set_active_transport_provider(provider_id.clone());
                                    state.bump_mutation_seq();
                                    owns_transport = true;
                                }
                                applied_items.push(item.clone());
                                queued_uris.push(item.uri.clone());
                            }
                            QueueAttempt::NoActiveDevice if index == 0 => {
                                start_embedded_queue_session(
                                    state.as_ref(),
                                    provider.as_ref(),
                                    transport.as_ref(),
                                    &item.uri,
                                )
                                .await?;
                                state.set_active_transport_provider(provider_id.clone());
                                state.bump_mutation_seq();
                                owns_transport = true;
                                played_first = true;
                            }
                            QueueAttempt::NoActiveDevice => {
                                if owns_transport {
                                    return Err(report_partial_queue_application(
                                        state.clone(),
                                        provider.clone(),
                                        transport.clone(),
                                        applied_items,
                                        queued_uris,
                                        &already_queued,
                                        played_first.then(|| items[0].clone()),
                                        state.current_mutation_seq(),
                                        anyhow::Error::new(ProviderError::NoActiveDevice).context(
                                            format!(
                                                "notification queue add for {} failed",
                                                item.uri
                                            ),
                                        ),
                                    )
                                    .await);
                                }
                                return Err(ProviderError::NoActiveDevice.into());
                            }
                        }
                    }
                    let queue = cache_optimistic_queue_application(
                        &state,
                        &provider_id,
                        played_first.then(|| items[0].clone()),
                        applied_items,
                        &already_queued,
                    )
                    .await;
                    state.emit_event(DaemonEvent::QueueChanged {
                        action: "notification-queue".to_string(),
                        uris: queued_uris,
                        queue,
                    });
                    spawn_queue_refresh_for_pair(
                        state.clone(),
                        provider,
                        transport,
                        state.current_mutation_seq(),
                    );
                    state
                        .store()
                        .set_notification_state(&id, NS::Done, None, Some("queued"))
                        .await?;
                }
            }
            state.emit_event(DaemonEvent::RemindersChanged {
                action: "notification-acted".to_string(),
            });
            Ok(ResponseData::Ack {
                message: "ok".to_string(),
            })
        }
        _ => unreachable!("non-reminders request routed to reminders dispatcher"),
    }
}

async fn queueable_items_for_provider_selection(
    state: &DaemonState,
    uri: &str,
) -> anyhow::Result<Vec<MediaItem>> {
    let resource = ResourceUri::parse(uri)?;
    let provider = state.provider_for_uri(&resource).await?;
    match resource.kind() {
        MediaKind::Track => {
            require_provider_capability(
                provider.as_ref(),
                "track catalog lookup",
                provider
                    .capabilities()
                    .catalog
                    .lookup_kinds
                    .contains(&MediaKind::Track),
            )?;
            Ok(match provider
                .media_item(RequestContext::FOREGROUND, &resource)
                .await?
            {
                Some(item) => {
                    validate_provider_lookup_result(provider.as_ref(), &resource, &item)?;
                    vec![item]
                }
                None => vec![media_item_from_uri(uri)?],
            })
        }
        MediaKind::Episode => Ok(vec![media_item_from_uri(uri)?]),
        MediaKind::Playlist => {
            require_provider_capability(
                provider.as_ref(),
                "playlist item reads",
                provider.capabilities().playlists.item_read,
            )?;
            let limit = provider
                .capabilities()
                .playlists
                .items_max_page_size
                .unwrap_or(50) as u32;
            let mut request = PageRequest::new(limit.max(1), 0);
            let mut items = Vec::new();
            let mut seen_cursors = std::collections::HashSet::new();
            for page_index in 0..PROVIDER_PAGINATION_MAX_PAGES {
                let page = match provider
                    .playlist_items(
                        RequestContext::FOREGROUND,
                        CollectionRequest {
                            uri: resource.clone(),
                            page: request.clone(),
                        },
                    )
                    .await?
                {
                    AccessOutcome::Available(page) => page,
                    AccessOutcome::Unavailable(reason) => {
                        anyhow::bail!("playlist items are unavailable: {reason:?}")
                    }
                };
                validate_provider_page_offset(&request, &page, "playlist_items")?;
                validate_provider_collection_items(
                    provider.as_ref(),
                    "playlist_items",
                    &[MediaKind::Track, MediaKind::Episode],
                    &page.items,
                )?;
                let logical_offset = request.offset.saturating_add(page.items.len() as u64);
                items.extend(page.items);
                let Some(next) = page.next else {
                    return Ok(items);
                };
                request = next_provider_page(
                    &request,
                    next,
                    logical_offset,
                    &mut seen_cursors,
                    page_index + 1,
                    "reminder playlist items",
                )?;
            }
            Err(ProviderError::Provider(format!(
                "reminder playlist item pagination exceeded {PROVIDER_PAGINATION_MAX_PAGES} pages"
            ))
            .into())
        }
        MediaKind::Album => {
            require_provider_capability(
                provider.as_ref(),
                "album tracks",
                provider.capabilities().catalog.album_tracks,
            )?;
            let limit = provider
                .capabilities()
                .catalog
                .album_tracks_max_page_size
                .unwrap_or(50) as u32;
            let mut request = PageRequest::new(limit.max(1), 0);
            let mut items = Vec::new();
            let mut seen_cursors = std::collections::HashSet::new();
            for page_index in 0..PROVIDER_PAGINATION_MAX_PAGES {
                let page = provider
                    .album_tracks(
                        RequestContext::FOREGROUND,
                        CollectionRequest {
                            uri: resource.clone(),
                            page: request.clone(),
                        },
                    )
                    .await?;
                validate_provider_page_offset(&request, &page, "album_tracks")?;
                validate_provider_collection_items(
                    provider.as_ref(),
                    "album_tracks",
                    &[MediaKind::Track],
                    &page.items,
                )?;
                items.extend(page.items);
                let Some(next) = page.next else {
                    return Ok(items);
                };
                request = next_provider_page(
                    &request,
                    next,
                    items.len() as u64,
                    &mut seen_cursors,
                    page_index + 1,
                    "reminder album tracks",
                )?;
            }
            Err(ProviderError::Provider(format!(
                "reminder album track pagination exceeded {PROVIDER_PAGINATION_MAX_PAGES} pages"
            ))
            .into())
        }
        MediaKind::Artist | MediaKind::Show => anyhow::bail!(
            "artist and show URIs cannot be appended to the queue; choose a track, episode, album, or playlist"
        ),
    }
}

#[cfg(test)]
fn require_queued_reminder_item(attempt: QueueAttempt) -> anyhow::Result<()> {
    match attempt {
        QueueAttempt::Queued => Ok(()),
        QueueAttempt::NoActiveDevice => Err(ProviderError::NoActiveDevice.into()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::Arc;

    use spotuify_core::{
        MusicProvider as _, Notification, NotificationState, RemoteTransport as _,
    };
    use spotuify_protocol::NotificationAction;

    use crate::provider_registry::{ProviderRegistry, ProviderRuntime};

    use super::*;

    struct TestEnv {
        _temp: tempfile::TempDir,
    }

    impl TestEnv {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            std::env::set_var("SPOTUIFY_FAKE_SPOTIFY", "1");
            std::env::set_var("SPOTUIFY_CACHE_DB", temp.path().join("cache.sqlite3"));
            std::env::set_var(
                "SPOTUIFY_ANALYTICS_DB",
                temp.path().join("analytics.sqlite3"),
            );
            std::env::set_var("SPOTUIFY_SEARCH_INDEX", temp.path().join("search-index"));
            std::env::set_var("SPOTUIFY_RUNTIME_DIR", temp.path().join("runtime"));
            Self { _temp: temp }
        }
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            std::env::remove_var("SPOTUIFY_FAKE_SPOTIFY");
            std::env::remove_var("SPOTUIFY_CACHE_DB");
            std::env::remove_var("SPOTUIFY_ANALYTICS_DB");
            std::env::remove_var("SPOTUIFY_SEARCH_INDEX");
            std::env::remove_var("SPOTUIFY_RUNTIME_DIR");
        }
    }

    #[test]
    fn no_active_device_does_not_count_as_a_queued_reminder_item() {
        let error = require_queued_reminder_item(QueueAttempt::NoActiveDevice).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<ProviderError>(),
            Some(ProviderError::NoActiveDevice)
        ));
    }

    #[tokio::test]
    async fn notification_play_updates_the_uri_selected_provider_transport() {
        let _guard = crate::ENV_LOCK.lock().await;
        let _env = TestEnv::new();

        let default = Arc::new(spotuify_provider_fake::FakeProvider::isolated("fake-a").unwrap());
        let selected = Arc::new(spotuify_provider_fake::FakeProvider::isolated("fake-b").unwrap());
        let registry = ProviderRegistry::new(
            default.id().clone(),
            [
                ProviderRuntime::with_transport(default.clone()).unwrap(),
                ProviderRuntime::with_transport(selected.clone()).unwrap(),
            ],
        )
        .unwrap();
        let state = Arc::new(DaemonState::new_with_providers(registry).await.unwrap());
        let notification = Notification {
            id: "notification-1".to_string(),
            reminder_id: "reminder-1".to_string(),
            media_uri: "fake-b:track:track-2".to_string(),
            media_kind: MediaKind::Track,
            name: "Fake Track Two".to_string(),
            subtitle: "Fake Artist".to_string(),
            image_url: None,
            due_at_ms: 1,
            fired_at_ms: 1,
            state: NotificationState::Unseen,
            snoozed_until_ms: None,
            acted: None,
            message: None,
        };
        state
            .store()
            .insert_notification(&notification)
            .await
            .unwrap();

        dispatch(
            state.clone(),
            Request::NotificationAct {
                id: notification.id.clone(),
                action: NotificationAction::Play,
                snooze_until_ms: None,
            },
            None,
        )
        .await
        .unwrap();

        let playback = selected
            .playback(RequestContext::PLAYBACK_CONTROL)
            .await
            .unwrap();
        assert_eq!(
            playback.item.as_ref().map(|item| item.uri.as_str()),
            Some("fake-b:track:track-2")
        );
        assert_eq!(
            state
                .store()
                .get_notification(&notification.id)
                .await
                .unwrap()
                .unwrap()
                .state,
            NotificationState::Done
        );
    }
}
