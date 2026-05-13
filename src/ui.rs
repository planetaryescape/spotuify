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

    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(11),
            Constraint::Min(12),
            Constraint::Length(3),
        ])
        .split(area);

    render_now_playing(frame, app, root[0]);
    render_body(frame, app, root[1]);
    render_status(frame, app, root[2]);
    if app.show_help {
        render_help(frame, area);
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
    let titles = [
        Screen::Search,
        Screen::Queue,
        Screen::Playlists,
        Screen::Devices,
    ]
    .into_iter()
    .map(|screen| Line::from(screen.label()))
    .collect::<Vec<_>>();
    let selected = match app.screen {
        Screen::Search => 0,
        Screen::Queue => 1,
        Screen::Playlists => 2,
        Screen::Devices => 3,
    };
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
        Screen::Search => render_search(frame, app, rows[1]),
        Screen::Queue => render_queue(frame, app, rows[1]),
        Screen::Playlists => render_playlists(frame, app, rows[1]),
        Screen::Devices => render_devices(frame, app, rows[1]),
    }
}

fn render_search(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);
    let prompt = if app.search_input_active {
        "searching"
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
    render_media_list(
        frame,
        area_title(" Results ", app.search_results.len()),
        &app.search_results,
        app.selected,
        rows[1],
    );
}

fn render_queue(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
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
    render_media_list(
        frame,
        " Upcoming ".to_string(),
        &app.queue.items,
        app.selected,
        rows[1],
    );
}

fn render_playlists(frame: &mut Frame<'_>, app: &App, area: Rect) {
    if app.selected_playlist_id.is_some() {
        let title = format!(
            " {}  (b back, a add current) ",
            app.selected_playlist_name.as_deref().unwrap_or("Playlist")
        );
        render_media_list(frame, title, &app.playlist_tracks, app.selected, area);
    } else {
        render_playlist_list(frame, &app.playlists, app.playlist_selected, area);
    }
}

fn render_devices(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let rows = app.devices.iter().map(device_row).collect::<Vec<_>>();
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
    state.select(if app.devices.is_empty() {
        None
    } else {
        Some(app.selected.min(app.devices.len() - 1))
    });
    frame.render_stateful_widget(table, area, &mut state);
}

fn render_media_list(
    frame: &mut Frame<'_>,
    title: String,
    items: &[MediaItem],
    selected: usize,
    area: Rect,
) {
    let rows = items.iter().map(media_item).collect::<Vec<_>>();
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

fn render_status(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let message = app
        .error
        .as_ref()
        .map(|error| (format!("error: {error}"), RED))
        .or_else(|| app.toast.as_ref().map(|toast| (toast.clone(), GREEN)))
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

fn render_help(frame: &mut Frame<'_>, area: Rect) {
    let area = centered_rect(74, 25, area);
    let lines = vec![
        Line::from(vec![Span::styled(
            "Keyboard help",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
        help_line("?", "close help"),
        help_line(
            "/",
            "search tracks, albums, playlists, and podcast episodes",
        ),
        help_line("1 2 3 4", "jump to Search, Queue, Playlists, Devices"),
        help_line("Tab / Shift-Tab", "next or previous pane"),
        help_line("j/k or ↑/↓", "move selection"),
        help_line("gg / G", "top or bottom"),
        help_line("Ctrl-d / Ctrl-u", "page down or page up"),
        help_line(
            "Enter",
            "play selected item, open playlist, or transfer device",
        ),
        help_line("Space", "play or pause"),
        help_line("n / p", "next or previous track"),
        help_line("← / →", "seek 15 seconds"),
        help_line("+ / -", "volume up or down"),
        help_line("e", "queue selected item"),
        help_line("l", "save current track or episode"),
        help_line(
            "A or a",
            "add current track or episode to selected playlist",
        ),
        help_line("x", "transfer to selected device"),
        help_line("s / r", "toggle shuffle or repeat"),
        help_line("u", "refresh Spotify data"),
        help_line("Esc or b", "back from playlist tracks"),
        help_line("q", "quit"),
    ];
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block(" Help "))
            .wrap(Wrap { trim: true })
            .style(Style::default().bg(PANEL)),
        area,
    );
}

fn help_line(key: &'static str, text: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key:<16}"), key_style()),
        Span::styled(text, Style::default().fg(TEXT)),
    ])
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

fn media_item(item: &MediaItem) -> ListItem<'static> {
    ListItem::new(vec![
        Line::from(vec![
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
            Span::raw("  "),
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
    match app.screen {
        Screen::Search if app.search_input_active => {
            "Type query  Enter: search  Esc: cancel".to_string()
        }
        Screen::Search => {
            "?: help  /: search  Enter: play  e: queue  Tab: switch  q: quit".to_string()
        }
        Screen::Queue => {
            "?: help  Enter: play  e: queue  Space: pause  n/p: next/prev  q: quit".to_string()
        }
        Screen::Playlists if app.selected_playlist_id.is_some() => {
            "?: help  Enter: play  a: add current  Esc/b: back  gg/G: top/bottom".to_string()
        }
        Screen::Playlists => {
            "?: help  Enter: open  a: add current to selected playlist  Tab: switch".to_string()
        }
        Screen::Devices => {
            "?: help  Enter/x: transfer  u: refresh  Tab: switch  q: quit".to_string()
        }
    }
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
