use ratatui::layout::{Constraint, Direction, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use ratatui_image::StatefulImage;

use crate::app::{App, ArtworkSubject, BannerState, FullscreenPanel, RightRailMode, Screen};
// top_hints is referenced via crate path inside render_hint_bar.
use crate::now_playing::{NowPlayingView, PlaybackDisplayState};
use crate::widgets::spectrum::SpectrumWidget;
use spotuify_core::active_lyric_line_index;
use spotuify_spotify::client::{MediaItem, MediaKind, Playlist};

use crate::widgets::style::{
    accent, accent_foreground, progress_filled, BG, BORDER, BORDER_STRONG, CHIP_BG, CHIP_FG,
    DANGER, KIND_ALBUM, KIND_ARTIST, KIND_PODCAST, PROGRESS_UNFILLED, SURFACE, TEXT, TEXT_MUTED,
    WARN,
};
pub const PLAYER_HEIGHT: u16 = 10;
pub const STATUS_HEIGHT: u16 = 3;

/// The root body/player/status split. Shared with the mouse geometry
/// in `app.rs`: below ~25 rows the solver shrinks the chrome, and
/// hit-test helpers that assumed "player is always the bottom
/// PLAYER_HEIGHT rows" drifted from what was drawn.
pub fn root_chrome_layout(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(12),
            Constraint::Length(PLAYER_HEIGHT),
            Constraint::Length(STATUS_HEIGHT),
        ])
        .split(area)
}

/// Carve a 1-row breathing space off the top of a pane's content
/// area so the first list item never butts directly against the
/// pane's title-bearing top border. No-op for panes shorter than 2
/// rows.
fn pad_pane_top(area: Rect) -> Rect {
    if area.height < 2 {
        return area;
    }
    Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height - 1,
    }
}

pub fn render(frame: &mut Frame<'_>, app: &mut App) {
    // Publish the album-adaptive palette for this frame; `accent()` /
    // `soft_accent()` / `accent_foreground()` and the style helpers all
    // read it, so every accent surface follows the cover art together.
    crate::widgets::style::set_active_palette(app.palette);
    // Fresh click-target registry for this frame; renderers re-register
    // exactly what they draw.
    app.hit_map.borrow_mut().clear();
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(BG)), area);

    let root = root_chrome_layout(area);

    render_body(frame, app, root[0]);
    render_now_playing(frame, app, root[1]);
    render_status(frame, app, root[2]);
    if app.command_palette.visible {
        render_command_palette(frame, area, app);
    }
    if app.playlist_picker.is_some() {
        render_playlist_picker(frame, area, app);
    }
    if app.device_picker.is_some() {
        render_device_picker(frame, area, app);
    }
    if app.audio_output_picker.is_some() {
        render_audio_output_picker(frame, area, app);
    }
    if app.reminder_picker.is_some() {
        render_reminder_picker(frame, area, app);
    }
    if app.artist_view.is_some() {
        render_artist_view(frame, area, app);
    }
    if app.fullscreen_panel.is_some() {
        render_fullscreen_panel(frame, area, app);
    }
    if app.show_help {
        render_help(frame, area, app);
    }
    if app.error.is_some() {
        render_error_modal(frame, area, app);
    }
    // Auth re-login modal sits between error and confirm — it's a
    // blocker (you can't usefully play anything without auth) but it
    // shouldn't paint *over* an error modal that the user hasn't
    // dismissed yet.
    if app.login_modal.is_some() {
        render_login_modal(frame, area, app);
    }
    // Phase 13 (P13-L) — destructive-action confirmation popup. Drawn
    // after every other overlay so it's always on top.
    if app.confirm_modal.is_some() {
        render_confirm_modal(frame, area, app);
    }
}

fn render_artist_view(frame: &mut Frame<'_>, area: Rect, app: &App) {
    use crate::app::{ArtistViewSide, ARTIST_ALBUM_GROUPS};
    use crate::widgets::style::{card_block, focused_card_block};
    let Some(view) = app.artist_view.as_ref() else {
        return;
    };
    let modal_area = centered_rect(88, 80, area);
    frame.render_widget(Clear, modal_area);
    let follow_hint = if view.is_followed == Some(true) {
        "Following ✓ · F unfollow"
    } else {
        "F follow"
    };
    let outer = focused_card_block(&format!(
        "Artist · {}  ·  {}  ·  Tab swap pane  ·  Enter play  ·  L library/all  ·  Esc close",
        view.artist_name, follow_hint
    ));
    let inner = outer.inner(modal_area);
    frame.render_widget(outer, modal_area);

    // Drop a 1-row gap below the modal's title bar so the Albums /
    // Tracks pane titles don't share a line with the outer title.
    let body = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner)[1];

    // Two columns side-by-side with a 1-col gap between them so Albums
    // and Tracks panes don't share borders / feel mooshed together.
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(body);
    let albums_col = columns[0];
    let tracks_col = columns[2];

    // ===== Albums (left) =====
    let albums_focused = view.focus == ArtistViewSide::Albums;
    let albums_title = if view.library_only {
        format!("Albums  Library · {}", view.in_library_count())
    } else {
        format!(
            "Albums  All · {}  ({} in library)",
            view.albums.len(),
            view.in_library_count()
        )
    };
    let albums_block = if albums_focused {
        focused_card_block(&albums_title)
    } else {
        card_block(&albums_title)
    };
    let albums_inner = pad_pane_top(albums_block.inner(albums_col));
    frame.render_widget(albums_block, albums_col);
    if view.loading_albums {
        let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
            [(app.last_progress_tick.elapsed().as_millis() / 80 % 10) as usize];
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" {spinner} "),
                    Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                ),
                Span::styled("Loading albums…", Style::default().fg(TEXT)),
            ]))
            .style(Style::default().bg(SURFACE)),
            albums_inner,
        );
    } else if view.visible_albums().is_empty() {
        let message = if view.library_only {
            "No saved albums for this artist. Press L to see all releases."
        } else {
            "No albums released by this artist."
        };
        frame.render_widget(
            Paragraph::new(Span::styled(message, Style::default().fg(TEXT_MUTED)))
                .style(Style::default().bg(SURFACE)),
            albums_inner,
        );
    } else {
        let visible = view.visible_albums();
        let selected = view.album_selected.min(visible.len() - 1);
        // Flatten into section header + album rows; remember which flat row
        // the selected album maps to so the highlight lands on it (headers
        // are rendered but never highlighted).
        let mut rows: Vec<ListItem<'_>> =
            Vec::with_capacity(visible.len() + ARTIST_ALBUM_GROUPS.len());
        let mut selected_row = 0usize;
        let mut current_group: Option<&str> = None;
        let mut started = false;
        for (idx, album) in visible.iter().enumerate() {
            let group = album.album_group.as_deref();
            if !started || group != current_group {
                let label = ARTIST_ALBUM_GROUPS
                    .iter()
                    .find(|(key, _)| Some(*key) == group)
                    .map_or("Other", |(_, label)| *label);
                rows.push(ListItem::new(Line::from(Span::styled(
                    label.to_string(),
                    Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                ))));
                current_group = group;
                started = true;
            }
            if idx == selected {
                selected_row = rows.len();
            }
            let heart = if album.in_library == Some(true) {
                Span::styled("♥ ", Style::default().fg(accent()))
            } else {
                Span::raw("")
            };
            rows.push(ListItem::new(vec![
                Line::from(vec![
                    Span::styled("💿  ", Style::default().fg(accent())),
                    heart,
                    Span::styled(
                        album.name.clone(),
                        Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(vec![
                    Span::raw("    "),
                    Span::styled(context_suffix(album), Style::default().fg(TEXT_MUTED)),
                ]),
            ]));
        }
        let list = List::new(rows)
            .highlight_style(
                Style::default()
                    .fg(accent_foreground())
                    .bg(accent())
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▌")
            .style(Style::default().bg(SURFACE));
        let mut state = ListState::default();
        state.select(Some(selected_row));
        frame.render_stateful_widget(list, albums_inner, &mut state);
    }

    // ===== Tracks (right) =====
    let tracks_focused = view.focus == ArtistViewSide::Tracks;
    let visible_albums = view.visible_albums();
    let focused_album_name = visible_albums
        .get(view.album_selected)
        .map_or("—", |a| a.name.as_str());
    let tracks_title = format!(
        "Tracks  {}  ·  {}",
        view.album_tracks.len(),
        focused_album_name
    );
    let tracks_block = if tracks_focused {
        focused_card_block(&tracks_title)
    } else {
        card_block(&tracks_title)
    };
    let tracks_inner = pad_pane_top(tracks_block.inner(tracks_col));
    frame.render_widget(tracks_block, tracks_col);
    if view.loading_tracks {
        let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
            [(app.last_progress_tick.elapsed().as_millis() / 80 % 10) as usize];
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" {spinner} "),
                    Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                ),
                Span::styled("Loading tracks…", Style::default().fg(TEXT)),
            ]))
            .style(Style::default().bg(SURFACE)),
            tracks_inner,
        );
    } else if view.album_tracks.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "No tracks for this album.",
                Style::default().fg(TEXT_MUTED),
            ))
            .style(Style::default().bg(SURFACE)),
            tracks_inner,
        );
    } else {
        let rows: Vec<ListItem<'_>> = view
            .album_tracks
            .iter()
            .enumerate()
            .map(|(idx, t)| {
                let duration = if t.duration_ms > 0 {
                    fmt_ms(t.duration_ms)
                } else {
                    String::new()
                };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" {:>2}. ", idx + 1),
                        Style::default().fg(TEXT_MUTED),
                    ),
                    Span::styled(
                        t.name.clone(),
                        Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("  {duration}"), Style::default().fg(TEXT_MUTED)),
                ]))
            })
            .collect();
        let list = List::new(rows)
            .highlight_style(
                Style::default()
                    .fg(accent_foreground())
                    .bg(accent())
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▌")
            .style(Style::default().bg(SURFACE));
        let mut state = ListState::default();
        state.select(if view.album_tracks.is_empty() {
            None
        } else {
            Some(view.track_selected.min(view.album_tracks.len() - 1))
        });
        frame.render_stateful_widget(list, tracks_inner, &mut state);
    }

    if let Some(err) = &view.error {
        let err_area = Rect {
            x: inner.x,
            y: inner.y + inner.height.saturating_sub(2),
            width: inner.width,
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                err.clone(),
                Style::default().fg(DANGER).add_modifier(Modifier::BOLD),
            )))
            .style(Style::default().bg(SURFACE)),
            err_area,
        );
    }
}

/// Render the interactive re-authentication modal. Visual treatment
/// uses the adaptive accent border (action / opportunity) rather than DANGER
/// of `render_confirm_modal` (danger) — re-login is recovery, not
/// destruction.
fn render_login_modal(frame: &mut Frame<'_>, area: Rect, app: &App) {
    use crate::app::LoginPhase;
    use crate::widgets::style::{button_chip, ButtonRole};
    let Some(modal) = app.login_modal.as_ref() else {
        return;
    };
    let area = centered_rect(60, 30, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(accent()).add_modifier(Modifier::BOLD))
        .title(Span::styled(
            " 🔒  Spotify re-authentication ",
            Style::default()
                .fg(accent_foreground())
                .bg(accent())
                .add_modifier(Modifier::BOLD),
        ));

    let (body, footer): (Vec<Line<'_>>, Line<'_>) = match &modal.phase {
        LoginPhase::AwaitingConfirm => (
            vec![
                Line::from(""),
                Line::from(Span::styled(
                    "Your Spotify session has expired.",
                    Style::default().fg(TEXT),
                )),
                Line::from(Span::styled(
                    "Press Enter to open your browser and re-authenticate.",
                    Style::default().fg(TEXT_MUTED),
                )),
                Line::from(""),
            ],
            Line::from(vec![
                Span::raw("  "),
                button_chip("Enter · re-auth", ButtonRole::Affirm),
                Span::raw("   "),
                button_chip("Esc · dismiss", ButtonRole::Cancel),
            ]),
        ),
        LoginPhase::InProgress => {
            use spotuify_spotify::auth::LoginProgress;
            let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
                [(app.last_progress_tick.elapsed().as_millis() / 80 % 10) as usize];
            // Render the latest progress event inside the modal —
            // the auth code path emits these events into the
            // LoginModal instead of `println!`, so the alt-screen
            // buffer stays clean even when the browser fails to
            // launch and the URL needs to be visible to the user.
            let body_lines: Vec<Line<'_>> = match &modal.last_progress {
                Some(LoginProgress::OpeningBrowser { .. }) | None => vec![
                    Line::from(""),
                    Line::from(vec![
                        Span::styled(
                            format!(" {spinner} "),
                            Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            "Opening browser — complete login there.",
                            Style::default().fg(TEXT),
                        ),
                    ]),
                    Line::from(Span::styled(
                        "This window will update automatically.",
                        Style::default().fg(TEXT_MUTED),
                    )),
                    Line::from(""),
                ],
                Some(LoginProgress::BrowserLaunchFailed {
                    auth_url, error, ..
                }) => vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        format!("Couldn't open the browser automatically ({error})."),
                        Style::default().fg(WARN).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(Span::styled(
                        "Open this URL in any browser to continue:",
                        Style::default().fg(TEXT),
                    )),
                    Line::from(Span::styled(
                        auth_url.clone(),
                        Style::default().fg(accent()),
                    )),
                    Line::from(""),
                ],
                Some(LoginProgress::WaitingForCallback) => vec![
                    Line::from(""),
                    Line::from(vec![
                        Span::styled(
                            format!(" {spinner} "),
                            Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled("Waiting for the OAuth callback…", Style::default().fg(TEXT)),
                    ]),
                    Line::from(Span::styled(
                        "Finish the sign-in in your browser; this window will close itself.",
                        Style::default().fg(TEXT_MUTED),
                    )),
                    Line::from(""),
                ],
                Some(LoginProgress::Saved) => vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        "✓  Spotify auth saved.",
                        Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                ],
            };
            (
                body_lines,
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        "Esc dismiss (browser stays open)",
                        Style::default().fg(TEXT_MUTED),
                    ),
                ]),
            )
        }
        LoginPhase::Failed(message) => (
            vec![
                Line::from(""),
                Line::from(Span::styled(
                    "Login failed:",
                    Style::default().fg(DANGER).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(message.clone(), Style::default().fg(TEXT))),
                Line::from(""),
            ],
            Line::from(vec![
                Span::raw("  "),
                button_chip("Enter · retry", ButtonRole::Affirm),
                Span::raw("   "),
                button_chip("Esc · dismiss", ButtonRole::Cancel),
            ]),
        ),
    };

    let mut lines = body;
    lines.push(footer);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .style(Style::default().bg(SURFACE)),
        area,
    );
}

/// Relative time label like "in 2h" / "3d ago" for a reminder/notification.
fn fmt_reminder_when(ms: i64) -> String {
    let now = chrono::Local::now().timestamp_millis();
    let delta = ms - now;
    let mins = delta.abs() / 60_000;
    let label = if mins < 1 {
        "now".to_string()
    } else if mins < 60 {
        format!("{mins}m")
    } else if mins < 1440 {
        format!("{}h", mins / 60)
    } else {
        format!("{}d", mins / 1440)
    };
    if label == "now" {
        label
    } else if delta >= 0 {
        format!("in {label}")
    } else {
        format!("{label} ago")
    }
}

fn notification_state_span(state: spotuify_core::NotificationState) -> Span<'static> {
    use spotuify_core::NotificationState as S;
    let (text, color) = match state {
        S::Unseen => ("● new", accent()),
        S::Seen => ("seen", TEXT_MUTED),
        S::Snoozed => ("snoozed", DANGER),
        S::Dismissed => ("dismissed", TEXT_MUTED),
        S::Done => ("done", TEXT_MUTED),
    };
    Span::styled(text, Style::default().fg(color))
}

/// Notifications screen: Inbox (fired reminders) above, Scheduled reminders
/// below. The combined selection (`app.selected`) indexes inbox first, then
/// scheduled, matching `App::active_len(Notifications)`.
fn render_notifications(frame: &mut Frame<'_>, app: &App, area: Rect) {
    use crate::widgets::style::card_block;
    let highlight = Style::default()
        .fg(accent_foreground())
        .bg(accent())
        .add_modifier(Modifier::BOLD);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);

    let n_count = app.notifications.len();
    let unseen = app
        .notifications
        .iter()
        .filter(|n| matches!(n.state, spotuify_core::NotificationState::Unseen))
        .count();
    let inbox_block = card_block(&format!(
        "Inbox · {unseen} new  ·  Enter play · s snooze · d dismiss"
    ));
    let inbox_inner = inbox_block.inner(rows[0]);
    frame.render_widget(inbox_block, rows[0]);
    if app.notifications.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  No reminders have fired yet.",
                Style::default().fg(TEXT_MUTED),
            )))
            .style(Style::default().bg(SURFACE)),
            inbox_inner,
        );
    } else {
        let items: Vec<ListItem<'_>> = app
            .notifications
            .iter()
            .map(|n| {
                ListItem::new(vec![
                    Line::from(vec![
                        Span::styled(
                            n.name.clone(),
                            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                        ),
                        Span::raw("  "),
                        notification_state_span(n.state),
                    ]),
                    Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            n.message.clone().unwrap_or_else(|| n.subtitle.clone()),
                            Style::default().fg(TEXT_MUTED),
                        ),
                        Span::styled("  ·  ", Style::default().fg(TEXT_MUTED)),
                        Span::styled(
                            fmt_reminder_when(n.due_at_ms),
                            Style::default().fg(TEXT_MUTED),
                        ),
                    ]),
                ])
            })
            .collect();
        let mut state = ListState::default();
        if app.selected < n_count {
            state.select(Some(app.selected));
        }
        frame.render_stateful_widget(
            List::new(items)
                .highlight_style(highlight)
                .highlight_symbol("▌")
                .style(Style::default().bg(SURFACE)),
            inbox_inner,
            &mut state,
        );
    }

    let sched_block = card_block(&format!(
        "Scheduled · {}  ·  Enter play · x cancel",
        app.reminders.len()
    ));
    let sched_inner = sched_block.inner(rows[1]);
    frame.render_widget(sched_block, rows[1]);
    if app.reminders.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  Nothing scheduled. Press R on a track/album/playlist to add one.",
                Style::default().fg(TEXT_MUTED),
            )))
            .style(Style::default().bg(SURFACE)),
            sched_inner,
        );
    } else {
        let items: Vec<ListItem<'_>> = app
            .reminders
            .iter()
            .map(|r| {
                let mut meta = vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("next {}", fmt_reminder_when(r.next_due_at_ms)),
                        Style::default().fg(TEXT_MUTED),
                    ),
                ];
                if r.recurrence.is_recurring() {
                    meta.push(Span::styled(
                        format!("  ·  {}", r.recurrence.label()),
                        Style::default().fg(accent()),
                    ));
                }
                ListItem::new(vec![
                    Line::from(Span::styled(
                        r.name.clone(),
                        Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(meta),
                ])
            })
            .collect();
        let mut state = ListState::default();
        if app.selected >= n_count {
            state.select(Some(app.selected - n_count));
        }
        frame.render_stateful_widget(
            List::new(items)
                .highlight_style(highlight)
                .highlight_symbol("▌")
                .style(Style::default().bg(SURFACE)),
            sched_inner,
            &mut state,
        );
    }
}

fn render_reminder_picker(frame: &mut Frame<'_>, area: Rect, app: &App) {
    use crate::app::REMINDER_PRESETS;
    use crate::widgets::style::{button_chip, focused_card_block, ButtonRole};
    let Some(picker) = app.reminder_picker.as_ref() else {
        return;
    };
    let area = centered_rect(64, 62, area);
    let block = focused_card_block(&format!("Remind me  ·  {}", picker.label));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let body = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

    let custom_idx = REMINDER_PRESETS.len() - 1;
    let items: Vec<ListItem<'_>> = REMINDER_PRESETS
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let text = if i == custom_idx {
                format!("{label}  [{}]", picker.custom)
            } else {
                (*label).to_string()
            };
            ListItem::new(Line::from(Span::styled(text, Style::default().fg(TEXT))))
        })
        .collect();
    let mut state = ListState::default();
    state.select(Some(picker.preset.min(REMINDER_PRESETS.len() - 1)));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_style(
                Style::default()
                    .fg(accent_foreground())
                    .bg(accent())
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▌"),
        body[0],
        &mut state,
    );

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Repeat: ", Style::default().fg(TEXT_MUTED)),
            Span::styled(
                picker.recurrence.label(),
                Style::default().fg(accent()).add_modifier(Modifier::BOLD),
            ),
            Span::styled("   (Tab to cycle)", Style::default().fg(TEXT_MUTED)),
        ]))
        .style(Style::default().bg(SURFACE)),
        body[1],
    );

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            button_chip("Enter set", ButtonRole::Affirm),
            Span::raw("  "),
            button_chip("Tab repeat", ButtonRole::Cancel),
            Span::raw("  "),
            Span::styled("Esc cancel", Style::default().fg(TEXT_MUTED)),
        ]))
        .style(Style::default().bg(SURFACE)),
        body[2],
    );
}

fn render_confirm_modal(frame: &mut Frame<'_>, area: Rect, app: &App) {
    use crate::widgets::style::{button_chip, ButtonRole};
    let Some(modal) = app.confirm_modal.as_ref() else {
        return;
    };
    let area = centered_rect(60, 30, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(DANGER).add_modifier(Modifier::BOLD))
        .title(Span::styled(
            format!(" ⚠  {} ", modal.title),
            Style::default()
                .fg(BG)
                .bg(DANGER)
                .add_modifier(Modifier::BOLD),
        ));
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(modal.body.clone(), Style::default().fg(TEXT))),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            button_chip("y · yes", ButtonRole::Danger),
            Span::raw("   "),
            button_chip("n · no", ButtonRole::Cancel),
            Span::raw("   "),
            Span::styled("Esc cancel", Style::default().fg(TEXT_MUTED)),
        ]),
    ];
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .style(Style::default().bg(SURFACE)),
        area,
    );
}

fn render_playlist_picker(frame: &mut Frame<'_>, area: Rect, app: &App) {
    use crate::widgets::style::{button_chip, focused_card_block, ButtonRole};
    let Some(picker) = app.playlist_picker.as_ref() else {
        return;
    };
    let area = centered_rect(72, 60, area);
    let playlists = app.filtered_playlists();
    let block = focused_card_block(&format!(
        "Add to playlist  ·  {} item(s)",
        picker.uris.len()
    ));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let body_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let rows: Vec<ListItem<'_>> = if playlists.is_empty() {
        let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
            [(app.last_progress_tick.elapsed().as_millis() / 80 % 10) as usize];
        vec![
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {spinner} "),
                    Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                ),
                Span::styled("Loading playlists…", Style::default().fg(TEXT)),
            ])),
            ListItem::new(Line::from(Span::styled(
                "    Auto-syncs on first auth. Esc cancels.",
                Style::default().fg(TEXT_MUTED),
            ))),
        ]
    } else {
        playlists
            .iter()
            .map(|playlist| {
                let checked = picker.selected_playlist_ids.contains(&playlist.id);
                let bullet = if checked {
                    Span::styled(
                        "●",
                        Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::styled("○", Style::default().fg(TEXT_MUTED))
                };
                ListItem::new(vec![
                    Line::from(vec![
                        bullet,
                        Span::raw("  "),
                        Span::styled(
                            playlist.name.clone(),
                            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    Line::from(vec![
                        Span::raw("    "),
                        Span::styled(
                            format!("{} tracks · by {}", playlist.tracks_total, playlist.owner),
                            Style::default().fg(TEXT_MUTED),
                        ),
                    ]),
                ])
            })
            .collect()
    };
    let mut state = ListState::default();
    state.select((!playlists.is_empty()).then(|| picker.selected.min(playlists.len() - 1)));
    frame.render_stateful_widget(
        List::new(rows)
            .highlight_style(
                Style::default()
                    .fg(accent_foreground())
                    .bg(accent())
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▌"),
        body_rows[0],
        &mut state,
    );

    // Footer with button chips.
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            button_chip("Space toggle", ButtonRole::Cancel),
            Span::raw("  "),
            button_chip("Enter add", ButtonRole::Affirm),
            Span::raw("  "),
            Span::styled("Esc cancel", Style::default().fg(TEXT_MUTED)),
        ]))
        .style(Style::default().bg(SURFACE)),
        body_rows[1],
    );
}

fn render_audio_output_picker(frame: &mut Frame<'_>, area: Rect, app: &App) {
    use crate::widgets::style::focused_card_block;
    let Some(picker) = app.audio_output_picker.as_ref() else {
        return;
    };
    let area = centered_rect(60, 50, area);
    let block = focused_card_block(&format!(
        "Audio output  ·  {} device(s)",
        picker.outputs.len()
    ));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let body_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let rows: Vec<ListItem<'_>> = picker
        .outputs
        .iter()
        .map(|name| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    " 🔊  ",
                    Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                ),
                Span::styled(name.clone(), Style::default().fg(TEXT)),
            ]))
        })
        .collect();
    let list = List::new(rows)
        .highlight_style(
            Style::default()
                .fg(TEXT)
                .bg(app.palette.soft_accent)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▌")
        .style(Style::default().bg(SURFACE));
    let mut state = ListState::default();
    state.select(Some(
        picker.selected.min(picker.outputs.len().saturating_sub(1)),
    ));
    frame.render_stateful_widget(list, body_rows[0], &mut state);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "↑/↓ select · Enter apply (restarts player) · Esc cancel",
            Style::default().fg(TEXT_MUTED),
        )))
        .style(Style::default().bg(SURFACE)),
        body_rows[1],
    );
}

fn render_device_picker(frame: &mut Frame<'_>, area: Rect, app: &App) {
    use crate::widgets::style::{button_chip, focused_card_block, ButtonRole};
    let Some(picker) = app.device_picker.as_ref() else {
        return;
    };
    let area = centered_rect(60, 50, area);
    let devices = app.filtered_devices();
    let block = focused_card_block(&format!("Devices  ·  {} available", devices.len()));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let body_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let rows: Vec<ListItem<'_>> = if devices.is_empty() {
        let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
            [(app.last_progress_tick.elapsed().as_millis() / 80 % 10) as usize];
        vec![
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {spinner} "),
                    Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                ),
                Span::styled("Loading devices…", Style::default().fg(TEXT)),
            ])),
            ListItem::new(Line::from(Span::styled(
                "    Open Spotify on a phone/laptop/speaker to make it visible.",
                Style::default().fg(TEXT_MUTED),
            ))),
        ]
    } else {
        devices
            .iter()
            .map(|device| {
                let icon = device_kind_icon(&device.kind);
                let mut header: Vec<Span<'_>> = vec![
                    Span::styled(
                        format!(" {icon}  "),
                        Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        device.name.clone(),
                        Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                    ),
                ];
                if device.is_active {
                    header.push(Span::raw("  "));
                    header.push(Span::styled(
                        "● active",
                        Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                    ));
                }
                if device.is_restricted {
                    header.push(Span::raw("  "));
                    header.push(Span::styled(
                        "restricted",
                        Style::default().fg(DANGER).add_modifier(Modifier::BOLD),
                    ));
                }
                let volume = if device.supports_volume {
                    format!("vol {}%", device.volume_percent.unwrap_or(0))
                } else {
                    "vol fixed".to_string()
                };
                let detail = Line::from(vec![
                    Span::raw("      "),
                    Span::styled(device.kind.clone(), Style::default().fg(TEXT_MUTED)),
                    Span::styled("  ·  ", Style::default().fg(TEXT_MUTED)),
                    Span::styled(volume, Style::default().fg(TEXT_MUTED)),
                ]);
                ListItem::new(vec![Line::from(header), detail])
            })
            .collect()
    };

    let mut state = ListState::default();
    state.select((!devices.is_empty()).then(|| picker.selected.min(devices.len() - 1)));
    frame.render_stateful_widget(
        List::new(rows)
            .highlight_style(
                Style::default()
                    .fg(accent_foreground())
                    .bg(accent())
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▌"),
        body_rows[0],
        &mut state,
    );

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            button_chip("Enter transfer", ButtonRole::Affirm),
            Span::raw("  "),
            Span::styled("j/k move", Style::default().fg(TEXT_MUTED)),
            Span::raw("  "),
            Span::styled("Esc cancel", Style::default().fg(TEXT_MUTED)),
        ]))
        .style(Style::default().bg(SURFACE)),
        body_rows[1],
    );
}

fn render_fullscreen_panel(frame: &mut Frame<'_>, area: Rect, app: &mut App) {
    let Some(panel) = app.fullscreen_panel else {
        return;
    };
    let area = area.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    frame.render_widget(Clear, area);
    match panel {
        FullscreenPanel::Queue => render_queue_fullscreen(frame, app, area),
        FullscreenPanel::Lyrics => render_lyrics(frame, app, area),
    }
}

fn render_queue_fullscreen(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    use crate::widgets::style::card_block;
    use tui_big_text::{BigText, PixelSize};

    // Focus / fullscreen mode = a hero header (giant art on the
    // left, big-text title + gauge in the middle) plus the queue
    // filling the rest of the screen. This is the "show me what's
    // playing, big" view the user gets with F.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(12), Constraint::Min(1)])
        .split(area);

    // Hero block.
    let hero_block = card_block("Queue Fullscreen  ·  F/Esc close");
    let hero_inner = hero_block.inner(rows[0]);
    frame.render_widget(hero_block, rows[0]);
    let hero_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(20), Constraint::Min(8)])
        .split(hero_inner);

    // Phase 6 — derive the canonical view ONCE so the hero title and
    // gauge are guaranteed to refer to the same track. Pre-Phase-6 this
    // block read title from `queue.currently_playing` while pulling
    // progress from `playback.progress_ms`, producing
    // "Title A / Progress against duration of Track B" mismatches when
    // the queue snapshot was a poll fresher than playback (or vice
    // versa). The view ties them together and surfaces `uri_mismatch`
    // so the rail can show a dim "(queue ahead)" hint elsewhere.
    let view = NowPlayingView::derive(&app.playback, &app.queue, &app.devices);
    if let Some(item) = view.item {
        let initial = item
            .name
            .chars()
            .next()
            .map_or_else(|| "♪".to_string(), |c| c.to_ascii_uppercase().to_string());
        let cover_seed = item.uri.clone();
        let cover_matches =
            view.active_uri.is_some() && view.art_url == app.current_art_url.as_deref();
        // Right side: big-text title + artist + gauge.
        let right_rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(4),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(1),
            ])
            .split(hero_cols[1]);
        // Bigger big-text (Full size) for focus mode.
        let title_chars_fit = (right_rows[1].width as usize / 4).max(8);
        let title_truncated = if item.name.chars().count() > title_chars_fit {
            let mut s: String = item
                .name
                .chars()
                .take(title_chars_fit.saturating_sub(1))
                .collect();
            s.push('…');
            s
        } else {
            item.name.clone()
        };
        let title = BigText::builder()
            .pixel_size(PixelSize::Full)
            .style(Style::default().fg(TEXT).add_modifier(Modifier::BOLD))
            .lines(vec![Line::from(title_truncated)])
            .build();
        frame.render_widget(title, right_rows[1]);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    kind_icon(&item.kind),
                    Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(item.subtitle.clone(), Style::default().fg(TEXT)),
                Span::styled(context_suffix(item), Style::default().fg(TEXT_MUTED)),
            ]))
            .style(Style::default().bg(SURFACE)),
            right_rows[2],
        );
        let progress = progress_ratio(view.progress_ms, view.duration_ms);
        frame.render_widget(
            Gauge::default()
                .gauge_style(Style::default().fg(progress_filled()).bg(PROGRESS_UNFILLED))
                .ratio(progress)
                .label(format!(
                    "{} / {}",
                    fmt_ms(view.progress_ms),
                    fmt_ms(view.duration_ms)
                ))
                .style(Style::default().bg(SURFACE)),
            right_rows[4],
        );
        render_current_cover_or_gradient(
            frame,
            app,
            cover_matches,
            hero_cols[0],
            &cover_seed,
            initial,
        );
    } else {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "Queue is unavailable until playback is active.",
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    "Press / to search and Enter to start playback.",
                    Style::default().fg(accent()),
                )),
            ])
            .style(Style::default().bg(SURFACE)),
            hero_cols[1],
        );
    }

    // Queue list below.
    let queue_items: &[MediaItem] = if app.queue.session_active {
        &app.queue.items
    } else {
        &[]
    };
    if !app.queue.session_active {
        let block = panel_block(" Up Next ");
        let inner = block.inner(rows[1]);
        frame.render_widget(block, rows[1]);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "No active Spotify session.",
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    "Start playback from Search or Library to load a live queue.",
                    Style::default().fg(accent()),
                )),
            ])
            .style(Style::default().bg(SURFACE)),
            inner,
        );
        return;
    }
    render_media_list(
        frame,
        area_title(" Up Next ", queue_items.len()),
        queue_items,
        usize::MAX,
        app,
        rows[1],
        false,
    );
}

fn render_now_playing(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let block = Block::default()
        .title(Line::from(vec![Span::styled(
            " spotuify ",
            Style::default()
                .fg(app.palette.foreground)
                .bg(app.palette.brand)
                .add_modifier(Modifier::BOLD),
        )]))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.palette.now_playing_rail))
        .style(Style::default().bg(app.palette.background));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let layout = now_playing_layout(inner);
    if let Some(cover) = layout.cover {
        render_cover(frame, app, cover);
    }
    if let Some(track) = layout.track {
        render_track(frame, app, track);
    }
    render_transport(frame, app, layout.transport, layout.compact_transport);
}

pub(crate) const TRANSPORT_FULL_WIDTH: u16 = 40;
pub(crate) const TRANSPORT_COMPACT_WIDTH: u16 = 26;
const COVER_WIDTH: u16 = 24;
const TRACK_MIN_WIDTH: u16 = 30;

/// Regions of the bottom now-playing bar, computed from its inner rect.
pub(crate) struct NowPlayingLayout {
    pub cover: Option<Rect>,
    pub track: Option<Rect>,
    pub transport: Rect,
    pub compact_transport: bool,
}

/// Width-aware layout for the bottom now-playing bar. Shared with the
/// mouse hit-testing in `app.rs` so click zones always match what's
/// drawn. Collapse order as the terminal narrows: cover art goes first,
/// then the transport switches to its compact form, then the track
/// panel is dropped so the controls keep the full row.
pub(crate) fn now_playing_layout(inner: Rect) -> NowPlayingLayout {
    let width = inner.width;
    if width >= COVER_WIDTH + TRACK_MIN_WIDTH + TRANSPORT_FULL_WIDTH {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(COVER_WIDTH),
                Constraint::Min(TRACK_MIN_WIDTH),
                Constraint::Length(TRANSPORT_FULL_WIDTH),
            ])
            .split(inner);
        return NowPlayingLayout {
            cover: Some(chunks[0]),
            track: Some(chunks[1]),
            transport: chunks[2],
            compact_transport: false,
        };
    }
    if width >= 24 + TRANSPORT_FULL_WIDTH {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(24),
                Constraint::Length(TRANSPORT_FULL_WIDTH),
            ])
            .split(inner);
        return NowPlayingLayout {
            cover: None,
            track: Some(chunks[0]),
            transport: chunks[1],
            compact_transport: false,
        };
    }
    if width >= 14 + TRANSPORT_COMPACT_WIDTH {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(14),
                Constraint::Length(TRANSPORT_COMPACT_WIDTH),
            ])
            .split(inner);
        return NowPlayingLayout {
            cover: None,
            track: Some(chunks[0]),
            transport: chunks[1],
            compact_transport: true,
        };
    }
    NowPlayingLayout {
        cover: None,
        track: None,
        transport: inner,
        compact_transport: true,
    }
}

fn render_cover(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let area = area.inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    // Reserve the top row as a gap so the " spotuify " chip on the block
    // border doesn't visually merge with the cover art below it.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    let area = cover_art_rect(rows[1]);
    let view = NowPlayingView::derive(&app.playback, &app.queue, &app.devices);
    let (seed, label) = if let (Some(uri), Some(item)) = (view.active_uri, view.item) {
        let first_char = item
            .name
            .chars()
            .next()
            .map_or_else(|| "♪".to_string(), |c| c.to_ascii_uppercase().to_string());
        (uri.to_string(), first_char)
    } else {
        ("spotuify:empty-state".to_string(), "♪".to_string())
    };
    let cover_matches = view.active_uri.is_some() && view.art_url == app.current_art_url.as_deref();
    render_current_cover_or_gradient(frame, app, cover_matches, area, &seed, label);
}

fn render_current_cover_or_gradient(
    frame: &mut Frame<'_>,
    app: &mut App,
    cover_matches: bool,
    area: Rect,
    fallback_seed: &str,
    fallback_label: String,
) {
    if cover_matches {
        if let Some(cover) = app.cover.as_mut() {
            let image = StatefulImage::default();
            frame.render_stateful_widget(image, area, cover);
            if let Some(Err(err)) = cover.last_encoding_result() {
                app.error = Some(err.to_string());
            }
            return;
        }
    }

    let art = crate::widgets::album_art::GradientArt::new(fallback_seed).with_label(fallback_label);
    frame.render_widget(art, area);
}

fn cover_art_rect(area: Rect) -> Rect {
    if area.width == 0 || area.height == 0 {
        return area;
    }
    let width = area.width.min(area.height.saturating_mul(2).max(1));
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y,
        width,
        height: area.height,
    }
}

fn render_track(frame: &mut Frame<'_>, app: &App, area: Rect) {
    // Phase 6 — derive once. All sub-fields (title, state label, progress,
    // device, volume) read from the same canonical view so they can never
    // disagree within a single frame.
    let view = NowPlayingView::derive(&app.playback, &app.queue, &app.devices);
    let Some(item) = view.item else {
        let empty = if app.playback_known {
            let title = if app.queue.session_active {
                "Ready when you are"
            } else {
                "No active Spotify session"
            };
            let hint = if app.queue.session_active {
                app.spotifyd_status
                    .as_deref()
                    .unwrap_or("Search for music or open a playlist, then press Enter to play.")
            } else {
                "Start playback from Search, Library, or Playlists."
            };
            Paragraph::new(vec![
                Line::from(Span::styled(
                    title,
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(hint, Style::default().fg(accent()))),
                Line::from(Span::styled(
                    "Search, queue, playlists, and podcasts are available from the tabs below.",
                    Style::default().fg(TEXT_MUTED),
                )),
            ])
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(SURFACE))
        } else {
            // Daemon hasn't told us what's currently playing yet. Show
            // a transient loading state so the user knows we're working,
            // not that nothing is playing.
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "Connecting…",
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    "Fetching current playback from Spotify.",
                    Style::default().fg(TEXT_MUTED),
                )),
            ])
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(SURFACE))
        };
        frame.render_widget(empty, area);
        return;
    };

    let state = match view.state {
        PlaybackDisplayState::Playing => "playing",
        PlaybackDisplayState::Paused => "paused",
        // Caller already returned for the Empty branch above.
        PlaybackDisplayState::Empty => "paused",
    };
    let progress_ms = view.progress_ms;
    let progress = progress_ratio(progress_ms, view.duration_ms);
    // 8 rows usable inside the player chrome (PLAYER_HEIGHT=10 minus
    // borders). Lay out the track area as a tight stack — title,
    // subtitle, a one-row seek gauge, and a single pad row at the
    // bottom so nothing hugs the bottom border.
    //   0   spacer
    //   1   title (bold)
    //   2   kind glyph + artist subtitle
    //   3+  spacer
    //   -2  state · device  +  gauge (single row, mm:ss label)
    //   -1  bottom pad
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

    let title_width = rows[1].width.saturating_sub(2) as usize;
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            truncate(&item.name, title_width),
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        )]))
        .style(Style::default().bg(SURFACE)),
        rows[1],
    );

    // Subtitle row: kind icon + artist (subtitle) muted text.
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                kind_icon(&item.kind),
                Style::default().fg(accent()).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                truncate(&item.subtitle, rows[2].width.saturating_sub(3) as usize),
                Style::default().fg(TEXT_MUTED),
            ),
        ]))
        .style(Style::default().bg(SURFACE)),
        rows[2],
    );

    // State + device on the left, gauge filling the rest of the row.
    // Geometry comes from `track_gauge_rect` — shared with the seek
    // click hit-test in app.rs so clicks land on the drawn gauge.
    let gauge_rect = track_gauge_rect(area);
    let label_rect = Rect::new(
        rows[4].x,
        rows[4].y,
        gauge_rect.x.saturating_sub(rows[4].x),
        1,
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(state, Style::default().fg(accent())),
            Span::styled(" on ", Style::default().fg(TEXT_MUTED)),
            Span::styled(truncate(&device_name(app), 20), Style::default().fg(TEXT)),
        ]))
        .style(Style::default().bg(SURFACE)),
        label_rect,
    );
    frame.render_widget(
        Gauge::default()
            .gauge_style(Style::default().fg(progress_filled()).bg(PROGRESS_UNFILLED))
            .ratio(progress)
            .label(format!(
                "{} / {}",
                fmt_ms(progress_ms),
                fmt_ms(view.duration_ms)
            ))
            .style(Style::default().bg(SURFACE)),
        gauge_rect,
    );
}

/// The seek gauge's rect inside the track panel. Shared with the mouse
/// hit-test in `app.rs` — the old hit-test sat one row below the drawn
/// gauge (forgot the bottom border) and measured the ratio from the
/// panel's left edge instead of the gauge start, skewing every seek.
pub(crate) fn track_gauge_rect(track: Rect) -> Rect {
    if track.width == 0 || track.height < 2 {
        return Rect::new(track.x, track.y, 0, 0);
    }
    let label_width = 28u16.min(track.width.saturating_sub(8));
    Rect::new(
        track.x + label_width,
        track.y + track.height.saturating_sub(2),
        track.width.saturating_sub(label_width),
        1,
    )
}

/// Column ranges (relative to the transport block's 1-cell inner
/// margin) of the prev / play-pause / next chips on the primary row.
/// Shared with mouse hit-testing — the old equal-thirds split fired
/// PlayPause for clicks on the ⏭ chip (chips are left-packed, not
/// evenly distributed).
pub(crate) fn transport_primary_ranges(compact: bool) -> [std::ops::Range<u16>; 3] {
    let chip: u16 = if compact { 3 } else { 7 };
    let gap: u16 = if compact { 2 } else { 3 };
    let first: u16 = 1;
    let second = first + chip + gap;
    let third = second + chip + gap;
    [
        first..first + chip,
        second..second + chip,
        third..third + chip,
    ]
}

/// Column range of the volume bar on the transport's volume row
/// (leading space + 2-col speaker glyph + 2-space gap before the bar).
pub(crate) fn transport_volume_bar_range(compact: bool) -> std::ops::Range<u16> {
    let bar_width: u16 = if compact { 8 } else { 16 };
    5..5 + bar_width
}

fn active_singalong_lyric_line_index(
    lines: &[spotuify_core::LyricLine],
    position_ms: u64,
    offset_ms: i64,
) -> Option<usize> {
    let active = active_lyric_line_index(lines, position_ms, offset_ms)?;
    if !lines[active].text.trim().is_empty() {
        return Some(active);
    }
    lines[..active]
        .iter()
        .rposition(|line| !line.text.trim().is_empty())
}

/// Toggle-chip labels for the transport's shuffle / repeat / like row.
/// Shared with `transport_toggle_ranges` so the mouse hit-testing in
/// `app.rs` tracks the rendered chip widths in both layouts.
pub(crate) fn transport_toggle_labels(
    repeat: &str,
    shuffle: bool,
    liked: bool,
    compact: bool,
) -> (&'static str, &'static str, &'static str) {
    let shuffle_label = match (compact, shuffle) {
        (false, true) => "SHUFFLE",
        (false, false) => "shuffle",
        (true, true) => "SHUF",
        (true, false) => "shuf",
    };
    let repeat_label = match (compact, repeat) {
        (false, "track") => "REPEAT ONE",
        (false, "context" | "on") => "REPEAT ALL",
        (false, _) => "repeat",
        (true, "track") => "REP 1",
        (true, "context" | "on") => "REP A",
        (true, _) => "rep",
    };
    let like_label = match (compact, liked) {
        (false, true) => "LIKED",
        (true, true) => "LIKE",
        (_, false) => "like",
    };
    (shuffle_label, repeat_label, like_label)
}

/// Column ranges (relative to the transport block's 1-cell inner
/// margin) of the shuffle / repeat / like chips on the toggles row.
pub(crate) fn transport_toggle_ranges(
    repeat: &str,
    shuffle: bool,
    liked: bool,
    compact: bool,
) -> [std::ops::Range<u16>; 3] {
    let (shuffle_label, repeat_label, like_label) =
        transport_toggle_labels(repeat, shuffle, liked, compact);
    let gap: u16 = 2;
    let shuffle_start: u16 = 1;
    let shuffle_end = shuffle_start + shuffle_label.len() as u16 + 2;
    let repeat_start = shuffle_end + gap;
    let repeat_end = repeat_start + repeat_label.len() as u16 + 2;
    let like_start = repeat_end + gap;
    let like_end = like_start + like_label.len() as u16 + 2;
    [
        shuffle_start..shuffle_end,
        repeat_start..repeat_end,
        like_start..like_end,
    ]
}

fn render_transport(frame: &mut Frame<'_>, app: &App, area: Rect, compact: bool) {
    use crate::widgets::style::{state_chip, StateRole};
    // Phase 6 — canonical view: volume falls back to devices cache for
    // the same active-device id (never a different device), liked
    // resolves against the view's active item.
    let view = NowPlayingView::derive(&app.playback, &app.queue, &app.devices);
    let volume = view.volume_percent.unwrap_or(0);
    let play_glyph = if view.is_playing { "⏸" } else { "▶" };
    let liked = view.item.is_some_and(|i| {
        app.marked_uris.contains(&i.uri) || app.library_items.iter().any(|saved| saved.uri == i.uri)
    });

    // Chunky transport chips: 7 cells wide each (`   X   `) so the
    // glyph sits in a chip the user can actually click. 3-cell gaps
    // between primary buttons. Compact mode shrinks chips to 3 cells
    // and gaps to 2 so the row fits `TRANSPORT_COMPACT_WIDTH`.
    let chip_pad = if compact { " " } else { "   " };
    let chip_gap = if compact { "  " } else { "   " };
    let big_chip = |glyph: &str, role: ButtonHeroRole| {
        let (fg, bg) = match role {
            ButtonHeroRole::Primary => (accent_foreground(), accent()),
            ButtonHeroRole::Secondary => (CHIP_FG, CHIP_BG),
        };
        Span::styled(
            format!("{chip_pad}{glyph}{chip_pad}"),
            Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
        )
    };

    let primary_row = Line::from(vec![
        Span::raw(" "),
        big_chip("⏮", ButtonHeroRole::Secondary),
        Span::raw(chip_gap),
        big_chip(play_glyph, ButtonHeroRole::Primary),
        Span::raw(chip_gap),
        big_chip("⏭", ButtonHeroRole::Secondary),
    ]);

    // Toggles: drop the small unicode glyphs (⇄ ↻ ♡) for plain word
    // labels — they render in the terminal's normal font weight and
    // are legible at any size. State communicates via chip colour:
    // Adaptive accent background when ON, dim CHIP_BG when OFF.
    let toggle_chip = |label: &str, active: bool| {
        if active {
            state_chip(label, StateRole::Active)
        } else {
            state_chip(label, StateRole::Idle)
        }
    };
    let (shuffle_label, repeat_label, like_label) = transport_toggle_labels(
        app.playback.repeat.as_str(),
        app.playback.shuffle,
        liked,
        compact,
    );
    let repeat_on = matches!(app.playback.repeat.as_str(), "track" | "context" | "on");
    let shuffle_chip = toggle_chip(shuffle_label, app.playback.shuffle);
    let repeat_chip = toggle_chip(repeat_label, repeat_on);
    let like_chip = toggle_chip(like_label, liked);
    let toggles_row = Line::from(vec![
        Span::raw(" "),
        shuffle_chip,
        Span::raw("  "),
        repeat_chip,
        Span::raw("  "),
        like_chip,
    ]);

    // Volume row — bar + numeric.
    let speaker_glyph = if volume == 0 {
        "🔇"
    } else if volume < 33 {
        "🔈"
    } else if volume < 66 {
        "🔉"
    } else {
        "🔊"
    };
    let bar_width: usize = if compact { 8 } else { 16 };
    let filled = ((volume as usize) * bar_width).div_ceil(100).min(bar_width);
    let bar: String = "█".repeat(filled) + &"░".repeat(bar_width - filled);
    let volume_row = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            format!("{speaker_glyph}  "),
            Style::default().fg(TEXT_MUTED),
        ),
        Span::styled(bar, Style::default().fg(accent())),
        Span::styled(format!("  {volume:>3}"), Style::default().fg(TEXT_MUTED)),
    ]);

    let inner = area.inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    // 8 usable rows (PLAYER_HEIGHT=10 minus borders). Distribute as:
    //   row 0-1 breathing room so controls sit in the lower player band
    //   row 2   primary buttons
    //   row 3   pad
    //   row 4   toggles
    //   row 5   viz hint
    //   row 6   volume
    //   row 7   bottom pad
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);
    frame.render_widget(
        Paragraph::new(primary_row).style(Style::default().bg(SURFACE)),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new(toggles_row).style(Style::default().bg(SURFACE)),
        rows[3],
    );
    frame.render_widget(
        Paragraph::new(volume_row).style(Style::default().bg(SURFACE)),
        rows[5],
    );
    // Phase 7 — when the visualizer is enabled but has no active PCM
    // source (or stalled out), surface a one-line hint in the previously
    // empty pad row so the user understands why they're seeing flat
    // bars. Pulled from the daemon's diagnostics so the explanation
    // tracks daemon-side reality.
    if let Some(hint) = viz_status_hint(app) {
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                hint,
                Style::default()
                    .fg(TEXT_MUTED)
                    .add_modifier(Modifier::ITALIC),
            )]))
            .style(Style::default().bg(SURFACE)),
            rows[4],
        );
    }
}

/// Phase 7 — derive a short, human-readable viz status string when the
/// visualizer is enabled but has nothing to render. Returns `None` when
/// the visualizer is off or the source is live and producing frames.
///
/// Priority:
/// 1. Daemon-supplied `viz_hint` (set by `VizSourceChanged`)
/// 2. Backend-kind-aware fallback ("switch to embedded for sink tap" etc.)
/// 3. Generic "no source" message
fn viz_status_hint(app: &App) -> Option<String> {
    use spotuify_core::BackendKind;
    use spotuify_protocol::{VizActiveSource, VizSourceKindData};

    if !app.viz_enabled {
        return None;
    }

    // Live source: only warn when frames have actually stalled.
    if !matches!(app.viz_active_source, VizActiveSource::None) {
        let stalled = app
            .viz_last_frame_at
            .is_some_and(|t| t.elapsed().as_millis() > 2_000);
        return if stalled {
            Some("viz: frames stalled — source may have hung".to_string())
        } else {
            None
        };
    }

    // Active source is None — explain why.
    if let Some(hint) = app.viz_hint.as_deref() {
        return Some(format!("viz: {hint}"));
    }

    Some(match (app.viz_configured_source, app.viz_backend_kind) {
        (VizSourceKindData::Sink, Some(BackendKind::Embedded)) => {
            "viz: waiting for sink — embedded backend warming up".to_string()
        }
        (VizSourceKindData::Sink, _) => {
            "viz: no sink — switch playback to the embedded backend".to_string()
        }
        (VizSourceKindData::Auto, Some(BackendKind::Embedded)) => {
            "viz: warming up sink tap".to_string()
        }
        (VizSourceKindData::Auto, _) => {
            "viz: no PCM source — switch to embedded or set viz.source = \"loopback\"".to_string()
        }
        (VizSourceKindData::Loopback, _) => {
            "viz: loopback unavailable — install BlackHole (macOS) or check device".to_string()
        }
        (VizSourceKindData::None, _) => {
            // User explicitly set source=none; visualizer-enabled with
            // source-none is contradictory but accept it as "disabled".
            return None;
        }
    })
}

#[derive(Copy, Clone)]
enum ButtonHeroRole {
    Primary,
    Secondary,
}

fn render_body(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let outer = Block::default()
        .borders(Borders::LEFT | Borders::RIGHT)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(BG));
    let inner = outer.inner(area).inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    frame.render_widget(outer, area);

    // Tab row is exactly 1 line tall so the chip backgrounds don't
    // float over a hollow 3-row band. A blank pad row above and below
    // the tabs gives the strip breathing room without making the
    // chips look like they're hugging the floor of an empty column.
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(inner);
    let tabs_row = rows[1];
    // Tabs: each tab is `[N] Label`. The numeric prefix is a small
    // CHIP_BG chip so the keyboard shortcut reads as a button. The
    // active tab gets the inverted adaptive-accent treatment. Layout is computed
    // by `tab_strip_layout` (shared with mouse hit-testing) so narrow
    // terminals degrade to short labels / a window around the active
    // tab instead of silently clipping the right-hand tabs.
    let selected = Screen::ALL
        .iter()
        .position(|screen| *screen == app.screen)
        .unwrap_or(0);
    let (tab_line, _) = tab_strip_layout(selected, tabs_row.width);
    frame.render_widget(
        Paragraph::new(tab_line).style(Style::default().bg(BG)),
        tabs_row,
    );

    let body_area = rows[3];
    let content = if app.right_rail == RightRailMode::Hidden || body_area.width < 96 {
        vec![body_area]
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(54), Constraint::Length(38)])
            .split(body_area)
            .to_vec()
    };

    render_screen(frame, app, content[0]);
    if content.len() > 1 {
        render_right_rail(frame, app, content[1]);
    }
}

/// Width-aware tab strip. Tries the roomiest presentation first and
/// degrades in steps as the terminal narrows: full labels with the wide
/// divider, full labels with a tight divider, short labels (the active
/// tab keeps its full label), then a window of short-label tabs around
/// the active one with `‹`/`›` overflow markers. Returns the styled
/// line plus each visible tab's column range (relative to the strip
/// start) so mouse hit-testing in `app.rs` always matches what's drawn.
pub(crate) fn tab_strip_layout(
    selected: usize,
    width: u16,
) -> (Line<'static>, Vec<(usize, std::ops::Range<u16>)>) {
    let screens = Screen::ALL;
    let n = screens.len();

    let cell_width = |index: usize, short: bool| -> u16 {
        let screen = screens[index];
        let label = if short && index != selected {
            screen.short_label()
        } else {
            screen.label()
        };
        (screen.key_label().len() + label.len() + 4) as u16
    };
    let total_width = |short: bool, divider_width: u16| -> u16 {
        (0..n).map(|i| cell_width(i, short)).sum::<u16>()
            + divider_width.saturating_mul(n as u16 - 1)
    };

    let modes: [(bool, &str); 4] = [(false, "  │  "), (false, " │ "), (true, " │ "), (true, " ")];
    let fitting_mode = modes
        .iter()
        .find(|(short, divider)| total_width(*short, divider.chars().count() as u16) <= width);

    let (short, divider, start, end, left_marker, right_marker) = match fitting_mode {
        Some((short, divider)) => (*short, *divider, 0, n, false, false),
        None => {
            // Window around the selected tab: grow rightward then
            // leftward while the strip (plus overflow markers) fits.
            let divider = " ";
            let fits = |start: usize, end: usize| -> bool {
                let cells: u16 = (start..end).map(|i| cell_width(i, true)).sum();
                let dividers = (end - start).saturating_sub(1) as u16;
                let markers = if start > 0 { 2u16 } else { 0 } + if end < n { 2u16 } else { 0 };
                cells + dividers + markers <= width
            };
            let (mut start, mut end) = (selected, selected + 1);
            loop {
                let grew_right = end < n && fits(start, end + 1);
                if grew_right {
                    end += 1;
                }
                let grew_left = start > 0 && fits(start - 1, end);
                if grew_left {
                    start -= 1;
                }
                if !grew_right && !grew_left {
                    break;
                }
            }
            (true, divider, start, end, start > 0, end < n)
        }
    };

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut ranges: Vec<(usize, std::ops::Range<u16>)> = Vec::new();
    let mut x: u16 = 0;
    let marker_style = Style::default().fg(TEXT_MUTED).bg(BG);
    if left_marker {
        spans.push(Span::styled("‹ ", marker_style));
        x += 2;
    }
    for (index, screen) in screens.iter().copied().enumerate().take(end).skip(start) {
        if index > start {
            spans.push(Span::styled(
                divider.to_string(),
                Style::default().fg(BORDER_STRONG).bg(BG),
            ));
            x += divider.chars().count() as u16;
        }
        let is_active = index == selected;
        let key_chip_style = if is_active {
            Style::default()
                .fg(accent())
                .bg(BG)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(CHIP_FG)
                .bg(CHIP_BG)
                .add_modifier(Modifier::BOLD)
        };
        let label_style = if is_active {
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(TEXT_MUTED)
        };
        let label = if short && !is_active {
            screen.short_label()
        } else {
            screen.label()
        };
        let cell_start = x;
        let chip = format!(" {} ", screen.key_label());
        x += chip.chars().count() as u16;
        spans.push(Span::styled(chip, key_chip_style));
        let label_cell = format!(" {label} ");
        x += label_cell.chars().count() as u16;
        spans.push(Span::styled(label_cell, label_style));
        ranges.push((index, cell_start..x));
    }
    if right_marker {
        spans.push(Span::styled(" ›", marker_style));
    }
    (Line::from(spans), ranges)
}

fn render_screen(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    match app.screen {
        Screen::Player => render_player_page(frame, app, area),
        Screen::Search => render_search(frame, app, area),
        Screen::Library => render_library(frame, app, area),
        Screen::Playlists => render_playlists(frame, app, area),
        Screen::Queue => render_queue(frame, app, area),
        Screen::History => render_history(frame, app, area),
        Screen::Devices => render_devices(frame, app, area),
        Screen::Diagnostics => render_diagnostics(frame, app, area),
        Screen::Lyrics => render_lyrics(frame, app, area),
        Screen::Notifications => render_notifications(frame, app, area),
    }
}

/// Listening history: sessions (newest first) flattened into a track list, with
/// a dim session header on each session's first row. Selection indexes the flat
/// track list (matching `App::visible_items` on the History screen), so Enter /
/// e / o / O work through the standard item-action path.
fn render_history(frame: &mut Frame<'_>, app: &App, area: Rect) {
    use crate::widgets::style::card_block;
    let total_tracks: usize = app.history_sessions.iter().map(|s| s.tracks.len()).sum();
    let session_count = app.history_sessions.len();
    let block = card_block(&format!(
        "History · {session_count} session{} · {total_tracks} tracks  ·  Enter play · e queue · o artist · O album",
        if session_count == 1 { "" } else { "s" }
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.history_loading && app.history_sessions.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  Loading history…",
                Style::default().fg(TEXT_MUTED),
            )))
            .style(Style::default().bg(SURFACE)),
            inner,
        );
        return;
    }
    if let Some(err) = &app.history_error {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("  {err}"),
                Style::default().fg(DANGER),
            )))
            .style(Style::default().bg(SURFACE)),
            inner,
        );
        return;
    }
    if total_tracks == 0 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  No listening history yet. Tracks you play show up here.",
                Style::default().fg(TEXT_MUTED),
            )))
            .style(Style::default().bg(SURFACE)),
            inner,
        );
        return;
    }

    let highlight = Style::default()
        .fg(accent_foreground())
        .bg(accent())
        .add_modifier(Modifier::BOLD);
    let mut items: Vec<ListItem<'_>> = Vec::with_capacity(total_tracks);
    for session in &app.history_sessions {
        for (i, track) in session.tracks.iter().enumerate() {
            let track_line = Line::from(vec![
                Span::styled(
                    track.name.clone(),
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(track.subtitle.clone(), Style::default().fg(TEXT_MUTED)),
            ]);
            if i == 0 {
                let label = session
                    .context_label
                    .clone()
                    .unwrap_or_else(|| "Mixed session".to_string());
                let header = Line::from(Span::styled(
                    format!("— {label} · {}", fmt_reminder_when(session.started_at_ms)),
                    Style::default().fg(accent()),
                ));
                items.push(ListItem::new(vec![header, track_line]));
            } else {
                items.push(ListItem::new(track_line));
            }
        }
    }
    let mut state = ListState::default();
    if app.selected < total_tracks {
        state.select(Some(app.selected));
    }
    frame.render_stateful_widget(
        List::new(items).highlight_style(highlight),
        inner,
        &mut state,
    );
}

fn render_right_rail(frame: &mut Frame<'_>, app: &App, area: Rect) {
    match app.right_rail {
        RightRailMode::Queue => render_queue_rail(frame, app, area),
        RightRailMode::Lyrics => render_lyrics_rail(frame, app, area),
        RightRailMode::Hints => render_hints_rail(frame, app, area),
        RightRailMode::Hidden => {}
    }
    // Title-row click targets. The hide chip rect matches the text the
    // user actually sees ("Q hide" used to be drawn on the LEFT while
    // the hotspot was the rightmost 10 columns). Fullscreen is pushed
    // first so the toggle chip wins where they overlap.
    use crate::hit::HitTarget;
    let mut map = app.hit_map.borrow_mut();
    let title_row = Rect::new(area.x, area.y, area.width, 1);
    match app.right_rail {
        RightRailMode::Queue => {
            map.push(title_row, HitTarget::RailFullscreen);
            // card_block titles render as ` {title} ` starting one cell
            // inside the border: "Q hide" begins after "Queue  ·  ".
            // chars().count(), NOT len(): "·" is 2 bytes wide in UTF-8
            // but 1 terminal cell.
            let hide_x = area.x + 2 + "Queue  ·  ".chars().count() as u16;
            map.push(
                Rect::new(hide_x, area.y, "Q hide".len() as u16, 1),
                HitTarget::RailToggle,
            );
        }
        RightRailMode::Lyrics => {
            // Lyrics reuses the fullscreen chrome; keep the legacy
            // right-edge hide hotspot until that title grows a chip.
            map.push(title_row, HitTarget::RailFullscreen);
            let width = 10.min(area.width);
            map.push(
                Rect::new(area.x + area.width.saturating_sub(width), area.y, width, 1),
                HitTarget::RailToggle,
            );
        }
        RightRailMode::Hints => {
            map.push(title_row, HitTarget::RailToggle);
        }
        RightRailMode::Hidden => {}
    }
}

fn render_queue_rail(frame: &mut Frame<'_>, app: &App, area: Rect) {
    use crate::widgets::style::{card_block, section_chip, state_chip, StateRole};

    let view = NowPlayingView::derive(&app.playback, &app.queue, &app.devices);

    let session_active = app.queue.session_active;
    let queue_items: &[MediaItem] = if session_active {
        &app.queue.items
    } else {
        &[]
    };
    let queue_title = if session_active {
        format!("Queue  ·  Q hide  ·  {}", queue_items.len())
    } else {
        "Queue  ·  Q hide  ·  no active session".to_string()
    };
    let block = card_block(&queue_title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = Vec::new();
    // Only render the "Now" header when Spotify reports an active
    // session. With no session, `currently_playing` is the last track
    // from a dead session — labelling it "now playing" would be a lie.
    if let Some(item) = app
        .queue
        .currently_playing
        .as_ref()
        .filter(|_| session_active)
    {
        let mut now_row = vec![
            section_chip("Now"),
            Span::raw("  "),
            state_chip(
                if view.is_playing { "playing" } else { "paused" },
                if view.is_playing {
                    StateRole::Active
                } else {
                    StateRole::Idle
                },
            ),
        ];
        if view.uri_mismatch {
            // Phase 6 — when the queue snapshot has advanced past
            // playback (or vice versa), the queue's "currently_playing"
            // disagrees with the bottom player. Surface that as a dim
            // hint so the user knows the queue rail isn't the source of
            // truth — the bottom-player chrome is.
            now_row.push(Span::raw("  "));
            now_row.push(Span::styled(
                "(queue ahead)",
                Style::default()
                    .fg(TEXT_MUTED)
                    .add_modifier(Modifier::ITALIC),
            ));
        }
        lines.push(Line::from(now_row));
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                item.name.clone(),
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(item.subtitle.clone(), Style::default().fg(TEXT_MUTED)),
        ]));
        lines.push(Line::from(""));
    }
    lines.push(Line::from(vec![section_chip("Up Next")]));
    if !session_active {
        lines.push(Line::from(Span::styled(
            " no active Spotify session",
            Style::default().fg(TEXT_MUTED),
        )));
    } else if queue_items.is_empty() {
        lines.push(Line::from(Span::styled(
            " queue is empty — press `e` on any track or album to enqueue",
            Style::default().fg(TEXT_MUTED),
        )));
    } else {
        lines.extend(
            queue_items
                .iter()
                .take(12)
                .enumerate()
                .map(|(index, item)| {
                    Line::from(vec![
                        Span::styled(
                            format!(" {:>2}. ", index + 1),
                            Style::default().fg(TEXT_MUTED),
                        ),
                        Span::styled(
                            item.name.clone(),
                            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                        ),
                    ])
                }),
        );
        if queue_items.len() > 12 {
            lines.push(Line::from(Span::styled(
                format!(" + {} more", queue_items.len() - 12),
                Style::default().fg(TEXT_MUTED),
            )));
        }
    }
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(SURFACE)),
        inner,
    );
}

fn render_lyrics_rail(frame: &mut Frame<'_>, app: &App, area: Rect) {
    // Rail re-uses the fullscreen lyrics renderer (card chrome,
    // header thumb, active-line emphasis, footer chip). The narrow
    // rect just constrains it.
    render_lyrics(frame, app, area);
}

fn render_hints_rail(frame: &mut Frame<'_>, app: &App, area: Rect) {
    use crate::widgets::style::{card_block, key_chip, section_chip};

    let block = card_block(&format!(
        "Keymap  ·  H hide  ·  {}",
        app.current_action_context().label()
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let actions =
        crate::tui_actions::actions_for_context(app.current_action_context(), app.selected_count());
    // Group by category string so the rail reads as sections, not a
    // flat wall of shortcuts.
    let mut by_cat: std::collections::BTreeMap<&'static str, Vec<_>> =
        std::collections::BTreeMap::new();
    for action in actions.into_iter().take(40) {
        by_cat.entry(action.category).or_default().push(action);
    }
    // Render in a curated section order; anything outside the list
    // falls into a trailing "Other" bucket.
    let order = [
        "Playback",
        "Navigation",
        "Selection",
        "View",
        "Edit",
        "Diagnostics",
        "Help",
    ];
    let mut lines: Vec<Line<'_>> = Vec::new();
    for cat in order {
        if let Some(rows) = by_cat.remove(cat) {
            lines.push(Line::from(vec![section_chip(cat)]));
            for action in rows {
                lines.push(Line::from(vec![
                    Span::raw(" "),
                    key_chip(action.shortcut),
                    Span::raw(" "),
                    Span::styled(action.label.to_string(), Style::default().fg(TEXT)),
                ]));
            }
            lines.push(Line::from(""));
        }
    }
    // Any leftover categories (future-proofing).
    for (cat, rows) in by_cat {
        lines.push(Line::from(vec![section_chip(cat)]));
        for action in rows {
            lines.push(Line::from(vec![
                Span::raw(" "),
                key_chip(action.shortcut),
                Span::raw(" "),
                Span::styled(action.label.to_string(), Style::default().fg(TEXT)),
            ]));
        }
        lines.push(Line::from(""));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(SURFACE)),
        inner,
    );
}

fn render_player_page(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let items = app.visible_items();
    if !app.player_large {
        render_media_list(
            frame,
            area_title(" Home ", items.len()),
            &items,
            app.selected,
            app,
            area,
            true,
        );
        return;
    }

    let home_area = if app.viz_enabled && area.height >= 18 {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(8), Constraint::Length(8)])
            .split(area);
        render_spectrum(frame, app, rows[1]);
        rows[0]
    } else {
        area
    };
    render_home_body(frame, app, &items, home_area);
}

fn render_home_body(frame: &mut Frame<'_>, app: &App, items: &[MediaItem], area: Rect) {
    if area.width >= 112 && area.height >= 10 {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Ratio(2, 3), Constraint::Ratio(1, 3)])
            .split(area);
        render_home_feed(frame, app, items, columns[0]);
        render_home_queue_panel(frame, app, columns[1]);
    } else {
        let queue_height = area.height.clamp(5, 9);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(5), Constraint::Length(queue_height)])
            .split(area);
        render_home_feed(frame, app, items, rows[0]);
        render_home_queue_panel(frame, app, rows[1]);
    }
}

fn render_home_feed(frame: &mut Frame<'_>, app: &App, items: &[MediaItem], area: Rect) {
    use crate::widgets::style::{card_block, focused_card_block};

    if items.is_empty() {
        let block = card_block("Home");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "Fetching saved songs and podcasts...",
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    "Your Home feed fills from the local library and recent plays.",
                    Style::default().fg(TEXT_MUTED),
                )),
            ])
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(SURFACE)),
            inner,
        );
        return;
    }

    // Split with the FULL-list index carried along: clicks in either
    // column must resolve to the index `app.selected` actually uses.
    let mut music = Vec::new();
    let mut music_idx = Vec::new();
    let mut podcasts = Vec::new();
    let mut podcast_idx = Vec::new();
    for (i, item) in items.iter().enumerate() {
        if matches!(item.kind, MediaKind::Show | MediaKind::Episode) {
            podcasts.push(item.clone());
            podcast_idx.push(i);
        } else {
            music.push(item.clone());
            music_idx.push(i);
        }
    }
    let selected_uri = items.get(app.selected).map(|item| item.uri.as_str());
    let music_focused = selected_uri.is_some_and(|uri| music.iter().any(|item| item.uri == uri));
    let podcast_focused =
        selected_uri.is_some_and(|uri| podcasts.iter().any(|item| item.uri == uri));

    if area.width >= 76 && !podcasts.is_empty() {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Ratio(3, 5), Constraint::Ratio(2, 5)])
            .split(area);
        render_home_section(
            frame,
            &format!("Home · Liked Songs  {}", music.len()),
            &music,
            selected_uri,
            music_focused || !podcast_focused,
            app,
            columns[0],
            &music_idx,
        );
        render_home_section(
            frame,
            &format!("Podcasts  {}", podcasts.len()),
            &podcasts,
            selected_uri,
            podcast_focused,
            app,
            columns[1],
            &podcast_idx,
        );
    } else {
        let block = if music_focused || podcasts.is_empty() {
            focused_card_block(&format!("Home  {}", items.len()))
        } else {
            card_block(&format!("Home  {}", items.len()))
        };
        let inner = pad_pane_top(block.inner(area));
        frame.render_widget(block, area);
        render_media_rows(frame, app, items, app.selected, inner, None, None);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_home_section(
    frame: &mut Frame<'_>,
    title: &str,
    items: &[MediaItem],
    selected_uri: Option<&str>,
    focused: bool,
    app: &App,
    area: Rect,
    hit_remap: &[usize],
) {
    use crate::widgets::style::{card_block, focused_card_block};

    let block = if focused {
        focused_card_block(title)
    } else {
        card_block(title)
    };
    let inner = pad_pane_top(block.inner(area));
    frame.render_widget(block, area);
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "Saved music appears here.",
                Style::default().fg(TEXT_MUTED),
            )))
            .style(Style::default().bg(SURFACE)),
            inner,
        );
        return;
    }
    let selected = selected_uri
        .and_then(|uri| items.iter().position(|item| item.uri == uri))
        .unwrap_or(usize::MAX);
    render_media_rows(frame, app, items, selected, inner, None, Some(hit_remap));
}

fn render_home_queue_panel(frame: &mut Frame<'_>, app: &App, area: Rect) {
    use crate::widgets::style::{card_block, section_chip};

    let queue_items: &[MediaItem] = if app.queue.session_active {
        &app.queue.items
    } else {
        &[]
    };
    let block = card_block(&format!("Queue · Up Next  {}", queue_items.len()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = vec![Line::from(vec![section_chip("Up Next")])];
    if !app.queue.session_active {
        lines.push(Line::from(Span::styled(
            " no active Spotify session",
            Style::default().fg(TEXT_MUTED),
        )));
    } else if queue_items.is_empty() {
        lines.push(Line::from(Span::styled(
            " queue is empty",
            Style::default().fg(TEXT_MUTED),
        )));
    } else {
        lines.extend(
            queue_items
                .iter()
                .take(10)
                .enumerate()
                .map(|(index, item)| {
                    Line::from(vec![
                        Span::styled(
                            format!(" {:>2}. ", index + 1),
                            Style::default().fg(TEXT_MUTED),
                        ),
                        Span::styled(
                            truncate(&item.name, area.width.saturating_sub(6) as usize),
                            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                        ),
                    ])
                }),
        );
        if queue_items.len() > 10 {
            lines.push(Line::from(Span::styled(
                format!(" + {} more", queue_items.len() - 10),
                Style::default().fg(TEXT_MUTED),
            )));
        }
    }
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(SURFACE)),
        inner,
    );
}

/// Phase 17 — render the 12-band FFT spectrum at the bottom of the
/// `player_large` left pane. Pure cell-level rendering: writes a column
/// of 8-level block glyphs per band into the frame buffer.
fn render_spectrum(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let title = spectrum_title(app);
    let block = panel_block(&title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    frame.render_widget(
        SpectrumWidget::new(&app.spectrum_bands)
            .color_scheme(&app.viz_color_scheme)
            .accent(app.palette.brand),
        inner,
    );
}

fn spectrum_title(app: &App) -> String {
    use spotuify_protocol::VizActiveSource;
    let active = match app.viz_active_source {
        VizActiveSource::Sink => "sink".to_string(),
        VizActiveSource::LoopbackCpal => "loopback (cpal)".to_string(),
        VizActiveSource::LoopbackPipewire => "loopback (pipewire)".to_string(),
        VizActiveSource::None => "no source".to_string(),
    };
    let cfg = app.viz_configured_source.as_str();
    format!(" Spectrum  source={active}  configured={cfg} ")
}

/// Estimate how many rows a lyric line occupies once wrapped to `width`.
/// Char-count proxy for display width — exact enough to keep the active
/// line centered (the teleprompter scroll tolerates being a row off).
fn wrapped_row_count(text: &str, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    text.chars().count().div_ceil(width).max(1)
}

fn render_lyrics(frame: &mut Frame<'_>, app: &App, area: Rect) {
    use crate::widgets::style::{card_block, section_chip};

    let block = card_block("Lyrics");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);
    // Phase 6 — derive the canonical view ONCE. `view.item` is live
    // playback only, and lyrics rendering anchors to the same URI used
    // by the bottom player. `view.lyrics_match()` gates the synced-line
    // picker below so an in-flight lyrics fetch can never paint stale
    // lines against a different track's progress.
    let view = NowPlayingView::derive(&app.playback, &app.queue, &app.devices);
    let track = view.item;

    // Header: tiny gradient thumb · 2-col gutter · track name (bold) +
    // artist (muted). The gutter is what prevents the Y-thumb from
    // sitting right against the track title — without it the header
    // reads as one mashed-up blob.
    let header_columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(2),
            Constraint::Min(8),
        ])
        .split(rows[1]);
    if let Some(item) = track {
        let initial = item
            .name
            .chars()
            .next()
            .map_or_else(|| "♪".to_string(), |c| c.to_ascii_uppercase().to_string());
        frame.render_widget(
            crate::widgets::album_art::GradientArt::new(&item.uri).with_label(initial),
            header_columns[0],
        );
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    item.name.clone(),
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    item.subtitle.clone(),
                    Style::default().fg(TEXT_MUTED),
                )),
                Line::from(Span::styled(
                    context_suffix(item),
                    Style::default().fg(TEXT_MUTED),
                )),
            ])
            .style(Style::default().bg(SURFACE)),
            header_columns[2],
        );
    } else {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "No active track.",
                Style::default().fg(TEXT_MUTED),
            )))
            .style(Style::default().bg(SURFACE)),
            rows[1],
        );
    }

    // Body: synced lyrics with active-line emphasis, or empty state.
    let Some(lyrics) = &app.lyrics else {
        let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
            [(app.last_progress_tick.elapsed().as_millis() / 80 % 10) as usize];
        let lines = if app.lyrics_loading {
            vec![
                Line::from(vec![
                    Span::styled(
                        format!(" {spinner} "),
                        Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("Fetching synced lyrics…", Style::default().fg(TEXT)),
                ]),
                Line::from(Span::styled(
                    "Spotify provider first, LRCLIB fallback.",
                    Style::default().fg(TEXT_MUTED),
                )),
            ]
        } else if let Some(err) = &app.lyrics_error {
            vec![
                Line::from(Span::styled(err.clone(), Style::default().fg(WARN))),
                Line::from(Span::styled(
                    "Press u to retry.",
                    Style::default().fg(TEXT_MUTED),
                )),
            ]
        } else {
            vec![
                Line::from(Span::styled(
                    "No synced lyrics for this track.",
                    Style::default().fg(TEXT),
                )),
                Line::from(Span::styled(
                    "(some tracks are instrumental, or the provider doesn't have them.)",
                    Style::default().fg(TEXT_MUTED),
                )),
            ]
        };
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: true })
                .style(Style::default().bg(SURFACE)),
            rows[2],
        );
        return;
    };

    let visible = rows[2].height.max(1) as usize;
    // Phase 6 — only highlight the active line when the lyrics's track
    // URI matches the currently-active playback URI. Otherwise, the
    // lyrics are leftover from a previous track and any "active" line
    // would be a lie. `view.progress_ms` is 0 when no live item exists,
    // so even falling back to it would point at line 0 — render the
    // lyrics as a static read with no highlight instead.
    let lyrics_active = view.lyrics_match(app.lyrics_track_uri.as_deref());
    let active = lyrics
        .synced
        .then(|| {
            if !lyrics_active {
                return None;
            }
            active_singalong_lyric_line_index(&lyrics.lines, view.progress_ms, app.lyrics_offset_ms)
        })
        .flatten();
    // Teleprompter scroll: keep the active line vertically centered and
    // let the lyrics auto-scroll past it. The pane wraps long lines, so
    // we center by *visual* rows (sum of wrapped row counts above the
    // active line), not by logical line index — otherwise wrapped lines
    // above the highlight shove it to the bottom of the pane.
    let wrap_width = rows[2].width.max(1) as usize;
    let scroll_rows: u16 = active.map_or(0, |a| {
        let rows_above: usize = lyrics.lines[..a]
            .iter()
            .map(|line| wrapped_row_count(&line.text, wrap_width))
            .sum();
        rows_above
            .saturating_sub(visible / 2)
            .min(u16::MAX as usize) as u16
    });
    let body: Vec<Line<'_>> = lyrics
        .lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let distance = active.map_or(usize::MAX, |a| a.abs_diff(index));
            let style = if Some(index) == active {
                Style::default()
                    .fg(TEXT)
                    .bg(CHIP_BG)
                    .add_modifier(Modifier::BOLD)
            } else if distance == 1 {
                Style::default().fg(TEXT)
            } else if distance == 2 {
                Style::default().fg(TEXT_MUTED)
            } else {
                Style::default().fg(BORDER_STRONG)
            };
            Line::from(Span::styled(line.text.clone(), style))
        })
        .collect();
    frame.render_widget(
        Paragraph::new(body)
            .wrap(Wrap { trim: false })
            .scroll((scroll_rows, 0))
            .style(Style::default().bg(SURFACE)),
        rows[2],
    );

    // Footer: provider chip + offset.
    let footer = if app.lyrics_loading {
        vec![Span::styled("Fetching…", Style::default().fg(accent()))]
    } else if let Some(lyrics) = &app.lyrics {
        vec![
            section_chip(lyrics.provider.label()),
            Span::raw("  "),
            Span::styled(
                format!(
                    "{} lines  ·  offset {:+}ms",
                    lyrics.lines.len(),
                    app.lyrics_offset_ms
                ),
                Style::default().fg(TEXT_MUTED),
            ),
        ]
    } else {
        vec![Span::styled("No provider", Style::default().fg(TEXT_MUTED))]
    };
    frame.render_widget(
        Paragraph::new(Line::from(footer)).style(Style::default().bg(SURFACE)),
        rows[3],
    );
}

fn render_search(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);
    let sort_label = match app.search_sort {
        spotuify_protocol::SearchSortData::Relevance => "Relevance",
        spotuify_protocol::SearchSortData::Name => "Name",
        spotuify_protocol::SearchSortData::Duration => "Duration",
        spotuify_protocol::SearchSortData::Artist => "Artist",
        spotuify_protocol::SearchSortData::Date => "Date",
    };
    let filter_label = app
        .search_kind_filter
        .as_ref()
        .map_or("All", |kind| kind.label());
    let prompt = if app.search_input_active {
        format!("typing global search  ·  S sort: {sort_label}  ·  T type: {filter_label}")
    } else {
        format!("/ search  ·  S sort: {sort_label}  ·  T type: {filter_label}")
    };
    let input_style = if app.search_input_active {
        Style::default().fg(TEXT).bg(SURFACE)
    } else {
        Style::default().fg(TEXT_MUTED).bg(SURFACE)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("/ ", Style::default().fg(accent())),
            Span::styled(&app.search_query, Style::default().fg(TEXT)),
            Span::styled(format!("  {prompt}"), Style::default().fg(TEXT_MUTED)),
        ]))
        .block(panel_block(" Search "))
        .style(input_style),
        rows[0],
    );
    let items = app.visible_items();
    let title = if app.is_searching {
        " Results  searching... ".to_string()
    } else {
        area_title(" Results ", items.len())
    };
    if items.is_empty() && app.is_searching {
        // First pages are still streaming in — skeletons, not the
        // empty-state copy (waiting and no-results must look different).
        let block = crate::widgets::style::card_block(&title);
        let inner = pad_pane_top(block.inner(rows[1]));
        frame.render_widget(block, rows[1]);
        frame.render_widget(
            Paragraph::new(crate::widgets::skeleton::skeleton_rows(
                ((inner.height as usize) / 2).min(5),
                inner.width,
            ))
            .style(Style::default().bg(SURFACE)),
            inner,
        );
    } else if items.is_empty() {
        render_media_list(frame, title, &items, app.selected, app, rows[1], true);
    } else {
        let artwork = app.selected_artwork_subject();
        let (results_area, preview_area) = split_art_preview_area(rows[1], artwork.as_ref());
        render_search_groups(frame, app, &items, results_area);
        if let (Some(subject), Some(preview_area)) = (artwork.as_ref(), preview_area) {
            render_artwork_preview(frame, app, subject, preview_area);
        }
    }
}

fn render_search_groups(frame: &mut Frame<'_>, app: &App, items: &[MediaItem], area: Rect) {
    use crate::widgets::style::{card_block, focused_card_block};

    let groups: [(MediaKind, &str, &str); 6] = [
        (MediaKind::Track, "Tracks", "♪"),
        (MediaKind::Artist, "Artists", "👤"),
        (MediaKind::Album, "Albums", "💿"),
        (MediaKind::Playlist, "Playlists", "≣"),
        (MediaKind::Show, "Podcasts", "🎙"),
        (MediaKind::Episode, "Episodes", "▶"),
    ];
    // Carry each group item's FULL-list index so clicks in a group
    // pane select the index `app.selected` actually uses.
    let visible_groups = groups
        .into_iter()
        .map(|(kind, title, icon)| {
            let mut group_items = Vec::new();
            let mut group_indices = Vec::new();
            for (i, item) in items.iter().enumerate() {
                if item.kind == kind {
                    group_items.push(item.clone());
                    group_indices.push(i);
                }
            }
            (kind, title, icon, group_items, group_indices)
        })
        .filter(|(_, _, _, group_items, _)| !group_items.is_empty())
        .collect::<Vec<_>>();
    if visible_groups.is_empty() {
        render_media_list(
            frame,
            area_title(" Results ", 0),
            &[],
            app.selected,
            app,
            area,
            true,
        );
        return;
    }

    // Lay out as a 2-row grid when there are >3 groups and the area
    // is wide enough; otherwise a single row of cards. This keeps any
    // single card from getting squished below ~22 cols.
    let single_row = visible_groups.len() <= 3 || area.width >= 144;
    if single_row {
        let constraints = visible_groups
            .iter()
            .map(|_| Constraint::Ratio(1, visible_groups.len() as u32))
            .collect::<Vec<_>>();
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(constraints)
            .split(area);
        render_group_cards(frame, app, items, &visible_groups, &columns);
    } else {
        // Two-row grid: split groups roughly in half.
        let half = visible_groups.len().div_ceil(2);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
            .split(area);
        let top: Vec<_> = visible_groups[..half].to_vec();
        let bot: Vec<_> = visible_groups[half..].to_vec();
        let mk_cols = |row: Rect, n: usize| {
            let cs: Vec<Constraint> = (0..n).map(|_| Constraint::Ratio(1, n as u32)).collect();
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints(cs)
                .split(row)
        };
        let top_cols = mk_cols(rows[0], top.len().max(1));
        let bot_cols = mk_cols(rows[1], bot.len().max(1));
        render_group_cards(frame, app, items, &top, &top_cols);
        render_group_cards(frame, app, items, &bot, &bot_cols);
    }

    #[allow(clippy::type_complexity)]
    fn render_group_cards(
        frame: &mut Frame<'_>,
        app: &App,
        items: &[MediaItem],
        groups: &[(MediaKind, &str, &str, Vec<MediaItem>, Vec<usize>)],
        columns: &std::rc::Rc<[Rect]>,
    ) {
        let selected_uri = items.get(app.selected).map(|item| item.uri.as_str());
        for (idx, (kind, title, icon, group_items, group_indices)) in groups.iter().enumerate() {
            let area = columns[idx];
            let focused = selected_uri.is_some_and(|uri| group_items.iter().any(|i| i.uri == uri));
            let title_with_count = format!("{icon}  {title}  {}", group_items.len());
            let block = if focused {
                focused_card_block(&title_with_count)
            } else {
                card_block(&title_with_count)
            };
            let inner = pad_pane_top(block.inner(area));
            frame.render_widget(block, area);
            let selected_index = selected_uri
                .and_then(|uri| group_items.iter().position(|item| item.uri == uri))
                .unwrap_or(usize::MAX);
            let footer = app.search_panes.get(kind).and_then(|pane| {
                if pane.loading {
                    Some(Span::styled(
                        "↓ loading more…",
                        Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                    ))
                } else if let Some(error) = pane.error.as_deref() {
                    Some(Span::styled(
                        format!("! {error}"),
                        Style::default().fg(WARN).add_modifier(Modifier::BOLD),
                    ))
                } else if pane.exhausted && !group_items.is_empty() {
                    Some(Span::styled("— end —", Style::default().fg(TEXT_MUTED)))
                } else {
                    None
                }
            });
            render_media_rows(
                frame,
                app,
                group_items,
                selected_index,
                inner,
                footer,
                Some(group_indices),
            );
        }
    }
}

/// Register the visible rows of a just-rendered item list into the hit
/// map so mouse clicks resolve against what was actually drawn.
/// `index_for` maps the local (pane) item position to the screen's
/// selection-space index; return `None` to leave a row unclickable.
fn register_row_hits(
    app: &App,
    inner: Rect,
    first_visible: usize,
    len: usize,
    rows_per_item: u16,
    index_for: impl Fn(usize) -> Option<usize>,
) {
    if inner.width == 0 || inner.height == 0 || rows_per_item == 0 {
        return;
    }
    let mut map = app.hit_map.borrow_mut();
    for i in first_visible..len {
        let offset_rows = ((i - first_visible) as u16).saturating_mul(rows_per_item);
        if offset_rows >= inner.height {
            break;
        }
        let y = inner.y + offset_rows;
        let height = rows_per_item.min(inner.height - offset_rows);
        if let Some(index) = index_for(i) {
            map.push(
                Rect::new(inner.x, y, inner.width, height),
                crate::hit::HitTarget::Row { index },
            );
        }
    }
}

/// Renders just the rows of a media list into a pre-sized inner rect,
/// without drawing its own block. Used by `render_search_groups` where
/// each card already supplies its own border + title chip.
///
/// Each item occupies TWO terminal rows: name on the first, subtitle
/// (artist) + context (album/show) on the second. Matches the convention
/// used by `media_item_with` in queue/library views so tracks with the
/// same title but different artists are visually distinguishable.
/// `hit_remap` maps pane positions to the screen's selection-space
/// indices for the hit map (None = identity).
fn render_media_rows(
    frame: &mut Frame<'_>,
    app: &App,
    items: &[MediaItem],
    selected: usize,
    area: Rect,
    footer: Option<Span<'_>>,
    hit_remap: Option<&[usize]>,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    if items.is_empty() {
        let placeholder = match footer {
            Some(span) => span,
            None => Span::styled("no results", Style::default().fg(TEXT_MUTED)),
        };
        frame.render_widget(
            Paragraph::new(placeholder).style(Style::default().bg(SURFACE)),
            area,
        );
        return;
    }
    // Reserve the bottom row for the pane footer (↓ loading more… or
    // — end —) when present. The rows area shrinks by one; items
    // window calculation uses the shrunken height.
    let (rows_area, footer_area) = if footer.is_some() && area.height >= 2 {
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(area);
        (split[0], Some(split[1]))
    } else {
        (area, None)
    };
    // Each item is 2 rows; the visible item count is the area's height
    // halved. At least 1 so a 1-row card still shows the top item's name.
    let rows_per_item = 2usize;
    let visible_items = ((rows_area.height as usize) / rows_per_item).max(1);
    // `usize::MAX` is the "no selection in this list" sentinel (used by
    // `render_search_groups` for unfocused panes). Without this branch
    // the saturating-sub math anchored those panes to the *bottom* of
    // their list, so tabbing between panels appeared to "shuffle"
    // every pane's visible slice.
    let start =
        if selected == usize::MAX || selected < visible_items / 2 || items.len() <= visible_items {
            0
        } else {
            selected
                .saturating_sub(visible_items / 2)
                .min(items.len().saturating_sub(visible_items))
        };
    let mut lines: Vec<Line<'_>> = Vec::with_capacity(visible_items * rows_per_item);
    for (i, item) in items.iter().enumerate().skip(start).take(visible_items) {
        let is_sel = i == selected;
        let marker = if app.marked_uris.contains(&item.uri) {
            Span::styled(
                "●",
                Style::default().fg(accent()).add_modifier(Modifier::BOLD),
            )
        } else if is_sel {
            Span::styled(
                "▌",
                Style::default().fg(accent()).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw(" ")
        };
        let name_style = if is_sel {
            Style::default().fg(accent()).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD)
        };
        // Line 1: marker + name. Reserve 4 cols for marker + spacing.
        let name_budget = area.width.saturating_sub(4) as usize;
        let truncated_name = truncate(&item.name, name_budget);
        lines.push(Line::from(vec![
            marker,
            Span::raw(" "),
            Span::styled(truncated_name, name_style),
        ]));
        // Line 2: indent + subtitle (artist) + context suffix (album).
        // Context suffix is empty for items without a context.
        let suffix = context_suffix(item);
        let subtitle_budget = (area.width as usize).saturating_sub(4 + suffix.chars().count());
        let truncated_subtitle = truncate(&item.subtitle, subtitle_budget);
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(truncated_subtitle, Style::default().fg(TEXT_MUTED)),
            Span::styled(suffix, Style::default().fg(TEXT_MUTED)),
        ]));
    }
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(SURFACE)),
        rows_area,
    );
    register_row_hits(
        app,
        rows_area,
        start,
        items.len(),
        rows_per_item as u16,
        |i| hit_remap.map_or(Some(i), |map| map.get(i).copied()),
    );
    if let (Some(footer_rect), Some(footer_span)) = (footer_area, footer) {
        frame.render_widget(
            Paragraph::new(footer_span).style(Style::default().bg(SURFACE)),
            footer_rect,
        );
    }
}

fn render_library(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    use crate::widgets::style::{card_block, focused_card_block};

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);
    render_filter_bar(frame, app, " Library Filter ", rows[0]);
    let items = app.visible_items();

    // Empty-state branch: spinner + reassurance. Auto-sync owned by
    // the daemon means the user just waits.
    if items.is_empty() {
        let block = card_block("Library");
        let inner = block.inner(rows[1]);
        frame.render_widget(block, rows[1]);
        let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
            [(app.last_progress_tick.elapsed().as_millis() / 80 % 10) as usize];
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(format!(" {spinner} "), Style::default().fg(accent()).add_modifier(Modifier::BOLD)),
                    Span::styled(
                        "Fetching your library…",
                        Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(Span::styled(
                    "The daemon syncs this in the background; tracks, albums, and podcasts appear as they arrive.",
                    Style::default().fg(TEXT_MUTED),
                )),
            ])
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(SURFACE)),
            inner,
        );
        return;
    }

    // Split into Music (Track + Album + Artist) and Podcasts (Show +
    // Episode) so the user can find their subscribed shows without
    // hunting through 5,000 saved tracks. Full-list indices ride along
    // so clicks in either column select what the keyboard would.
    let mut music = Vec::new();
    let mut music_idx = Vec::new();
    let mut podcasts = Vec::new();
    let mut podcast_idx = Vec::new();
    for (i, item) in items.iter().enumerate() {
        if matches!(item.kind, MediaKind::Show | MediaKind::Episode) {
            podcasts.push(item.clone());
            podcast_idx.push(i);
        } else {
            music.push(item.clone());
            music_idx.push(i);
        }
    }
    let global_uri = items.get(app.selected).map(|i| i.uri.clone());
    let music_focused = global_uri
        .as_ref()
        .is_some_and(|u| music.iter().any(|i| &i.uri == u));
    let podcasts_focused = !music_focused;

    let artwork = app.selected_artwork_subject();
    let (list_area, preview_area) = split_art_preview_area(rows[1], artwork.as_ref());

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(2, 3), Constraint::Ratio(1, 3)])
        .split(list_area);

    render_library_section(
        frame,
        &format!("Music  {}", music.len()),
        &music,
        global_uri.as_deref(),
        music_focused,
        app,
        columns[0],
        &music_idx,
    );
    render_library_section(
        frame,
        &format!("Podcasts  {}", podcasts.len()),
        &podcasts,
        global_uri.as_deref(),
        podcasts_focused,
        app,
        columns[1],
        &podcast_idx,
    );
    if let (Some(subject), Some(preview_area)) = (artwork.as_ref(), preview_area) {
        render_artwork_preview(frame, app, subject, preview_area);
    }
    let _ = card_block;
    let _ = focused_card_block;
}

#[allow(clippy::too_many_arguments)]
fn render_library_section(
    frame: &mut Frame<'_>,
    title: &str,
    items: &[MediaItem],
    selected_uri: Option<&str>,
    focused: bool,
    app: &App,
    area: Rect,
    hit_remap: &[usize],
) {
    use crate::widgets::style::{card_block, focused_card_block};
    let block = if focused {
        focused_card_block(title)
    } else {
        card_block(title)
    };
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                if title.starts_with("Podcasts") {
                    "No subscribed podcasts."
                } else {
                    "No saved music yet."
                },
                Style::default().fg(TEXT_MUTED),
            )))
            .style(Style::default().bg(SURFACE)),
            inner,
        );
        return;
    }
    let list_items: Vec<ListItem<'_>> = items
        .iter()
        .map(|item| media_item(item, app.marked_uris.contains(&item.uri)))
        .collect();
    let local_selected = selected_uri.and_then(|uri| items.iter().position(|i| i.uri == uri));
    let list = List::new(list_items)
        .highlight_style(
            Style::default()
                .fg(accent_foreground())
                .bg(accent())
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▌")
        .style(Style::default().bg(SURFACE));
    let mut state = ListState::default();
    state.select(local_selected);
    frame.render_stateful_widget(list, inner, &mut state);
    // `state.offset()` is the widget-computed scroll position — the
    // hit map gets the rows that were ACTUALLY drawn.
    register_row_hits(app, inner, state.offset(), items.len(), 2, |i| {
        hit_remap.get(i).copied()
    });
}

fn render_queue(frame: &mut Frame<'_>, app: &App, area: Rect) {
    use crate::widgets::style::{card_block, section_chip, state_chip, StateRole};

    // Phase 6 — derive once; the "Now Playing" card and the "Up Next"
    // highlight read from the same active URI so a queue-poll snapshot
    // can't paint queue's currently_playing as "Now" while highlighting
    // a different track as the active row in "Up Next".
    let view = NowPlayingView::derive(&app.playback, &app.queue, &app.devices);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5), // Now-playing card
            Constraint::Length(3), // filter bar
            Constraint::Min(1),    // upcoming list
        ])
        .split(area);

    // Now-playing card.
    let now_block = card_block("Now Playing");
    let now_inner = now_block.inner(rows[0]);
    frame.render_widget(now_block, rows[0]);
    if let Some(item) = view.item {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(
                        kind_icon(&item.kind),
                        Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        item.name.clone(),
                        Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    state_chip(
                        if view.is_playing { "playing" } else { "paused" },
                        if view.is_playing {
                            StateRole::Active
                        } else {
                            StateRole::Idle
                        },
                    ),
                ]),
                Line::from(vec![
                    Span::raw("   "),
                    Span::styled(item.subtitle.clone(), Style::default().fg(TEXT_MUTED)),
                    Span::styled(context_suffix(item), Style::default().fg(TEXT_MUTED)),
                ]),
            ])
            .style(Style::default().bg(SURFACE)),
            now_inner,
        );
    } else {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "Nothing playing right now.",
                    Style::default().fg(TEXT_MUTED),
                )),
                Line::from(Span::styled(
                    "Press / to search and Enter to start playback.",
                    Style::default().fg(accent()),
                )),
            ])
            .style(Style::default().bg(SURFACE)),
            now_inner,
        );
    }

    render_filter_bar(frame, app, " Queue Filter ", rows[1]);

    // Upcoming list with section chip and counts. Duplicate queue rows
    // are meaningful: Spotify lets the same track appear more than once.
    let items = app.visible_items();
    let up_block = card_block(&format!("Up Next  {}", items.len()));
    let up_inner = up_block.inner(rows[2]);
    frame.render_widget(up_block, rows[2]);
    if items.is_empty() {
        let _ = section_chip; // explicitly unused in empty branch
                              // First seconds after launch: no queue snapshot has arrived
                              // yet. Show skeleton rows, not "No active session" — loading
                              // and empty used to be indistinguishable.
        if app.queue_updated_at.is_none() {
            let mut lines = vec![Line::from(Span::styled(
                "Loading queue…",
                Style::default().fg(TEXT_MUTED),
            ))];
            lines.extend(crate::widgets::skeleton::skeleton_rows(
                ((up_inner.height as usize).saturating_sub(1) / 2).min(4),
                up_inner.width,
            ));
            frame.render_widget(
                Paragraph::new(lines).style(Style::default().bg(SURFACE)),
                up_inner,
            );
            return;
        }
        let empty_lines = if !app.queue.session_active {
            vec![
                Line::from(Span::styled(
                    "No active Spotify session.",
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    "Start playback from Search or Library to load a live queue.",
                    Style::default().fg(accent()),
                )),
            ]
        } else {
            vec![
                Line::from(Span::styled(
                    "Queue is empty.",
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    "Press `e` on any track or album to enqueue it.",
                    Style::default().fg(accent()),
                )),
            ]
        };
        frame.render_widget(
            Paragraph::new(empty_lines).style(Style::default().bg(SURFACE)),
            up_inner,
        );
        return;
    }
    // Phase 6 — highlight the row that matches the canonical view's
    // active URI; falls back to `None` when no live playback exists so
    // no row is highlighted as "now playing" when nothing actually is.
    let now_playing_uri = view.active_uri;
    let list = List::new(
        items
            .iter()
            .map(|item| {
                media_item_with(
                    item,
                    app.marked_uris.contains(&item.uri),
                    now_playing_uri == Some(item.uri.as_str()),
                    app.palette.now_playing_rail,
                )
            })
            .collect::<Vec<_>>(),
    )
    .highlight_style(
        // Match the player/search/library lists: soft accent so the
        // selected row doesn't read like a second seeker bar.
        Style::default()
            .fg(TEXT)
            .bg(app.palette.soft_accent)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("▌")
    .style(Style::default().bg(SURFACE));
    let mut state = ListState::default();
    state.select(if app.selected >= items.len() {
        None
    } else {
        Some(app.selected)
    });
    frame.render_stateful_widget(list, up_inner, &mut state);
    register_row_hits(app, up_inner, state.offset(), items.len(), 2, Some);
}

fn render_playlists(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    use crate::widgets::style::card_block;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);
    render_filter_bar(frame, app, " Playlist Filter ", rows[0]);

    if let Some(name) = app.selected_playlist_name.as_deref() {
        // Inside-a-playlist view: card with the playlist name as the
        // chip + the track list as the body. `b` to go back is one of
        // the hint-bar shortcuts so we don't need to crowd the title.
        let block = card_block(&format!("{name}  ·  press b to go back"));
        let inner = block.inner(rows[1]);
        frame.render_widget(block, rows[1]);
        let items = app.visible_items();
        if items.is_empty() {
            let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
                [(app.last_progress_tick.elapsed().as_millis() / 80 % 10) as usize];
            frame.render_widget(
                Paragraph::new(vec![Line::from(vec![
                    Span::styled(
                        format!(" {spinner} "),
                        Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("Loading tracks…", Style::default().fg(TEXT)),
                ])])
                .style(Style::default().bg(SURFACE)),
                inner,
            );
            return;
        }
        let list = List::new(
            items
                .iter()
                .map(|item| media_item(item, app.marked_uris.contains(&item.uri)))
                .collect::<Vec<_>>(),
        )
        .highlight_style(
            Style::default()
                .fg(accent_foreground())
                .bg(accent())
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▌")
        .style(Style::default().bg(SURFACE));
        let mut state = ListState::default();
        state.select(if app.selected >= items.len() {
            None
        } else {
            Some(app.selected)
        });
        frame.render_stateful_widget(list, inner, &mut state);
        register_row_hits(app, inner, state.offset(), items.len(), 2, Some);
    } else {
        let playlists = app.filtered_playlists();
        let artwork = app.selected_artwork_subject();
        let (list_area, preview_area) = split_art_preview_area(rows[1], artwork.as_ref());
        render_playlist_list(frame, app, &playlists, app.playlist_selected, list_area);
        if let (Some(subject), Some(preview_area)) = (artwork.as_ref(), preview_area) {
            render_artwork_preview(frame, app, subject, preview_area);
        }
    }
}

fn render_devices(frame: &mut Frame<'_>, app: &App, area: Rect) {
    use crate::widgets::style::{card_block, state_chip, StateRole};
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);
    render_filter_bar(frame, app, " Device Filter ", chunks[0]);
    let devices = app.filtered_devices();
    let block = card_block(&format!(
        "Devices  {}  ·  Enter/x transfer · O audio output",
        devices.len()
    ));
    let inner = block.inner(chunks[1]);
    frame.render_widget(block, chunks[1]);

    if devices.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "No visible devices",
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    "Open Spotify on a phone/laptop/speaker to make it visible. Press u to refresh.",
                    Style::default().fg(accent()),
                )),
            ])
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(SURFACE)),
            inner,
        );
        return;
    }

    // Table layout: spread columns across the full width with a
    // spacer row between devices so the list breathes. Each device
    // takes 2 rows (content + blank gap) so the selection
    // highlight reads as a single chunky row rather than crawling
    // up tight rows.
    let table_rows: Vec<ratatui::widgets::Row<'_>> = devices
        .iter()
        .flat_map(|device| {
            let icon = device_kind_icon(&device.kind);
            let state_role = if device.is_restricted {
                StateRole::Error
            } else if device.is_active {
                StateRole::Active
            } else {
                StateRole::Idle
            };
            let state_label = if device.is_restricted {
                "restricted"
            } else if device.is_active {
                "playing"
            } else {
                "idle"
            };
            let volume_cell: Vec<Span<'_>> = if device.supports_volume {
                let v = device.volume_percent.unwrap_or(0) as usize;
                let width = 16;
                let filled = (v * width).div_ceil(100).min(width);
                let bar: String = "█".repeat(filled) + &"░".repeat(width - filled);
                let pct = device.volume_percent.unwrap_or(0);
                vec![
                    Span::styled("🔊  ", Style::default().fg(TEXT_MUTED)),
                    Span::styled(bar, Style::default().fg(accent())),
                    Span::styled(format!("  {pct:>3}"), Style::default().fg(TEXT_MUTED)),
                ]
            } else {
                vec![Span::styled(
                    "🔊  fixed".to_string(),
                    Style::default().fg(TEXT_MUTED),
                )]
            };
            let row = ratatui::widgets::Row::new(vec![
                ratatui::widgets::Cell::from(Line::from(Span::styled(
                    format!(" {icon} "),
                    Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                ))),
                ratatui::widgets::Cell::from(Line::from(vec![Span::styled(
                    device.name.clone(),
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                )])),
                ratatui::widgets::Cell::from(Line::from(Span::styled(
                    device.kind.clone(),
                    Style::default().fg(TEXT_MUTED),
                ))),
                ratatui::widgets::Cell::from(Line::from(state_chip(state_label, state_role))),
                ratatui::widgets::Cell::from(Line::from(volume_cell)),
            ]);
            // Trailing spacer row gives vertical breathing room.
            [row, ratatui::widgets::Row::new(Vec::<&str>::new())]
        })
        .collect();
    let table = ratatui::widgets::Table::new(
        table_rows,
        [
            Constraint::Length(5),
            Constraint::Min(20),
            Constraint::Length(14),
            Constraint::Length(14),
            Constraint::Length(28),
        ],
    )
    .row_highlight_style(
        Style::default()
            .fg(accent_foreground())
            .bg(accent())
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("▌ ")
    .style(Style::default().bg(SURFACE));
    let mut state = ratatui::widgets::TableState::default();
    // Each device occupies two rows (content + spacer); selecting
    // index N maps to row 2*N so the highlight lands on the content.
    state.select(if devices.is_empty() {
        None
    } else {
        Some(app.selected.min(devices.len() - 1) * 2)
    });
    frame.render_stateful_widget(table, inner, &mut state);
    // Hit map: device index = table row / 2 — the old 1-row mapping
    // selected device 2k for a click on device k (and Enter then
    // transferred playback to the wrong device).
    let table_offset = state.offset();
    {
        let mut map = app.hit_map.borrow_mut();
        for (index, _) in devices.iter().enumerate() {
            let table_row = index * 2;
            if table_row + 1 < table_offset {
                continue;
            }
            let offset_rows = (table_row.saturating_sub(table_offset)) as u16;
            if offset_rows >= inner.height {
                break;
            }
            let height = 2u16.min(inner.height - offset_rows);
            map.push(
                Rect::new(inner.x, inner.y + offset_rows, inner.width, height),
                crate::hit::HitTarget::Row { index },
            );
        }
    }
}

fn device_kind_icon(kind: &str) -> &'static str {
    let k = kind.to_ascii_lowercase();
    if k.contains("smartphone") || k.contains("phone") || k.contains("tablet") {
        "📱"
    } else if k.contains("computer") || k.contains("laptop") {
        "🖥"
    } else if k.contains("tv") {
        "📺"
    } else if k.contains("speaker") {
        "🔊"
    } else if k.contains("car") {
        "🚗"
    } else if k.contains("game") || k.contains("console") {
        "🎮"
    } else if k.contains("cast") {
        "📡"
    } else {
        "🎧"
    }
}

fn render_filter_bar(frame: &mut Frame<'_>, app: &App, title: &str, area: Rect) {
    let style = if app.list_filter_active {
        Style::default().fg(TEXT).bg(SURFACE)
    } else {
        Style::default().fg(TEXT_MUTED).bg(SURFACE)
    };
    let prompt = if app.list_filter_active {
        "type to filter current list"
    } else {
        "Ctrl-f filters this list only"
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("filter ", Style::default().fg(accent())),
            Span::styled(&app.list_filter_query, Style::default().fg(TEXT)),
            Span::styled(format!("  {prompt}"), Style::default().fg(TEXT_MUTED)),
        ]))
        .block(panel_block(title))
        .style(style),
        area,
    );
}

fn render_diagnostics(frame: &mut Frame<'_>, app: &App, area: Rect) {
    use crate::widgets::style::{card_block, section_chip, state_chip, StateRole};

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    // ===== Left card: health + findings =====
    let left_block = card_block("Diagnostics");
    let left_inner = left_block.inner(columns[0]);
    frame.render_widget(left_block, columns[0]);
    let mut left = Vec::new();
    if let Some(report) = &app.diagnostics_report {
        left.push(Line::from(vec![
            section_chip("Health"),
            Span::raw(" "),
            state_chip(
                report.health_class.as_str(),
                if report.healthy {
                    StateRole::Active
                } else {
                    StateRole::Error
                },
            ),
        ]));
        left.push(Line::from(""));
        left.push(Line::from(vec![
            Span::styled("Daemon   ", Style::default().fg(TEXT_MUTED)),
            Span::styled(
                format!(
                    "pid {:?}, uptime {:?}s",
                    report.daemon.daemon_pid, report.daemon.uptime_secs
                ),
                Style::default().fg(TEXT),
            ),
        ]));
        left.push(Line::from(vec![
            Span::styled("Auth     ", Style::default().fg(TEXT_MUTED)),
            Span::styled(&report.keychain_token.message, Style::default().fg(TEXT)),
        ]));
        left.push(Line::from(vec![
            Span::styled("Logs     ", Style::default().fg(TEXT_MUTED)),
            Span::styled(&report.logs_path, Style::default().fg(TEXT)),
        ]));
        left.push(Line::from(""));
        left.push(Line::from(vec![
            section_chip("Findings"),
            Span::raw(" "),
            state_chip(
                &format!("{}", report.findings.len()),
                if report.findings.is_empty() {
                    StateRole::Active
                } else {
                    StateRole::Warn
                },
            ),
        ]));
        if report.findings.is_empty() {
            left.push(Line::from(Span::styled(
                "Nothing to flag.",
                Style::default().fg(accent()),
            )));
        } else {
            left.extend(report.findings.iter().take(6).map(|finding| {
                Line::from(vec![
                    Span::styled("• ", Style::default().fg(WARN)),
                    Span::styled(&finding.message, Style::default().fg(TEXT)),
                ])
            }));
        }
    } else {
        let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
            [(app.last_progress_tick.elapsed().as_millis() / 80 % 10) as usize];
        left.push(Line::from(vec![
            Span::styled(
                format!(" {spinner} "),
                Style::default().fg(accent()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Loading doctor…",
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ),
        ]));
        left.push(Line::from(Span::styled(
            "Auto-fetching the daemon report, cache stats, and recent logs.",
            Style::default().fg(TEXT_MUTED),
        )));
    }
    frame.render_widget(
        Paragraph::new(left)
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(SURFACE)),
        left_inner,
    );

    // ===== Right card: cache + logs + ops =====
    let right_block = card_block("Cache · Logs · Operations");
    let right_inner = right_block.inner(columns[1]);
    frame.render_widget(right_block, columns[1]);

    let mut right = Vec::new();
    if let Some(status) = &app.cache_status {
        right.push(Line::from(vec![section_chip("Cache / Index")]));
        right.push(Line::from(format!("  media items: {}", status.media_items)));
        right.push(Line::from(format!(
            "  library items: {}",
            status.library_items
        )));
        right.push(Line::from(format!("  playlists: {}", status.playlists)));
        right.push(Line::from(format!(
            "  playlist items: {}",
            status.playlist_items
        )));
        right.push(Line::from(format!(
            "  index docs: {}",
            status.index_documents
        )));
        right.push(Line::from(format!(
            "  lyrics: {} cached / {} offsets",
            status.lyrics_cache, status.lyrics_offsets
        )));
        if !status.cover_cache_path.is_empty() {
            right.push(Line::from(format!(
                "  cover cache: {} files / {} bytes",
                status.cover_cache_files, status.cover_cache_bytes
            )));
            right.push(Line::from(format!(
                "  cover ttl: {} days",
                status.cover_cache_ttl_secs / 86_400
            )));
        }
        right.push(Line::from(""));
    }

    let log_lines = app.filtered_diagnostics_logs();
    right.push(Line::from(if app.list_filter_query.is_empty() {
        vec![
            section_chip("Recent Logs"),
            Span::raw(" "),
            Span::styled(
                format!("({})", log_lines.len()),
                Style::default().fg(TEXT_MUTED),
            ),
            Span::styled("  ·  Ctrl-f filter", Style::default().fg(TEXT_MUTED)),
        ]
    } else {
        vec![
            section_chip("Recent Logs"),
            Span::raw(" "),
            Span::styled(
                format!("matching `{}`", app.list_filter_query),
                Style::default().fg(accent()),
            ),
        ]
    }));
    if log_lines.is_empty() {
        right.push(Line::from(Span::styled(
            if app.diagnostics_logs.is_empty() {
                "  no logs loaded yet — auto-fetch in progress"
            } else {
                "  no matching logs"
            },
            Style::default().fg(TEXT_MUTED),
        )));
    } else {
        let visible_count = 12usize;
        let start = app
            .selected
            .min(log_lines.len().saturating_sub(visible_count));
        let end = (start + visible_count).min(log_lines.len());
        for (offset, line) in log_lines[start..end].iter().enumerate() {
            let index = start + offset;
            right.push(format_log_line(line, index == app.selected));
        }
    }

    right.push(Line::from(""));
    right.push(Line::from(vec![
        section_chip("Operations"),
        Span::styled("  ·  u to undo selected", Style::default().fg(TEXT_MUTED)),
    ]));
    if app.operations.is_empty() {
        right.push(Line::from(Span::styled(
            "  no recorded operations yet",
            Style::default().fg(TEXT_MUTED),
        )));
    } else {
        for (i, op) in app.operations.iter().take(20).enumerate() {
            let cursor = if i == app.operations_cursor {
                Span::styled(
                    "▌ ",
                    Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw("  ")
            };
            let status_chip = match op.status.label() {
                "Confirmed" | "confirmed" | "ok" => state_chip("ok", StateRole::Active),
                "Failed" | "failed" => state_chip("fail", StateRole::Error),
                "Pending" | "pending" => state_chip("pending", StateRole::Warn),
                other => state_chip(other, StateRole::Idle),
            };
            let summary = format!(
                " {:<16}  {}",
                op.kind.label(),
                op.subject_uris.first().map_or("-", String::as_str),
            );
            right.push(Line::from(vec![
                cursor,
                status_chip,
                Span::styled(summary, Style::default().fg(TEXT)),
            ]));
        }
    }
    frame.render_widget(
        Paragraph::new(right)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(TEXT).bg(SURFACE)),
        right_inner,
    );
}

/// Parse a recent-log line for its severity prefix (`ERROR`/`WARN`/
/// `INFO`/`DEBUG`) and turn it into a coloured chip plus the rest of
/// the message. `selected = true` flips the row to inverted.
fn format_log_line(line: &str, selected: bool) -> Line<'static> {
    use crate::widgets::style::state_chip;
    let (level, role, rest) = classify_log(line);
    let chip = state_chip(level, role);
    let mut spans = vec![
        if selected {
            Span::styled(
                "▌ ",
                Style::default().fg(accent()).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw("  ")
        },
        chip,
        Span::raw(" "),
    ];
    let body_style = if selected {
        Style::default().fg(TEXT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(TEXT)
    };
    spans.push(Span::styled(rest.to_string(), body_style));
    Line::from(spans)
}

fn classify_log(line: &str) -> (&'static str, crate::widgets::style::StateRole, &str) {
    use crate::widgets::style::StateRole;
    let upper = line.to_ascii_uppercase();
    if upper.contains("ERROR") {
        ("ERR", StateRole::Error, line)
    } else if upper.contains("WARN") {
        ("WRN", StateRole::Warn, line)
    } else if upper.contains("DEBUG") {
        ("DBG", StateRole::Idle, line)
    } else if upper.contains("TRACE") {
        ("TRC", StateRole::Idle, line)
    } else {
        ("INF", StateRole::Accent, line)
    }
}

// `health_style`: replaced by `state_chip(... , StateRole::Active|Error)`.

fn render_command_palette(frame: &mut Frame<'_>, area: Rect, app: &App) {
    use crate::widgets::style::{
        button_chip, focused_card_block, key_chip, state_chip, ButtonRole, StateRole,
    };
    let area = centered_rect(82, 60, area);
    let block = focused_card_block(&format!(
        "Command Palette  ·  {}",
        app.command_palette.context.label()
    ));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

    // Input row with cursor block.
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " › ",
                Style::default().fg(accent()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(app.command_palette.input.clone(), Style::default().fg(TEXT)),
            Span::styled(
                "▍",
                Style::default().fg(accent()).add_modifier(Modifier::BOLD),
            ),
        ]))
        .style(Style::default().bg(SURFACE)),
        rows[0],
    );

    let commands = app.command_palette.visible_commands();
    let items: Vec<ListItem<'_>> = commands
        .iter()
        .map(|command| {
            ListItem::new(Line::from(vec![
                Span::raw(" "),
                key_chip(command.shortcut),
                Span::raw("  "),
                Span::styled(
                    command.label.to_string(),
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                state_chip(command.category, StateRole::Accent),
            ]))
        })
        .collect();
    let mut state = ListState::default();
    state.select(if items.is_empty() {
        None
    } else {
        Some(app.command_palette.selected.min(items.len() - 1))
    });
    frame.render_stateful_widget(
        List::new(items)
            .highlight_style(
                Style::default()
                    .fg(accent_foreground())
                    .bg(accent())
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▌")
            .style(Style::default().bg(SURFACE)),
        rows[1],
        &mut state,
    );

    // Footer with action chips.
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            button_chip("Enter run", ButtonRole::Affirm),
            Span::raw("  "),
            Span::styled("↑/↓ move", Style::default().fg(TEXT_MUTED)),
            Span::raw("  "),
            Span::styled("Esc close", Style::default().fg(TEXT_MUTED)),
        ]))
        .style(Style::default().bg(SURFACE)),
        rows[2],
    );
}

fn render_error_modal(frame: &mut Frame<'_>, area: Rect, app: &App) {
    use crate::widgets::style::{button_chip, ButtonRole};
    let Some(error) = &app.error else {
        return;
    };
    // Categorise the error so the title chip carries a glyph users
    // can recognise at a distance.
    let upper = error.to_ascii_uppercase();
    // `scope X required` is what Spotify's API returns on 403 missing
    // permissions, surfaced via `SpotifyError::Forbidden`. Match it
    // explicitly so the user sees the recovery path even when the
    // literal "403" never appears in the wrapped error.
    // True scope/auth issue: Spotify literally said "scope" or it's a
    // 401. These the user can fix with logout + login.
    let scope_drift = upper.contains("SCOPE") && upper.contains("REQUIRED");
    let is_auth = scope_drift
        || upper.contains("401")
        || upper.contains("UNAUTH")
        || upper.contains("MISSING THE")
        || upper.contains("INSUFFICIENT");
    // Spotify locked editorial / algorithmic playlists (Daily Mix,
    // Discover Weekly, Made For You, Spotify-curated mood pages, etc.)
    // behind a "first-party only" wall in Nov 2024. No scope unlocks
    // them; the endpoint just returns 403.
    let is_curated_playlist = upper.contains("403")
        && upper.contains("PLAYLISTS/")
        && (upper.contains("/TRACKS") || upper.contains("FORBIDDEN"));
    let (icon, title_chip_bg, hint) = if is_auth {
        (
            "🔒",
            DANGER,
            "Your Spotify token is missing a permission. Quit, run `spotuify logout && spotuify login`, then restart.",
        )
    } else if is_curated_playlist {
        (
            "🔒",
            WARN,
            "Spotify-curated playlists (Daily Mix, Discover Weekly, Made For You, etc.) no longer expose their tracks to third-party apps. Your own playlists still work.",
        )
    } else if upper.contains("403") || upper.contains("FORBIDDEN") {
        (
            "🔒",
            WARN,
            "Spotify refused this request. Common causes: Premium-only feature, restricted content, no active playback device. Try again with playback active.",
        )
    } else if upper.contains("411") {
        (
            "⚡",
            DANGER,
            "Spotify edge rejected the body. This is an internal bug — please file an issue.",
        )
    } else if upper.contains("5") && upper.contains("API") {
        (
            "✖",
            DANGER,
            "Spotify server error. Retry; if it persists check status.spotify.com.",
        )
    } else if upper.contains("NETWORK") || upper.contains("TIMED OUT") || upper.contains("DNS") {
        (
            "📡",
            WARN,
            "Network blip. The daemon will keep retrying in the background.",
        )
    } else {
        ("⚠", WARN, "Hit Esc to dismiss and try again.")
    };

    let area = centered_rect(72, 36, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(
            Style::default()
                .fg(title_chip_bg)
                .add_modifier(Modifier::BOLD),
        )
        .title(Span::styled(
            format!(" {icon}  Action failed "),
            Style::default()
                .fg(BG)
                .bg(title_chip_bg)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(inner);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            error.clone(),
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        )))
        .wrap(Wrap { trim: true })
        .style(Style::default().bg(SURFACE)),
        rows[1],
    );

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(TEXT_MUTED),
        )))
        .wrap(Wrap { trim: true })
        .style(Style::default().bg(SURFACE)),
        rows[2],
    );

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            button_chip("Esc dismiss", ButtonRole::Cancel),
            Span::raw("   "),
            Span::styled("? help", Style::default().fg(TEXT_MUTED)),
        ]))
        .style(Style::default().bg(SURFACE)),
        rows[3],
    );
}

fn render_media_list(
    frame: &mut Frame<'_>,
    title: String,
    items: &[MediaItem],
    selected: usize,
    app: &App,
    area: Rect,
    register_hits: bool,
) {
    if items.is_empty() {
        let message = empty_media_state(app);
        frame.render_widget(
            Paragraph::new(message)
                .block(panel_block(&title))
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(TEXT_MUTED).bg(SURFACE)),
            area,
        );
        return;
    }
    let now_playing_uri = app.playback.item.as_ref().map(|i| i.uri.as_str());
    let rows = items
        .iter()
        .map(|item| {
            media_item_with(
                item,
                app.marked_uris.contains(&item.uri),
                now_playing_uri == Some(item.uri.as_str()),
                app.palette.now_playing_rail,
            )
        })
        .collect::<Vec<_>>();
    let list = List::new(rows)
        .block(panel_block(&title))
        .highlight_style(
            // The soft accent keeps the family but stops the selection
            // chip from looking like a second seeker bar.
            Style::default()
                .fg(TEXT)
                .bg(app.palette.soft_accent)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(" ")
        .style(Style::default().bg(SURFACE));
    let mut state = ListState::default();
    state.select(if items.is_empty() || selected >= items.len() {
        None
    } else {
        Some(selected)
    });
    frame.render_stateful_widget(list, area, &mut state);
    if register_hits {
        // The list draws inside its own panel block.
        let inner = panel_block(&title).inner(area);
        register_row_hits(app, inner, state.offset(), items.len(), 2, Some);
    }
}

fn render_playlist_list(
    frame: &mut Frame<'_>,
    app: &App,
    playlists: &[Playlist],
    selected: usize,
    area: Rect,
) {
    use crate::widgets::style::card_block;
    if playlists.is_empty() {
        let block = card_block("Playlists");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(
                        " ⠋ ",
                        Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        "Fetching playlists…",
                        Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(Span::styled(
                    "Auto-refreshes on auth; stays cached after the first sync.",
                    Style::default().fg(TEXT_MUTED),
                )),
            ])
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(SURFACE)),
            inner,
        );
        return;
    }
    // Tabular layout so the right side of the screen isn't dead space.
    // Columns: art marker · name · owner · track count. A blank spacer
    // row between playlists gives the same breathing room the devices
    // table uses without making every row a 2-line stack.
    let table_rows: Vec<ratatui::widgets::Row<'_>> = playlists
        .iter()
        .flat_map(|playlist| {
            let marker = if playlist.image_url.is_some() {
                Span::styled(
                    " ▣ ",
                    Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(" ▢ ", Style::default().fg(TEXT_MUTED))
            };
            let row = ratatui::widgets::Row::new(vec![
                ratatui::widgets::Cell::from(Line::from(marker)),
                ratatui::widgets::Cell::from(Line::from(Span::styled(
                    playlist.name.clone(),
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                ))),
                ratatui::widgets::Cell::from(Line::from(Span::styled(
                    playlist.owner.clone(),
                    Style::default().fg(TEXT_MUTED),
                ))),
                ratatui::widgets::Cell::from(Line::from(Span::styled(
                    format!("{} tracks", playlist.tracks_total),
                    Style::default().fg(TEXT_MUTED),
                ))),
            ]);
            [row, ratatui::widgets::Row::new(Vec::<&str>::new())]
        })
        .collect();
    let table = ratatui::widgets::Table::new(
        table_rows,
        [
            Constraint::Length(4),
            Constraint::Min(20),
            Constraint::Length(28),
            Constraint::Length(14),
        ],
    )
    .block(card_block(&format!(
        "Playlists  {}  ·  Enter open · e enqueue · a add",
        playlists.len()
    )))
    .row_highlight_style(
        Style::default()
            .fg(accent_foreground())
            .bg(accent())
            .add_modifier(Modifier::BOLD),
    )
    .style(Style::default().bg(SURFACE));
    let mut state = ratatui::widgets::TableState::default();
    // Each playlist occupies two table rows (content + spacer). The
    // selection state must point at the content row so the highlight
    // lands on the right line.
    state.select(if playlists.is_empty() {
        None
    } else {
        Some(selected.min(playlists.len() - 1) * 2)
    });
    frame.render_stateful_widget(table, area, &mut state);
    // Hit map: each playlist spans its content row + spacer row; the
    // table's offset is in TABLE rows (2 per playlist).
    let inner = card_block("x").inner(area);
    let table_offset = state.offset();
    {
        let mut map = app.hit_map.borrow_mut();
        for (index, _) in playlists.iter().enumerate() {
            let table_row = index * 2;
            if table_row + 1 < table_offset {
                continue;
            }
            let offset_rows = (table_row.saturating_sub(table_offset)) as u16;
            if offset_rows >= inner.height {
                break;
            }
            let height = 2u16.min(inner.height - offset_rows);
            map.push(
                Rect::new(inner.x, inner.y + offset_rows, inner.width, height),
                crate::hit::HitTarget::Row { index },
            );
        }
    }
}

fn split_art_preview_area(area: Rect, artwork: Option<&ArtworkSubject>) -> (Rect, Option<Rect>) {
    if artwork.is_none() || area.width < 92 || area.height < 9 {
        return (area, None);
    }
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(42),
            Constraint::Length(1),
            Constraint::Length(30),
        ])
        .split(area);
    (columns[0], Some(columns[2]))
}

fn render_artwork_preview(
    frame: &mut Frame<'_>,
    app: &mut App,
    subject: &ArtworkSubject,
    area: Rect,
) {
    use crate::widgets::style::card_block;
    let block = card_block("Artwork");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let text_height = inner.height.min(4);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(text_height)])
        .split(inner);
    let art_area = cover_art_rect(rows[0]);

    if subject.image_url.is_some() {
        if let Some(cover) = app.selected_art_cover.as_mut() {
            let image = StatefulImage::default();
            frame.render_stateful_widget(image, art_area, cover);
            if let Some(Err(err)) = cover.last_encoding_result() {
                app.error = Some(err.to_string());
            }
        } else {
            frame.render_widget(
                crate::widgets::album_art::GradientArt::new(&subject.uri)
                    .with_label(subject.label.clone()),
                art_area,
            );
        }
    } else {
        frame.render_widget(
            crate::widgets::album_art::GradientArt::new(&subject.uri)
                .with_label(subject.label.clone()),
            art_area,
        );
    }

    let text_width = rows[1].width.saturating_sub(2) as usize;
    let status = if subject.image_url.is_some() {
        "cover from Spotify"
    } else {
        "generated fallback"
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                truncate(&subject.title, text_width),
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                truncate(&subject.subtitle, text_width),
                Style::default().fg(TEXT_MUTED),
            )),
            Line::from(Span::styled(
                truncate(&subject.detail, text_width),
                Style::default().fg(TEXT_MUTED),
            )),
            Line::from(Span::styled(status, Style::default().fg(accent()))),
        ])
        .wrap(Wrap { trim: true })
        .style(Style::default().bg(SURFACE)),
        rows[1],
    );
}

fn empty_media_state(app: &App) -> Vec<Line<'static>> {
    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
        [(app.last_progress_tick.elapsed().as_millis() / 80 % 10) as usize];
    let spinner_owned = spinner.to_string();
    match app.screen {
        Screen::Search if app.is_searching => vec![
            Line::from(vec![
                Span::styled(format!(" {spinner_owned} "), Style::default().fg(accent()).add_modifier(Modifier::BOLD)),
                Span::styled(
                    "Searching Spotify and local cache…",
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(Span::styled(
                "Local matches surface first; remote results stream in.",
                Style::default().fg(TEXT_MUTED),
            )),
        ],
        Screen::Search => vec![
            Line::from(Span::styled(
                "Ready to search.",
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Press / and type an artist, song, album, or playlist.",
                Style::default().fg(accent()),
            )),
            Line::from(Span::styled(
                "Once results land: g t/r/b/p/s/e jumps to Tracks/Artists/Albums/Playlists/Shows/Episodes.",
                Style::default().fg(TEXT_MUTED),
            )),
        ],
        Screen::Library => vec![
            Line::from(vec![
                Span::styled(format!(" {spinner_owned} "), Style::default().fg(accent()).add_modifier(Modifier::BOLD)),
                Span::styled(
                    "Fetching your library…",
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(Span::styled(
                "It refreshes automatically and stays cached.",
                Style::default().fg(TEXT_MUTED),
            )),
        ],
        Screen::Queue => vec![
            if !app.queue.session_active {
                Line::from(Span::styled(
                    "No active Spotify session.",
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(Span::styled(
                    "Queue is empty.",
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                ))
            },
            if !app.queue.session_active {
                Line::from(Span::styled(
                    "Start playback from Search or Library to load a live queue.",
                    Style::default().fg(accent()),
                ))
            } else {
                Line::from(Span::styled(
                    "Press `e` on any track or album to enqueue.",
                    Style::default().fg(accent()),
                ))
            },
        ],
        Screen::Playlists if app.selected_playlist_id.is_some() => vec![
            Line::from(vec![
                Span::styled(format!(" {spinner_owned} "), Style::default().fg(accent()).add_modifier(Modifier::BOLD)),
                Span::styled(
                    "Loading tracks…",
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(Span::styled(
                "Press b to go back.",
                Style::default().fg(TEXT_MUTED),
            )),
        ],
        Screen::Player if !app.queue.session_active => vec![
            Line::from(Span::styled(
                "Fetching your Home feed.",
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Saved songs, albums, podcasts, and recent plays appear here.",
                Style::default().fg(accent()),
            )),
            Line::from(Span::styled(
                "Use Search while the cache warms up.",
                Style::default().fg(TEXT_MUTED),
            )),
        ],
        Screen::Player => vec![
            Line::from(Span::styled(
                "Home feed is empty.",
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Use Search or Library to start something.",
                Style::default().fg(accent()),
            )),
        ],
        _ => vec![Line::from(Span::styled(
            "Nothing here yet.",
            Style::default().fg(TEXT_MUTED),
        ))],
    }
}

fn render_status(frame: &mut Frame<'_>, app: &App, area: Rect) {
    // Status area is 3 rows: top border, ephemeral message row, hint
    // chip row. The hint row is ALWAYS rendered with the current
    // contextual shortcuts so the user can find the next action no
    // matter what else is happening above.
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(BORDER_STRONG));
    let inner = block.inner(area);
    frame.render_widget(Block::default().style(Style::default().bg(BG)), area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(inner);

    render_ephemeral_status(frame, app, rows[0]);
    render_hint_bar(frame, app, rows[1]);
}

fn render_ephemeral_status(frame: &mut Frame<'_>, app: &App, area: Rect) {
    use crate::widgets::style::{key_chip, state_chip, StateRole};
    // Real banners win; otherwise surface a pending in-place upgrade as a
    // synthetic `UpdateAvailable` banner so it never clobbers an Auth or
    // rate-limit notice.
    let active_banner: Option<BannerState> = app
        .banner
        .clone()
        .or_else(|| app.update_available.then_some(BannerState::UpdateAvailable));
    if let Some(banner) = &active_banner {
        let (text, color) = banner_message(banner);
        let (icon, role) = match banner {
            BannerState::Auth { .. } => ("🔒", StateRole::Error),
            BannerState::RateLimited { .. } => ("⏱", StateRole::Warn),
            BannerState::Compat { .. } | BannerState::Deprecated { .. } => ("ⓘ", StateRole::Warn),
            BannerState::UpdateAvailable => ("⟳", StateRole::Warn),
            BannerState::UpgradeAvailable { .. } => ("⤓", StateRole::Warn),
            BannerState::AuthMigration { .. } => ("🔑", StateRole::Warn),
        };
        // Build a single line: severity chip · message · action chip
        // (when the banner names a recovery key).
        let mut spans: Vec<Span<'static>> = vec![
            state_chip(icon, role),
            Span::raw("  "),
            Span::styled(
                text,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
        ];
        if matches!(
            banner,
            BannerState::Auth {
                kind: spotuify_protocol::AuthErrorKind::ScopeReauthRequired
            }
        ) {
            spans.push(Span::raw("  "));
            spans.push(key_chip("R"));
            spans.push(Span::styled(" re-auth", Style::default().fg(TEXT_MUTED)));
        }
        if matches!(banner, BannerState::UpdateAvailable) {
            spans.push(Span::raw("  "));
            spans.push(key_chip("R"));
            spans.push(Span::styled(" restart", Style::default().fg(TEXT_MUTED)));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(BG)),
            area,
        );
        return;
    }
    if !app.pending_receipts.is_empty() {
        let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
            [(app.last_progress_tick.elapsed().as_millis() / 80 % 10) as usize];
        let first = &app.pending_receipts[0].action;
        let len = app.pending_receipts.len();
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" {spinner} "),
                    Style::default()
                        .fg(WARN)
                        .bg(BG)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("{len} pending: {first}"), Style::default().fg(WARN)),
            ]))
            .style(Style::default().bg(BG)),
            area,
        );
        return;
    }
    if let Some(toast) = &app.toast {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " ✓ ",
                    Style::default()
                        .fg(accent_foreground())
                        .bg(accent())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(toast.clone(), Style::default().fg(accent())),
            ]))
            .style(Style::default().bg(BG)),
            area,
        );
        return;
    }
    if app.is_syncing {
        let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
            [(app.last_progress_tick.elapsed().as_millis() / 80 % 10) as usize];
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" {spinner} "),
                    Style::default()
                        .fg(accent())
                        .bg(BG)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "Syncing Spotify… Ctrl+C quits",
                    Style::default().fg(accent()),
                ),
            ]))
            .style(Style::default().bg(BG)),
            area,
        );
        return;
    }
    // No ephemeral message: leave the row blank but keep the area
    // background consistent so the layout doesn't shift.
    frame.render_widget(Paragraph::new("").style(Style::default().bg(BG)), area);
}

fn render_hint_bar(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let mut hints =
        crate::tui_actions::top_hints(app.current_action_context(), app.selected_count());
    // Filter out actions that don't apply to the focused item's kind
    // — e.g. queue/like/add-to-playlist don't work on an Artist URI.
    let focused_kind = current_focused_kind(app);
    hints.retain(|hint| action_applies_to_kind(hint.id, focused_kind.as_ref()));
    if hints.is_empty() {
        return;
    }
    // Width-aware: `top_hints` returns the context's full keymap in
    // priority order with Help last. Fit as many as the row allows,
    // always reserving room for the trailing "? Help" so the complete
    // keymap stays one keypress away even on narrow terminals.
    let hint_width = |hint: &crate::tui_actions::ActionSpec| -> u16 {
        // key chip " X " (shortcut + 2) + space + label.
        (hint.shortcut.chars().count() + 3 + hint.label.chars().count()) as u16
    };
    const SEPARATOR_WIDTH: u16 = 3; // " · "
    let help = hints.pop().expect("hints checked non-empty above");
    let help_cost = hint_width(&help) + SEPARATOR_WIDTH;
    let mut budget = area.width.saturating_sub(1); // leading space
    let mut fitted: Vec<crate::tui_actions::ActionSpec> = Vec::new();
    for hint in hints {
        let cost = hint_width(&hint)
            + if fitted.is_empty() {
                0
            } else {
                SEPARATOR_WIDTH
            };
        if budget < cost + help_cost {
            break;
        }
        budget -= cost;
        fitted.push(hint);
    }
    fitted.push(help);

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(fitted.len() * 4);
    spans.push(Span::raw(" "));
    for (idx, hint) in fitted.into_iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(BORDER_STRONG)));
        }
        spans.push(crate::widgets::style::key_chip(hint.shortcut));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            hint.label.to_string(),
            Style::default().fg(TEXT_MUTED),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(BG)),
        area,
    );
}

/// The MediaKind of whatever row the cursor is on right now (search
/// results / library / queue / playlist tracks). Returns None when
/// the active surface has no selectable items.
fn current_focused_kind(app: &App) -> Option<MediaKind> {
    // One source of truth: `selected` indexes the filtered/sorted
    // visible list. Indexing the raw backing arrays here made the hint
    // bar filter actions against the WRONG row's kind whenever a
    // search sort or filter was active. Screens without selectable
    // items return an empty visible list -> None, same as before.
    app.selected_visible_item().map(|item| item.kind)
}

/// Does this action make sense for the given selected-item kind?
/// Artists, shows, and (to a lesser extent) playlists don't support
/// the same mutations as tracks — e.g. you can't enqueue an artist URI
/// against Spotify's `/me/player/queue` and you can't add an artist to
/// a playlist.
fn action_applies_to_kind(action: crate::tui_actions::TuiAction, kind: Option<&MediaKind>) -> bool {
    use crate::tui_actions::TuiAction;
    let Some(kind) = kind else {
        return true;
    };
    match action {
        TuiAction::QueueSelection => matches!(kind, MediaKind::Track | MediaKind::Episode),
        TuiAction::AddSelectionToPlaylist => {
            matches!(kind, MediaKind::Track | MediaKind::Episode)
        }
        // Like uses /me/library for saved items and /me/following for
        // artists. All valid.
        TuiAction::LikeSelection => true,
        _ => true,
    }
}

pub(crate) fn auth_banner_message(kind: spotuify_protocol::AuthErrorKind) -> String {
    use spotuify_protocol::AuthErrorKind;
    match kind {
        AuthErrorKind::NotLoggedIn => "No Spotify login found. Press Enter to log in.".to_string(),
        AuthErrorKind::ScopeReauthRequired => {
            "Spotify permissions out of date. Quit, run `spotuify logout && spotuify login`, then restart."
                .to_string()
        }
        AuthErrorKind::ExpiredRefresh => {
            "Spotify refresh token expired. Run `spotuify login`.".to_string()
        }
        AuthErrorKind::InvalidGrant => {
            "Spotify auth rejected. Run `spotuify logout && spotuify login`.".to_string()
        }
        AuthErrorKind::Forbidden => {
            "Spotify denied the request (forbidden). Run `spotuify login` to refresh permissions."
                .to_string()
        }
    }
}

fn banner_message(banner: &BannerState) -> (String, Color) {
    match banner {
        BannerState::RateLimited {
            retry_after_secs,
            scope,
        } => (
            format!("rate limited on {scope}; retrying in {retry_after_secs}s"),
            WARN,
        ),
        BannerState::Auth { kind } => (auth_banner_message(*kind), DANGER),
        BannerState::Deprecated { endpoint } => (
            format!("Spotify removed {endpoint}; using fallback where possible"),
            WARN,
        ),
        BannerState::Compat { endpoint } => (
            format!("Spotify changed {endpoint}; local compatibility applied"),
            WARN,
        ),
        BannerState::UpdateAvailable => (
            "Update installed — restart daemon to apply".to_string(),
            accent(),
        ),
        BannerState::UpgradeAvailable {
            latest_version,
            action,
        } => (
            format!("spotuify {latest_version} available · {action}"),
            accent(),
        ),
        BannerState::AuthMigration { can_login_dev_app } => {
            let message = if *can_login_dev_app {
                "First-party auth is rate-limited by Spotify — run `spotuify login --dev-app` to switch"
            } else {
                "First-party auth is rate-limited by Spotify — run `spotuify onboard` to switch"
            };
            (message.to_string(), WARN)
        }
    }
}

fn render_help(frame: &mut Frame<'_>, area: Rect, app: &App) {
    use crate::widgets::style::{card_block, key_chip, section_chip};
    let area = centered_rect(82, 70, area);
    let block = card_block("Help  ·  ? toggle  ·  Ctrl-p commands  ·  Esc close");
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(inner);

    // Search box at the top.
    let cursor_glyph = if app.help_query.is_empty() { "▍" } else { "" };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Type to filter shortcuts and FAQs:",
                Style::default().fg(TEXT_MUTED),
            )),
            Line::from(vec![
                Span::styled(
                    " / ",
                    Style::default()
                        .fg(accent_foreground())
                        .bg(accent())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(app.help_query.clone(), Style::default().fg(TEXT)),
                Span::styled(
                    cursor_glyph,
                    Style::default().fg(accent()).add_modifier(Modifier::BOLD),
                ),
            ]),
        ])
        .style(Style::default().bg(SURFACE)),
        rows[0],
    );

    let faqs: Vec<(&str, &str)> = vec![
        (
            "play a playlist",
            "Press 4, pick a playlist, Enter, then Enter on a track",
        ),
        ("search", "Press /, type a query, Enter"),
        (
            "queue multiple tracks",
            "Mark with m, then press e to append",
        ),
        ("replace vs append", "Enter replaces the queue, e appends"),
        ("no active device", "Press 6 for Devices, Enter to transfer"),
        (
            "re-authorize Spotify",
            "spotuify logout && spotuify login, then restart",
        ),
    ];
    let actions =
        crate::tui_actions::actions_for_context(app.current_action_context(), app.selected_count());
    let query = app.help_query.to_ascii_lowercase();
    let matches_query = |a: &str, b: &str| {
        query.is_empty()
            || a.to_ascii_lowercase().contains(&query)
            || b.to_ascii_lowercase().contains(&query)
    };

    // Build two columns: FAQs on the left, shortcuts on the right.
    let body_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[1]);

    let mut left_lines: Vec<Line<'_>> = vec![Line::from(vec![section_chip("FAQ")])];
    for (q, ans) in &faqs {
        if matches_query(q, ans) {
            left_lines.push(Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    format!("How do I {q}?"),
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                ),
            ]));
            left_lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(ans.to_string(), Style::default().fg(TEXT_MUTED)),
            ]));
            left_lines.push(Line::from(""));
        }
    }
    frame.render_widget(
        Paragraph::new(left_lines)
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(SURFACE)),
        body_cols[0],
    );

    let mut right_lines: Vec<Line<'_>> = vec![Line::from(vec![section_chip("Shortcuts")])];
    for action in actions {
        if matches_query(action.shortcut, action.label) {
            right_lines.push(Line::from(vec![
                Span::raw(" "),
                key_chip(action.shortcut),
                Span::raw(" "),
                Span::styled(action.label.to_string(), Style::default().fg(TEXT)),
            ]));
            if let Some(cli) = action.cli {
                right_lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(format!("CLI: {cli}"), Style::default().fg(TEXT_MUTED)),
                ]));
            }
        }
    }
    frame.render_widget(
        Paragraph::new(right_lines)
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(SURFACE)),
        body_cols[1],
    );
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn media_item(item: &MediaItem, marked: bool) -> ListItem<'static> {
    media_item_with(
        item,
        marked,
        false,
        crate::widgets::style::UiPalette::DEFAULT.now_playing_rail,
    )
}

fn media_item_with(
    item: &MediaItem,
    marked: bool,
    now_playing: bool,
    now_playing_rail: Color,
) -> ListItem<'static> {
    // Row 1: rail · marker · kind glyph · name (bold) · duration
    // Row 2: rail · aligned indent · artist · album/context
    // The now-playing row gets a coloured vertical rail down its
    // left edge (`▌` is the half-block, which renders as a thin
    // vertical band in most terminals); that, plus the slightly
    // tinted background, makes the live row identifiable even when
    // the user has selected a different row above or below it.
    // Marker priority: now-playing ▶ > marked ● > nothing.
    let rail = if now_playing {
        Span::styled(
            "▌",
            Style::default()
                .fg(now_playing_rail)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw(" ")
    };
    let marker = if now_playing {
        Span::styled(
            "▶",
            Style::default().fg(accent()).add_modifier(Modifier::BOLD),
        )
    } else if marked {
        Span::styled(
            "●",
            Style::default().fg(accent()).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw(" ")
    };
    let duration = if item.duration_ms > 0 {
        format!("  {}", fmt_ms(item.duration_ms))
    } else {
        String::new()
    };
    let name_style = if now_playing {
        Style::default().fg(accent()).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(TEXT).add_modifier(Modifier::BOLD)
    };
    let row_style = Style::default().bg(SURFACE);
    ListItem::new(vec![
        Line::from(vec![
            rail.clone(),
            Span::raw(" "),
            marker,
            Span::raw(" "),
            Span::styled(
                kind_icon(&item.kind),
                Style::default()
                    .fg(kind_color(&item.kind))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(item.name.clone(), name_style),
            Span::styled(duration, Style::default().fg(TEXT_MUTED)),
        ])
        .style(row_style),
        Line::from(vec![
            rail,
            Span::raw("      "),
            Span::styled(item.subtitle.clone(), Style::default().fg(TEXT_MUTED)),
            Span::styled(context_suffix(item), Style::default().fg(TEXT_MUTED)),
        ])
        .style(row_style),
    ])
}

// `device_row`: replaced by the inline ListItem rendering in
// `render_devices` so each row carries a kind icon, state chip,
// and volume bar.

fn panel_block(title: &str) -> Block<'_> {
    // Legacy block; new screens should use `widgets::style::card_block`
    // (which adds the adaptive accent title chip). This helper reuses the
    // shared BORDER_STRONG palette so the two block styles read as one
    // family instead of competing.
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_set(symbols::border::ROUNDED)
        .border_style(Style::default().fg(BORDER_STRONG))
        .style(Style::default().bg(SURFACE))
}

// `key_style`, `toggle_style`, and `hint_text` were removed: every
// caller now goes through `widgets::style::{key_chip, state_chip,
// section_chip, button_chip}` so the chip palette is the single
// source of truth.

pub fn kind_icon(kind: &MediaKind) -> &'static str {
    match kind {
        MediaKind::Track => "♪",
        MediaKind::Episode => "◉",
        MediaKind::Show => "◎",
        MediaKind::Album => "▣",
        MediaKind::Artist => "★",
        MediaKind::Playlist => "≡",
    }
}

fn kind_color(kind: &MediaKind) -> Color {
    match kind {
        MediaKind::Track => accent(),
        MediaKind::Episode => KIND_PODCAST,
        MediaKind::Show => KIND_PODCAST,
        MediaKind::Album => KIND_ALBUM,
        MediaKind::Artist => KIND_ARTIST,
        MediaKind::Playlist => WARN,
    }
}

fn context_suffix(item: &MediaItem) -> String {
    let mut parts = Vec::new();
    // Prefer the dedicated album field (tracks); fall back to the context label.
    let album = item
        .album
        .as_deref()
        .filter(|value| !value.is_empty())
        .unwrap_or(item.context.as_str());
    if !album.is_empty() {
        parts.push(album.to_string());
    }
    // Episode listened state.
    if item.kind == MediaKind::Episode {
        if item.fully_played == Some(true) {
            parts.push("✓ played".to_string());
        } else if let Some(resume) = item.resume_position_ms.filter(|ms| *ms > 0) {
            parts.push(format!(
                "{} left",
                fmt_ms(item.duration_ms.saturating_sub(resume))
            ));
        }
    }
    if item.duration_ms > 0 {
        parts.push(fmt_ms(item.duration_ms));
    }
    if let Some(added) = item.added_at_ms.and_then(fmt_added_date) {
        parts.push(added);
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("  {}", parts.join(" · "))
    }
}

/// Short "added" date label (e.g. `added Mar 2024`) for track rows.
fn fmt_added_date(ms: i64) -> Option<String> {
    if ms <= 0 {
        return None;
    }
    chrono::DateTime::from_timestamp_millis(ms).map(|dt| dt.format("added %b %Y").to_string())
}

fn area_title(title: &str, count: usize) -> String {
    format!("{title} {count} ")
}

fn progress_ratio(progress_ms: u64, duration_ms: u64) -> f64 {
    if duration_ms == 0 {
        0.0
    } else {
        (progress_ms as f64 / duration_ms as f64).clamp(0.0, 1.0)
    }
}

fn fmt_ms(ms: u64) -> String {
    let total = ms / 1_000;
    let minutes = total / 60;
    let seconds = total % 60;
    format!("{minutes}:{seconds:02}")
}

fn device_name(app: &App) -> String {
    app.playback
        .device
        .as_ref()
        .map_or_else(|| "no device".to_string(), |device| device.name.clone())
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut out = value
        .chars()
        .take(max.saturating_sub(1))
        .collect::<String>();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui_actions::CommandPalette;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use ratatui_image::picker::Picker;
    use std::collections::HashSet;

    #[test]
    fn tab_strip_shows_all_tabs_full_labels_when_wide() {
        let (_, ranges) = tab_strip_layout(0, 200);
        assert_eq!(ranges.len(), Screen::ALL.len());
        let strip_end = ranges.last().expect("ranges").1.end;
        assert!(strip_end <= 200);
    }

    #[test]
    fn tab_strip_degrades_but_keeps_all_tabs_at_medium_width() {
        // 116 usable cols (120-col terminal minus chrome) can't fit full
        // labels, but the short-label mode keeps every tab reachable.
        let (_, ranges) = tab_strip_layout(0, 116);
        assert_eq!(ranges.len(), Screen::ALL.len());
        assert!(ranges.last().expect("ranges").1.end <= 116);
    }

    #[test]
    fn tab_strip_windows_around_selected_when_very_narrow() {
        // 40 cols can't show ten tabs even short-labelled; the window
        // must still contain the selected tab and stay inside width.
        for selected in [0, 5, Screen::ALL.len() - 1] {
            let (_, ranges) = tab_strip_layout(selected, 40);
            assert!(
                ranges.iter().any(|(index, _)| *index == selected),
                "selected tab {selected} must stay visible"
            );
            assert!(ranges.last().expect("ranges").1.end <= 40);
        }
    }

    #[test]
    fn tab_strip_ranges_are_disjoint_and_ordered() {
        let (_, ranges) = tab_strip_layout(3, 90);
        for pair in ranges.windows(2) {
            assert!(pair[0].1.end <= pair[1].1.start);
        }
    }

    #[test]
    fn now_playing_layout_collapse_order() {
        let inner = |w| Rect::new(0, 0, w, 8);
        // Wide: all three regions.
        let full = now_playing_layout(inner(120));
        assert!(full.cover.is_some() && full.track.is_some());
        assert_eq!(full.transport.width, TRANSPORT_FULL_WIDTH);
        // Cover drops first; transport keeps full width.
        let mid = now_playing_layout(inner(80));
        assert!(mid.cover.is_none() && mid.track.is_some());
        assert_eq!(mid.transport.width, TRANSPORT_FULL_WIDTH);
        // Then transport compacts.
        let narrow = now_playing_layout(inner(50));
        assert!(narrow.track.is_some());
        assert!(narrow.compact_transport);
        assert_eq!(narrow.transport.width, TRANSPORT_COMPACT_WIDTH);
        // Finally the track panel goes; controls own the row.
        let tiny = now_playing_layout(inner(30));
        assert!(tiny.track.is_none());
        assert_eq!(tiny.transport.width, 30);
    }

    #[test]
    fn compact_transport_toggle_ranges_fit_compact_width() {
        let ranges = transport_toggle_ranges("context", true, true, true);
        assert!(ranges[2].end < TRANSPORT_COMPACT_WIDTH);
    }

    #[test]
    fn wrapped_row_count_estimates_wrapping() {
        assert_eq!(wrapped_row_count("", 10), 1); // empty line still takes a row
        assert_eq!(wrapped_row_count("hello", 10), 1); // fits
        assert_eq!(wrapped_row_count("0123456789", 10), 1); // exact width
        assert_eq!(wrapped_row_count("01234567890", 10), 2); // one over → 2 rows
        assert_eq!(wrapped_row_count("anything", 0), 1); // zero width guard
    }
    use std::time::Instant;

    #[test]
    fn scope_reauth_banner_message_names_the_logout_login_recovery_path() {
        // User shouldn't have to guess what to do. The banner names the
        // exact CLI invocation they need to run.
        let msg = auth_banner_message(spotuify_protocol::AuthErrorKind::ScopeReauthRequired);
        assert!(
            msg.contains("logout"),
            "ScopeReauthRequired banner should tell the user to logout"
        );
        assert!(
            msg.contains("login"),
            "ScopeReauthRequired banner should tell the user to login"
        );
        assert!(
            msg.contains("permissions") || msg.contains("scopes"),
            "ScopeReauthRequired banner should explain it's a permissions issue, not a generic error"
        );
    }

    #[test]
    fn expired_refresh_banner_message_directs_user_to_login_command() {
        let msg = auth_banner_message(spotuify_protocol::AuthErrorKind::ExpiredRefresh);
        assert!(msg.contains("login"), "ExpiredRefresh should mention login");
    }

    #[test]
    fn forbidden_banner_message_directs_user_to_login_command() {
        let msg = auth_banner_message(spotuify_protocol::AuthErrorKind::Forbidden);
        assert!(msg.contains("login"), "Forbidden should mention login");
    }

    #[test]
    fn invalid_grant_banner_message_directs_user_to_logout_then_login() {
        let msg = auth_banner_message(spotuify_protocol::AuthErrorKind::InvalidGrant);
        assert!(
            msg.contains("logout") && msg.contains("login"),
            "InvalidGrant should walk the user through logout + login"
        );
    }

    #[test]
    fn not_logged_in_banner_message_directs_user_to_modal_login() {
        let msg = auth_banner_message(spotuify_protocol::AuthErrorKind::NotLoggedIn);
        assert!(
            msg.contains("Press Enter") && msg.contains("log in"),
            "NotLoggedIn should match the auto-open login modal path"
        );
    }

    #[test]
    fn singalong_active_line_skips_blank_provider_rows() {
        let lines = vec![
            spotuify_core::LyricLine {
                start_ms: 0,
                text: "Alors on danse".to_string(),
                is_rtl: false,
            },
            spotuify_core::LyricLine {
                start_ms: 1_000,
                text: String::new(),
                is_rtl: false,
            },
            spotuify_core::LyricLine {
                start_ms: 2_000,
                text: "Qui dit etude dit travail".to_string(),
                is_rtl: false,
            },
        ];

        assert_eq!(active_singalong_lyric_line_index(&lines, 1_500, 0), Some(0));
        assert_eq!(active_singalong_lyric_line_index(&lines, 2_500, 0), Some(2));
    }

    fn test_app() -> App {
        App {
            playback: spotuify_spotify::client::Playback::default(),
            queue: spotuify_spotify::client::Queue::default(),
            devices: Vec::new(),
            playlists: Vec::new(),
            inaccessible_playlist_ids: std::collections::HashSet::new(),
            last_played: None,
            recent_items: Vec::new(),
            library_items: Vec::new(),
            playlist_tracks: Vec::new(),
            search_results: Vec::new(),
            search_version: 0,
            search_panes: std::collections::HashMap::new(),
            search_user_steered: false,
            is_searching: false,
            action_in_flight: false,
            screen: Screen::Search,
            search_query: String::new(),
            search_input_active: false,
            list_filter_query: String::new(),
            list_filter_active: false,
            selected: 0,
            playlist_selected: 0,
            selected_playlist_id: None,
            selected_playlist_name: None,
            toast: None,
            notifications: Vec::new(),
            reminders: Vec::new(),
            history_sessions: Vec::new(),
            history_loading: false,
            history_error: None,
            search_sort: spotuify_protocol::SearchSortData::Relevance,
            search_kind_filter: None,
            error: None,
            last_progress_tick: Instant::now(),
            awaiting_track_change_until: None,
            current_art_url: None,
            cover: None,
            palette: crate::widgets::style::UiPalette::default(),
            selected_art_url: None,
            selected_art_cover: None,
            playback_updated_at: None,
            queue_updated_at: None,
            devices_updated_at: None,
            playback_known: true,
            started_at: Instant::now(),
            auth_revoked_observed: false,
            pending_auth_modal_until: None,
            picker: Picker::halfblocks(),
            spotifyd_status: None,
            is_syncing: false,
            last_sync: None,
            last_library_sync: None,
            show_help: false,
            help_query: String::new(),
            command_palette: CommandPalette::default(),
            marked_uris: HashSet::new(),
            mark_anchor: None,
            player_large: true,
            right_rail: RightRailMode::Hidden,
            fullscreen_panel: None,
            viz_enabled: false,
            viz_configured_source: spotuify_protocol::VizSourceKindData::Auto,
            viz_active_source: spotuify_protocol::VizActiveSource::None,
            spectrum_bands: [0.0; 12],
            spectrum_peak: 0.0,
            viz_color_scheme: "spotify-green".to_string(),
            viz_last_frame_at: None,
            viz_hint: None,
            viz_backend_kind: None,
            diagnostics_report: None,
            cache_status: None,
            diagnostics_logs: Vec::new(),
            lyrics: None,
            lyrics_track_uri: None,
            lyrics_failed_track_uri: None,
            lyrics_offset_ms: 0,
            lyrics_loading: false,
            lyrics_error: None,
            confirm_modal: None,
            playlist_picker: None,
            device_picker: None,
            audio_output_picker: None,
            reminder_picker: None,
            login_modal: None,
            operations: Vec::new(),
            operations_cursor: 0,
            pending_receipts: Vec::new(),
            banner: None,
            binary_fingerprint: None,
            update_available: false,
            artist_view: None,
            refresh_requested: false,
            pending_g: false,
            hit_map: std::cell::RefCell::new(crate::hit::HitMap::default()),
        }
    }

    #[test]
    fn now_playing_chrome_uses_dynamic_palette_roles() {
        let mut app = test_app();
        app.palette = crate::widgets::style::UiPalette {
            accent: Color::Indexed(160),
            brand: Color::Indexed(160),
            soft_accent: Color::Indexed(52),
            background: Color::Indexed(17),
            foreground: Color::Indexed(231),
            now_playing_rail: Color::Indexed(209),
        };
        let mut terminal = Terminal::new(TestBackend::new(90, PLAYER_HEIGHT)).expect("terminal");
        terminal
            .draw(|frame| render_now_playing(frame, &mut app, frame.area()))
            .expect("draw");
        let buf = terminal.backend().buffer();
        let area = buf.area();
        let mut saw_title_chip = false;
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = &buf[(x, y)];
                saw_title_chip |= cell.bg == Color::Indexed(160) && cell.fg == Color::Indexed(231);
            }
        }
        assert!(saw_title_chip);
        assert_eq!(buf[(0, 0)].fg, Color::Indexed(209));
    }

    fn item(uri: &str, name: &str) -> MediaItem {
        item_kind(uri, name, MediaKind::Track)
    }

    fn item_kind(uri: &str, name: &str, kind: MediaKind) -> MediaItem {
        MediaItem {
            id: Some(uri.rsplit(':').next().unwrap_or(uri).to_string()),
            uri: uri.to_string(),
            name: name.to_string(),
            subtitle: "Artist".to_string(),
            context: "Album".to_string(),
            duration_ms: 180_000,
            image_url: None,
            kind,
            source: None,
            freshness: None,
            explicit: None,
            is_playable: None,
            ..Default::default()
        }
    }

    fn render_lines(app: &mut App, width: u16, height: u16) -> Vec<String> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal should build");
        terminal
            .draw(|frame| render(frame, app))
            .expect("render should complete");
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn full_frame_renders_without_panic_at_narrow_widths() {
        // Regression net for the responsive collapse: every tier of the
        // tab strip + now-playing layout must render a full frame, all
        // the way down to absurdly small terminals.
        for (width, height) in [(160u16, 40u16), (110, 32), (80, 28), (50, 24), (34, 18)] {
            let mut app = test_app();
            let lines = render_lines(&mut app, width, height);
            assert_eq!(lines.len(), height as usize, "{width}x{height} frame");
        }
    }

    #[test]
    fn playlists_hint_bar_advertises_queue_whole_playlist() {
        let mut app = test_app();
        app.screen = Screen::Playlists;
        let lines = render_lines(&mut app, 170, 32);
        assert!(
            lines.iter().any(|line| line.contains("Queue playlist")),
            "hint bar should advertise queueing the whole playlist"
        );
        assert!(
            lines.iter().any(|line| line.contains("Help")),
            "hint bar should always end with the Help escape hatch"
        );
        // Narrow terminals keep the head of the list + Help.
        let narrow = render_lines(&mut app, 60, 24);
        assert!(
            narrow.iter().any(|line| line.contains("Help")),
            "Help must survive narrow widths"
        );
    }

    #[test]
    fn queue_rail_renders_beside_the_current_screen() {
        let mut app = test_app();
        app.screen = Screen::Search;
        app.right_rail = RightRailMode::Queue;
        app.queue.session_active = true;
        app.queue.items = vec![item("spotify:track:first", "First Up")];

        let lines = render_lines(&mut app, 120, 32);

        assert!(
            lines.iter().any(|line| line.contains("Search")),
            "main screen should still render"
        );
        // Title chip now reads "Queue  ·  Q hide  ·  N" — match the
        // hide hint substring so we're tolerant to the exact chip
        // formatting.
        let rail_line = lines
            .iter()
            .find(|line| line.contains("Q hide"))
            .expect("queue rail title should be visible");
        let rail_x = rail_line
            .find("Q hide")
            .expect("queue rail should have an x position");
        assert!(
            rail_x > 78,
            "queue rail should render on the right, got x={rail_x}"
        );
        assert!(
            lines.iter().any(|line| line.contains("First Up")),
            "queue items should render in the rail"
        );
    }

    #[test]
    fn queue_rail_hides_cached_items_without_active_session() {
        let mut app = test_app();
        app.screen = Screen::Search;
        app.right_rail = RightRailMode::Queue;
        app.queue.session_active = false;
        app.queue.items = vec![item("spotify:track:first", "First Up")];

        let output = render_lines(&mut app, 120, 32).join("\n");

        assert!(output.contains("Q hide"));
        assert!(!output.contains("First Up"));
    }

    #[test]
    fn player_controls_render_below_main_content() {
        let mut app = test_app();
        app.screen = Screen::Library;
        app.playback.item = Some(item("spotify:track:now", "Now Playing"));
        // Use a unique artist string we can grep for. The title now
        // renders as big-text block glyphs (no longer plain "Now
        // Playing" in the symbol dump), so we look for the artist
        // subtitle to confirm the active-track info is visible.
        if let Some(ref mut t) = app.playback.item {
            t.subtitle = "Test Artist Confirmed".to_string();
        }

        let lines = render_lines(&mut app, 120, 32);
        let glyphs = ["▶", "⏸", "⏭", "⏮"];
        let controls_y = lines
            .iter()
            .position(|line| glyphs.iter().any(|g| line.contains(g)))
            .expect("transport chips should be visible");

        assert!(
            controls_y >= 22,
            "transport chips should be in the bottom player area, got row {controls_y}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("Test Artist Confirmed")),
            "bottom player should show the active track's artist subtitle"
        );
    }

    #[test]
    fn search_results_render_in_kind_groups() {
        let mut app = test_app();
        app.screen = Screen::Search;
        app.search_results = vec![
            item_kind("spotify:track:first", "First Song", MediaKind::Track),
            item_kind("spotify:artist:artist-one", "Artist One", MediaKind::Artist),
            item_kind("spotify:playlist:mix", "Road Mix", MediaKind::Playlist),
            item_kind("spotify:show:podcast", "Signal Podcast", MediaKind::Show),
        ];

        let lines = render_lines(&mut app, 140, 32);

        assert!(lines.iter().any(|line| line.contains("Tracks  1")));
        assert!(lines.iter().any(|line| line.contains("Artists  1")));
        assert!(lines.iter().any(|line| line.contains("Playlists  1")));
        assert!(lines.iter().any(|line| line.contains("Podcasts  1")));
        assert!(lines.iter().any(|line| line.contains("First Song")));
        assert!(lines.iter().any(|line| line.contains("Artist One")));
        assert!(lines.iter().any(|line| line.contains("Road Mix")));
        assert!(lines.iter().any(|line| line.contains("Signal Podcast")));
    }

    #[test]
    fn snapshot_23_tabs() {
        let mut app = test_app();
        app.screen = Screen::Library;
        let lines = render_lines(&mut app, 140, 32);
        // Tabs live in the first 3-4 rows of the body.
        let tabs_band = &lines[0..6];
        println!(
            "\n--- 23-tabs — chip-styled numeric prefixes, active tab inverted ---\n{}\n--- end ---\n",
            tabs_band.join("\n")
        );
        assert!(
            tabs_band.iter().any(|l| l.contains("Library")),
            "Library tab should render"
        );
    }

    #[test]
    fn snapshot_16_modals() {
        // 16a: playlist picker
        let mut app = test_app();
        app.playlists = vec![
            Playlist {
                id: "p1".to_string(),
                name: "Quiet Storm".to_string(),
                owner: "me".to_string(),
                tracks_total: 41,
                image_url: Some("x".to_string()),
                snapshot_id: None,
            },
            Playlist {
                id: "p2".to_string(),
                name: "Coding".to_string(),
                owner: "me".to_string(),
                tracks_total: 12,
                image_url: None,
                snapshot_id: None,
            },
        ];
        app.playlist_picker = Some(crate::app::PlaylistPickerModal {
            uris: vec!["spotify:track:wonder".to_string()],
            selected: 0,
            selected_playlist_ids: HashSet::from(["p1".to_string()]),
        });
        let lines = render_lines(&mut app, 100, 28);
        println!(
            "\n--- 16a-playlist-picker ---\n{}\n--- end ---\n",
            lines.join("\n")
        );

        // 16b: confirm modal
        let mut app2 = test_app();
        app2.confirm_modal = Some(crate::app::ConfirmModal {
            title: "Reset cache".to_string(),
            body: "This will delete all cached playlists and library items.".to_string(),
            on_confirm: crate::tui_actions::TuiAction::Refresh,
        });
        let lines2 = render_lines(&mut app2, 100, 28);
        println!(
            "\n--- 16b-confirm-modal ---\n{}\n--- end ---\n",
            lines2.join("\n")
        );

        // 16c: error modal — 403 / scope flavour.
        let mut app3 = test_app();
        app3.error = Some(
            "Spotify API 403 on POST /playlists/abc/tracks: scope playlist-modify-public required"
                .to_string(),
        );
        let lines3 = render_lines(&mut app3, 100, 28);
        println!(
            "\n--- 16c-error-modal (403/scope) ---\n{}\n--- end ---\n",
            lines3.join("\n")
        );
    }

    #[test]
    fn snapshot_13_lyrics() {
        let mut app = test_app();
        app.screen = Screen::Lyrics;
        app.playback.item = Some(item("spotify:track:doves", "Doves in the Wind"));
        if let Some(ref mut t) = app.playback.item {
            t.subtitle = "SZA · Kendrick Lamar".to_string();
        }
        app.lyrics = Some(spotuify_core::SyncedLyrics {
            provider: spotuify_core::LyricsProvider::Lrclib,
            track_uri: "spotify:track:doves".to_string(),
            lines: vec![
                spotuify_core::LyricLine {
                    start_ms: 0,
                    text: "Real lovers don't change".to_string(),
                    is_rtl: false,
                },
                spotuify_core::LyricLine {
                    start_ms: 4000,
                    text: "They only put up with us a little longer".to_string(),
                    is_rtl: false,
                },
                spotuify_core::LyricLine {
                    start_ms: 8000,
                    text: "Real lovers don't lie".to_string(),
                    is_rtl: false,
                },
                spotuify_core::LyricLine {
                    start_ms: 12000,
                    text: "Lookin' in your eyes I can taste the truth".to_string(),
                    is_rtl: false,
                },
                spotuify_core::LyricLine {
                    start_ms: 16000,
                    text: "Doves in the wind".to_string(),
                    is_rtl: false,
                },
                spotuify_core::LyricLine {
                    start_ms: 20000,
                    text: "I'm callin' for you".to_string(),
                    is_rtl: false,
                },
            ],
            fetched_at_ms: 0,
            synced: true,
            language: None,
            source_url: None,
        });
        app.playback.progress_ms = 16500;
        app.lyrics_offset_ms = 0;
        let lines = render_lines(&mut app, 140, 32);
        let body_start = 4;
        let body_end = lines.len() - (PLAYER_HEIGHT as usize + STATUS_HEIGHT as usize);
        let body = &lines[body_start..body_end];
        println!(
            "\n--- 13-lyrics — active-line emphasis + thumb header + provider footer ---\n{}\n--- end ---\n",
            body.join("\n")
        );
    }

    #[test]
    fn snapshot_12_diagnostics() {
        let mut app = test_app();
        app.screen = Screen::Diagnostics;
        app.diagnostics_logs = vec![
            "2026-05-15T12:00:00Z INFO  daemon: started".to_string(),
            "2026-05-15T12:00:01Z DEBUG spotify: token refreshed".to_string(),
            "2026-05-15T12:00:02Z WARN  spotify: rate-limit hit, backing off 30s".to_string(),
            "2026-05-15T12:00:03Z ERROR spotify: 411 on PUT /me/tracks (legacy build?)".to_string(),
            "2026-05-15T12:00:04Z INFO  player: ready spotuify-hume".to_string(),
        ];
        let lines = render_lines(&mut app, 140, 32);
        let body_start = 4;
        let body_end = lines.len() - (PLAYER_HEIGHT as usize + STATUS_HEIGHT as usize);
        let body = &lines[body_start..body_end];
        println!(
            "\n--- 12-diagnostics — section chips + log severity chips ---\n{}\n--- end ---\n",
            body.join("\n")
        );
    }

    #[test]
    fn snapshot_11_devices() {
        let mut app = test_app();
        app.screen = Screen::Devices;
        app.devices = vec![
            spotuify_spotify::client::Device {
                id: Some("a".into()),
                name: "iPhone — Bhekani".to_string(),
                kind: "Smartphone".to_string(),
                is_active: false,
                is_restricted: false,
                supports_volume: true,
                volume_percent: Some(45),
            },
            spotuify_spotify::client::Device {
                id: Some("b".into()),
                name: "Living Room".to_string(),
                kind: "Speaker".to_string(),
                is_active: true,
                is_restricted: false,
                supports_volume: true,
                volume_percent: Some(72),
            },
            spotuify_spotify::client::Device {
                id: Some("c".into()),
                name: "Studio Mac".to_string(),
                kind: "Computer".to_string(),
                is_active: false,
                is_restricted: false,
                supports_volume: false,
                volume_percent: None,
            },
            spotuify_spotify::client::Device {
                id: Some("d".into()),
                name: "Old AirPlay".to_string(),
                kind: "CastAudio".to_string(),
                is_active: false,
                is_restricted: true,
                supports_volume: false,
                volume_percent: None,
            },
        ];
        app.selected = 1;
        let lines = render_lines(&mut app, 140, 32);
        let body_start = 4;
        let body_end = lines.len() - (PLAYER_HEIGHT as usize + STATUS_HEIGHT as usize);
        let body = &lines[body_start..body_end];
        println!(
            "\n--- 11-devices — kind icons + state chips + volume bar ---\n{}\n--- end ---\n",
            body.join("\n")
        );
    }

    #[test]
    fn snapshot_10_queue() {
        let mut app = test_app();
        app.screen = Screen::Queue;
        app.playback.item = Some(item("spotify:track:now", "Have You Ever Loved Somebody"));
        app.playback.is_playing = true;
        if let Some(ref mut t) = app.playback.item {
            t.subtitle = "Luther Vandross".to_string();
        }
        app.queue.session_active = true;
        app.queue.currently_playing = app.playback.item.clone();
        app.queue.items = vec![
            item_kind_full(
                "spotify:track:next1",
                "Sweet Thing",
                "Mary J. Blige",
                247_000,
                MediaKind::Track,
            ),
            item_kind_full(
                "spotify:track:next2",
                "Never Too Much",
                "Luther Vandross",
                248_000,
                MediaKind::Track,
            ),
            item_kind_full(
                "spotify:track:next3",
                "A House Is Not a Home",
                "Luther Vandross",
                281_000,
                MediaKind::Track,
            ),
        ];
        // Queue screen reads visible_items(), which only exposes live
        // queue rows when Spotify reports an active session.
        app.selected = 1;
        let lines = render_lines(&mut app, 140, 32);
        let body_start = 4;
        let body_end = lines.len() - (PLAYER_HEIGHT as usize + STATUS_HEIGHT as usize);
        let body = &lines[body_start..body_end];
        println!(
            "\n--- 10-queue — now-playing card + Up Next with counts ---\n{}\n--- end ---\n",
            body.join("\n")
        );
    }

    #[test]
    fn snapshot_09_playlists() {
        let mut app = test_app();
        app.screen = Screen::Playlists;
        app.playlists = vec![
            Playlist {
                id: "p1".to_string(),
                name: "Quiet Storm".to_string(),
                owner: "me".to_string(),
                tracks_total: 41,
                image_url: Some("x".to_string()),
                snapshot_id: None,
            },
            Playlist {
                id: "p2".to_string(),
                name: "Coding".to_string(),
                owner: "anita".to_string(),
                tracks_total: 12,
                image_url: None,
                snapshot_id: None,
            },
            Playlist {
                id: "p3".to_string(),
                name: "Sunday Roast".to_string(),
                owner: "me".to_string(),
                tracks_total: 27,
                image_url: Some("x".to_string()),
                snapshot_id: None,
            },
        ];
        app.playlist_selected = 1;
        let lines = render_lines(&mut app, 140, 32);
        let body_start = 4;
        let body_end = lines.len() - (PLAYER_HEIGHT as usize + STATUS_HEIGHT as usize);
        let body = &lines[body_start..body_end];
        println!(
            "\n--- 09-playlists — list with art/no-art markers and owner ---\n{}\n--- end ---\n",
            body.join("\n")
        );
        let rendered = body.join("\n");
        assert!(rendered.contains("Artwork"));
        assert!(rendered.contains("Coding"));
        assert!(rendered.contains("generated fallback"));
    }

    #[test]
    fn snapshot_08_library_rows() {
        let mut app = test_app();
        app.screen = Screen::Library;
        app.library_items = vec![
            item_kind_full(
                "spotify:track:t1",
                "Never Too Much",
                "Luther Vandross",
                248_000,
                MediaKind::Track,
            ),
            item_kind_full(
                "spotify:track:t2",
                "If This World Were Mine",
                "Luther Vandross",
                281_000,
                MediaKind::Track,
            ),
            item_kind_full(
                "spotify:album:a1",
                "Forever, for Always, for Love",
                "Luther Vandross",
                0,
                MediaKind::Album,
            ),
            item_kind_full(
                "spotify:artist:lv",
                "Luther Vandross",
                "Artist",
                0,
                MediaKind::Artist,
            ),
        ];
        app.marked_uris.insert("spotify:track:t1".to_string());
        app.selected = 1;
        let lines = render_lines(&mut app, 140, 32);
        let body_start = 4;
        let body_end = lines.len() - (PLAYER_HEIGHT as usize + STATUS_HEIGHT as usize);
        let body = &lines[body_start..body_end];
        println!(
            "\n--- 08-library — library rows with marker + duration + 2-line layout ---\n{}\n--- end ---\n",
            body.join("\n")
        );
    }

    #[test]
    fn library_album_selection_renders_artwork_preview() {
        let mut app = test_app();
        app.screen = Screen::Library;
        let mut album = item_kind_full(
            "spotify:album:a1",
            "Forever, for Always, for Love",
            "Luther Vandross",
            0,
            MediaKind::Album,
        );
        album.image_url = Some("https://example.test/album.jpg".to_string());
        app.library_items = vec![
            item_kind_full(
                "spotify:track:t1",
                "Never Too Much",
                "Luther Vandross",
                248_000,
                MediaKind::Track,
            ),
            album,
        ];
        app.selected = 1;

        let lines = render_lines(&mut app, 140, 32);
        let body_start = 4;
        let body_end = lines.len() - (PLAYER_HEIGHT as usize + STATUS_HEIGHT as usize);
        let rendered = lines[body_start..body_end].join("\n");

        assert!(rendered.contains("Artwork"));
        assert!(rendered.contains("Forever, for Always"));
        assert!(rendered.contains("cover from Spotify"));
    }

    fn item_kind_full(
        uri: &str,
        name: &str,
        artist: &str,
        duration_ms: u64,
        kind: MediaKind,
    ) -> MediaItem {
        let mut m = item_kind(uri, name, kind);
        m.subtitle = artist.to_string();
        m.duration_ms = duration_ms;
        m
    }

    #[test]
    fn snapshot_07_search_groups() {
        let mut app = test_app();
        app.screen = Screen::Search;
        app.search_query = "luther".to_string();
        app.search_results = vec![
            item_kind("spotify:track:nev", "Never Too Much", MediaKind::Track),
            item_kind(
                "spotify:track:hav",
                "Have You Ever Loved Somebody",
                MediaKind::Track,
            ),
            item_kind(
                "spotify:track:hou",
                "A House Is Not a Home",
                MediaKind::Track,
            ),
            item_kind("spotify:artist:lv", "Luther Vandross", MediaKind::Artist),
            item_kind(
                "spotify:album:nev-album",
                "Never Too Much",
                MediaKind::Album,
            ),
            item_kind(
                "spotify:album:gif",
                "The Night I Fell in Love",
                MediaKind::Album,
            ),
            item_kind(
                "spotify:playlist:smooth",
                "Smooth Soul",
                MediaKind::Playlist,
            ),
            item_kind("spotify:show:soul-pod", "Soul Stories", MediaKind::Show),
            item_kind(
                "spotify:episode:soul-ep",
                "Episode 7: Luther",
                MediaKind::Episode,
            ),
        ];
        app.selected = 3; // points to the artist

        let lines = render_lines(&mut app, 140, 32);
        let body_start = 4;
        let body_end = lines.len() - (PLAYER_HEIGHT as usize + STATUS_HEIGHT as usize);
        let body = &lines[body_start..body_end];
        println!(
            "\n--- 07-search-groups — focused on Artists card ---\n{}\n--- end ---\n",
            body.join("\n")
        );
        let joined = body.join(" ");
        assert!(joined.contains("Tracks"), "Tracks card missing");
        assert!(joined.contains("Artists"), "Artists card missing");
        assert!(
            joined.contains("Luther Vandross"),
            "Luther Vandross row missing"
        );
    }

    #[test]
    fn snapshot_06_player_body() {
        let mut app = test_app();
        app.screen = Screen::Player;
        app.player_large = true;
        app.viz_enabled = true;
        app.playback.item = Some(item("spotify:track:doves", "Doves in the Wind"));
        app.playback.is_playing = true;
        app.playback.shuffle = true;
        app.playback.repeat = "context".to_string();
        app.playback.device = Some(spotuify_spotify::client::Device {
            id: Some("d1".to_string()),
            name: "Living Room".to_string(),
            kind: "Speaker".to_string(),
            is_active: true,
            is_restricted: false,
            supports_volume: true,
            volume_percent: Some(60),
        });
        app.queue.session_active = true;
        app.queue.items = vec![
            item("spotify:track:next1", "Sweet Thing"),
            item("spotify:track:next2", "Never Too Much"),
            item("spotify:track:next3", "A House Is Not a Home"),
        ];
        app.spectrum_bands = [
            0.2, 0.5, 0.9, 0.7, 0.3, 0.8, 0.6, 0.4, 0.5, 0.85, 0.65, 0.45,
        ];

        let lines = render_lines(&mut app, 140, 40);
        // Player body sits between the tabs and the bottom player chrome.
        let body_start = 4; // approx after tabs+banner
        let body_end = lines.len() - (PLAYER_HEIGHT as usize + STATUS_HEIGHT as usize);
        let body = &lines[body_start..body_end];
        println!(
            "\n--- 06-player-body — body region at 140x40 ---\n{}\n--- end ---\n",
            body.join("\n")
        );
        assert!(
            body.iter().any(|l| l.contains("Up Next")),
            "queue card should be in the body"
        );
    }

    #[test]
    fn player_body_renders_actionable_home_feed_and_queue_panel() {
        let mut app = test_app();
        app.screen = Screen::Player;
        app.player_large = true;
        app.library_items = vec![
            item("spotify:track:first", "First Saved Track"),
            item_kind("spotify:show:show", "Saved Show", MediaKind::Show),
            item_kind(
                "spotify:episode:episode",
                "Saved Episode",
                MediaKind::Episode,
            ),
        ];
        app.queue.session_active = true;
        app.queue.items = vec![item("spotify:track:next", "Next Queue Track")];

        let lines = render_lines(&mut app, 140, 40);
        let body_start = 4;
        let body_end = lines.len() - (PLAYER_HEIGHT as usize + STATUS_HEIGHT as usize);
        let body = lines[body_start..body_end].join("\n");
        let joined = lines.join("\n");

        assert!(joined.contains("Home"), "home title missing: {joined}");
        assert!(
            body.contains("First Saved Track"),
            "saved track missing: {body}"
        );
        assert!(
            body.contains("Saved Show") || body.contains("Saved Episode"),
            "podcast item missing: {body}"
        );
        assert!(
            body.contains("Next Queue Track"),
            "queue panel missing live queue item: {body}"
        );
    }

    #[test]
    fn snapshot_03_transport_chips() {
        let mut app = test_app();
        app.screen = Screen::Library;
        app.playback.item = Some(item("spotify:track:now", "Doves in the Wind"));
        app.playback.is_playing = true;
        app.playback.shuffle = true;
        app.playback.repeat = "context".to_string();
        app.playback.device = Some(spotuify_spotify::client::Device {
            id: Some("d1".to_string()),
            name: "Living Room".to_string(),
            kind: "Speaker".to_string(),
            is_active: true,
            is_restricted: false,
            supports_volume: true,
            volume_percent: Some(72),
        });

        let lines = render_lines(&mut app, 140, 32);
        // Player chrome is the bottom PLAYER_HEIGHT rows.
        let player_rows = &lines[lines.len() - (PLAYER_HEIGHT as usize + STATUS_HEIGHT as usize)
            ..lines.len() - STATUS_HEIGHT as usize];
        println!(
            "\n--- 03-transport — player chrome (PLAYER_HEIGHT={}) at 140 cols ---\n{}\n--- end ---\n",
            PLAYER_HEIGHT,
            player_rows.join("\n")
        );
        // Visual sanity: chip glyphs visible, volume bar present.
        let joined = player_rows.join(" ");
        assert!(joined.contains('⏸'), "play/pause glyph missing");
        assert!(joined.contains('⏭'), "next glyph missing");
        assert!(joined.contains('⏮'), "prev glyph missing");
        assert!(
            joined.contains('█') || joined.contains('▰'),
            "volume bar missing"
        );
    }

    #[test]
    fn snapshot_02_hint_bar_after_revamp() {
        // Print the bottom 4 rows of the TUI at 140 cols so we can
        // verify visually that the hint bar uses chip-styled keys and
        // is never displaced by toast/banner/pending text.
        let mut app = test_app();
        app.screen = Screen::Search;
        app.search_results = vec![item("spotify:track:wonder", "Wonderwall")];

        let lines_no_toast = render_lines(&mut app, 140, 32);
        let bottom_no_toast = &lines_no_toast[lines_no_toast.len() - 4..];

        app.toast = Some("Liked Wonderwall".to_string());
        let lines_toast = render_lines(&mut app, 140, 32);
        let bottom_toast = &lines_toast[lines_toast.len() - 4..];

        println!(
            "\n--- 02-hint-bar (no toast) — bottom 4 rows of 140-wide TUI ---\n{}\n--- end ---\n",
            bottom_no_toast.join("\n")
        );
        println!(
            "\n--- 02-hint-bar (with toast) — toast on its own row, hints still rendered below ---\n{}\n--- end ---\n",
            bottom_toast.join("\n")
        );

        // The hint row must contain shortcut copy in BOTH cases so a
        // toast never hides discoverability.
        assert!(
            bottom_no_toast.iter().any(|l| l.contains('·')),
            "hint row missing without toast"
        );
        assert!(
            bottom_toast.iter().any(|l| l.contains('·')),
            "hint row missing when toast is visible"
        );
    }

    #[test]
    fn hint_bar_in_status_row_renders_chip_styled_shortcuts_for_current_screen() {
        // Status bar always carries a dedicated hint row. The chip
        // format is `[key] [label] · [key] [label] · …` so the user
        // can scan the next action without parsing colons.
        let mut app = test_app();
        app.screen = Screen::Library;
        app.library_items = vec![item("spotify:track:one", "One")];

        let lines = render_lines(&mut app, 140, 32);
        let status_rows = &lines[lines.len().saturating_sub(STATUS_HEIGHT as usize)..];
        let joined = status_rows.join("\n");

        // The hint row is delimited by `·` between shortcuts. At least
        // two shortcuts visible → at least one separator.
        assert!(
            joined.contains('·'),
            "hint bar should separate shortcuts with `·`, got rows: {status_rows:?}"
        );
        // And the row should reference at least one known global
        // shortcut's label (Play / Queue / Mark / Filter / Like) so
        // the user actually sees actionable copy.
        let has_known_action = ["Play", "Queue", "Mark", "Filter", "Like"]
            .iter()
            .any(|label| joined.contains(label));
        assert!(
            has_known_action,
            "hint bar should label a known action, got rows: {status_rows:?}"
        );
    }

    #[test]
    fn empty_library_and_diagnostics_do_not_tell_user_to_manual_sync() {
        let mut app = test_app();
        app.screen = Screen::Library;
        let library = render_lines(&mut app, 120, 32).join("\n");
        // Daemon owns the sync now — the empty-state copy explains
        // that and tells the user they can just wait.
        assert!(
            library.contains("syncs this in the background")
                || library.contains("Fetching your library"),
            "library empty state should explain auto-sync, got: {}",
            &library[..library.len().min(400)]
        );
        assert!(!library.contains("Run spotuify sync library"));
        assert!(!library.contains("Press u to force"));

        app.screen = Screen::Diagnostics;
        let diagnostics = render_lines(&mut app, 120, 32).join("\n");
        assert!(diagnostics.contains("Loading doctor"));
        assert!(!diagnostics.contains("Press u to fetch"));
    }

    #[test]
    fn fullscreen_queue_overlay_uses_queue_items_from_any_screen() {
        let mut app = test_app();
        app.screen = Screen::Search;
        app.fullscreen_panel = Some(FullscreenPanel::Queue);
        app.queue.session_active = true;
        app.queue.items = vec![item("spotify:track:first", "First Up")];

        let output = render_lines(&mut app, 120, 32).join("\n");

        assert!(output.contains("Queue Fullscreen"));
        assert!(output.contains("First Up"));
        assert!(output.contains("F/Esc close"));
    }

    #[test]
    fn queue_screen_preserves_duplicate_queue_rows() {
        let mut app = test_app();
        app.screen = Screen::Queue;
        app.queue.session_active = true;
        app.queue.items = vec![
            item("spotify:track:same", "Same Up"),
            item("spotify:track:same", "Same Up"),
        ];

        let output = render_lines(&mut app, 120, 32).join("\n");

        assert_eq!(
            output.matches("Same Up").count(),
            2,
            "queue screen should render one row per queued occurrence: {output}"
        );
    }

    #[test]
    fn fullscreen_queue_overlay_preserves_duplicate_queue_rows() {
        let mut app = test_app();
        app.screen = Screen::Search;
        app.fullscreen_panel = Some(FullscreenPanel::Queue);
        app.queue.session_active = true;
        app.queue.items = vec![
            item("spotify:track:same", "Same Up"),
            item("spotify:track:same", "Same Up"),
        ];

        let output = render_lines(&mut app, 120, 32).join("\n");

        assert_eq!(
            output.matches("Same Up").count(),
            2,
            "fullscreen queue should render one row per queued occurrence: {output}"
        );
    }

    #[test]
    fn fullscreen_queue_overlay_hides_cached_items_without_active_session() {
        let mut app = test_app();
        app.screen = Screen::Search;
        app.fullscreen_panel = Some(FullscreenPanel::Queue);
        app.queue.session_active = false;
        app.queue.items = vec![item("spotify:track:first", "First Up")];

        let output = render_lines(&mut app, 120, 32).join("\n");

        assert!(output.contains("Queue Fullscreen"));
        assert!(output.contains("No active Spotify session."));
        assert!(!output.contains("First Up"));
    }
}
