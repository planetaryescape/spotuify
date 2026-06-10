//! `reminders` request handlers (split out of the dispatch god-function).

use std::sync::Arc;

use spotuify_core::now_ms;
use spotuify_protocol::{DaemonEvent, OperationSource, Request, ResponseData};

use crate::handler::*;
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
                    state
                        .transport(crate::state::TransportCmd::PlayUri {
                            uri: notification.media_uri.clone(),
                            position_ms: 0,
                        })
                        .await
                        .map_err(|err| anyhow::anyhow!("play failed: {err}"))?;
                    state
                        .store()
                        .set_notification_state(&id, NS::Done, None, Some("played"))
                        .await?;
                }
                NA::Queue => {
                    let mut client = state.spotify_client().await?;
                    let items =
                        queueable_items_for_selection(&state, &mut client, &notification.media_uri)
                            .await?;
                    for item in &items {
                        let _ = queue_one(&state, &mut client, &item.uri).await?;
                    }
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
