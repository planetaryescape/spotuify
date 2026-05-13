use ratatui::layout::{Alignment, Constraint, Direction, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Row, Table, TableState,
    Tabs, Wrap,
};
use ratatui::Frame;
use ratatui_image::StatefulImage;

use crate::app::{App, Screen};
use crate::spotify::{Device, MediaItem, MediaKind, Playlist};
use crate::tui_actions::top_hints;

const GREEN: Color = Color::Rgb(30, 215, 96);
const BG: Color = Color::Rgb(8, 10, 12);
const PANEL: Color = Color::Rgb(18, 22, 25);
const MUTED: Color = Color::Rgb(118, 128, 135);
const TEXT: Color = Color::Rgb(230, 238, 242);
const WARN: Color = Color::Rgb(245, 185, 65);
const RED: Color = Color::Rgb(245, 88, 88);

pub fn render(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(BG)), area);

    let player_height = if app.screen == Screen::Player && app.player_large {
        13
    } else {
        9
    };
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(player_height),
            Constraint::Min(12),
            Constraint::Length(3),
        ])
        .split(area);

    render_now_playing(frame, app, root[0]);
    render_body(frame, app, root[1]);
    render_status(frame, app, root[2]);
    if app.command_palette.visible {
        render_command_palette(frame, area, app);
    }
    if app.show_help {
        render_help(frame, area, app);
    }
    if app.error.is_some() {
        render_error_modal(frame, area, app);
    }
}

fn render_now_playing(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let block = Block::default()
        .title(Line::from(vec![Span::styled(
            " spotuify ",
            Style::default()
                .fg(BG)
                .bg(GREEN)
                .add_modifier(Modifier::BOLD),
        )]))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(35, 44, 49)))
        .style(Style::default().bg(PANEL));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(18),
            Constraint::Min(30),
            Constraint::Length(26),
        ])
        .split(inner);

    render_cover(frame, app, chunks[0]);
    render_track(frame, app, chunks[1]);
    render_transport(frame, app, chunks[2]);
}

fn render_cover(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let area = area.inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    if let Some(cover) = &mut app.cover {
        let image = StatefulImage::default();
        frame.render_stateful_widget(image, area, cover);
        if let Some(Err(err)) = cover.last_encoding_result() {
            app.error = Some(err.to_string());
        }
    } else {
        let art = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled("  .----.  ", Style::default().fg(GREEN))),
            Line::from(Span::styled(" / .--. \\ ", Style::default().fg(GREEN))),
            Line::from(Span::styled(" | |  | | ", Style::default().fg(GREEN))),
            Line::from(Span::styled(" \\ '--' / ", Style::default().fg(GREEN))),
            Line::from(Span::styled("  '----'  ", Style::default().fg(GREEN))),
        ])
        .alignment(Alignment::Center)
        .style(Style::default().bg(PANEL));
        frame.render_widget(art, area);
    }
}

fn render_track(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let Some(item) = app.playback.item.as_ref().or(app.last_played.as_ref()) else {
        let hint = app
            .spotifyd_status
            .as_deref()
            .unwrap_or("Search for music or open a playlist, then press Enter to play.");
        let empty = Paragraph::new(vec![
            Line::from(Span::styled(
                "Ready when you are",
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(hint, Style::default().fg(GREEN))),
            Line::from(Span::styled(
                "Search, queue, playlists, and podcasts are available from the tabs below.",
                Style::default().fg(MUTED),
            )),
        ])
        .wrap(Wrap { trim: true })
        .style(Style::default().bg(PANEL));
        frame.render_widget(empty, area);
        return;
    };

    let state = if app.playback.item.is_none() {
        "last played"
    } else if app.playback.is_playing {
        "playing"
    } else {
        "paused"
    };
    let progress_ms = if app.playback.item.is_some() {
        app.playback.progress_ms
    } else {
        0
    };
    let progress = progress_ratio(progress_ms, item.duration_ms);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(2),
        ])
        .split(area);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                kind_icon(&item.kind),
                Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                truncate(&item.name, rows[0].width.saturating_sub(4) as usize),
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ),
        ]))
        .style(Style::default().bg(PANEL)),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            &item.subtitle,
            Style::default().fg(Color::Rgb(185, 194, 199)),
        )]))
        .style(Style::default().bg(PANEL)),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(state, Style::default().fg(GREEN)),
            Span::styled(" on ", Style::default().fg(MUTED)),
            Span::styled(device_name(app), Style::default().fg(TEXT)),
        ]))
        .style(Style::default().bg(PANEL)),
        rows[2],
    );
    frame.render_widget(
        Gauge::default()
            .gauge_style(Style::default().fg(GREEN).bg(Color::Rgb(38, 45, 49)))
            .ratio(progress)
            .label(format!(
                "{} / {}",
                fmt_ms(progress_ms),
                fmt_ms(item.duration_ms)
            ))
            .style(Style::default().bg(PANEL)),
        rows[4],
    );
}

fn render_transport(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let volume = app
        .playback
        .device
        .as_ref()
        .and_then(|device| device.volume_percent)
        .unwrap_or(0);
    let lines = vec![
        Line::from(vec![Span::styled(
            "Controls",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        )]),
        Line::from(vec![
            Span::styled("Space", key_style()),
            Span::raw(" play/pause  "),
            Span::styled("n/p", key_style()),
            Span::raw(" next/prev"),
        ]),
        Line::from(vec![
            Span::styled("←/→", key_style()),
            Span::raw(" seek  "),
            Span::styled("+/-", key_style()),
            Span::raw(" volume"),
        ]),
        Line::from(vec![
            Span::styled("l", key_style()),
            Span::raw(" save  "),
            Span::styled("A", key_style()),
            Span::raw(" add current to playlist"),
        ]),
        Line::from(vec![
            Span::styled("shuffle ", Style::default().fg(MUTED)),
            Span::styled(
                if app.playback.shuffle { "on" } else { "off" },
                toggle_style(app.playback.shuffle),
            ),
            Span::styled("  repeat ", Style::default().fg(MUTED)),
            Span::styled(&app.playback.repeat, Style::default().fg(GREEN)),
        ]),
        Line::from(vec![Span::styled(
            format!("volume {volume}%"),
            Style::default().fg(MUTED),
        )]),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(PANEL)),
        area,
    );
}

fn render_body(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let outer = Block::default()
        .borders(Borders::LEFT | Borders::RIGHT)
        .border_style(Style::default().fg(Color::Rgb(25, 31, 35)))
        .style(Style::default().bg(BG));
    let inner = outer.inner(area).inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    frame.render_widget(outer, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(inner);
    let titles = Screen::ALL
        .into_iter()
        .enumerate()
        .map(|(index, screen)| Line::from(format!("{} {}", index + 1, screen.label())))
        .collect::<Vec<_>>();
    let selected = Screen::ALL
        .iter()
        .position(|screen| *screen == app.screen)
        .unwrap_or(0);
    frame.render_widget(
        Tabs::new(titles)
            .select(selected)
            .highlight_style(
                Style::default()
                    .fg(BG)
                    .bg(GREEN)
                    .add_modifier(Modifier::BOLD),
            )
            .style(Style::default().fg(MUTED).bg(BG))
            .divider(Span::styled(" ", Style::default().bg(BG))),
        rows[0],
    );

    match app.screen {
        Screen::Player => render_player_page(frame, app, rows[1]),
        Screen::Search => render_search(frame, app, rows[1]),
        Screen::Library => render_library(frame, app, rows[1]),
        Screen::Playlists => render_playlists(frame, app, rows[1]),
        Screen::Queue => render_queue(frame, app, rows[1]),
        Screen::Devices => render_devices(frame, app, rows[1]),
        Screen::Diagnostics => render_diagnostics(frame, app, rows[1]),
    }
}

fn render_player_page(frame: &mut Frame<'_>, app: &App, area: Rect) {
    if !app.player_large {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(area);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "Small player mode",
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    "Press z for the large player. Playback controls remain global.",
                    Style::default().fg(GREEN),
                )),
            ])
            .block(panel_block(" Player "))
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(PANEL)),
            columns[0],
        );
        render_media_list(
            frame,
            " Queue Preview ".to_string(),
            &app.queue.items.iter().take(8).cloned().collect::<Vec<_>>(),
            0,
            app,
            columns[1],
        );
        return;
    }

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(area);
    let player_text = if let Some(item) = app.playback.item.as_ref().or(app.last_played.as_ref()) {
        vec![
            Line::from(Span::styled(
                &item.name,
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(&item.subtitle, Style::default().fg(TEXT))),
            Line::from(Span::styled(
                context_suffix(item),
                Style::default().fg(MUTED),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled("Device ", Style::default().fg(MUTED)),
                Span::styled(device_name(app), Style::default().fg(GREEN)),
            ]),
            Line::from(vec![
                Span::styled("State  ", Style::default().fg(MUTED)),
                Span::styled(
                    if app.playback.is_playing {
                        "playing"
                    } else {
                        "paused"
                    },
                    Style::default().fg(GREEN),
                ),
            ]),
            Line::from(vec![
                Span::styled("Mode   ", Style::default().fg(MUTED)),
                Span::styled(
                    format!(
                        "shuffle {} / repeat {}",
                        if app.playback.shuffle { "on" } else { "off" },
                        app.playback.repeat
                    ),
                    Style::default().fg(TEXT),
                ),
            ]),
        ]
    } else {
        vec![
            Line::from(Span::styled(
                "No active playback",
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Press / to search or 4 to open a playlist.",
                Style::default().fg(GREEN),
            )),
            Line::from(Span::styled(
                "If Spotify says no active device, press 6 for Devices.",
                Style::default().fg(MUTED),
            )),
        ]
    };
    frame.render_widget(
        Paragraph::new(player_text)
            .block(panel_block(" Player "))
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(PANEL)),
        columns[0],
    );

    render_media_list(
        frame,
        " Queue Preview ".to_string(),
        &app.queue.items.iter().take(8).cloned().collect::<Vec<_>>(),
        0,
        app,
        columns[1],
    );
}

fn render_search(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);
    let prompt = if app.search_input_active {
        "typing global search"
    } else {
        "press / to search tracks, episodes, albums, playlists"
    };
    let input_style = if app.search_input_active {
        Style::default().fg(TEXT).bg(Color::Rgb(24, 34, 29))
    } else {
        Style::default().fg(MUTED).bg(PANEL)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("/ ", Style::default().fg(GREEN)),
            Span::styled(&app.search_query, Style::default().fg(TEXT)),
            Span::styled(format!("  {prompt}"), Style::default().fg(MUTED)),
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
    render_media_list(frame, title, &items, app.selected, app, rows[1]);
}

fn render_library(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);
    render_filter_bar(frame, app, " Library Filter ", rows[0]);
    let items = app.visible_items();
    render_media_list(
        frame,
        area_title(" Library ", items.len()),
        &items,
        app.selected,
        app,
        rows[1],
    );
}

fn render_queue(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(1),
        ])
        .split(area);
    let current = app
        .queue
        .currently_playing
        .as_ref()
        .map(|item| format!("{} - {}", item.name, item.subtitle))
        .unwrap_or_else(|| "Queue is unavailable until playback is active".to_string());
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Now ", Style::default().fg(GREEN)),
            Span::styled(current, Style::default().fg(TEXT)),
        ]))
        .block(panel_block(" Queue "))
        .style(Style::default().bg(PANEL)),
        rows[0],
    );
    render_filter_bar(frame, app, " Queue Filter ", rows[1]);
    let items = app.visible_items();
    render_media_list(
        frame,
        " Upcoming ".to_string(),
        &items,
        app.selected,
        app,
        rows[2],
    );
}

fn render_playlists(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);
    render_filter_bar(frame, app, " Playlist Filter ", rows[0]);
    if app.selected_playlist_id.is_some() {
        let title = format!(
            " {}  (b back, a add current) ",
            app.selected_playlist_name.as_deref().unwrap_or("Playlist")
        );
        let items = app.visible_items();
        render_media_list(frame, title, &items, app.selected, app, rows[1]);
    } else {
        let playlists = app.filtered_playlists();
        render_playlist_list(frame, &playlists, app.playlist_selected, rows[1]);
    }
}

fn render_devices(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);
    render_filter_bar(frame, app, " Device Filter ", chunks[0]);
    let devices = app.filtered_devices();
    if devices.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "No visible devices",
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    "Start Spotify or spotifyd, then press u to refresh.",
                    Style::default().fg(GREEN),
                )),
            ])
            .block(panel_block(" Devices "))
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(PANEL)),
            chunks[1],
        );
        return;
    }
    let rows = devices.iter().map(device_row).collect::<Vec<_>>();
    let table = Table::new(
        rows,
        [
            Constraint::Min(18),
            Constraint::Length(12),
            Constraint::Length(9),
            Constraint::Length(8),
        ],
    )
    .header(Row::new(["Device", "Type", "State", "Volume"]).style(Style::default().fg(MUTED)))
    .block(panel_block(" Devices  (Enter/x transfer to selected) "))
    .row_highlight_style(
        Style::default()
            .fg(BG)
            .bg(GREEN)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol(" ")
    .style(Style::default().bg(PANEL));
    let mut state = TableState::default();
    state.select(if devices.is_empty() {
        None
    } else {
        Some(app.selected.min(devices.len() - 1))
    });
    frame.render_stateful_widget(table, chunks[1], &mut state);
}

fn render_filter_bar(frame: &mut Frame<'_>, app: &App, title: &str, area: Rect) {
    let style = if app.list_filter_active {
        Style::default().fg(TEXT).bg(Color::Rgb(24, 34, 29))
    } else {
        Style::default().fg(MUTED).bg(PANEL)
    };
    let prompt = if app.list_filter_active {
        "type to filter current list"
    } else {
        "Ctrl-f filters this list only"
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("filter ", Style::default().fg(GREEN)),
            Span::styled(&app.list_filter_query, Style::default().fg(TEXT)),
            Span::styled(format!("  {prompt}"), Style::default().fg(MUTED)),
        ]))
        .block(panel_block(title))
        .style(style),
        area,
    );
}

fn render_diagnostics(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    let mut left = Vec::new();
    if let Some(report) = &app.diagnostics_report {
        left.push(Line::from(vec![
            Span::styled("Health ", Style::default().fg(MUTED)),
            Span::styled(report.health_class.as_str(), health_style(report.healthy)),
        ]));
        left.push(Line::from(vec![
            Span::styled("Daemon ", Style::default().fg(MUTED)),
            Span::styled(
                format!(
                    "pid {:?}, uptime {:?}s",
                    report.daemon.daemon_pid, report.daemon.uptime_secs
                ),
                Style::default().fg(TEXT),
            ),
        ]));
        left.push(Line::from(vec![
            Span::styled("Auth   ", Style::default().fg(MUTED)),
            Span::styled(&report.keychain_token.message, Style::default().fg(TEXT)),
        ]));
        left.push(Line::from(vec![
            Span::styled("Logs   ", Style::default().fg(MUTED)),
            Span::styled(&report.logs_path, Style::default().fg(TEXT)),
        ]));
        left.push(Line::from(""));
        left.push(Line::from(Span::styled(
            "Findings",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        )));
        if report.findings.is_empty() {
            left.push(Line::from(Span::styled(
                "No findings",
                Style::default().fg(GREEN),
            )));
        } else {
            left.extend(report.findings.iter().take(6).map(|finding| {
                Line::from(vec![
                    Span::styled("- ", Style::default().fg(WARN)),
                    Span::styled(&finding.message, Style::default().fg(TEXT)),
                ])
            }));
        }
    } else {
        left.push(Line::from(Span::styled(
            "Diagnostics not loaded",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        )));
        left.push(Line::from(Span::styled(
            "Press u to fetch doctor, cache, and recent logs from the daemon.",
            Style::default().fg(GREEN),
        )));
    }
    frame.render_widget(
        Paragraph::new(left)
            .block(panel_block(" Diagnostics "))
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(PANEL)),
        columns[0],
    );

    let mut right = Vec::new();
    if let Some(status) = &app.cache_status {
        right.push(Line::from(Span::styled(
            "Cache / Index",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        )));
        right.push(Line::from(format!("media items: {}", status.media_items)));
        right.push(Line::from(format!(
            "library items: {}",
            status.library_items
        )));
        right.push(Line::from(format!("playlists: {}", status.playlists)));
        right.push(Line::from(format!(
            "playlist items: {}",
            status.playlist_items
        )));
        right.push(Line::from(format!(
            "index docs: {}",
            status.index_documents
        )));
        right.push(Line::from(""));
    }
    right.push(Line::from(Span::styled(
        "Recent Logs",
        Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
    )));
    if app.diagnostics_logs.is_empty() {
        right.push(Line::from(Span::styled(
            "No logs loaded",
            Style::default().fg(MUTED),
        )));
    } else {
        right.extend(
            app.diagnostics_logs
                .iter()
                .rev()
                .take(12)
                .rev()
                .map(|line| Line::from(line.clone())),
        );
    }
    frame.render_widget(
        Paragraph::new(right)
            .block(panel_block(" Cache + Logs "))
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(TEXT).bg(PANEL)),
        columns[1],
    );
}

fn health_style(healthy: bool) -> Style {
    if healthy {
        Style::default().fg(GREEN).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(RED).add_modifier(Modifier::BOLD)
    }
}

fn render_command_palette(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let area = centered_rect(78, 52, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(area);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Ctrl-p ", key_style()),
            Span::styled(&app.command_palette.input, Style::default().fg(TEXT)),
            Span::styled(
                format!("  {}", app.command_palette.context.label()),
                Style::default().fg(MUTED),
            ),
        ]))
        .block(panel_block(" Command Palette "))
        .style(Style::default().bg(PANEL)),
        rows[0],
    );
    let commands = app.command_palette.visible_commands();
    let items = commands
        .iter()
        .map(|command| {
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<10}", command.shortcut), key_style()),
                Span::styled(command.label, Style::default().fg(TEXT)),
                Span::styled(
                    format!("  {}", command.category),
                    Style::default().fg(MUTED),
                ),
            ]))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    state.select(if items.is_empty() {
        None
    } else {
        Some(app.command_palette.selected.min(items.len() - 1))
    });
    frame.render_stateful_widget(
        List::new(items)
            .block(panel_block(" Commands "))
            .highlight_style(
                Style::default()
                    .fg(BG)
                    .bg(GREEN)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(" ")
            .style(Style::default().bg(PANEL)),
        rows[1],
        &mut state,
    );
    frame.render_widget(
        Paragraph::new("Enter run   Up/Down move   Esc close")
            .style(Style::default().fg(MUTED).bg(PANEL)),
        rows[2],
    );
}

fn render_error_modal(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let Some(error) = &app.error else {
        return;
    };
    let area = centered_rect(72, 28, area);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Action failed",
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(error.clone(), Style::default().fg(RED))),
            Line::from(""),
            Line::from(Span::styled(
                "Enter/Esc dismiss",
                Style::default().fg(MUTED),
            )),
        ])
        .block(panel_block(" Error "))
        .wrap(Wrap { trim: true })
        .style(Style::default().bg(PANEL)),
        area,
    );
}

fn render_media_list(
    frame: &mut Frame<'_>,
    title: String,
    items: &[MediaItem],
    selected: usize,
    app: &App,
    area: Rect,
) {
    if items.is_empty() {
        let message = empty_media_state(app);
        frame.render_widget(
            Paragraph::new(message)
                .block(panel_block(&title))
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(MUTED).bg(PANEL)),
            area,
        );
        return;
    }
    let rows = items
        .iter()
        .map(|item| media_item(item, app.marked_uris.contains(&item.uri)))
        .collect::<Vec<_>>();
    let list = List::new(rows)
        .block(panel_block(&title))
        .highlight_style(
            Style::default()
                .fg(BG)
                .bg(GREEN)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(" ")
        .style(Style::default().bg(PANEL));
    let mut state = ListState::default();
    state.select(if items.is_empty() {
        None
    } else {
        Some(selected.min(items.len() - 1))
    });
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_playlist_list(
    frame: &mut Frame<'_>,
    playlists: &[Playlist],
    selected: usize,
    area: Rect,
) {
    if playlists.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "No playlists loaded",
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    "Press u to refresh or run spotuify playlists.",
                    Style::default().fg(GREEN),
                )),
            ])
            .block(panel_block(" Playlists "))
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(PANEL)),
            area,
        );
        return;
    }
    let rows = playlists
        .iter()
        .map(|playlist| {
            let image_marker = if playlist.image_url.is_some() {
                "■"
            } else {
                "□"
            };
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(format!("{image_marker} "), Style::default().fg(GREEN)),
                    Span::styled(
                        &playlist.name,
                        Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {} tracks", playlist.tracks_total),
                        Style::default().fg(MUTED),
                    ),
                ]),
                Line::from(Span::styled(
                    format!("  by {}", playlist.owner),
                    Style::default().fg(MUTED),
                )),
            ])
        })
        .collect::<Vec<_>>();
    let list = List::new(rows)
        .block(panel_block(" Playlists  (Enter open, a add current) "))
        .highlight_style(
            Style::default()
                .fg(BG)
                .bg(GREEN)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(" ")
        .style(Style::default().bg(PANEL));
    let mut state = ListState::default();
    state.select(if playlists.is_empty() {
        None
    } else {
        Some(selected.min(playlists.len() - 1))
    });
    frame.render_stateful_widget(list, area, &mut state);
}

fn empty_media_state(app: &App) -> Vec<Line<'static>> {
    match app.screen {
        Screen::Search if app.is_searching => vec![
            Line::from(Span::styled(
                "Searching Spotify and local cache...",
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Local results appear first when cached; remote results refresh in the background.",
                Style::default().fg(MUTED),
            )),
        ],
        Screen::Search => vec![
            Line::from(Span::styled(
                "No search results yet",
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Press / and type an artist, song, album, or playlist.",
                Style::default().fg(GREEN),
            )),
        ],
        Screen::Library => vec![
            Line::from(Span::styled(
                "No cached library items",
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Run spotuify sync library or press u to refresh cached data.",
                Style::default().fg(GREEN),
            )),
        ],
        Screen::Queue => vec![
            Line::from(Span::styled(
                "Queue is empty or unavailable",
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Search, open a playlist, then press e to queue tracks.",
                Style::default().fg(GREEN),
            )),
        ],
        Screen::Playlists if app.selected_playlist_id.is_some() => vec![
            Line::from(Span::styled(
                "No tracks loaded for this playlist",
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Press b to return to playlists or u to refresh.",
                Style::default().fg(GREEN),
            )),
        ],
        _ => vec![Line::from(Span::styled(
            "Nothing to show here yet",
            Style::default().fg(MUTED),
        ))],
    }
}

fn render_status(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let message = app
        .toast
        .as_ref()
        .map(|toast| (toast.clone(), GREEN))
        .or_else(|| {
            app.is_syncing
                .then(|| ("Syncing Spotify... Ctrl+C quits".to_string(), GREEN))
        })
        .unwrap_or_else(|| (hint_text(app), MUTED));

    let status = Paragraph::new(Line::from(vec![
        Span::styled(" ", Style::default().bg(GREEN)),
        Span::raw(" "),
        Span::styled(message.0, Style::default().fg(message.1)),
    ]))
    .block(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::Rgb(35, 44, 49))),
    )
    .style(Style::default().bg(BG));
    frame.render_widget(status, area);
}

fn render_help(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let area = centered_rect(74, 25, area);
    let mut rows = vec![
        ("Ctrl-p".to_string(), "Open command palette".to_string()),
        (
            "How do I play a playlist?".to_string(),
            "Press 4, choose a playlist, Enter, then Enter a track".to_string(),
        ),
        (
            "How do I search?".to_string(),
            "Press /, type a query, Enter".to_string(),
        ),
        (
            "How do I queue multiple tracks?".to_string(),
            "Mark with m, then press e".to_string(),
        ),
        (
            "How do I fix no active device?".to_string(),
            "Press 6 for Devices, then Enter on a device".to_string(),
        ),
    ];
    rows.extend(
        crate::tui_actions::actions_for_context(app.current_action_context(), app.selected_count())
            .into_iter()
            .map(|action| {
                (
                    action.shortcut.to_string(),
                    action
                        .cli
                        .map(|cli| format!("{} ({cli})", action.label))
                        .unwrap_or_else(|| action.label.to_string()),
                )
            }),
    );
    let query = app.help_query.to_ascii_lowercase();
    let rows = rows
        .into_iter()
        .filter(|(shortcut, text)| {
            query.is_empty()
                || shortcut.to_ascii_lowercase().contains(&query)
                || text.to_ascii_lowercase().contains(&query)
        })
        .collect::<Vec<_>>();
    let mut lines = vec![
        Line::from(vec![Span::styled(
            "Keyboard help",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        )]),
        Line::from(vec![
            Span::styled("Search help: ", Style::default().fg(MUTED)),
            Span::styled(&app.help_query, Style::default().fg(GREEN)),
        ]),
        Line::from(""),
    ];
    lines.extend(rows.into_iter().map(|(shortcut, text)| {
        Line::from(vec![
            Span::styled(format!("{shortcut:<18}"), key_style()),
            Span::styled(text, Style::default().fg(TEXT)),
        ])
    }));
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block(" Help "))
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(PANEL)),
        area,
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
    let marker = if marked { "[x] " } else { "    " };
    ListItem::new(vec![
        Line::from(vec![
            Span::styled(marker, Style::default().fg(GREEN)),
            Span::styled(
                kind_icon(&item.kind),
                Style::default()
                    .fg(kind_color(&item.kind))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                item.name.clone(),
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {}", item.kind.label()),
                Style::default().fg(MUTED),
            ),
        ]),
        Line::from(vec![
            Span::raw("      "),
            Span::styled(
                item.subtitle.clone(),
                Style::default().fg(Color::Rgb(178, 188, 193)),
            ),
            Span::styled(context_suffix(item), Style::default().fg(MUTED)),
        ]),
    ])
}

fn device_row(device: &Device) -> Row<'static> {
    let state = if device.is_restricted {
        "restricted"
    } else if device.is_active {
        "active"
    } else {
        "idle"
    };
    let volume = if device.supports_volume {
        device
            .volume_percent
            .map(|value| format!("{value}%"))
            .unwrap_or_else(|| "-".to_string())
    } else {
        "fixed".to_string()
    };
    Row::new([
        device.name.clone(),
        device.kind.clone(),
        state.to_string(),
        volume,
    ])
}

fn panel_block(title: &str) -> Block<'_> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_set(symbols::border::ROUNDED)
        .border_style(Style::default().fg(Color::Rgb(42, 51, 56)))
        .style(Style::default().bg(PANEL))
}

fn key_style() -> Style {
    Style::default().fg(GREEN).add_modifier(Modifier::BOLD)
}

fn hint_text(app: &App) -> String {
    top_hints(app.current_action_context(), app.selected_count())
        .into_iter()
        .map(|hint| format!("{}: {}", hint.shortcut, hint.label))
        .collect::<Vec<_>>()
        .join("  ")
}

fn toggle_style(active: bool) -> Style {
    if active {
        Style::default().fg(GREEN).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(MUTED)
    }
}

pub fn kind_icon(kind: &MediaKind) -> &'static str {
    match kind {
        MediaKind::Track => "♪",
        MediaKind::Episode => "◉",
        MediaKind::Album => "▣",
        MediaKind::Artist => "★",
        MediaKind::Playlist => "≡",
    }
}

fn kind_color(kind: &MediaKind) -> Color {
    match kind {
        MediaKind::Track => GREEN,
        MediaKind::Episode => Color::Rgb(180, 128, 255),
        MediaKind::Album => Color::Rgb(91, 179, 255),
        MediaKind::Artist => Color::Rgb(255, 177, 66),
        MediaKind::Playlist => WARN,
    }
}

fn context_suffix(item: &MediaItem) -> String {
    let mut parts = Vec::new();
    if !item.context.is_empty() {
        parts.push(item.context.clone());
    }
    if item.duration_ms > 0 {
        parts.push(fmt_ms(item.duration_ms));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("  {}", parts.join(" · "))
    }
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
        .map(|device| device.name.clone())
        .unwrap_or_else(|| "no device".to_string())
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
