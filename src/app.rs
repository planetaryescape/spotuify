use std::collections::HashSet;
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
use crate::protocol::{
    CacheStatus, DoctorReport, PlaybackCommand, Request, Response, ResponseData, SearchScopeData,
};
use crate::spotify::{Device, MediaItem, Playback, Playlist, Queue};
use crate::tui_actions::{ActionContext, CommandPalette, TuiAction};
use crate::ui;

const TUI_SEARCH_TIMEOUT: Duration = Duration::from_secs(15);
const TUI_PLAYLIST_TIMEOUT: Duration = Duration::from_secs(30);
const TUI_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const TUI_REFRESH_TIMEOUT: Duration = Duration::from_secs(45);
const TUI_LIBRARY_REFRESH_INTERVAL: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Screen {
    Player,
    Search,
    Library,
    Playlists,
    Queue,
    Devices,
    Diagnostics,
}

impl Screen {
    pub const ALL: [Self; 7] = [
        Self::Player,
        Self::Search,
        Self::Library,
        Self::Playlists,
        Self::Queue,
        Self::Devices,
        Self::Diagnostics,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Player => "Player",
            Self::Search => "Search",
            Self::Library => "Library",
            Self::Playlists => "Playlists",
            Self::Queue => "Queue",
            Self::Devices => "Devices",
            Self::Diagnostics => "Diagnostics",
        }
    }

    pub fn action_context(self, in_input: bool, playlist_open: bool) -> ActionContext {
        match self {
            Self::Player => ActionContext::Player,
            Self::Search if in_input => ActionContext::SearchInput,
            Self::Search => ActionContext::SearchResults,
            Self::Library => ActionContext::Library,
            Self::Playlists if playlist_open => ActionContext::PlaylistTracks,
            Self::Playlists => ActionContext::Playlists,
            Self::Queue => ActionContext::Queue,
            Self::Devices => ActionContext::Devices,
            Self::Diagnostics => ActionContext::Diagnostics,
        }
    }
}

pub struct App {
    pub playback: Playback,
    pub queue: Queue,
    pub devices: Vec<Device>,
    pub playlists: Vec<Playlist>,
    pub last_played: Option<MediaItem>,
    pub library_items: Vec<MediaItem>,
    pub playlist_tracks: Vec<MediaItem>,
    pub search_results: Vec<MediaItem>,
    pub is_searching: bool,
    pub action_in_flight: bool,
    pub screen: Screen,
    pub search_query: String,
    pub search_input_active: bool,
    pub list_filter_query: String,
    pub list_filter_active: bool,
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
    pub help_query: String,
    pub command_palette: CommandPalette,
    pub marked_uris: HashSet<String>,
    pub mark_anchor: Option<usize>,
    pub player_large: bool,
    pub diagnostics_report: Option<DoctorReport>,
    pub cache_status: Option<CacheStatus>,
    pub diagnostics_logs: Vec<String>,
    refresh_requested: bool,
    pending_g: bool,
}

struct RefreshSnapshot {
    playback: Option<Playback>,
    queue: Option<Queue>,
    devices: Option<Vec<Device>>,
    playlists: Option<Vec<Playlist>>,
    library: Option<Vec<MediaItem>>,
    recent: Option<Vec<MediaItem>>,
    cover: Option<(String, image::DynamicImage)>,
    doctor: Option<DoctorReport>,
    cache_status: Option<CacheStatus>,
    logs: Option<Vec<String>>,
    library_refresh_attempted: bool,
    errors: Vec<String>,
    elapsed_ms: u128,
}

enum AsyncResult {
    Refresh(Box<RefreshSnapshot>),
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
    Command(Box<std::result::Result<CommandResult, String>>),
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
            library_items: Vec::new(),
            playlist_tracks: Vec::new(),
            search_results: Vec::new(),
            is_searching: false,
            action_in_flight: false,
            screen: Screen::Player,
            search_query: String::new(),
            search_input_active: false,
            list_filter_query: String::new(),
            list_filter_active: false,
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
            help_query: String::new(),
            command_palette: CommandPalette::default(),
            marked_uris: HashSet::new(),
            mark_anchor: None,
            player_large: true,
            diagnostics_report: None,
            cache_status: None,
            diagnostics_logs: Vec::new(),
            refresh_requested: true,
            pending_g: false,
        })
    }

    pub(crate) fn visible_items(&self) -> Vec<MediaItem> {
        let items: &[MediaItem] = match self.screen {
            Screen::Search => &self.search_results,
            Screen::Library => &self.library_items,
            Screen::Queue => &self.queue.items,
            Screen::Playlists if self.selected_playlist_id.is_some() => &self.playlist_tracks,
            _ => &[],
        };
        items
            .iter()
            .filter(|item| matches_filter(&self.list_filter_query, media_item_filter_text(item)))
            .cloned()
            .collect()
    }

    fn selected_item(&self) -> Option<MediaItem> {
        self.visible_items().get(self.selected).cloned()
    }

    fn selected_playlist(&self) -> Option<Playlist> {
        self.filtered_playlists()
            .get(self.playlist_selected)
            .cloned()
    }

    pub(crate) fn current_action_context(&self) -> ActionContext {
        self.screen.action_context(
            self.search_input_active || self.list_filter_active,
            self.selected_playlist_id.is_some(),
        )
    }

    pub(crate) fn selected_count(&self) -> usize {
        self.marked_uris.len()
    }

    pub(crate) fn selected_target_uris(&self) -> Vec<String> {
        let visible = self.visible_items();
        if !self.marked_uris.is_empty() {
            let mut uris = visible
                .iter()
                .filter(|item| self.marked_uris.contains(&item.uri))
                .map(|item| item.uri.clone())
                .collect::<Vec<_>>();
            if uris.is_empty() {
                uris = self.marked_uris.iter().cloned().collect();
                uris.sort();
            }
            return uris;
        }
        self.selected_item()
            .map(|item| vec![item.uri])
            .unwrap_or_default()
    }

    pub(crate) fn requests_for_action(&self, action: TuiAction) -> Vec<Request> {
        let uris = self.selected_target_uris();
        match action {
            TuiAction::QueueSelection => uris
                .into_iter()
                .map(|uri| Request::QueueAdd { uri })
                .collect(),
            TuiAction::LikeSelection => uris
                .into_iter()
                .map(|uri| Request::LibrarySave {
                    uri: Some(uri),
                    current: false,
                })
                .collect(),
            TuiAction::AddSelectionToPlaylist => {
                let Some((playlist, _)) = self.selected_playlist_target() else {
                    return Vec::new();
                };
                if uris.is_empty() {
                    Vec::new()
                } else {
                    vec![Request::PlaylistAddItems { playlist, uris }]
                }
            }
            _ => Vec::new(),
        }
    }

    fn selected_playlist_target(&self) -> Option<(String, String)> {
        if let Some(id) = &self.selected_playlist_id {
            return Some((
                id.clone(),
                self.selected_playlist_name
                    .clone()
                    .unwrap_or_else(|| "playlist".to_string()),
            ));
        }
        self.selected_playlist()
            .map(|playlist| (playlist.id, playlist.name))
    }

    pub(crate) fn filtered_playlists(&self) -> Vec<Playlist> {
        self.playlists
            .iter()
            .filter(|playlist| {
                matches_filter(
                    &self.list_filter_query,
                    format!("{} {} {}", playlist.name, playlist.owner, playlist.id),
                )
            })
            .cloned()
            .collect()
    }

    pub(crate) fn filtered_devices(&self) -> Vec<Device> {
        self.devices
            .iter()
            .filter(|device| {
                matches_filter(
                    &self.list_filter_query,
                    format!("{} {}", device.name, device.kind),
                )
            })
            .cloned()
            .collect()
    }

    fn clamp_selection(&mut self) {
        let len = self.active_len();
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
            Screen::Player | Screen::Diagnostics => 0,
            Screen::Search | Screen::Library | Screen::Queue => self.visible_items().len(),
            Screen::Playlists if self.selected_playlist_id.is_some() => self.visible_items().len(),
            Screen::Playlists => self.filtered_playlists().len(),
            Screen::Devices => self.filtered_devices().len(),
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
        let next = next_index(self.active_selection(), self.active_len());
        self.set_active_selection(next);
    }

    fn move_up(&mut self) {
        let prev = prev_index(self.active_selection(), self.active_len());
        self.set_active_selection(prev);
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
        if let Some(library) = snapshot.library {
            self.library_items = library;
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
        if let Some(doctor) = snapshot.doctor {
            self.diagnostics_report = Some(doctor);
        }
        if let Some(cache_status) = snapshot.cache_status {
            self.cache_status = Some(cache_status);
        }
        if let Some(logs) = snapshot.logs {
            self.diagnostics_logs = logs;
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
            AsyncResult::Refresh(snapshot) => self.apply_refresh(*snapshot),
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
                match *result {
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
    let include_diagnostics = app.screen == Screen::Diagnostics;
    tokio::spawn(async move {
        let snapshot = match time::timeout(
            TUI_REFRESH_TIMEOUT,
            fetch_refresh(current_art_url, refresh_library, include_diagnostics),
        )
        .await
        {
            Ok(snapshot) => snapshot,
            Err(_) => RefreshSnapshot {
                playback: None,
                queue: None,
                devices: None,
                playlists: None,
                library: None,
                recent: None,
                cover: None,
                doctor: None,
                cache_status: None,
                logs: None,
                library_refresh_attempted: refresh_library,
                errors: vec![format!(
                    "refresh timed out after {}s",
                    TUI_REFRESH_TIMEOUT.as_secs()
                )],
                elapsed_ms: TUI_REFRESH_TIMEOUT.as_millis(),
            },
        };
        let _ = async_tx.send(AsyncResult::Refresh(Box::new(snapshot)));
    });
}

async fn fetch_refresh(
    current_art_url: Option<String>,
    refresh_library: bool,
    include_diagnostics: bool,
) -> RefreshSnapshot {
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
    let library = if refresh_library {
        match request_data(Request::LibraryList { limit: 100 }).await {
            Ok(ResponseData::MediaItems { items }) => Some(items),
            Ok(_) => {
                tracing::warn!("unexpected library response");
                None
            }
            Err(err) => {
                tracing::warn!(error = %err, "failed to fetch cached library");
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

    let (doctor, cache_status, logs) = if include_diagnostics {
        let doctor = match request_data(Request::GetDoctorReport).await {
            Ok(ResponseData::DoctorReport { report }) => Some(report),
            Ok(_) => {
                tracing::warn!("unexpected doctor response");
                None
            }
            Err(err) => {
                tracing::warn!(error = %err, "failed to fetch doctor report");
                None
            }
        };
        let cache_status = match request_data(Request::CacheStatus).await {
            Ok(ResponseData::CacheStatus { status }) => Some(status),
            Ok(_) => {
                tracing::warn!("unexpected cache status response");
                None
            }
            Err(err) => {
                tracing::warn!(error = %err, "failed to fetch cache status");
                None
            }
        };
        let logs = match request_data(Request::LogsTail { lines: 40 }).await {
            Ok(ResponseData::Logs { lines }) => Some(lines),
            Ok(_) => {
                tracing::warn!("unexpected logs response");
                None
            }
            Err(err) => {
                tracing::warn!(error = %err, "failed to fetch logs");
                None
            }
        };
        (doctor, cache_status, logs)
    } else {
        (None, None, None)
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
        library,
        recent,
        cover,
        doctor,
        cache_status,
        logs,
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

    if app.error.is_some() {
        if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
            app.error = None;
        }
        return Ok(false);
    }

    if app.command_palette.visible {
        if let Some(action) = handle_palette_key(app, key) {
            return apply_tui_action(app, action, async_tx);
        }
        return Ok(false);
    }

    if app.search_input_active || app.list_filter_active {
        handle_text_input(app, key, async_tx);
        return Ok(false);
    }

    if app.show_help {
        handle_help_key(app, key);
        return Ok(false);
    }

    if let Some(action) = action_from_key(app, key) {
        return apply_tui_action(app, action, async_tx);
    }
    Ok(false)
}

fn handle_text_input(app: &mut App, key: KeyEvent, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    if app.search_input_active {
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => app.search_input_active = false,
            (KeyCode::Enter, _) => {
                app.search_input_active = false;
                start_search(app, async_tx);
            }
            (KeyCode::Backspace, _) => {
                app.search_query.pop();
            }
            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                app.search_query.push(c);
            }
            _ => {}
        }
        return;
    }

    match (key.code, key.modifiers) {
        (KeyCode::Esc | KeyCode::Enter, _) => app.list_filter_active = false,
        (KeyCode::Backspace, _) => {
            app.list_filter_query.pop();
            app.set_active_selection(0);
        }
        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            app.list_filter_query.push(c);
            app.set_active_selection(0);
        }
        _ => {}
    }
}

fn handle_help_key(app: &mut App, key: KeyEvent) {
    match (key.code, key.modifiers) {
        (KeyCode::Esc | KeyCode::Enter, _) | (KeyCode::Char('?'), _) | (KeyCode::Char('q'), _) => {
            app.show_help = false;
            app.help_query.clear();
        }
        (KeyCode::Backspace, _) => {
            app.help_query.pop();
        }
        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => app.help_query.push(c),
        _ => {}
    }
}

fn handle_palette_key(app: &mut App, key: KeyEvent) -> Option<TuiAction> {
    match (key.code, key.modifiers) {
        (KeyCode::Enter, _) => app.command_palette.confirm(),
        (KeyCode::Esc, _) => {
            app.command_palette.close();
            None
        }
        (KeyCode::Backspace, _) => {
            app.command_palette.on_backspace();
            None
        }
        (KeyCode::Down, _) | (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
            app.command_palette.select_next();
            None
        }
        (KeyCode::Up, _) | (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
            app.command_palette.select_prev();
            None
        }
        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            app.command_palette.on_char(c);
            None
        }
        _ => None,
    }
}

fn action_from_key(app: &mut App, key: KeyEvent) -> Option<TuiAction> {
    if app.pending_g {
        app.pending_g = false;
        if matches!(
            (key.code, key.modifiers),
            (KeyCode::Char('g'), KeyModifiers::NONE)
        ) {
            return Some(TuiAction::JumpTop);
        }
    }

    match (key.code, key.modifiers) {
        (KeyCode::Char('q'), _) => Some(TuiAction::Quit),
        (KeyCode::Char('?'), _) => Some(TuiAction::Help),
        (KeyCode::Char('p'), KeyModifiers::CONTROL) => Some(TuiAction::OpenCommandPalette),
        (KeyCode::Char('1'), KeyModifiers::NONE) => Some(TuiAction::OpenPlayer),
        (KeyCode::Char('2'), KeyModifiers::NONE) => Some(TuiAction::OpenSearch),
        (KeyCode::Char('3'), KeyModifiers::NONE) => Some(TuiAction::OpenLibrary),
        (KeyCode::Char('4'), KeyModifiers::NONE) => Some(TuiAction::OpenPlaylists),
        (KeyCode::Char('5'), KeyModifiers::NONE) => Some(TuiAction::OpenQueue),
        (KeyCode::Char('6'), KeyModifiers::NONE) => Some(TuiAction::OpenDevices),
        (KeyCode::Char('7'), KeyModifiers::NONE) => Some(TuiAction::OpenDiagnostics),
        (KeyCode::Tab, _) => Some(next_screen_action(app.screen)),
        (KeyCode::BackTab, _) => Some(prev_screen_action(app.screen)),
        (KeyCode::Esc, _) if app.selected_count() > 0 => Some(TuiAction::ClearMarks),
        (KeyCode::Esc, _) => Some(TuiAction::Back),
        (KeyCode::Char('/'), KeyModifiers::NONE) => Some(TuiAction::StartSearchInput),
        (KeyCode::Char('f'), KeyModifiers::CONTROL) => Some(TuiAction::StartListFilter),
        (KeyCode::Char('j') | KeyCode::Down, _) => Some(TuiAction::MoveDown),
        (KeyCode::Char('k') | KeyCode::Up, _) => Some(TuiAction::MoveUp),
        (KeyCode::PageDown, _) | (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
            Some(TuiAction::PageDown)
        }
        (KeyCode::PageUp, _) | (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
            Some(TuiAction::PageUp)
        }
        (KeyCode::Char('g'), KeyModifiers::NONE) => {
            app.pending_g = true;
            None
        }
        (KeyCode::Char('G'), _) => Some(TuiAction::JumpBottom),
        (KeyCode::Enter, _) if app.screen == Screen::Devices => Some(TuiAction::TransferDevice),
        (KeyCode::Enter, _)
            if app.screen == Screen::Playlists && app.selected_playlist_id.is_none() =>
        {
            Some(TuiAction::OpenSelected)
        }
        (KeyCode::Enter, _) => Some(TuiAction::PlaySelected),
        (KeyCode::Char(' '), KeyModifiers::NONE) => Some(TuiAction::PlayPause),
        (KeyCode::Char('n'), KeyModifiers::NONE) => Some(TuiAction::Next),
        (KeyCode::Char('p'), KeyModifiers::NONE) => Some(TuiAction::Previous),
        (KeyCode::Left, _) => Some(TuiAction::SeekBack),
        (KeyCode::Right, _) => Some(TuiAction::SeekForward),
        (KeyCode::Char('+') | KeyCode::Char('='), KeyModifiers::NONE) => Some(TuiAction::VolumeUp),
        (KeyCode::Char('-'), KeyModifiers::NONE) => Some(TuiAction::VolumeDown),
        (KeyCode::Char('s'), KeyModifiers::NONE) => Some(TuiAction::ToggleShuffle),
        (KeyCode::Char('r'), KeyModifiers::NONE) => Some(TuiAction::CycleRepeat),
        (KeyCode::Char('e'), KeyModifiers::NONE) => Some(TuiAction::QueueSelection),
        (KeyCode::Char('x'), KeyModifiers::NONE) => Some(TuiAction::TransferDevice),
        (KeyCode::Char('a'), KeyModifiers::NONE) | (KeyCode::Char('A'), _) => {
            Some(TuiAction::AddSelectionToPlaylist)
        }
        (KeyCode::Char('l'), KeyModifiers::NONE) => Some(TuiAction::LikeSelection),
        (KeyCode::Char('u'), KeyModifiers::NONE) => Some(TuiAction::Refresh),
        (KeyCode::Char('b'), KeyModifiers::NONE) => Some(TuiAction::Back),
        (KeyCode::Char('m'), KeyModifiers::NONE) => Some(TuiAction::ToggleMark),
        (KeyCode::Char('M'), _) => Some(TuiAction::MarkRange),
        (KeyCode::Char('z'), KeyModifiers::NONE) => Some(TuiAction::TogglePlayerMode),
        _ => None,
    }
}

fn apply_tui_action(
    app: &mut App,
    action: TuiAction,
    async_tx: &mpsc::UnboundedSender<AsyncResult>,
) -> Result<bool> {
    match action {
        TuiAction::Quit => return Ok(true),
        TuiAction::Help => app.show_help = !app.show_help,
        TuiAction::OpenCommandPalette => app
            .command_palette
            .open(app.current_action_context(), app.selected_count()),
        TuiAction::OpenPlayer
        | TuiAction::OpenSearch
        | TuiAction::OpenLibrary
        | TuiAction::OpenPlaylists
        | TuiAction::OpenQueue
        | TuiAction::OpenDevices => {
            apply_screen_switch(app, action);
        }
        TuiAction::OpenDiagnostics => {
            switch_screen(app, Screen::Diagnostics);
            app.request_refresh();
        }
        TuiAction::MoveDown => app.move_down(),
        TuiAction::MoveUp => app.move_up(),
        TuiAction::PageDown => app.page_down(),
        TuiAction::PageUp => app.page_up(),
        TuiAction::JumpTop => app.move_top(),
        TuiAction::JumpBottom => app.move_bottom(),
        TuiAction::Back => app.back(),
        TuiAction::Refresh => app.request_refresh(),
        TuiAction::StartSearchInput => {
            app.screen = Screen::Search;
            app.search_input_active = true;
            app.list_filter_active = false;
        }
        TuiAction::StartListFilter => {
            app.list_filter_active = true;
            app.search_input_active = false;
            app.set_active_selection(0);
        }
        TuiAction::SubmitSearch => start_search(app, async_tx),
        TuiAction::CancelInput => {
            app.search_input_active = false;
            app.list_filter_active = false;
        }
        TuiAction::PlayPause => command_then_refresh(
            app,
            async_tx,
            if app.playback.is_playing {
                CommandKind::Pause
            } else {
                CommandKind::Resume
            },
        ),
        TuiAction::Next => command_then_refresh(app, async_tx, CommandKind::Next),
        TuiAction::Previous => command_then_refresh(app, async_tx, CommandKind::Previous),
        TuiAction::SeekBack => {
            let position = app.playback.progress_ms.saturating_sub(15_000);
            command_then_refresh(
                app,
                async_tx,
                CommandKind::Seek {
                    position_ms: position,
                },
            );
        }
        TuiAction::SeekForward => {
            let position = app.playback.progress_ms.saturating_add(15_000);
            command_then_refresh(
                app,
                async_tx,
                CommandKind::Seek {
                    position_ms: position,
                },
            );
        }
        TuiAction::VolumeUp => adjust_volume(app, async_tx, 5),
        TuiAction::VolumeDown => adjust_volume(app, async_tx, -5),
        TuiAction::ToggleShuffle => command_then_refresh(
            app,
            async_tx,
            CommandKind::Shuffle {
                state: !app.playback.shuffle,
            },
        ),
        TuiAction::CycleRepeat => {
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
        TuiAction::OpenSelected => open_playlist(app, async_tx),
        TuiAction::PlaySelected => activate_selected(app, async_tx),
        TuiAction::QueueSelection => queue_selection(app, async_tx),
        TuiAction::LikeSelection => like_selection(app, async_tx),
        TuiAction::AddSelectionToPlaylist => add_selection_to_playlist(app, async_tx),
        TuiAction::TransferDevice => transfer_selected(app, async_tx),
        TuiAction::ToggleMark => toggle_mark_selected(app),
        TuiAction::MarkRange => mark_range(app),
        TuiAction::ClearMarks => clear_marks(app),
        TuiAction::TogglePlayerMode => app.player_large = !app.player_large,
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
                source: crate::protocol::SearchSourceData::Hybrid,
                limit: 10,
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

fn queue_selection(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    let requests = app.requests_for_action(TuiAction::QueueSelection);
    if requests.is_empty() {
        if let Some(item) = app.playback.item.clone() {
            command_then_refresh(app, async_tx, CommandKind::QueueItem { item });
        }
        return;
    }
    let count = requests.len();
    requests_then_refresh(app, async_tx, requests, format!("Queued {count} item(s)"));
}

fn transfer_selected(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    let Some(device) = app.filtered_devices().get(app.selected).cloned() else {
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

fn like_selection(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    let requests = app.requests_for_action(TuiAction::LikeSelection);
    if requests.is_empty() {
        if let Some(item) = app.playback.item.clone() {
            command_then_refresh(app, async_tx, CommandKind::SaveItem { item });
        }
        return;
    }
    let count = requests.len();
    requests_then_refresh(app, async_tx, requests, format!("Liked {count} item(s)"));
}

fn add_selection_to_playlist(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    let target_count = app.selected_target_uris().len();
    let requests = app.requests_for_action(TuiAction::AddSelectionToPlaylist);
    if requests.is_empty() {
        if target_count == 0 {
            add_current_to_playlist(app, async_tx);
        } else {
            app.screen = Screen::Playlists;
            app.toast = Some("Pick a playlist, then press a to add marked items".to_string());
        }
        return;
    }
    requests_then_refresh(
        app,
        async_tx,
        requests,
        format!("Added {target_count} item(s) to playlist"),
    );
}

fn toggle_mark_selected(app: &mut App) {
    let Some(item) = app.selected_item() else {
        app.toast = Some("Nothing to mark in this view".to_string());
        return;
    };
    if app.marked_uris.contains(&item.uri) {
        app.marked_uris.remove(&item.uri);
        app.toast = Some(format!("Unmarked {}", item.name));
    } else {
        app.marked_uris.insert(item.uri.clone());
        app.mark_anchor = Some(app.active_selection());
        app.toast = Some(format!("Marked {}", item.name));
    }
}

fn mark_range(app: &mut App) {
    let items = app.visible_items();
    if items.is_empty() {
        return;
    }
    let current = app.active_selection().min(items.len() - 1);
    let anchor = app.mark_anchor.unwrap_or(current).min(items.len() - 1);
    let (start, end) = if anchor <= current {
        (anchor, current)
    } else {
        (current, anchor)
    };
    for item in &items[start..=end] {
        app.marked_uris.insert(item.uri.clone());
    }
    app.toast = Some(format!("Marked {} item(s)", end + 1 - start));
}

fn clear_marks(app: &mut App) {
    let count = app.marked_uris.len();
    app.marked_uris.clear();
    app.mark_anchor = None;
    app.toast = Some(format!("Cleared {count} marked item(s)"));
}

fn add_current_to_playlist(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    let Some(item) = app.playback.item.clone() else {
        app.error = Some("Nothing is playing".to_string());
        return;
    };

    let playlist = (app.screen == Screen::Playlists)
        .then(|| app.selected_playlist_target())
        .flatten();

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
        let _ = async_tx.send(AsyncResult::Command(Box::new(result)));
    });
}

fn requests_then_refresh(
    app: &mut App,
    async_tx: &mpsc::UnboundedSender<AsyncResult>,
    requests: Vec<Request>,
    message: String,
) {
    if requests.is_empty() || !begin_action(app) {
        return;
    }
    let async_tx = async_tx.clone();
    tokio::spawn(async move {
        let result =
            match time::timeout(TUI_COMMAND_TIMEOUT, execute_requests(requests, message)).await {
                Ok(result) => result.map_err(short_error),
                Err(_) => Err(format!(
                    "Spotify command timed out after {}s",
                    TUI_COMMAND_TIMEOUT.as_secs()
                )),
            };
        let _ = async_tx.send(AsyncResult::Command(Box::new(result)));
    });
}

async fn execute_requests(requests: Vec<Request>, message: String) -> Result<CommandResult> {
    for request in requests {
        match request_data(request).await? {
            ResponseData::Mutation { .. } => {}
            _ => anyhow::bail!("unexpected command response"),
        }
    }
    Ok(CommandResult {
        message: Some(message),
        request_refresh: true,
        ..CommandResult::default()
    })
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
    app.list_filter_active = false;
    app.list_filter_query.clear();
    app.clamp_selection();
}

fn next_screen_action(screen: Screen) -> TuiAction {
    let index = Screen::ALL
        .iter()
        .position(|candidate| *candidate == screen)
        .unwrap_or(0);
    screen_action(Screen::ALL[(index + 1) % Screen::ALL.len()])
}

fn prev_screen_action(screen: Screen) -> TuiAction {
    let index = Screen::ALL
        .iter()
        .position(|candidate| *candidate == screen)
        .unwrap_or(0);
    screen_action(Screen::ALL[index.checked_sub(1).unwrap_or(Screen::ALL.len() - 1)])
}

fn screen_action(screen: Screen) -> TuiAction {
    match screen {
        Screen::Player => TuiAction::OpenPlayer,
        Screen::Search => TuiAction::OpenSearch,
        Screen::Library => TuiAction::OpenLibrary,
        Screen::Playlists => TuiAction::OpenPlaylists,
        Screen::Queue => TuiAction::OpenQueue,
        Screen::Devices => TuiAction::OpenDevices,
        Screen::Diagnostics => TuiAction::OpenDiagnostics,
    }
}

fn apply_screen_switch(app: &mut App, action: TuiAction) -> bool {
    match action {
        TuiAction::OpenPlayer => switch_screen(app, Screen::Player),
        TuiAction::OpenSearch => switch_screen(app, Screen::Search),
        TuiAction::OpenLibrary => switch_screen(app, Screen::Library),
        TuiAction::OpenPlaylists => switch_screen(app, Screen::Playlists),
        TuiAction::OpenQueue => switch_screen(app, Screen::Queue),
        TuiAction::OpenDevices => switch_screen(app, Screen::Devices),
        TuiAction::OpenDiagnostics => switch_screen(app, Screen::Diagnostics),
        _ => return false,
    }
    true
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

fn matches_filter(query: &str, text: String) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    let text = text.to_ascii_lowercase();
    query
        .split_whitespace()
        .all(|token| text.contains(&token.to_ascii_lowercase()))
}

fn media_item_filter_text(item: &MediaItem) -> String {
    format!(
        "{} {} {} {}",
        item.name, item.subtitle, item.context, item.uri
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spotify::MediaKind;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn test_app() -> App {
        App {
            playback: Playback::default(),
            queue: Queue::default(),
            devices: Vec::new(),
            playlists: Vec::new(),
            last_played: None,
            library_items: Vec::new(),
            playlist_tracks: Vec::new(),
            search_results: Vec::new(),
            is_searching: false,
            action_in_flight: false,
            screen: Screen::Player,
            search_query: String::new(),
            search_input_active: false,
            list_filter_query: String::new(),
            list_filter_active: false,
            selected: 0,
            playlist_selected: 0,
            selected_playlist_id: None,
            selected_playlist_name: None,
            toast: None,
            error: None,
            last_progress_tick: Instant::now(),
            current_art_url: None,
            cover: None,
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
            diagnostics_report: None,
            cache_status: None,
            diagnostics_logs: Vec::new(),
            refresh_requested: false,
            pending_g: false,
        }
    }

    fn item(uri: &str, name: &str) -> MediaItem {
        MediaItem {
            id: Some(uri.rsplit(':').next().unwrap_or(uri).to_string()),
            uri: uri.to_string(),
            name: name.to_string(),
            subtitle: "Artist".to_string(),
            context: "Album".to_string(),
            duration_ms: 180_000,
            image_url: None,
            kind: MediaKind::Track,
            source: None,
            freshness: None,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn text_input_captures_space_before_global_play_pause() {
        let mut app = test_app();
        app.search_input_active = true;
        let (tx, mut rx) = mpsc::unbounded_channel();

        let should_quit = handle_key(&mut app, key(KeyCode::Char(' ')), &tx).unwrap();

        assert!(!should_quit);
        assert_eq!(app.search_query, " ");
        assert!(
            !app.action_in_flight,
            "space must not dispatch playback while typing"
        );
        assert!(
            rx.try_recv().is_err(),
            "typing must not enqueue async daemon work"
        );
    }

    #[test]
    fn current_list_filter_changes_visible_items_without_global_search_query() {
        let mut app = test_app();
        app.screen = Screen::Search;
        app.search_query = "luther".to_string();
        app.search_results = vec![
            item("spotify:track:alpha", "Alpha Song"),
            item("spotify:track:beta", "Beta Song"),
        ];
        app.list_filter_query = "beta".to_string();

        let visible = app.visible_items();

        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].uri, "spotify:track:beta");
        assert_eq!(
            app.search_query, "luther",
            "list filter must not mutate global search"
        );
    }

    #[test]
    fn multi_select_queue_requests_follow_visible_list_order() {
        let mut app = test_app();
        app.screen = Screen::Search;
        app.search_results = vec![
            item("spotify:track:first", "First"),
            item("spotify:track:second", "Second"),
        ];
        app.marked_uris.insert("spotify:track:second".to_string());
        app.marked_uris.insert("spotify:track:first".to_string());

        let requests = app.requests_for_action(TuiAction::QueueSelection);

        assert_eq!(
            requests,
            vec![
                Request::QueueAdd {
                    uri: "spotify:track:first".to_string(),
                },
                Request::QueueAdd {
                    uri: "spotify:track:second".to_string(),
                },
            ]
        );
    }

    #[test]
    fn multi_select_add_to_playlist_batches_marked_uris_into_one_request() {
        let mut app = test_app();
        app.screen = Screen::Playlists;
        app.playlists = vec![Playlist {
            id: "playlist-1".to_string(),
            name: "Road Trip".to_string(),
            owner: "me".to_string(),
            tracks_total: 0,
            image_url: None,
        }];
        app.marked_uris.insert("spotify:track:first".to_string());
        app.marked_uris.insert("spotify:track:second".to_string());

        let requests = app.requests_for_action(TuiAction::AddSelectionToPlaylist);

        assert_eq!(
            requests,
            vec![Request::PlaylistAddItems {
                playlist: "playlist-1".to_string(),
                uris: vec![
                    "spotify:track:first".to_string(),
                    "spotify:track:second".to_string(),
                ],
            }]
        );
    }

    #[test]
    fn command_palette_opens_with_current_device_context() {
        let mut app = test_app();
        app.screen = Screen::Devices;
        let (tx, _) = mpsc::unbounded_channel();

        let should_quit = apply_tui_action(&mut app, TuiAction::OpenCommandPalette, &tx).unwrap();

        assert!(!should_quit);
        assert!(app.command_palette.visible);
        let labels = app
            .command_palette
            .visible_commands()
            .into_iter()
            .map(|command| command.label)
            .collect::<Vec<_>>();
        assert!(labels.contains(&"Transfer Device"));
        assert!(!labels.contains(&"Queue Selected"));
    }
}
