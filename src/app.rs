use std::io;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use tokio::sync::mpsc;
use tokio::time;

use crate::actions::{CommandKind, CommandResult};
use crate::daemon::ipc_client::IpcClient;
use crate::protocol::{PlaybackCommand, Request, Response, ResponseData, SearchScopeData};
use crate::spotify::{Device, MediaItem, Playback, Playlist, Queue};
use crate::ui;

const TUI_SEARCH_TIMEOUT: Duration = Duration::from_secs(15);
const TUI_PLAYLIST_TIMEOUT: Duration = Duration::from_secs(30);
const TUI_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const TUI_REFRESH_TIMEOUT: Duration = Duration::from_secs(45);
const TUI_LIBRARY_REFRESH_INTERVAL: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Screen {
    Search,
    Queue,
    Playlists,
    Devices,
}

impl Screen {
    pub fn label(self) -> &'static str {
        match self {
            Self::Search => "Search",
            Self::Queue => "Queue",
            Self::Playlists => "Playlists",
            Self::Devices => "Devices",
        }
    }
}

pub struct App {
    pub playback: Playback,
    pub queue: Queue,
    pub devices: Vec<Device>,
    pub playlists: Vec<Playlist>,
    pub last_played: Option<MediaItem>,
    pub playlist_tracks: Vec<MediaItem>,
    pub search_results: Vec<MediaItem>,
    pub is_searching: bool,
    pub action_in_flight: bool,
    pub screen: Screen,
    pub search_query: String,
    pub search_input_active: bool,
    pub selected: usize,
    pub playlist_selected: usize,
    pub selected_playlist_id: Option<String>,
    pub selected_playlist_name: Option<String>,
    pub toast: Option<String>,
    pub error: Option<String>,
    pub last_progress_tick: Instant,
    pub current_art_url: Option<String>,
    pub cover: Option<StatefulProtocol>,
    pub picker: Picker,
    pub spotifyd_status: Option<String>,
    pub is_syncing: bool,
    pub last_sync: Option<Instant>,
    pub last_library_sync: Option<Instant>,
    pub show_help: bool,
    refresh_requested: bool,
    pending_g: bool,
}

struct RefreshSnapshot {
    playback: Option<Playback>,
    queue: Option<Queue>,
    devices: Option<Vec<Device>>,
    playlists: Option<Vec<Playlist>>,
    recent: Option<Vec<MediaItem>>,
    cover: Option<(String, image::DynamicImage)>,
    library_refresh_attempted: bool,
    errors: Vec<String>,
    elapsed_ms: u128,
}

enum AsyncResult {
    Refresh(RefreshSnapshot),
    Search {
        query: String,
        result: std::result::Result<Vec<MediaItem>, String>,
        elapsed_ms: u128,
    },
    PlaylistTracks {
        playlist_id: String,
        playlist_name: String,
        result: std::result::Result<Vec<MediaItem>, String>,
    },
    Command(std::result::Result<CommandResult, String>),
}

impl App {
    async fn new() -> Result<Self> {
        let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());

        Ok(Self {
            playback: Playback::default(),
            queue: Queue::default(),
            devices: Vec::new(),
            playlists: Vec::new(),
            last_played: None,
            playlist_tracks: Vec::new(),
            search_results: Vec::new(),
            is_searching: false,
            action_in_flight: false,
            screen: Screen::Playlists,
            search_query: String::new(),
            search_input_active: false,
            selected: 0,
            playlist_selected: 0,
            selected_playlist_id: None,
            selected_playlist_name: None,
            toast: None,
            error: None,
            last_progress_tick: Instant::now(),
            current_art_url: None,
            cover: None,
            picker,
            spotifyd_status: None,
            is_syncing: false,
            last_sync: None,
            last_library_sync: None,
            show_help: false,
            refresh_requested: true,
            pending_g: false,
        })
    }

    fn visible_items(&self) -> &[MediaItem] {
        match self.screen {
            Screen::Search => &self.search_results,
            Screen::Queue => &self.queue.items,
            Screen::Playlists if self.selected_playlist_id.is_some() => &self.playlist_tracks,
            _ => &[],
        }
    }

    fn selected_item(&self) -> Option<MediaItem> {
        self.visible_items().get(self.selected).cloned()
    }

    fn selected_playlist(&self) -> Option<Playlist> {
        self.playlists.get(self.playlist_selected).cloned()
    }

    fn clamp_selection(&mut self) {
        let len = match self.screen {
            Screen::Search => self.search_results.len(),
            Screen::Queue => self.queue.items.len(),
            Screen::Playlists if self.selected_playlist_id.is_some() => self.playlist_tracks.len(),
            Screen::Playlists => self.playlists.len(),
            Screen::Devices => self.devices.len(),
        };
        if len == 0 {
            self.selected = 0;
            self.playlist_selected = 0;
            return;
        }
        if self.screen == Screen::Playlists && self.selected_playlist_id.is_none() {
            self.playlist_selected = self.playlist_selected.min(len - 1);
        } else {
            self.selected = self.selected.min(len - 1);
        }
    }

    fn active_len(&self) -> usize {
        match self.screen {
            Screen::Search => self.search_results.len(),
            Screen::Queue => self.queue.items.len(),
            Screen::Playlists if self.selected_playlist_id.is_some() => self.playlist_tracks.len(),
            Screen::Playlists => self.playlists.len(),
            Screen::Devices => self.devices.len(),
        }
    }

    fn active_selection(&self) -> usize {
        if self.screen == Screen::Playlists && self.selected_playlist_id.is_none() {
            self.playlist_selected
        } else {
            self.selected
        }
    }

    fn set_active_selection(&mut self, index: usize) {
        if self.screen == Screen::Playlists && self.selected_playlist_id.is_none() {
            self.playlist_selected = index;
        } else {
            self.selected = index;
        }
        self.clamp_selection();
    }

    fn move_down(&mut self) {
        match self.screen {
            Screen::Playlists if self.selected_playlist_id.is_none() => {
                self.playlist_selected = next_index(self.playlist_selected, self.playlists.len());
            }
            Screen::Devices => self.selected = next_index(self.selected, self.devices.len()),
            _ => self.selected = next_index(self.selected, self.visible_items().len()),
        }
    }

    fn move_up(&mut self) {
        match self.screen {
            Screen::Playlists if self.selected_playlist_id.is_none() => {
                self.playlist_selected = prev_index(self.playlist_selected, self.playlists.len());
            }
            Screen::Devices => self.selected = prev_index(self.selected, self.devices.len()),
            _ => self.selected = prev_index(self.selected, self.visible_items().len()),
        }
    }

    fn move_top(&mut self) {
        self.set_active_selection(0);
    }

    fn move_bottom(&mut self) {
        self.set_active_selection(self.active_len().saturating_sub(1));
    }

    fn page_down(&mut self) {
        let len = self.active_len();
        if len == 0 {
            return;
        }
        self.set_active_selection((self.active_selection() + 10).min(len - 1));
    }

    fn page_up(&mut self) {
        self.set_active_selection(self.active_selection().saturating_sub(10));
    }

    fn back(&mut self) {
        if self.screen == Screen::Playlists && self.selected_playlist_id.is_some() {
            self.selected_playlist_id = None;
            self.selected_playlist_name = None;
            self.playlist_tracks.clear();
            self.selected = 0;
        }
    }

    fn request_refresh(&mut self) {
        self.refresh_requested = true;
    }

    fn apply_refresh(&mut self, snapshot: RefreshSnapshot) {
        let had_sync = self.last_sync.is_some();
        self.is_syncing = false;
        self.last_sync = Some(Instant::now());

        if let Some(playback) = snapshot.playback {
            self.playback = playback;
            self.last_progress_tick = Instant::now();
        }
        if let Some(queue) = snapshot.queue {
            self.queue = queue;
        }
        if let Some(devices) = snapshot.devices {
            self.devices = devices;
        }
        if let Some(playlists) = snapshot.playlists {
            self.playlists = playlists;
        }
        if let Some(recent) = snapshot.recent {
            if let Some(item) = recent.first() {
                self.last_played = Some(item.clone());
            }
            if self.search_results.is_empty() && self.search_query.is_empty() {
                self.search_results = recent;
            }
        }
        if let Some((url, image)) = snapshot.cover {
            self.current_art_url = Some(url);
            self.cover = Some(self.picker.new_resize_protocol(image));
        } else if self.playback.item.is_none() && self.last_played.is_none() {
            self.current_art_url = None;
            self.cover = None;
        }

        if snapshot.errors.is_empty() {
            self.error = None;
            if !had_sync {
                self.toast = Some(format!("Synced Spotify in {}ms", snapshot.elapsed_ms));
            }
        } else {
            let error = snapshot.errors.join("; ");
            tracing::warn!(error, "Spotify sync finished with errors");
            self.error = Some(error);
        }
        if snapshot.library_refresh_attempted {
            self.last_library_sync = Some(Instant::now());
        }
        self.clamp_selection();
    }

    fn tick_progress(&mut self) {
        let now = Instant::now();
        if self.playback.is_playing {
            let elapsed = now.duration_since(self.last_progress_tick).as_millis() as u64;
            self.playback.progress_ms = self.playback.progress_ms.saturating_add(elapsed);
            if let Some(item) = &self.playback.item {
                self.playback.progress_ms = self.playback.progress_ms.min(item.duration_ms);
            }
        }
        self.last_progress_tick = now;
    }

    fn apply_async_result(&mut self, result: AsyncResult) {
        match result {
            AsyncResult::Refresh(snapshot) => self.apply_refresh(snapshot),
            AsyncResult::Search {
                query,
                result,
                elapsed_ms,
            } => {
                if query != self.search_query {
                    tracing::debug!(query, current = %self.search_query, "dropping stale search result");
                    return;
                }
                self.is_searching = false;
                self.screen = Screen::Search;
                match result {
                    Ok(results) => {
                        self.search_results = results;
                        self.selected = 0;
                        self.toast = Some(format!(
                            "{} results in {}ms",
                            self.search_results.len(),
                            elapsed_ms
                        ));
                        self.error = None;
                    }
                    Err(error) => self.error = Some(error),
                }
                self.clamp_selection();
            }
            AsyncResult::PlaylistTracks {
                playlist_id,
                playlist_name,
                result,
            } => {
                self.action_in_flight = false;
                match result {
                    Ok(tracks) => {
                        self.selected_playlist_id = Some(playlist_id);
                        self.selected_playlist_name = Some(playlist_name);
                        self.playlist_tracks = tracks;
                        self.selected = 0;
                        self.toast = Some(format!("Loaded {} tracks", self.playlist_tracks.len()));
                        self.error = None;
                    }
                    Err(error) => self.error = Some(error),
                }
                self.clamp_selection();
            }
            AsyncResult::Command(result) => {
                self.action_in_flight = false;
                match result {
                    Ok(result) => {
                        if let Some(playback) = result.playback {
                            self.playback = playback;
                            self.last_progress_tick = Instant::now();
                        }
                        if let Some(queue) = result.queue {
                            self.queue = queue;
                        }
                        if let Some(devices) = result.devices {
                            self.devices = devices;
                        }
                        self.error = None;
                        if let Some(message) = result.message {
                            self.toast = Some(message);
                        }
                        if result.request_refresh {
                            self.request_refresh();
                        }
                    }
                    Err(error) => self.error = Some(error),
                }
                self.clamp_selection();
            }
        }
    }
}

pub async fn run_tui() -> Result<()> {
    crate::daemon::server::ensure_daemon_running().await?;
    let mut app = App::new().await?;
    let mut terminal = setup_terminal().context("failed to set up terminal")?;
    let result = run_loop(&mut terminal, &mut app).await;
    restore_terminal(&mut terminal).context("failed to restore terminal")?;
    result
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    let mut events = EventStream::new();
    let mut poll = time::interval(Duration::from_secs(4));
    let mut progress = time::interval(Duration::from_millis(250));
    let mut sigint = std::pin::pin!(tokio::signal::ctrl_c());
    let (async_tx, mut async_rx) = mpsc::unbounded_channel();
    let mut refresh_in_flight = false;

    loop {
        if app.refresh_requested && !refresh_in_flight {
            app.refresh_requested = false;
            spawn_refresh(app, async_tx.clone(), &mut refresh_in_flight);
        }

        terminal.draw(|frame| ui::render(frame, app))?;

        tokio::select! {
            result = &mut sigint => {
                if let Err(err) = result {
                    tracing::warn!(error = %err, "failed to listen for Ctrl+C");
                }
                break;
            }
            _ = progress.tick() => app.tick_progress(),
            _ = poll.tick() => app.request_refresh(),
            result = async_rx.recv() => {
                if let Some(result) = result {
                    if matches!(result, AsyncResult::Refresh(_)) {
                        refresh_in_flight = false;
                    }
                    app.apply_async_result(result);
                }
            }
            event = events.next() => {
                let Some(event) = event else { break; };
                match event.context("failed to read terminal event")? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if handle_key(app, key, &async_tx)? {
                            break;
                        }
                    }
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn spawn_refresh(
    app: &mut App,
    async_tx: mpsc::UnboundedSender<AsyncResult>,
    refresh_in_flight: &mut bool,
) {
    *refresh_in_flight = true;
    app.is_syncing = true;
    let current_art_url = app.current_art_url.clone();
    let refresh_library = app
        .last_library_sync
        .is_none_or(|last_sync| last_sync.elapsed() >= TUI_LIBRARY_REFRESH_INTERVAL);
    tokio::spawn(async move {
        let snapshot = match time::timeout(
            TUI_REFRESH_TIMEOUT,
            fetch_refresh(current_art_url, refresh_library),
        )
        .await
        {
            Ok(snapshot) => snapshot,
            Err(_) => RefreshSnapshot {
                playback: None,
                queue: None,
                devices: None,
                playlists: None,
                recent: None,
                cover: None,
                library_refresh_attempted: refresh_library,
                errors: vec![format!(
                    "refresh timed out after {}s",
                    TUI_REFRESH_TIMEOUT.as_secs()
                )],
                elapsed_ms: TUI_REFRESH_TIMEOUT.as_millis(),
            },
        };
        let _ = async_tx.send(AsyncResult::Refresh(snapshot));
    });
}

async fn fetch_refresh(current_art_url: Option<String>, refresh_library: bool) -> RefreshSnapshot {
    let started = Instant::now();
    let mut errors = Vec::new();

    let playback = match request_data(Request::PlaybackGet).await {
        Ok(ResponseData::Playback { playback }) => Some(playback),
        Ok(_) => {
            errors.push("unexpected playback response".to_string());
            None
        }
        Err(err) => {
            errors.push(short_error(err));
            None
        }
    };
    let queue = match request_data(Request::QueueGet).await {
        Ok(ResponseData::Queue { queue }) => Some(queue),
        Ok(_) => {
            tracing::warn!("unexpected queue response");
            None
        }
        Err(err) => {
            tracing::warn!(error = %err, "failed to fetch queue");
            None
        }
    };
    let devices = match request_data(Request::DevicesList).await {
        Ok(ResponseData::Devices { devices }) => Some(devices),
        Ok(_) => {
            tracing::warn!("unexpected devices response");
            None
        }
        Err(err) => {
            tracing::warn!(error = %err, "failed to fetch devices");
            None
        }
    };
    let playlists = if refresh_library {
        match request_data(Request::PlaylistsList).await {
            Ok(ResponseData::Playlists { playlists }) => Some(playlists),
            Ok(_) => {
                tracing::warn!("unexpected playlists response");
                None
            }
            Err(err) => {
                tracing::warn!(error = %err, "failed to fetch playlists");
                None
            }
        }
    } else {
        None
    };
    let recent = if refresh_library {
        match request_data(Request::RecentlyPlayed).await {
            Ok(ResponseData::MediaItems { items }) => Some(items),
            Ok(_) => {
                tracing::warn!("unexpected recently played response");
                None
            }
            Err(err) => {
                tracing::warn!(error = %err, "failed to fetch recently played");
                None
            }
        }
    } else {
        None
    };

    let art_url = playback
        .as_ref()
        .and_then(|playback| playback.item.as_ref())
        .or_else(|| recent.as_ref().and_then(|items| items.first()))
        .and_then(|item| item.image_url.clone());
    let cover = if art_url.is_some() && art_url != current_art_url {
        let url = art_url.unwrap_or_default();
        match request_data(Request::Image { url: url.clone() })
            .await
            .and_then(|data| match data {
                ResponseData::Image { bytes } => {
                    image::load_from_memory(&bytes).context("failed to decode cover art")
                }
                _ => anyhow::bail!("unexpected image response"),
            }) {
            Ok(image) => Some((url, image)),
            Err(err) => {
                tracing::warn!(error = %err, "failed to fetch cover art");
                None
            }
        }
    } else {
        None
    };

    tracing::debug!(
        elapsed_ms = started.elapsed().as_millis(),
        "Spotify refresh finished"
    );
    RefreshSnapshot {
        playback,
        queue,
        devices,
        playlists,
        recent,
        cover,
        library_refresh_attempted: refresh_library,
        errors,
        elapsed_ms: started.elapsed().as_millis(),
    }
}

fn handle_key(
    app: &mut App,
    key: KeyEvent,
    async_tx: &mpsc::UnboundedSender<AsyncResult>,
) -> Result<bool> {
    if matches!(key.code, KeyCode::Char('c')) && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Ok(true);
    }

    if app.search_input_active {
        match key.code {
            KeyCode::Esc => app.search_input_active = false,
            KeyCode::Enter => {
                app.search_input_active = false;
                start_search(app, async_tx);
            }
            KeyCode::Backspace => {
                app.search_query.pop();
            }
            KeyCode::Char(c) => app.search_query.push(c),
            _ => {}
        }
        return Ok(false);
    }

    if app.show_help {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') | KeyCode::Char('q') => {
                app.show_help = false;
            }
            _ => {}
        }
        return Ok(false);
    }

    if app.pending_g {
        app.pending_g = false;
        if matches!(key.code, KeyCode::Char('g')) {
            app.move_top();
            return Ok(false);
        }
    }

    match key.code {
        KeyCode::Char('q') => return Ok(true),
        KeyCode::Char('?') => app.show_help = true,
        KeyCode::Char('1') => switch_screen(app, Screen::Search),
        KeyCode::Char('2') => switch_screen(app, Screen::Queue),
        KeyCode::Char('3') => switch_screen(app, Screen::Playlists),
        KeyCode::Char('4') => switch_screen(app, Screen::Devices),
        KeyCode::Tab => cycle_screen(app),
        KeyCode::BackTab => cycle_prev_screen(app),
        KeyCode::Esc => app.back(),
        KeyCode::Char('/') => {
            app.screen = Screen::Search;
            app.search_input_active = true;
        }
        KeyCode::Char('j') | KeyCode::Down => app.move_down(),
        KeyCode::Char('k') | KeyCode::Up => app.move_up(),
        KeyCode::PageDown => app.page_down(),
        KeyCode::PageUp => app.page_up(),
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => app.page_down(),
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => app.page_up(),
        KeyCode::Char('g') => app.pending_g = true,
        KeyCode::Char('G') => app.move_bottom(),
        KeyCode::Enter => activate_selected(app, async_tx),
        KeyCode::Char(' ') => command_then_refresh(
            app,
            async_tx,
            if app.playback.is_playing {
                CommandKind::Pause
            } else {
                CommandKind::Resume
            },
        ),
        KeyCode::Char('n') => command_then_refresh(app, async_tx, CommandKind::Next),
        KeyCode::Char('p') => command_then_refresh(app, async_tx, CommandKind::Previous),
        KeyCode::Left => {
            let position = app.playback.progress_ms.saturating_sub(15_000);
            command_then_refresh(
                app,
                async_tx,
                CommandKind::Seek {
                    position_ms: position,
                },
            );
        }
        KeyCode::Right => {
            let position = app.playback.progress_ms.saturating_add(15_000);
            command_then_refresh(
                app,
                async_tx,
                CommandKind::Seek {
                    position_ms: position,
                },
            );
        }
        KeyCode::Char('+') | KeyCode::Char('=') => adjust_volume(app, async_tx, 5),
        KeyCode::Char('-') => adjust_volume(app, async_tx, -5),
        KeyCode::Char('s') => {
            command_then_refresh(
                app,
                async_tx,
                CommandKind::Shuffle {
                    state: !app.playback.shuffle,
                },
            );
        }
        KeyCode::Char('r') => {
            let next = match app.playback.repeat.as_str() {
                "off" => "context",
                "context" => "track",
                _ => "off",
            };
            command_then_refresh(
                app,
                async_tx,
                CommandKind::Repeat {
                    state: next.to_string(),
                },
            );
        }
        KeyCode::Char('e') => queue_selected(app, async_tx),
        KeyCode::Char('x') => transfer_selected(app, async_tx),
        KeyCode::Char('a') | KeyCode::Char('A') => add_current_to_playlist(app, async_tx),
        KeyCode::Char('l') => save_current(app, async_tx),
        KeyCode::Char('u') => app.request_refresh(),
        KeyCode::Char('b') => app.back(),
        _ => {}
    }
    Ok(false)
}

fn start_search(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    let query = app.search_query.clone();
    if query.trim().is_empty() {
        app.search_results.clear();
        app.is_searching = false;
        app.screen = Screen::Search;
        app.selected = 0;
        app.toast = Some("Type a search query".to_string());
        app.error = None;
        return;
    }

    app.is_searching = true;
    app.screen = Screen::Search;
    app.selected = 0;
    app.toast = Some("Searching Spotify...".to_string());
    app.error = None;

    let async_tx = async_tx.clone();
    tokio::spawn(async move {
        let started = Instant::now();
        let result = match time::timeout(
            TUI_SEARCH_TIMEOUT,
            request_data(Request::Search {
                query: query.clone(),
                scope: SearchScopeData::All,
            }),
        )
        .await
        {
            Ok(Ok(ResponseData::SearchResults { items })) => Ok(items),
            Ok(Ok(_)) => Err("unexpected search response".to_string()),
            Ok(Err(err)) => Err(short_error(err)),
            Err(_) => Err(format!(
                "search timed out after {}s",
                TUI_SEARCH_TIMEOUT.as_secs()
            )),
        };
        let _ = async_tx.send(AsyncResult::Search {
            query,
            result,
            elapsed_ms: started.elapsed().as_millis(),
        });
    });
}

fn activate_selected(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    match app.screen {
        Screen::Playlists if app.selected_playlist_id.is_none() => {
            open_playlist(app, async_tx);
        }
        Screen::Devices => transfer_selected(app, async_tx),
        _ => {
            if let Some(item) = app.selected_item() {
                command_then_refresh(app, async_tx, CommandKind::PlayItem { item });
            }
        }
    }
}

fn open_playlist(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    let Some(playlist) = app.selected_playlist() else {
        return;
    };
    if !begin_action(app) {
        return;
    }

    let async_tx = async_tx.clone();
    tokio::spawn(async move {
        let result = match time::timeout(
            TUI_PLAYLIST_TIMEOUT,
            request_data(Request::PlaylistTracks {
                playlist: playlist.id.clone(),
            }),
        )
        .await
        {
            Ok(Ok(ResponseData::MediaItems { items })) => Ok(items),
            Ok(Ok(_)) => Err("unexpected playlist response".to_string()),
            Ok(Err(err)) => Err(short_error(err)),
            Err(_) => Err(format!(
                "playlist load timed out after {}s",
                TUI_PLAYLIST_TIMEOUT.as_secs()
            )),
        };
        let _ = async_tx.send(AsyncResult::PlaylistTracks {
            playlist_id: playlist.id,
            playlist_name: playlist.name,
            result,
        });
    });
}

fn queue_selected(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    let item = app.selected_item().or_else(|| app.playback.item.clone());
    if let Some(item) = item {
        command_then_refresh(app, async_tx, CommandKind::QueueItem { item });
    }
}

fn transfer_selected(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    let Some(device) = app.devices.get(app.selected).cloned() else {
        return;
    };
    if device.id.is_none() {
        app.error = Some("Selected device has no transferable id".to_string());
        return;
    }
    command_then_refresh(
        app,
        async_tx,
        CommandKind::Transfer {
            device,
            play: app.playback.is_playing,
        },
    );
}

fn add_current_to_playlist(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    let Some(item) = app.playback.item.clone() else {
        app.error = Some("Nothing is playing".to_string());
        return;
    };

    let playlist = if app.screen == Screen::Playlists {
        if let Some(id) = &app.selected_playlist_id {
            let name = app
                .selected_playlist_name
                .clone()
                .unwrap_or_else(|| "playlist".to_string());
            Some((id.clone(), name))
        } else {
            app.selected_playlist()
                .map(|playlist| (playlist.id, playlist.name))
        }
    } else {
        None
    };

    let Some((playlist_id, playlist_name)) = playlist else {
        app.screen = Screen::Playlists;
        app.toast = Some("Pick a playlist, then press a to add current item".to_string());
        return;
    };

    command_then_refresh(
        app,
        async_tx,
        CommandKind::AddToPlaylist {
            item,
            playlist_id,
            playlist_name,
        },
    );
}

fn save_current(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    if let Some(item) = app.playback.item.clone() {
        command_then_refresh(app, async_tx, CommandKind::SaveItem { item });
    }
}

fn command_then_refresh(
    app: &mut App,
    async_tx: &mpsc::UnboundedSender<AsyncResult>,
    command: CommandKind,
) {
    if !begin_action(app) {
        return;
    }
    let async_tx = async_tx.clone();
    tokio::spawn(async move {
        let result = match time::timeout(TUI_COMMAND_TIMEOUT, execute_command(command)).await {
            Ok(result) => result.map_err(short_error),
            Err(_) => Err(format!(
                "Spotify command timed out after {}s",
                TUI_COMMAND_TIMEOUT.as_secs()
            )),
        };
        let _ = async_tx.send(AsyncResult::Command(result));
    });
}

async fn execute_command(command: CommandKind) -> Result<CommandResult> {
    let request = match command {
        CommandKind::Pause => Request::PlaybackCommand {
            command: PlaybackCommand::Pause,
        },
        CommandKind::Resume => Request::PlaybackCommand {
            command: PlaybackCommand::Resume,
        },
        CommandKind::TogglePlayback => Request::PlaybackCommand {
            command: PlaybackCommand::Toggle,
        },
        CommandKind::PlayItem { item } => Request::PlaybackCommand {
            command: PlaybackCommand::PlayUri { uri: item.uri },
        },
        CommandKind::PlayUri { uri } => Request::PlaybackCommand {
            command: PlaybackCommand::PlayUri { uri },
        },
        CommandKind::Next => Request::PlaybackCommand {
            command: PlaybackCommand::Next,
        },
        CommandKind::Previous => Request::PlaybackCommand {
            command: PlaybackCommand::Previous,
        },
        CommandKind::Seek { position_ms } => Request::PlaybackCommand {
            command: PlaybackCommand::Seek { position_ms },
        },
        CommandKind::Volume { volume_percent } => Request::PlaybackCommand {
            command: PlaybackCommand::Volume { volume_percent },
        },
        CommandKind::Shuffle { state } => Request::PlaybackCommand {
            command: PlaybackCommand::Shuffle { state },
        },
        CommandKind::Repeat { state } => Request::PlaybackCommand {
            command: PlaybackCommand::Repeat { state },
        },
        CommandKind::QueueItem { item } => Request::QueueAdd { uri: item.uri },
        CommandKind::QueueUri { uri } => Request::QueueAdd { uri },
        CommandKind::Transfer { device, play: _ } => Request::DeviceTransfer {
            device: device.id.unwrap_or(device.name),
        },
        CommandKind::AddToPlaylist {
            item,
            playlist_id,
            playlist_name: _,
        } => Request::PlaylistAddItems {
            playlist: playlist_id,
            uris: vec![item.uri],
        },
        CommandKind::SaveItem { item } => Request::LibrarySave {
            uri: Some(item.uri),
            current: false,
        },
        CommandKind::SaveCurrent => Request::LibrarySave {
            uri: None,
            current: true,
        },
    };

    match request_data(request).await? {
        ResponseData::Mutation { receipt } => Ok(CommandResult {
            message: Some(receipt.message),
            request_refresh: true,
            ..CommandResult::default()
        }),
        _ => anyhow::bail!("unexpected command response"),
    }
}

async fn request_data(request: Request) -> Result<ResponseData> {
    crate::daemon::server::ensure_daemon_running().await?;
    let mut client = IpcClient::connect().await?;
    match client.request(request).await? {
        Response::Ok { data } => Ok(data),
        Response::Error { message, .. } => anyhow::bail!(message),
    }
}

fn begin_action(app: &mut App) -> bool {
    if app.action_in_flight {
        app.toast = Some("Still working...".to_string());
        return false;
    }
    app.action_in_flight = true;
    app.toast = Some("Working...".to_string());
    app.error = None;
    true
}

fn adjust_volume(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>, delta: i16) {
    let current = app
        .playback
        .device
        .as_ref()
        .and_then(|device| device.volume_percent)
        .unwrap_or(50) as i16;
    let volume = (current + delta).clamp(0, 100) as u8;
    command_then_refresh(
        app,
        async_tx,
        CommandKind::Volume {
            volume_percent: volume,
        },
    );
}

fn switch_screen(app: &mut App, screen: Screen) {
    app.screen = screen;
    app.selected = 0;
    app.clamp_selection();
}

fn cycle_screen(app: &mut App) {
    app.screen = match app.screen {
        Screen::Search => Screen::Queue,
        Screen::Queue => Screen::Playlists,
        Screen::Playlists => Screen::Devices,
        Screen::Devices => Screen::Search,
    };
    app.selected = 0;
    app.clamp_selection();
}

fn cycle_prev_screen(app: &mut App) {
    app.screen = match app.screen {
        Screen::Search => Screen::Devices,
        Screen::Queue => Screen::Search,
        Screen::Playlists => Screen::Queue,
        Screen::Devices => Screen::Playlists,
    };
    app.selected = 0;
    app.clamp_selection();
}

fn next_index(index: usize, len: usize) -> usize {
    if len == 0 {
        0
    } else {
        (index + 1).min(len - 1)
    }
}

fn prev_index(index: usize, len: usize) -> usize {
    if len == 0 {
        0
    } else {
        index.saturating_sub(1)
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).context("failed to create terminal")
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn short_error(err: anyhow::Error) -> String {
    err.to_string()
}
