use std::collections::HashSet;
use std::io;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Margin, Rect};
use ratatui::Terminal;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use tokio::sync::mpsc;
use tokio::time;

use crate::tui_actions::{ActionContext, CommandPalette, TuiAction};
use crate::ui;
use spotuify_cli::actions::{CommandKind, CommandResult};
use spotuify_core::SyncedLyrics;
use spotuify_protocol::ipc_client::IpcClient;
use spotuify_protocol::{
    CacheStatus, DaemonEvent, DoctorReport, PlaybackCommand, Request, Response, ResponseData,
    SearchScopeData,
};
use spotuify_spotify::client::{Device, MediaItem, MediaKind, Playback, Playlist, Queue};
use spotuify_spotify::config::Config;

const TUI_PLAYLIST_TIMEOUT: Duration = Duration::from_secs(30);
const TUI_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
// 5 minutes — the initial library sync paginates Spotify's
// `/me/tracks` 50 items at a time. A 5,000-track library takes
// ~100 round-trips; 45 s was nowhere near enough and a single
// timeout pushed the next attempt out 15 minutes via the cooldown.
const TUI_REFRESH_TIMEOUT: Duration = Duration::from_secs(300);
const TUI_REFRESH_CONCURRENCY: usize = 6;
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
    Lyrics,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RightRailMode {
    #[default]
    Hidden,
    Queue,
    Lyrics,
    Hints,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FullscreenPanel {
    Queue,
    Lyrics,
}

impl Screen {
    pub const ALL: [Self; 8] = [
        Self::Player,
        Self::Search,
        Self::Library,
        Self::Playlists,
        Self::Queue,
        Self::Devices,
        Self::Diagnostics,
        Self::Lyrics,
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
            Self::Lyrics => "Lyrics",
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
            Self::Lyrics => ActionContext::Lyrics,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingReceiptState {
    pub receipt_id: spotuify_protocol::ReceiptId,
    pub action: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BannerState {
    RateLimited {
        retry_after_secs: u64,
        scope: String,
    },
    Auth {
        kind: spotuify_protocol::AuthErrorKind,
    },
    Deprecated {
        endpoint: String,
    },
    Compat {
        endpoint: String,
    },
}

/// Phase 13 (P13-L) — destructive-action confirmation modal. Captures
/// the deferred action so on `y` we dispatch it; on `n`/`Esc` we just
/// close the modal. Mirrors spotify-player commit #966.
pub struct ConfirmModal {
    pub title: String,
    pub body: String,
    pub on_confirm: TuiAction,
}

pub struct PlaylistPickerModal {
    pub uris: Vec<String>,
    pub selected: usize,
    pub selected_playlist_ids: HashSet<String>,
}

pub struct DevicePickerModal {
    pub selected: usize,
}

/// Modal that fires when the daemon emits
/// `DaemonEvent::AuthError { kind: InvalidGrant }` — the user's
/// refresh token has been revoked and we need them to OAuth again.
///
/// Three-phase lifecycle so the modal can show progress instead of
/// freezing during the browser handshake. See the key-routing branch
/// in `handle_key` for the transition rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginModal {
    pub phase: LoginPhase,
    /// Latest progress event from the OAuth flow. Rendered inside the
    /// modal so the URL / "browser opened" / "saved" messages never
    /// leak to stdout while the TUI owns the alt-screen buffer.
    pub last_progress: Option<spotuify_spotify::auth::LoginProgress>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginPhase {
    /// Initial state. Shows "Spotify session expired. Press Enter to
    /// re-authenticate, Esc to dismiss."
    AwaitingConfirm,
    /// User pressed Enter, OAuth task spawned. Browser is open
    /// somewhere; we're waiting for the redirect callback.
    InProgress,
    /// Login attempt failed (browser closed, network error, token
    /// exchange rejected). Body shows the error; Enter retries, Esc
    /// dismisses.
    Failed(String),
}

/// Per-MediaKind pagination state for the streaming search.
///
/// Tracks the next Spotify offset to request, whether a page is in
/// flight (so repeated scroll keypresses don't fire duplicate
/// requests), and whether Spotify has signalled exhaustion (an empty
/// page or the `limit + offset > 1000` wall).
#[derive(Debug, Default, Clone)]
pub struct SearchPaneState {
    pub loading: bool,
    pub exhausted: bool,
    pub error: Option<String>,
    pub next_offset: u32,
    /// Pages keyed by Spotify offset. We store each page under its
    /// offset rather than appending to a flat list so out-of-order
    /// arrivals (Spotify's three-parallel initial fanout, or later
    /// scroll-fetches whose responses race) don't scramble the
    /// rendered order. Rebuilding the flat `search_results` walks
    /// `pages` in offset order, giving the user a stable, stream-as-
    /// you-go list that only ever grows downwards.
    pub pages: std::collections::BTreeMap<u32, Vec<MediaItem>>,
}

pub struct App {
    pub playback: Playback,
    pub queue: Queue,
    pub devices: Vec<Device>,
    pub playlists: Vec<Playlist>,
    pub inaccessible_playlist_ids: HashSet<String>,
    pub last_played: Option<MediaItem>,
    pub library_items: Vec<MediaItem>,
    pub playlist_tracks: Vec<MediaItem>,
    pub search_results: Vec<MediaItem>,
    /// Monotonic version stamped on each new search. Carried by
    /// `Request::SearchStream`/`SearchPage` and echoed in
    /// `DaemonEvent::SearchPage`/`SearchComplete`. Stale events whose
    /// version doesn't match `search_version` are dropped.
    pub search_version: u64,
    /// Per-pane scroll-pagination state. Populated when a streaming
    /// search runs; cleared on each `start_search`.
    pub search_panes: std::collections::HashMap<MediaKind, SearchPaneState>,
    /// True once the user has explicitly moved the cursor or picked a
    /// pane (`g t/r/b/p/s/e`) during the current search. While false,
    /// `app.selected` is auto-snapped to the first item of the highest-
    /// priority non-empty kind (Tracks > Artists > Albums > Playlists >
    /// Shows > Episodes) each time new results stream in. This stops
    /// the focused-pane lottery: previously `app.selected = 0` pointed
    /// at whichever provider's response landed first, so identical
    /// queries could highlight Artists one run and Podcasts the next.
    pub search_user_steered: bool,
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
    /// Set when the user just issued a track-changing command
    /// (Next/Previous/PlayItem/PlayUri). Suppresses refresh-time
    /// overwrites of `progress_ms` while Spotify finishes transitioning
    /// — without this, the daemon's first refresh after the command
    /// often still reports the OLD track, and we'd snap the bar back
    /// to wherever that track was. Cleared once we observe the
    /// expected track URI change or after a short timeout.
    pub awaiting_track_change_until: Option<Instant>,
    pub current_art_url: Option<String>,
    pub cover: Option<StatefulProtocol>,
    /// Wall-clock of the most recent event-driven write to `self.playback`.
    /// Used by the `AsyncResult::Seed` apply path to drop stale seeds when
    /// a newer `DaemonEvent::PlaybackChanged` has already updated state.
    /// `None` means no write has happened yet (pre-bootstrap or post-clear).
    pub playback_updated_at: Option<Instant>,
    /// `false` until the daemon has confirmed playback state at least
    /// once (via Seed or PlaybackChanged). Distinguishes "we don't know
    /// yet" (render as "Connecting…") from "Spotify says nothing is
    /// playing" (render as "Ready when you are"). A 3-second
    /// degraded-daemon escape hatch flips this to `true` even without
    /// a daemon reply so the spinner can't lock the UI forever.
    pub playback_known: bool,
    /// Mirror of `playback_updated_at` for `self.queue`.
    pub queue_updated_at: Option<Instant>,
    /// Mirror of `playback_updated_at` for `self.devices`.
    pub devices_updated_at: Option<Instant>,
    /// Wall-clock the TUI process started. Used by the cold-start
    /// auth-modal grace window: if `DaemonEvent::AuthError` lands
    /// within ~5s of launch, defer the modal so the macOS Keychain
    /// dialog (often shown at the same moment) can be dealt with
    /// first without a second modal stacking on top.
    pub started_at: Instant,
    /// Set to `true` when an `AuthError { InvalidGrant }` event
    /// arrives; cleared on any subsequent success signal
    /// (PlaybackChanged, SyncFinished, PlayerReady). Read by the
    /// deferred modal-open handler to skip the modal if the daemon
    /// self-healed during the grace window.
    pub auth_revoked_observed: bool,
    /// Set when an `AuthError` arrives within the startup grace
    /// window and a deferred modal-open is in flight. Prevents
    /// re-scheduling on rapid repeated events.
    pub pending_auth_modal_until: Option<Instant>,
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
    pub right_rail: RightRailMode,
    pub fullscreen_panel: Option<FullscreenPanel>,
    // Phase 17 — visualization state. `spectrum_bands` is updated on every
    // DaemonEvent::SpectrumFrame; the player_large layout renders it as a
    // 12-bar equalizer at the bottom of the left pane when `viz_enabled`
    // is true.
    pub viz_enabled: bool,
    pub viz_configured_source: spotuify_protocol::VizSourceKindData,
    pub viz_active_source: spotuify_protocol::VizActiveSource,
    pub spectrum_bands: [f32; 12],
    pub spectrum_peak: f32,
    pub viz_color_scheme: String,
    pub viz_last_frame_at: Option<Instant>,
    /// Phase 7 — daemon-supplied human-readable explanation when the
    /// active source is `None` (e.g. "switch to embedded backend",
    /// "install BlackHole on macOS"). Used by the bottom-panel viz
    /// status line.
    pub viz_hint: Option<String>,
    /// Phase 7 — backend kind reported by the daemon at the most recent
    /// `VizSourceChanged`. Used to phrase the TUI viz status hint
    /// correctly. `None` until the first source-change event.
    pub viz_backend_kind: Option<spotuify_core::BackendKind>,
    pub diagnostics_report: Option<DoctorReport>,
    pub cache_status: Option<CacheStatus>,
    pub diagnostics_logs: Vec<String>,
    pub lyrics: Option<SyncedLyrics>,
    pub lyrics_track_uri: Option<String>,
    pub lyrics_failed_track_uri: Option<String>,
    pub lyrics_offset_ms: i64,
    pub lyrics_loading: bool,
    pub lyrics_error: Option<String>,
    /// Phase 13 (P13-L) — modal popup for destructive-action
    /// confirmation. Active modal blocks normal input until y/n.
    pub confirm_modal: Option<ConfirmModal>,
    pub playlist_picker: Option<PlaylistPickerModal>,
    pub device_picker: Option<DevicePickerModal>,
    /// Interactive re-authentication modal. Opens automatically when
    /// the daemon emits `DaemonEvent::AuthError { kind: InvalidGrant }`.
    /// Key routing slots it right after the error modal so it blocks
    /// other input while a session-expired prompt is live.
    pub login_modal: Option<LoginModal>,
    /// Phase 12 (F16 scaffold): last 20 operations rendered in a panel
    /// inside the Diagnostics screen. Pass 2 (P12.6) populates this via
    /// `Request::OpsLog` and binds `u` to undo the selected row.
    pub operations: Vec<spotuify_protocol::Operation>,
    /// Selection cursor inside `operations`.
    pub operations_cursor: usize,
    pub pending_receipts: Vec<PendingReceiptState>,
    pub banner: Option<BannerState>,
    /// When Enter is pressed on an artist, we open a two-column view:
    /// albums on the left, tracks of the focused album on the right.
    pub artist_view: Option<ArtistViewState>,
    pub(crate) refresh_requested: bool,
    pub(crate) pending_g: bool,
}

#[derive(Clone, Debug)]
pub struct ArtistViewState {
    pub artist_uri: String,
    pub artist_name: String,
    pub albums: Vec<MediaItem>,
    pub album_selected: usize,
    pub album_tracks: Vec<MediaItem>,
    pub track_selected: usize,
    pub focus: ArtistViewSide,
    pub loading_albums: bool,
    pub loading_tracks: bool,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtistViewSide {
    Albums,
    Tracks,
}

struct LyricsSnapshot {
    track_uri: String,
    lyrics: Option<SyncedLyrics>,
    offset_ms: i64,
}

struct RefreshSnapshot {
    // Push-driven fields (playback / queue / devices / cover) intentionally
    // absent: those have a single event-driven writer per the architecture
    // contract — see `apply_daemon_event` and `apply_seed`. Refresh is now
    // poll-only ancillary state.
    playlists: Option<Vec<Playlist>>,
    library: Option<Vec<MediaItem>>,
    recent: Option<Vec<MediaItem>>,
    doctor: Option<DoctorReport>,
    cache_status: Option<CacheStatus>,
    logs: Option<Vec<String>>,
    operations: Option<Vec<spotuify_protocol::Operation>>,
    lyrics: Option<LyricsSnapshot>,
    lyrics_error: Option<String>,
    lyrics_error_track_uri: Option<String>,
    library_refresh_attempted: bool,
    errors: Vec<String>,
    elapsed_ms: u128,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RefreshRead {
    Playlists,
    Library,
    Recent,
    Doctor,
    CacheStatus,
    Logs,
    Operations,
}

impl RefreshRead {
    fn request(self) -> Request {
        match self {
            Self::Playlists => Request::PlaylistsList,
            Self::Library => Request::LibraryList { limit: 100 },
            Self::Recent => Request::RecentlyPlayed,
            Self::Doctor => Request::GetDoctorReport,
            Self::CacheStatus => Request::CacheStatus,
            Self::Logs => Request::LogsTail { lines: 40 },
            Self::Operations => Request::OpsLog {
                limit: 20,
                since_ms: None,
                source: None,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RefreshPlan {
    library: bool,
    diagnostics: bool,
    lyrics: bool,
}

#[allow(clippy::large_enum_variant)]
enum AsyncResult {
    Refresh(Box<RefreshSnapshot>),
    Search {
        query: String,
        result: std::result::Result<Vec<MediaItem>, String>,
    },
    PlaylistTracks {
        playlist_id: String,
        playlist_name: String,
        expected_total: u64,
        result: std::result::Result<Vec<MediaItem>, String>,
    },
    ArtistAlbums {
        artist_uri: String,
        result: std::result::Result<Vec<MediaItem>, String>,
    },
    AlbumTracks {
        album_uri: String,
        result: std::result::Result<Vec<MediaItem>, String>,
    },
    Command(Box<std::result::Result<CommandResult, String>>),
    DaemonEvent(DaemonEvent),
    /// Cover-art fetch result. URL is the version — `apply_async_result`
    /// accepts iff `self.current_art_url == Some(url)`. Stale fetches
    /// (track advanced before our fetch completed) self-discard.
    CoverFetched {
        url: String,
        image: image::DynamicImage,
    },
    /// One-shot bootstrap or recovery seed for push-driven state.
    /// Issued on TUI startup, daemon-event reconnect, and
    /// `RecvError::Lagged`. `fetched_at` is the timestamp at which the
    /// seed RPC was issued; apply writes a field only when no newer
    /// event-driven write has happened since.
    Seed {
        playback: Option<Playback>,
        queue: Option<Queue>,
        devices: Option<Vec<Device>>,
        viz: Option<spotuify_protocol::VizDiagnostics>,
        /// Recently-played items from the daemon's SQLite cache.
        /// Drives `app.last_played` so the player widget falls back
        /// to something meaningful at t≈0ms when no track is
        /// currently playing — without this the user stares at a
        /// blank widget until the next `SyncFinished` triggers a
        /// refresh (~3-13s depending on rate-limit state).
        recent: Option<Vec<MediaItem>>,
        fetched_at: Instant,
    },
    /// Result of the interactive OAuth re-login flow spawned from the
    /// `LoginModal`. Apply: on Ok, close the modal and fire
    /// `Request::ReloadAuth` so the daemon picks up the fresh token.
    /// On Err, transition the modal to `LoginPhase::Failed(msg)`.
    LoginCompleted {
        result: std::result::Result<(), String>,
    },
    /// Progress update from the OAuth flow. Rendered inside the
    /// LoginModal so status lines never bleed to stdout while the
    /// TUI owns the alt-screen buffer.
    LoginProgress(spotuify_spotify::auth::LoginProgress),
    /// Fired by `tokio::time::sleep` when the cold-start grace timer
    /// elapses. The handler opens the LoginModal only if the
    /// auth-revoked condition still holds — if the daemon
    /// self-healed in the grace window, the modal stays closed.
    OpenLoginModalIfStillNeeded,
}

impl App {
    async fn new() -> Result<Self> {
        let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
        // Visualizer ships ON by default: this is a music player and the
        // spectrum is part of the player's identity. Users on a Connect
        // backend (no PCM samples) won't see bars move — the spectrum
        // region still draws a flat baseline so the layout stays stable.
        // Override with `[viz] enabled = false` in spotuify.toml.
        let loaded_config = Config::load().ok();
        let viz_color_scheme = loaded_config
            .as_ref()
            .map(|c| c.viz.color_scheme.clone())
            .unwrap_or_else(|| "spotify-green".to_string());
        let viz_enabled_default = loaded_config
            .as_ref()
            .map(|c| c.viz.enabled)
            .unwrap_or(true);

        Ok(Self {
            playback: Playback::default(),
            queue: Queue::default(),
            devices: Vec::new(),
            playlists: Vec::new(),
            inaccessible_playlist_ids: HashSet::new(),
            last_played: None,
            library_items: Vec::new(),
            playlist_tracks: Vec::new(),
            search_results: Vec::new(),
            search_version: 0,
            search_panes: std::collections::HashMap::new(),
            search_user_steered: false,
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
            awaiting_track_change_until: None,
            current_art_url: None,
            cover: None,
            playback_updated_at: None,
            queue_updated_at: None,
            devices_updated_at: None,
            playback_known: false,
            started_at: Instant::now(),
            auth_revoked_observed: false,
            pending_auth_modal_until: None,
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
            right_rail: RightRailMode::Lyrics,
            fullscreen_panel: None,
            viz_enabled: viz_enabled_default,
            viz_configured_source: spotuify_protocol::VizSourceKindData::Auto,
            viz_active_source: spotuify_protocol::VizActiveSource::None,
            spectrum_bands: [0.0; 12],
            spectrum_peak: 0.0,
            viz_color_scheme,
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
            login_modal: None,
            operations: Vec::new(),
            operations_cursor: 0,
            pending_receipts: Vec::new(),
            banner: None,
            artist_view: None,
            refresh_requested: true,
            pending_g: false,
        })
    }

    /// Recompute `search_results` from each pane's offset-keyed pages
    /// in deterministic order: kinds in render priority (Tracks first),
    /// pages within a kind in ascending offset order, items within a
    /// page in arrival order. URIs that appear in multiple pages /
    /// kinds resolve to the first occurrence, so a stable position is
    /// reserved as soon as the URI is first seen.
    ///
    /// Called whenever a `SearchPage` event updates a pane. Cost is
    /// O(N) in total search items, which is bounded by Spotify's
    /// `limit + offset ≤ 1000` wall (~6,000 items across all six
    /// kinds at most) — well below the threshold where a more clever
    /// incremental update would be worth the complexity.
    pub(crate) fn rebuild_search_results(&mut self) {
        use std::collections::HashSet;
        const RENDER_ORDER: [MediaKind; 6] = [
            MediaKind::Track,
            MediaKind::Artist,
            MediaKind::Album,
            MediaKind::Playlist,
            MediaKind::Show,
            MediaKind::Episode,
        ];
        let mut combined = Vec::with_capacity(self.search_results.len());
        let mut seen: HashSet<String> = HashSet::new();
        for kind in RENDER_ORDER {
            let Some(pane) = self.search_panes.get(&kind) else {
                continue;
            };
            for page in pane.pages.values() {
                for item in page {
                    if seen.insert(item.uri.clone()) {
                        combined.push(item.clone());
                    }
                }
            }
        }
        self.search_results = combined;
    }

    pub(crate) fn visible_items(&self) -> Vec<MediaItem> {
        let items: &[MediaItem] = match self.screen {
            Screen::Player | Screen::Queue if self.queue.session_active => &self.queue.items,
            Screen::Player | Screen::Queue => &[],
            Screen::Search => &self.search_results,
            Screen::Library => &self.library_items,
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
            .filter(|playlist| !self.inaccessible_playlist_ids.contains(&playlist.id))
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
        let mut devices = self
            .devices
            .iter()
            .filter(|device| {
                matches_filter(
                    &self.list_filter_query,
                    format!("{} {}", device.name, device.kind),
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        // Stable identity-based order: sorting on `is_active` would
        // make rows jump every time Spotify's active-device telemetry
        // flips during a poll. The active/restricted state is already
        // visible in the row, so it doesn't need to drive ordering.
        // Fall back to name then id for deterministic placement of
        // devices that share an id (shouldn't happen) or whose id is
        // missing in fake/test payloads.
        devices.sort_by(|a, b| {
            a.id.cmp(&b.id).then_with(|| {
                a.name
                    .to_ascii_lowercase()
                    .cmp(&b.name.to_ascii_lowercase())
            })
        });
        devices
    }

    pub(crate) fn filtered_diagnostics_logs(&self) -> Vec<String> {
        self.diagnostics_logs
            .iter()
            .filter(|line| matches_filter(&self.list_filter_query, (*line).clone()))
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
            Screen::Player => self.visible_items().len(),
            Screen::Lyrics => 0,
            Screen::Diagnostics => self.filtered_diagnostics_logs().len(),
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
        // Cursor moved by an explicit user-input path (move_up/down,
        // page_up/down, jump_top/bottom, mouse click). On Search we
        // take that as a signal to stop auto-snapping to the
        // preferred kind on each streaming result page.
        if self.screen == Screen::Search {
            self.search_user_steered = true;
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

    /// When a daemon error indicates the OAuth refresh token was
    /// revoked, route the user straight into the in-TUI login flow
    /// instead of dropping them into the generic "Action failed"
    /// modal that ends with "run `spotuify login`". The user should
    /// never have to quit the TUI to recover. Returns `true` when
    /// the error was an auth-revoked variant and was consumed by the
    /// `LoginModal`; the caller should NOT also set `self.error` in
    /// that case.
    fn open_login_modal_if_auth_revoked(&mut self, error: &str) -> bool {
        if auth_error_kind_from_error(error).is_none() {
            return false;
        }
        if self.login_modal.is_none() {
            self.login_modal = Some(LoginModal {
                phase: LoginPhase::AwaitingConfirm,
                last_progress: None,
            });
        }
        // Clear any latent error string so the generic modal does
        // not paint over the login modal on next render.
        self.error = None;
        true
    }

    fn apply_refresh(&mut self, snapshot: RefreshSnapshot) {
        let had_sync = self.last_sync.is_some();
        // Capture before we consume the snapshot's library field below.
        let library_arrived = snapshot.library.is_some();
        self.is_syncing = false;
        self.last_sync = Some(Instant::now());

        // NOTE: playback / queue / devices / cover are *not* applied
        // here. They each have a single authoritative writer:
        //   - `playback`  ← DaemonEvent::PlaybackChanged + Seed
        //   - `queue`     ← DaemonEvent::QueueChanged + Seed
        //   - `devices`   ← DaemonEvent::DevicesChanged + Seed
        //   - `cover` / `current_art_url`  ← derived from PlaybackChanged
        //     via spawn_cover_fetch → AsyncResult::CoverFetched
        // Refresh is now poll-only ancillary state (library/recent/
        // playlists/diagnostics/cache/lyrics/ops/logs).
        if let Some(playlists) = snapshot.playlists {
            self.playlists = playlists;
        }
        if let Some(library) = snapshot.library {
            // Library renders as two side-by-side panels: Music on
            // the left (Track / Album / Artist), Podcasts on the right
            // (Show / Episode). Navigation (j/k) walks `library_items`
            // as a single flat list, though — so if music and podcasts
            // are interleaved (which the SQL `ORDER BY fetched_at_ms`
            // gives us, since shows get re-fetched on their own
            // cadence), the cursor lurches between panels mid-scroll.
            // Partition into music-first / podcasts-last so the flat
            // list mirrors the panel layout: scrolling down stays in
            // Music until you genuinely cross the boundary.
            self.library_items = partition_library_for_navigation(library);
        }
        if let Some(recent) = snapshot.recent {
            if let Some(item) = recent.first() {
                self.last_played = Some(item.clone());
            }
            if self.search_results.is_empty() && self.search_query.is_empty() {
                self.search_results = recent;
            }
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
        if let Some(operations) = snapshot.operations {
            self.operations = operations;
            if self.operations_cursor >= self.operations.len() {
                self.operations_cursor = self.operations.len().saturating_sub(1);
            }
        }
        if let Some(lyrics) = snapshot.lyrics {
            // Phase 6 — lyrics fetches are async; by the time the result
            // arrives the user may already be on a different track. Drop
            // the result if the lyrics URI no longer matches the active
            // playback URI, so the user never sees stale lyrics scrolling
            // against a different song's progress.
            let active_uri = self.playback.item.as_ref().map(|i| i.uri.as_str());
            if active_uri == Some(lyrics.track_uri.as_str()) {
                self.lyrics_track_uri = Some(lyrics.track_uri);
                self.lyrics_failed_track_uri = None;
                self.lyrics_offset_ms = lyrics.offset_ms;
                self.lyrics = lyrics.lyrics;
                self.lyrics_loading = false;
                self.lyrics_error = if self.lyrics.is_some() {
                    None
                } else {
                    Some("No lyrics found for this track".to_string())
                };
            } else {
                tracing::debug!(
                    target: "spotuify_tui::merge",
                    lyrics_uri = %lyrics.track_uri,
                    active_uri = active_uri.unwrap_or(""),
                    "tui_lyrics_stale_dropped"
                );
            }
        }
        if let Some(error) = snapshot.lyrics_error {
            self.lyrics_loading = false;
            self.lyrics_error = Some(error);
            if let Some(uri) = snapshot.lyrics_error_track_uri {
                let active_uri = self.playback.item.as_ref().map(|i| i.uri.as_str());
                if active_uri == Some(uri.as_str()) {
                    self.lyrics_failed_track_uri = Some(uri);
                }
            }
        }

        if snapshot.errors.is_empty() {
            if !had_sync {
                self.toast = Some(format!("Synced Spotify in {}ms", snapshot.elapsed_ms));
            }
        } else {
            let error = snapshot.errors.join("; ");
            tracing::warn!(error, "Spotify sync finished with errors");
            if !self.open_login_modal_if_auth_revoked(&error) {
                self.error = Some(error);
            }
        }
        // Only mark the library as freshly synced when items
        // actually came back. A timeout or transient error would
        // otherwise put the next attempt on a 15-minute cooldown
        // and the user gets stuck looking at "Fetching your library…"
        // forever.
        if snapshot.library_refresh_attempted && library_arrived {
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

    /// Merge a freshly polled playback snapshot into local state.
    ///
    /// Spotify's `/me/player` reports a position that was true *at the
    /// moment Spotify processed the request*, not at the moment we
    /// receive the response. Overwriting our smoothly-ticking local
    /// progress with that stale value yanks the seek bar backwards by
    /// the round-trip latency every poll cycle.
    ///
    /// Strategy: only re-anchor `progress_ms` when an event the user
    /// would expect to cause a jump has occurred. Between such events,
    /// keep ticking from the last anchor — that's what makes the bar
    /// feel synced with the audio.
    fn merge_playback(&mut self, incoming: spotuify_core::Playback) {
        // First confirmed answer from the daemon (even an empty
        // playback) flips the spinner off. Without this, the
        // "Connecting…" state would persist for accounts where Spotify
        // is genuinely idle.
        self.playback_known = true;
        let current_uri = self.playback.item.as_ref().map(|i| i.uri.as_str());
        let incoming_uri = incoming.item.as_ref().map(|i| i.uri.as_str());
        let track_changed = current_uri != incoming_uri;
        let is_playing_changed = self.playback.is_playing != incoming.is_playing;
        let shuffle_changed = self.playback.shuffle != incoming.shuffle;
        let repeat_changed = self.playback.repeat != incoming.repeat;
        let device_changed = self.playback.device.as_ref().map(|d| d.id.clone())
            != incoming.device.as_ref().map(|d| d.id.clone());

        // Phase 6 — when the daemon labels the snapshot as
        // PlayerEvent or CommandResult, it's authoritative: trust
        // the progress completely (event-derived state always beats
        // the local extrapolation guess). Same-track drift > 1.5s
        // also re-anchors so remote-device seeks don't drift.
        let authoritative = matches!(
            incoming.source,
            Some(spotuify_core::PlaybackStateSource::PlayerEvent)
                | Some(spotuify_core::PlaybackStateSource::CommandResult)
        );
        let drift_ms = (incoming.progress_ms as i64 - self.playback.progress_ms as i64).abs();
        let drift_reanchor = !track_changed && drift_ms > 1500;
        if drift_reanchor || authoritative {
            tracing::debug!(
                target: "spotuify_tui::merge",
                drift_ms,
                source = ?incoming.source,
                "tui_progress_reanchored"
            );
        }

        // While a Next/Previous is in flight, ignore refreshes that
        // still report the OLD track. Once Spotify has transitioned
        // (or the window expires), normal merging resumes.
        //
        // Phase 6 exception: a PlayerEvent / CommandResult snapshot
        // bypasses the guard — those signals come from the source of
        // truth and should never be held back.
        let awaiting = self
            .awaiting_track_change_until
            .filter(|deadline| Instant::now() < *deadline)
            .is_some();
        if awaiting && incoming_uri != current_uri && !authoritative {
            // Pull in everything except the optimistic progress + item;
            // stale Web API polls often report the old track or no
            // active session while Spotify is still activating the new
            // Connect target.
            let preserved_progress = self.playback.progress_ms;
            let preserved_item = self.playback.item.clone();
            self.playback = incoming;
            self.playback.progress_ms = preserved_progress;
            self.playback.item = preserved_item;
            return;
        }
        // Track has changed (or the window expired) — clear the guard.
        if track_changed {
            self.awaiting_track_change_until = None;
        }

        // First-ever sync (no anchor yet, local progress is 0).
        let first_sync = self.last_sync.is_none() && self.playback.item.is_none();

        let must_resync = track_changed
            || is_playing_changed
            || shuffle_changed
            || repeat_changed
            || device_changed
            || first_sync
            || authoritative
            || drift_reanchor;

        if must_resync {
            self.playback = incoming;
            self.last_progress_tick = Instant::now();
        } else {
            // Pull in everything from the snapshot EXCEPT the stale
            // progress timestamp, so the bar keeps ticking smoothly.
            let preserved_progress = self.playback.progress_ms;
            self.playback = incoming;
            self.playback.progress_ms = preserved_progress;
            // Do not reset `last_progress_tick`: tick_progress already
            // advances real time since the last frame; leaving the
            // anchor alone keeps the rate accurate.
        }
    }

    #[cfg(test)]
    fn apply_async_result(&mut self, result: AsyncResult) {
        let (dummy_tx, _rx) = mpsc::unbounded_channel();
        self.apply_async_result_with(result, &dummy_tx);
    }

    fn apply_async_result_with(
        &mut self,
        result: AsyncResult,
        async_tx: &mpsc::UnboundedSender<AsyncResult>,
    ) {
        match result {
            AsyncResult::Refresh(snapshot) => self.apply_refresh(*snapshot),
            AsyncResult::Search { query, result } => {
                if query != self.search_query {
                    tracing::debug!(query, current = %self.search_query, "dropping stale search result");
                    return;
                }
                // Under streaming-search the SearchStream ack arrives
                // here BEFORE any pages have streamed in. We only use
                // this path to surface daemon-side request failures
                // (timeout, connection refused). On success the actual
                // results land via DaemonEvent::SearchPage; is_searching
                // is cleared by DaemonEvent::SearchComplete.
                if let Err(error) = result {
                    self.is_searching = false;
                    if !self.open_login_modal_if_auth_revoked(&error) {
                        self.error = Some(error);
                    }
                    self.clamp_selection();
                }
            }
            AsyncResult::PlaylistTracks {
                playlist_id,
                playlist_name,
                expected_total,
                result,
            } => {
                self.action_in_flight = false;
                match result {
                    Ok(tracks) => {
                        self.selected_playlist_id = Some(playlist_id);
                        self.selected_playlist_name = Some(playlist_name);
                        self.playlist_tracks = tracks;
                        self.selected = 0;
                        self.toast =
                            Some(if self.playlist_tracks.is_empty() && expected_total > 0 {
                                "Loading tracks...".to_string()
                            } else {
                                format!("Loaded {} tracks", self.playlist_tracks.len())
                            });
                    }
                    Err(error) => {
                        if playlist_tracks_forbidden(&error) {
                            self.inaccessible_playlist_ids.insert(playlist_id);
                            self.toast = Some(format!(
                                "{playlist_name} is unavailable to third-party apps"
                            ));
                        } else if !self.open_login_modal_if_auth_revoked(&error) {
                            self.error = Some(error);
                        }
                    }
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
                        if let Some(message) = result.message {
                            self.toast = Some(message);
                        }
                        if result.request_refresh {
                            self.request_refresh();
                        }
                    }
                    Err(error) => {
                        if !self.open_login_modal_if_auth_revoked(&error) {
                            self.error = Some(error);
                        }
                    }
                }
                self.clamp_selection();
            }
            AsyncResult::ArtistAlbums { artist_uri, result } => {
                let mut auto_load: Option<String> = None;
                if let Some(view) = self.artist_view.as_mut() {
                    if view.artist_uri != artist_uri {
                        return;
                    }
                    view.loading_albums = false;
                    match result {
                        Ok(items) => {
                            view.albums = items;
                            view.album_selected = 0;
                            view.error = None;
                            auto_load = view.albums.first().map(|a| a.uri.clone());
                        }
                        Err(err) => view.error = Some(err),
                    }
                }
                if let Some(album_uri) = auto_load {
                    load_album_tracks(self, async_tx, album_uri);
                }
            }
            AsyncResult::AlbumTracks { album_uri, result } => {
                if let Some(view) = self.artist_view.as_mut() {
                    let expected_uri = view.albums.get(view.album_selected).map(|a| a.uri.clone());
                    if expected_uri.as_deref() != Some(&album_uri) {
                        // Result is for a different album than the
                        // user has focused now; drop it.
                        return;
                    }
                    view.loading_tracks = false;
                    match result {
                        Ok(items) => {
                            view.album_tracks = items;
                            view.track_selected = 0;
                            view.error = None;
                        }
                        Err(err) => view.error = Some(err),
                    }
                }
            }
            AsyncResult::DaemonEvent(event) => self.apply_daemon_event(event, async_tx),
            AsyncResult::CoverFetched { url, image } => {
                if self.current_art_url.as_deref() == Some(url.as_str()) {
                    self.cover = Some(self.picker.new_resize_protocol(image));
                } else {
                    tracing::debug!(
                        target: "spotuify_tui::merge",
                        stale_url = %url,
                        "tui_cover_stale_dropped"
                    );
                }
            }
            AsyncResult::Seed {
                playback,
                queue,
                devices,
                viz,
                recent,
                fetched_at,
            } => self.apply_seed(playback, queue, devices, viz, recent, fetched_at, async_tx),
            AsyncResult::LoginCompleted { result } => match result {
                Ok(()) => {
                    self.login_modal = None;
                    self.banner = None;
                    self.toast = Some("Logged in to Spotify".to_string());
                    // Tell the daemon to drop its cached (broken) token
                    // and clear the auth-revoked latch so the next call
                    // re-reads the fresh credentials we just persisted.
                    spawn_reload_auth(async_tx.clone());
                }
                Err(message) => {
                    if let Some(modal) = self.login_modal.as_mut() {
                        modal.phase = LoginPhase::Failed(message);
                    } else {
                        // Modal was dismissed mid-flight; surface the
                        // error via toast so it isn't silently lost.
                        self.toast = Some(format!("Re-login failed: {message}"));
                    }
                }
            },
            AsyncResult::LoginProgress(event) => {
                if let Some(modal) = self.login_modal.as_mut() {
                    modal.last_progress = Some(event);
                }
            }
            AsyncResult::OpenLoginModalIfStillNeeded => {
                // Cold-start grace timer fired. Open the modal only
                // if the auth-revoked condition still holds — i.e. no
                // successful playback event has arrived since the
                // grace period started. Tracked via the
                // `auth_revoked_observed` flag set when the event
                // landed and cleared on any success signal.
                if self.auth_revoked_observed && self.login_modal.is_none() {
                    self.login_modal = Some(LoginModal {
                        phase: LoginPhase::AwaitingConfirm,
                        last_progress: None,
                    });
                }
                self.pending_auth_modal_until = None;
            }
        }
    }

    /// Apply a one-shot seed of push-driven state. Each field is
    /// written ONLY when no newer event-driven write has happened
    /// since `fetched_at` — events are authoritative, seed is just a
    /// bootstrap/recovery path.
    #[allow(clippy::too_many_arguments)]
    fn apply_seed(
        &mut self,
        playback: Option<Playback>,
        queue: Option<Queue>,
        devices: Option<Vec<Device>>,
        viz: Option<spotuify_protocol::VizDiagnostics>,
        recent: Option<Vec<MediaItem>>,
        fetched_at: Instant,
        async_tx: &mpsc::UnboundedSender<AsyncResult>,
    ) {
        if let Some(pb) = playback {
            let stale = self.playback_updated_at.is_some_and(|t| t >= fetched_at);
            if !stale {
                let new_art_url = pb.item.as_ref().and_then(|i| i.image_url.clone());
                self.merge_playback(pb);
                self.playback_updated_at = Some(Instant::now());
                self.handle_art_url_change(new_art_url, async_tx);
                self.request_lyrics_if_visible();
            }
        }
        if let Some(q) = queue {
            let stale = self.queue_updated_at.is_some_and(|t| t >= fetched_at);
            if !stale {
                self.queue = q;
                self.queue_updated_at = Some(Instant::now());
            }
        }
        if let Some(d) = devices {
            let stale = self.devices_updated_at.is_some_and(|t| t >= fetched_at);
            if !stale {
                self.devices = d;
                self.devices_updated_at = Some(Instant::now());
            }
        }
        if let Some(viz) = viz {
            self.viz_enabled = viz.enabled;
            self.viz_configured_source = viz.configured_source;
            self.viz_active_source = viz.active_source;
            self.viz_hint = viz.hint;
            self.viz_backend_kind = viz.backend_kind;
        }
        // Recently-played has no event-driven writer in the TUI yet
        // (refresh-only), so a None last_played is always a candidate
        // for the seed to fill. If the user has played anything ever,
        // `recent[0]` is what the player widget falls back to when no
        // track is currently playing.
        if let Some(items) = recent {
            if let Some(first) = items.first() {
                self.last_played = Some(first.clone());
            }
            if self.search_results.is_empty() && self.search_query.is_empty() {
                self.search_results = items;
            }
        }
    }

    /// Compare a new track's image URL against `self.current_art_url`.
    /// On change: clear the displayed cover immediately (no stale
    /// image in-frame) and spawn the dedicated cover-art fetch. The
    /// fetch result lands later as `AsyncResult::CoverFetched`, gated
    /// by URL match so out-of-order resolution self-discards.
    fn handle_art_url_change(
        &mut self,
        new_url: Option<String>,
        async_tx: &mpsc::UnboundedSender<AsyncResult>,
    ) {
        if new_url.as_deref() == self.current_art_url.as_deref() {
            return;
        }
        self.current_art_url = new_url.clone();
        self.cover = None;
        if let Some(url) = new_url {
            spawn_cover_fetch(url, async_tx.clone());
        }
    }

    fn apply_daemon_event(
        &mut self,
        event: DaemonEvent,
        async_tx: &mpsc::UnboundedSender<AsyncResult>,
    ) {
        match event {
            DaemonEvent::ShutdownRequested => {
                self.error = Some("Daemon is shutting down".to_string());
            }
            DaemonEvent::PlaybackChanged { action, playback } => {
                // Phase 3 — daemon embeds the fresh `Playback` snapshot
                // so we apply locally without a `PlaybackGet` round-trip.
                // Event is the sole authoritative writer for `self.playback`;
                // refresh no longer touches it. Cover-art fetch is spawned
                // here so it's keyed on the actual track change, not on a
                // periodic poll's stale snapshot.
                if let Some(pb) = playback {
                    let new_art_url = pb.item.as_ref().and_then(|i| i.image_url.clone());
                    self.merge_playback(pb);
                    self.playback_updated_at = Some(Instant::now());
                    self.handle_art_url_change(new_art_url, async_tx);
                    self.request_lyrics_if_visible();
                }
                // Toast only on real user-facing actions (Pause,
                // Play, Next, Previous, Seek, Toggle, transfer…).
                // Skip the daemon-internal events that fire on the
                // 3s background cadence — `synced`, `snapshot`, and
                // `optimistic-*` would otherwise spam the toast row
                // every cycle and the user can't tell anything ever
                // actually changed.
                if !is_background_event_action(&action) {
                    self.toast = Some(format!("Playback updated: {action}"));
                }
                // A successful playback event means the daemon's
                // auth latch has self-healed. Clear the cold-start
                // grace flag so the deferred modal opener skips.
                self.auth_revoked_observed = false;
            }
            DaemonEvent::QueueChanged {
                action,
                uris,
                queue,
            } => {
                if let Some(q) = queue {
                    self.queue = q;
                    self.queue_updated_at = Some(Instant::now());
                }
                if !is_background_event_action(&action) {
                    self.toast = Some(format!("Queue updated: {} item(s)", uris.len()));
                }
            }
            DaemonEvent::DevicesChanged { devices, .. } => {
                if let Some(d) = devices {
                    self.devices = d;
                    self.devices_updated_at = Some(Instant::now());
                }
            }
            DaemonEvent::EventStreamLagged { skipped } => {
                // Daemon told us the broadcast buffer overflowed and we
                // missed `skipped` events. Push-driven state may be
                // stale; re-seed to recover. The seed apply path's
                // tie-breaker guarantees we don't clobber any newer
                // events that arrive between the lag and the seed result.
                tracing::warn!(skipped, "event stream lagged; reseeding push state");
                spawn_seed(async_tx.clone());
            }
            DaemonEvent::PlaylistsChanged { action, playlist } => {
                if action == "tracks-inaccessible" {
                    if let Some(id) = playlist {
                        self.inaccessible_playlist_ids.insert(id);
                    }
                } else if action == "tracks-refreshed" {
                    if let Some(id) = playlist.as_deref() {
                        if self.selected_playlist_id.as_deref() == Some(id) {
                            if let Some(playlist) =
                                self.playlists.iter().find(|playlist| playlist.id == id)
                            {
                                spawn_playlist_tracks_request(
                                    async_tx.clone(),
                                    playlist.id.clone(),
                                    playlist.name.clone(),
                                    playlist.tracks_total,
                                );
                            }
                        }
                    }
                }
                self.request_refresh();
            }
            DaemonEvent::LibraryChanged { .. }
            | DaemonEvent::SearchUpdated { .. }
            | DaemonEvent::SyncFinished { .. }
            | DaemonEvent::MutationFinished { .. } => self.request_refresh(),
            DaemonEvent::SearchPage {
                query,
                kind,
                offset,
                version,
                items,
            } => {
                if query != self.search_query || version != self.search_version {
                    tracing::debug!(
                        query,
                        version,
                        current_query = %self.search_query,
                        current_version = self.search_version,
                        "dropping stale search-page event"
                    );
                    return;
                }
                let arrived = items.len();
                // Remember which item the cursor is on so a rebuild
                // can put it back even if the visible list reorders
                // (rare in steady-state, but possible if an earlier-
                // offset page lands after a later one).
                let selected_uri = self
                    .search_results
                    .get(self.selected)
                    .map(|i| i.uri.clone());

                let pane = self.search_panes.entry(kind).or_default();
                pane.loading = false;
                pane.error = None;
                // Empty page is the canonical exhaustion signal — Spotify's
                // `total` field is unreliable. We also flip exhausted when
                // we've paginated past the limit+offset≤1000 wall (the
                // daemon converts that 400 into an empty page).
                if arrived == 0 {
                    pane.exhausted = true;
                } else {
                    // Advance the offset cursor so the next scroll-trigger
                    // requests the page after this one. Use max() because
                    // the initial fanout fires offsets [0,10,20] in parallel
                    // and events can arrive out of order.
                    pane.next_offset = pane.next_offset.max(offset + arrived as u32);
                }
                // Key the page by its Spotify offset, NOT by arrival
                // order. Rebuilding from offset-sorted pages gives a
                // stable list that only ever grows downwards — no
                // reshuffling when offset=20 arrives before offset=10.
                pane.pages.insert(offset, items);

                self.rebuild_search_results();

                // Anchor the cursor on the highest-priority non-empty
                // kind until the user steers. Once steered, preserve
                // the URI under the cursor across rebuilds so the
                // user's selection follows the same item even if its
                // index shifted.
                if !self.search_user_steered {
                    if let Some(idx) = preferred_search_index(&self.search_results) {
                        self.selected = idx;
                    }
                } else if let Some(uri) = selected_uri {
                    if let Some(idx) = self.search_results.iter().position(|i| i.uri == uri) {
                        self.selected = idx;
                    }
                }
            }
            DaemonEvent::SearchComplete { query, version } => {
                if query != self.search_query || version != self.search_version {
                    return;
                }
                self.is_searching = false;
                let pane_error = self.search_panes.values().any(|pane| pane.error.is_some());
                if !pane_error {
                    self.toast = Some(format!("{} results", self.search_results.len()));
                }
            }
            DaemonEvent::SearchFailed {
                query,
                version,
                kind,
                offset: _,
                message,
            } => {
                if query != self.search_query || version != self.search_version {
                    return;
                }
                if let Some(kind) = kind {
                    let pane = self.search_panes.entry(kind).or_default();
                    pane.loading = false;
                    pane.error = Some(message.clone());
                } else {
                    self.is_searching = false;
                    for pane in self.search_panes.values_mut() {
                        pane.loading = false;
                        pane.error = Some(message.clone());
                    }
                }
                self.toast = Some(format!("Search failed: {message}"));
            }
            DaemonEvent::SyncStarted { target: _ } => {
                // Background polling is invisible to the user — the
                // 3s active cadence would spam a "Syncing recent..."
                // toast every cycle and never clear. Subscribers
                // already see the *real* news as `PlaybackChanged` /
                // `QueueChanged` events when state actually changes.
                // The `is_syncing` flag still flips so any explicit
                // user-initiated `spotuify sync` UI can render its
                // own progress indicator.
                self.is_syncing = true;
            }
            DaemonEvent::RateLimited {
                retry_after_secs,
                scope,
            } => {
                self.banner = Some(BannerState::RateLimited {
                    retry_after_secs,
                    scope: scope.clone(),
                });
                self.toast = Some(format!(
                    "Rate limited on {scope}; retrying in {retry_after_secs}s"
                ));
            }
            DaemonEvent::AuthError { kind } => {
                self.banner = Some(BannerState::Auth { kind });
                // For unauthenticated/revoked auth, auto-open
                // the interactive re-login modal so the user can fix
                // it with one keypress. Softer cases (`ScopeReauthRequired`)
                // stay banner-only — that one means the existing token
                // works but is missing newer scopes, and the user can
                // still navigate read-only until they re-auth.
                if matches!(
                    kind,
                    spotuify_protocol::AuthErrorKind::InvalidGrant
                        | spotuify_protocol::AuthErrorKind::NotLoggedIn
                ) && self.login_modal.is_none()
                {
                    self.auth_revoked_observed = true;
                    let in_cold_start = self.started_at.elapsed() < Duration::from_secs(5);
                    // Only defer when we actually have a tokio runtime
                    // to schedule the wake-up on. Unit tests construct
                    // an `App` without a runtime; falling back to the
                    // immediate-open path keeps them green and is the
                    // correct behaviour outside cold start anyway.
                    let can_defer = tokio::runtime::Handle::try_current().is_ok();
                    if in_cold_start && can_defer && self.pending_auth_modal_until.is_none() {
                        // Cold-start grace window: defer the modal by
                        // ~3.5s. macOS often pops a Keychain dialog at
                        // the same moment; stacking a TUI modal on top
                        // is confusing. If the daemon self-heals
                        // (auth_revoked latch clears + a success
                        // event arrives) during the grace window, the
                        // deferred handler suppresses the modal.
                        const GRACE: Duration = Duration::from_millis(3500);
                        let when = Instant::now() + GRACE;
                        self.pending_auth_modal_until = Some(when);
                        let tx = async_tx.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(GRACE).await;
                            let _ = tx.send(AsyncResult::OpenLoginModalIfStillNeeded);
                        });
                    } else {
                        self.login_modal = Some(LoginModal {
                            phase: LoginPhase::AwaitingConfirm,
                            last_progress: None,
                        });
                    }
                }
                self.toast =
                    Some("Authentication needs attention; run `spotuify login`".to_string());
            }
            DaemonEvent::MutationAccepted { receipt_id, action } => {
                if !self
                    .pending_receipts
                    .iter()
                    .any(|receipt| receipt.receipt_id == receipt_id)
                {
                    self.pending_receipts.push(PendingReceiptState {
                        receipt_id,
                        action: action.clone(),
                    });
                }
                self.toast = Some(format!("{action} pending ({receipt_id})"));
            }
            DaemonEvent::MutationFinalized {
                receipt_id,
                status,
                message,
            } => {
                self.pending_receipts
                    .retain(|receipt| receipt.receipt_id != receipt_id);
                self.toast = Some(format_mutation_toast(status, &message));
                self.request_refresh();
            }
            DaemonEvent::SchemaCompat {
                endpoint,
                missing_keys,
            } => {
                // The compat layer already normalised the payload —
                // there is nothing for the user to do, so don't show
                // them a banner with a raw URL + query string. Log it
                // for diagnostics only; the Diagnostics screen surfaces
                // the same info via the recent-events log if needed.
                tracing::warn!(
                    endpoint,
                    ?missing_keys,
                    "Spotify payload missing fields; compat applied"
                );
            }
            // Phase 9 — player backend lifecycle. Wire-level surfacing only;
            // richer banners (premium upsell, reconnect prompt) land with
            // the player banner work in a follow-up sub-phase.
            DaemonEvent::PlayerReady { name, .. } => {
                self.toast = Some(format!("Player ready: {name}"));
            }
            DaemonEvent::PlayerDegraded { reason } => {
                self.toast = Some(format!("Player degraded: {reason}"));
            }
            DaemonEvent::PremiumRequired => {
                self.error = Some(
                    "Streaming unavailable — Spotify Premium required. Browse and control still work."
                        .to_string(),
                );
            }
            DaemonEvent::SessionDisconnected { reason: _ } => {
                tracing::debug!("player session disconnected");
                self.toast = Some("Session disconnected. Reconnecting…".to_string());
            }
            DaemonEvent::PlayerFailed { reason, restarts } => {
                self.error = Some(format!(
                    "Player backend failed after {restarts} restart(s): {reason}. Run `spotuify reconnect`."
                ));
            }
            // Phase 10 — listen qualified. Surface a transient toast;
            // analytics tooling reads from the listen_facts table.
            DaemonEvent::ListenQualified { track_uri, .. } => {
                self.toast = Some(format!("Listen qualified: {track_uri}"));
            }
            // Phase 12 — ops log lifecycle. Foundation pass: refresh the
            // Diagnostics screen if it's open; feature pass (F16/P12.6)
            // adds the dedicated operations panel.
            DaemonEvent::OperationRecorded { kind, .. } => {
                self.toast = Some(format!("Op recorded: {}", kind.label()));
                self.request_refresh();
            }
            DaemonEvent::OperationUndone { success, .. } => {
                self.toast = Some(if success {
                    "Operation undone".to_string()
                } else {
                    "Operation undo failed".to_string()
                });
                self.request_refresh();
            }
            // Phase 13 — daemon told us the config was reloaded; pull
            // a fresh diagnostics report so the TUI shows the new state.
            DaemonEvent::ConfigReloaded => {
                self.toast = Some("Config reloaded".to_string());
                self.request_refresh();
            }
            // Phase 17 — real-time spectrum frame. Update the cached bands +
            // peak so the next render pulls them. We never request a screen
            // refresh here: SpectrumFrame fires at 30 Hz; relying on the
            // existing tick to repaint keeps CPU bounded.
            DaemonEvent::SpectrumFrame { bands, peak, .. } => {
                // bands is always length 12 per protocol contract; defensively
                // copy at most 12 to handle a future-compatible variant.
                let mut next = [0.0_f32; 12];
                for (i, b) in bands.iter().take(12).enumerate() {
                    next[i] = *b;
                }
                self.spectrum_bands = next;
                self.spectrum_peak = peak;
                self.viz_last_frame_at = Some(Instant::now());
            }
            DaemonEvent::VizSourceChanged {
                active,
                configured,
                hint,
                backend_kind,
            } => {
                self.viz_active_source = active;
                self.viz_configured_source = configured;
                self.viz_hint = hint;
                self.viz_backend_kind = backend_kind;
            }
        }
    }
}

fn auth_error_kind_from_error(error: &str) -> Option<spotuify_protocol::AuthErrorKind> {
    let lower = error.to_lowercase();
    if lower.contains("not logged in")
        || lower.contains("run `spotuify login`")
        || lower.contains("login required")
    {
        return Some(spotuify_protocol::AuthErrorKind::NotLoggedIn);
    }
    if lower.contains("auth revoked")
        || lower.contains("re-login required")
        || lower.contains("invalid_grant")
    {
        return Some(spotuify_protocol::AuthErrorKind::InvalidGrant);
    }
    None
}

impl App {
    fn request_lyrics_if_visible(&mut self) {
        let lyrics_visible =
            self.screen == Screen::Lyrics || self.right_rail == RightRailMode::Lyrics;
        if !lyrics_visible {
            return;
        }
        let active_uri = self.playback.item.as_ref().map(|item| item.uri.as_str());
        if active_uri.is_some()
            && active_uri != self.lyrics_track_uri.as_deref()
            && active_uri != self.lyrics_failed_track_uri.as_deref()
        {
            self.lyrics_error = None;
            self.request_refresh();
        }
    }
}

pub async fn run_tui() -> Result<()> {
    spotuify_daemon::server::ensure_daemon_running().await?;
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
    // Phase 8 — TUI is event-driven. The daemon's PlaybackChanged event
    // now carries the full Playback snapshot (Phase 3), and the daemon's
    // PlaybackClock extrapolates progress locally on each PlaybackGet, so
    // we no longer need a periodic poll to keep the UI fresh.
    //
    // We still need a safety net for a daemon that has died silently with
    // no events: a 30s heartbeat probe fires one PlaybackGet to catch the
    // edge case. The local 250ms progress tick is pure extrapolation, no
    // IPC, so it stays.
    let mut heartbeat = time::interval(Duration::from_secs(30));
    heartbeat.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    let mut progress = time::interval(Duration::from_millis(250));
    let mut sigint = std::pin::pin!(tokio::signal::ctrl_c());
    let (async_tx, mut async_rx) = mpsc::unbounded_channel();
    spawn_daemon_event_listener(async_tx.clone());
    let mut refresh_in_flight = false;
    // Escape hatch: if the daemon hasn't confirmed playback state
    // within 3s, fall back to the "Ready when you are" empty CTA so
    // the spinner can't lock the UI forever on a degraded daemon.
    let mut connecting_timeout = std::pin::pin!(tokio::time::sleep(Duration::from_secs(3)));
    let mut connecting_timeout_fired = false;

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
            _ = heartbeat.tick() => app.request_refresh(),
            _ = &mut connecting_timeout, if !connecting_timeout_fired => {
                connecting_timeout_fired = true;
                if !app.playback_known {
                    app.playback_known = true;
                }
            }
            result = async_rx.recv() => {
                if let Some(result) = result {
                    if matches!(result, AsyncResult::Refresh(_)) {
                        refresh_in_flight = false;
                    }
                    app.apply_async_result_with(result, &async_tx);
                }
            }
            event = events.next() => {
                let Some(event) = event else { break; };
                match event.context("failed to read terminal event")? {
                    Event::Key(key)
                        if key.kind == KeyEventKind::Press
                            && handle_key(app, key, &async_tx)? =>
                    {
                        break;
                    }
                    Event::Mouse(mouse)
                        if handle_mouse(app, terminal.size()?.into(), mouse, &async_tx)? =>
                    {
                        break;
                    }
                    Event::Key(_) | Event::Mouse(_) => {}
                    // Phase 17 — throttle the viz FFT broadcast rate down to
                    // 1 Hz when the terminal loses focus so background
                    // terminals don't burn CPU.
                    Event::FocusGained => {
                        spawn_viz_focus(true);
                    }
                    Event::FocusLost => {
                        spawn_viz_focus(false);
                    }
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

/// Phase 17 — fire-and-forget IPC call to update the daemon's viz focus
/// state. Failures are logged but never surfaced (focus throttling is a
/// best-effort optimization, not a correctness signal).
fn spawn_viz_focus(focused: bool) {
    tokio::spawn(async move {
        if let Ok(mut client) = IpcClient::connect().await {
            let _ = client
                .request(spotuify_protocol::Request::SetVizFocus { focused })
                .await;
        }
    });
}

fn spawn_daemon_event_listener(async_tx: mpsc::UnboundedSender<AsyncResult>) {
    tokio::spawn(async move {
        loop {
            let mut client = match IpcClient::connect().await {
                Ok(client) => client,
                Err(err) => {
                    tracing::debug!(error = %err, "daemon event stream connect failed");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };

            // Daemon (re)connected — push-state may have advanced
            // since our last subscription, so re-seed before letting
            // events drive the UI. Seed result lands as
            // AsyncResult::Seed and applies under the tie-breaker.
            spawn_seed(async_tx.clone());

            loop {
                match client.next_event().await {
                    Ok(event) => {
                        if async_tx.send(AsyncResult::DaemonEvent(event)).is_err() {
                            return;
                        }
                    }
                    Err(err) => {
                        tracing::debug!(error = %err, "daemon event stream stopped");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        break;
                    }
                }
            }
        }
    });
}

fn spawn_refresh(
    app: &mut App,
    async_tx: mpsc::UnboundedSender<AsyncResult>,
    refresh_in_flight: &mut bool,
) {
    *refresh_in_flight = true;
    app.is_syncing = true;
    // Pass the current track URI so the lyrics lane (the only piece
    // of refresh that still cares about playback identity) knows what
    // to fetch. `self.playback` is event-authoritative; reading it
    // here is safe.
    let track_uri = app.playback.item.as_ref().map(|item| item.uri.clone());
    let plan = refresh_plan(app);
    if plan.lyrics {
        app.lyrics_loading = true;
        app.lyrics_error = None;
    }
    tokio::spawn(async move {
        let snapshot = match time::timeout(
            TUI_REFRESH_TIMEOUT,
            fetch_refresh(
                track_uri.clone(),
                plan.library,
                plan.diagnostics,
                plan.lyrics,
            ),
        )
        .await
        {
            Ok(snapshot) => snapshot,
            Err(_) => RefreshSnapshot {
                playlists: None,
                library: None,
                recent: None,
                doctor: None,
                cache_status: None,
                logs: None,
                operations: None,
                lyrics: None,
                lyrics_error: plan.lyrics.then(|| {
                    format!(
                        "lyrics refresh timed out after {}s",
                        TUI_REFRESH_TIMEOUT.as_secs()
                    )
                }),
                lyrics_error_track_uri: track_uri,
                library_refresh_attempted: plan.library,
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

fn refresh_plan(app: &App) -> RefreshPlan {
    // Pre-fetch lyrics whenever the playing track has changed since
    // the last cached lyrics so opening the lyrics rail / tab is
    // instant + already synced to the active line. Subsequent
    // refreshes for the same track hit the daemon's lyrics cache.
    let playback_uri = app.playback.item.as_ref().map(|i| i.uri.as_str());
    let cached_uri = app.lyrics_track_uri.as_deref();
    let failed_uri = app.lyrics_failed_track_uri.as_deref();
    let lyrics_visible = app.screen == Screen::Lyrics || app.right_rail == RightRailMode::Lyrics;
    let need_lyrics_fetch = lyrics_visible
        && !app.lyrics_loading
        && playback_uri.is_some()
        && playback_uri != cached_uri
        && playback_uri != failed_uri;
    RefreshPlan {
        library: app
            .last_library_sync
            .is_none_or(|last_sync| last_sync.elapsed() >= TUI_LIBRARY_REFRESH_INTERVAL),
        diagnostics: app.screen == Screen::Diagnostics,
        lyrics: need_lyrics_fetch,
    }
}

async fn fetch_refresh(
    track_uri: Option<String>,
    refresh_library: bool,
    include_diagnostics: bool,
    include_lyrics: bool,
) -> RefreshSnapshot {
    let started = Instant::now();
    let errors: Vec<String> = Vec::new();

    if let Err(err) = spotuify_daemon::server::ensure_daemon_running().await {
        tracing::warn!(error = %err, "failed to ensure daemon before refresh");
    }

    let mut reads: Vec<RefreshRead> = Vec::new();
    if refresh_library {
        reads.extend([
            RefreshRead::Playlists,
            RefreshRead::Library,
            RefreshRead::Recent,
        ]);
    }
    if include_diagnostics {
        reads.extend([
            RefreshRead::Doctor,
            RefreshRead::CacheStatus,
            RefreshRead::Logs,
            RefreshRead::Operations,
        ]);
    }

    let results = futures::stream::iter(reads)
        .map(|read| async move {
            (
                read,
                request_data_without_daemon_start(read.request()).await,
            )
        })
        .buffer_unordered(TUI_REFRESH_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

    let mut playlists = None;
    let mut library = None;
    let mut recent = None;
    let mut doctor = None;
    let mut cache_status = None;
    let mut logs = None;
    let mut operations = None;

    for (read, result) in results {
        match read {
            RefreshRead::Playlists => match result {
                Ok(ResponseData::Playlists { playlists: value }) => playlists = Some(value),
                Ok(_) => tracing::warn!("unexpected playlists response"),
                Err(err) => tracing::warn!(error = %err, "failed to fetch playlists"),
            },
            RefreshRead::Library => match result {
                Ok(ResponseData::MediaItems { items }) => library = Some(items),
                Ok(_) => tracing::warn!("unexpected library response"),
                Err(err) => tracing::warn!(error = %err, "failed to fetch cached library"),
            },
            RefreshRead::Recent => match result {
                Ok(ResponseData::MediaItems { items }) => recent = Some(items),
                Ok(_) => tracing::warn!("unexpected recently played response"),
                Err(err) => tracing::warn!(error = %err, "failed to fetch recently played"),
            },
            RefreshRead::Doctor => match result {
                Ok(ResponseData::DoctorReport { report }) => doctor = Some(report),
                Ok(_) => tracing::warn!("unexpected doctor response"),
                Err(err) => tracing::warn!(error = %err, "failed to fetch doctor report"),
            },
            RefreshRead::CacheStatus => match result {
                Ok(ResponseData::CacheStatus { status }) => cache_status = Some(status),
                Ok(_) => tracing::warn!("unexpected cache status response"),
                Err(err) => tracing::warn!(error = %err, "failed to fetch cache status"),
            },
            RefreshRead::Logs => match result {
                Ok(ResponseData::Logs { lines }) => logs = Some(lines),
                Ok(_) => tracing::warn!("unexpected logs response"),
                Err(err) => tracing::warn!(error = %err, "failed to fetch logs"),
            },
            RefreshRead::Operations => match result {
                Ok(ResponseData::Operations { ops }) => operations = Some(ops),
                Ok(_) => tracing::warn!("unexpected ops_log response"),
                Err(err) => tracing::warn!(error = %err, "failed to fetch operations"),
            },
        }
    }

    let (lyrics, lyrics_error, lyrics_error_track_uri) =
        fetch_refresh_lyrics(include_lyrics, track_uri, false).await;

    tracing::debug!(
        elapsed_ms = started.elapsed().as_millis(),
        "Spotify refresh finished"
    );
    RefreshSnapshot {
        playlists,
        library,
        recent,
        doctor,
        cache_status,
        logs,
        operations,
        lyrics,
        lyrics_error,
        lyrics_error_track_uri,
        library_refresh_attempted: refresh_library,
        errors,
        elapsed_ms: started.elapsed().as_millis(),
    }
}

async fn fetch_refresh_lyrics(
    include_lyrics: bool,
    track_uri: Option<String>,
    force_refresh: bool,
) -> (Option<LyricsSnapshot>, Option<String>, Option<String>) {
    if !include_lyrics {
        return (None, None, None);
    }

    let Some(track_uri) = track_uri else {
        return (None, Some("No active track for lyrics".to_string()), None);
    };

    match request_data_without_daemon_start(Request::LyricsGet {
        track_uri: Some(track_uri.clone()),
        force_refresh,
    })
    .await
    {
        Ok(ResponseData::Lyrics { lyrics, offset_ms }) => (
            Some(LyricsSnapshot {
                track_uri,
                lyrics,
                offset_ms,
            }),
            None,
            None,
        ),
        Ok(_) => (
            None,
            Some("unexpected lyrics response".to_string()),
            Some(track_uri),
        ),
        Err(err) => (None, Some(short_error(err)), Some(track_uri)),
    }
}

fn request_force_lyrics(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    let track_uri = app.playback.item.as_ref().map(|item| item.uri.clone());
    if track_uri.is_none() {
        app.lyrics_error = Some("No active track for lyrics".to_string());
        return;
    }
    app.lyrics_loading = true;
    app.lyrics_error = None;
    let async_tx = async_tx.clone();
    tokio::spawn(async move {
        let started = Instant::now();
        if let Err(err) = spotuify_daemon::server::ensure_daemon_running().await {
            tracing::warn!(error = %err, "failed to ensure daemon before lyrics refresh");
        }
        let (lyrics, lyrics_error, lyrics_error_track_uri) =
            fetch_refresh_lyrics(true, track_uri, true).await;
        let _ = async_tx.send(AsyncResult::Refresh(Box::new(RefreshSnapshot {
            playlists: None,
            library: None,
            recent: None,
            doctor: None,
            cache_status: None,
            logs: None,
            operations: None,
            lyrics,
            lyrics_error,
            lyrics_error_track_uri,
            library_refresh_attempted: false,
            errors: Vec::new(),
            elapsed_ms: started.elapsed().as_millis(),
        })));
    });
}

/// Fire a single cover-art fetch for `url`, decode the response, and
/// post the result as `AsyncResult::CoverFetched`. The URL itself is
/// the version: when the result arrives, `apply_async_result_with`
/// accepts the image iff `app.current_art_url == Some(url)`. Stale
/// fetches (track advanced past the URL while we were in flight) drop
/// silently on arrival.
///
/// Failures are logged and swallowed — cover just stays cleared.
fn spawn_cover_fetch(url: String, async_tx: mpsc::UnboundedSender<AsyncResult>) {
    // Unit tests exercise the event-arm bookkeeping without a tokio
    // runtime; production always runs under tokio::main. Skip the
    // background fetch when there's no runtime to spawn into.
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    tokio::spawn(async move {
        match request_data_without_daemon_start(Request::CoverArt { url: url.clone() })
            .await
            .and_then(|data| match data {
                ResponseData::CoverArt { path, .. } => {
                    image::open(&path).with_context(|| format!("failed to decode cover art {path}"))
                }
                _ => anyhow::bail!("unexpected cover-art response"),
            }) {
            Ok(image) => {
                let _ = async_tx.send(AsyncResult::CoverFetched { url, image });
            }
            Err(err) => {
                tracing::warn!(error = %err, url = %url, "cover-art fetch failed");
            }
        }
    });
}

/// One-shot seed of push-driven state (playback / queue / devices).
/// Issued on TUI startup, after a daemon event-stream reconnect, and
/// when the broadcast subscription returns `RecvError::Lagged`.
///
/// Issues the three requests concurrently; partial failure is fine,
/// the missing fields stay `None` and existing TUI state is left
/// untouched by the tie-breaker in `apply_async_result_with`.
fn spawn_seed(async_tx: mpsc::UnboundedSender<AsyncResult>) {
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    tokio::spawn(async move {
        let fetched_at = Instant::now();
        let (playback_res, queue_res, devices_res, viz_res, recent_res) = tokio::join!(
            request_data_without_daemon_start(Request::PlaybackGet),
            request_data_without_daemon_start(Request::QueueGet),
            request_data_without_daemon_start(Request::DevicesList),
            request_data_without_daemon_start(Request::GetVizStatus),
            request_data_without_daemon_start(Request::RecentlyPlayed),
        );
        let seed_auth_kind = [
            &playback_res,
            &queue_res,
            &devices_res,
            &viz_res,
            &recent_res,
        ]
        .into_iter()
        .find_map(|result| match result {
            Err(err) => auth_error_kind_from_error(&err.to_string()),
            Ok(_) => None,
        });
        if let Some(kind) = seed_auth_kind {
            let _ = async_tx.send(AsyncResult::DaemonEvent(DaemonEvent::AuthError { kind }));
        }
        let playback = match playback_res {
            Ok(ResponseData::Playback { playback }) => Some(playback),
            Ok(other) => {
                tracing::debug!(?other, "seed: unexpected PlaybackGet response");
                None
            }
            Err(err) => {
                tracing::debug!(error = %err, "seed: PlaybackGet failed");
                None
            }
        };
        let queue = match queue_res {
            Ok(ResponseData::Queue { queue }) => Some(queue),
            Ok(other) => {
                tracing::debug!(?other, "seed: unexpected QueueGet response");
                None
            }
            Err(err) => {
                tracing::debug!(error = %err, "seed: QueueGet failed");
                None
            }
        };
        let devices = match devices_res {
            Ok(ResponseData::Devices { devices }) => Some(devices),
            Ok(other) => {
                tracing::debug!(?other, "seed: unexpected DevicesList response");
                None
            }
            Err(err) => {
                tracing::debug!(error = %err, "seed: DevicesList failed");
                None
            }
        };
        let viz = match viz_res {
            Ok(ResponseData::VizStatus { diagnostics }) => Some(diagnostics),
            Ok(other) => {
                tracing::debug!(?other, "seed: unexpected GetVizStatus response");
                None
            }
            Err(err) => {
                tracing::debug!(error = %err, "seed: GetVizStatus failed");
                None
            }
        };
        let recent = match recent_res {
            Ok(ResponseData::MediaItems { items }) => Some(items),
            Ok(other) => {
                tracing::debug!(?other, "seed: unexpected RecentlyPlayed response");
                None
            }
            Err(err) => {
                tracing::debug!(error = %err, "seed: RecentlyPlayed failed");
                None
            }
        };
        let _ = async_tx.send(AsyncResult::Seed {
            playback,
            queue,
            devices,
            viz,
            recent,
            fetched_at,
        });
    });
}

/// Run the interactive OAuth flow (browser handshake + localhost
/// callback + token persistence) in the background, then post the
/// outcome as `AsyncResult::LoginCompleted` so the modal can
/// transition.
///
/// Reuses `spotuify_spotify::auth::login` — the same code path the
/// `spotuify login` CLI subcommand uses. Errors are stringified at
/// the boundary so the TUI doesn't need to depend on SpotifyError.
fn spawn_login_flow(async_tx: mpsc::UnboundedSender<AsyncResult>) {
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    tokio::spawn(async move {
        let progress_tx = async_tx.clone();
        // OAuth status events flow through the same channel as
        // everything else and render INSIDE the LoginModal frame,
        // never touching stdout. This is what keeps the alt-screen
        // buffer clean while the OAuth flow runs.
        let progress = move |event: spotuify_spotify::auth::LoginProgress| {
            let _ = progress_tx.send(AsyncResult::LoginProgress(event));
        };
        let result = (async {
            let config = spotuify_spotify::config::Config::load()
                .context("failed to load Spotify config")?;
            spotuify_spotify::auth::login(&config, progress)
                .await
                .context("OAuth flow failed")?;
            Ok::<(), anyhow::Error>(())
        })
        .await;
        let _ = async_tx.send(AsyncResult::LoginCompleted {
            result: result.map_err(short_error),
        });
    });
}

/// Tell the daemon to drop its cached token + clear the auth-revoked
/// latch. Fire-and-forget — the next play / seed call will hit the
/// fresh credentials we just persisted.
fn spawn_reload_auth(async_tx: mpsc::UnboundedSender<AsyncResult>) {
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    tokio::spawn(async move {
        if let Err(err) = request_data_without_daemon_start(Request::ReloadAuth).await {
            tracing::warn!(error = %err, "ReloadAuth request failed");
        }
        // Silence unused — async_tx is here for symmetry with other
        // helpers that DO post back; reload-auth has no result event.
        let _ = async_tx;
    });
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

    if app.fullscreen_panel.is_some() && matches!(key.code, KeyCode::Esc) {
        app.fullscreen_panel = None;
        return Ok(false);
    }

    // Auth-revoked re-login modal sits right under the error modal in
    // routing precedence. It blocks all other input because the user
    // can't usefully do anything until they re-authenticate.
    if app.login_modal.is_some() {
        handle_login_modal_key(app, key, async_tx);
        return Ok(false);
    }

    // Phase 13 (P13-L) — confirmation modal: only y/n/Esc allowed.
    if app.confirm_modal.is_some() {
        match (key.code, key.modifiers) {
            (KeyCode::Char('y') | KeyCode::Char('Y'), _) => {
                if let Some(modal) = app.confirm_modal.take() {
                    return apply_tui_action(app, modal.on_confirm, async_tx);
                }
            }
            (KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc, _) => {
                app.confirm_modal = None;
            }
            _ => {}
        }
        return Ok(false);
    }

    if app.playlist_picker.is_some() {
        handle_playlist_picker_key(app, key, async_tx);
        return Ok(false);
    }

    if app.device_picker.is_some() {
        handle_device_picker_key(app, key, async_tx);
        return Ok(false);
    }

    if app.artist_view.is_some() {
        handle_artist_view_key(app, key, async_tx);
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MouseOutcome {
    Action(TuiAction),
    Seek(u64),
    Select(usize),
}

fn handle_mouse(
    app: &mut App,
    area: Rect,
    mouse: MouseEvent,
    async_tx: &mpsc::UnboundedSender<AsyncResult>,
) -> Result<bool> {
    let Some(outcome) = mouse_outcome(app, area, mouse) else {
        return Ok(false);
    };
    match outcome {
        MouseOutcome::Action(action) => apply_tui_action(app, action, async_tx),
        MouseOutcome::Seek(position_ms) => {
            command_then_refresh_transport(app, async_tx, CommandKind::Seek { position_ms });
            Ok(false)
        }
        MouseOutcome::Select(index) => {
            app.set_active_selection(index);
            Ok(false)
        }
    }
}

fn mouse_outcome(app: &App, area: Rect, mouse: MouseEvent) -> Option<MouseOutcome> {
    if let Some(outcome) = mouse_player_outcome(app, area, mouse) {
        return Some(outcome);
    }
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        return None;
    }
    if let Some(action) = mouse_tab_action(area, mouse.column, mouse.row) {
        return Some(MouseOutcome::Action(action));
    }
    let (main, rail) = body_content_areas(area, app.right_rail);
    if let Some(rail) = rail.filter(|rail| rect_contains(*rail, mouse.column, mouse.row)) {
        return mouse_rail_outcome(app.right_rail, rail, mouse.column, mouse.row);
    }
    mouse_row_selection(app, main, mouse.column, mouse.row).map(MouseOutcome::Select)
}

fn mouse_player_outcome(app: &App, area: Rect, mouse: MouseEvent) -> Option<MouseOutcome> {
    let player = bottom_player_area(area);
    if !rect_contains(player, mouse.column, mouse.row) {
        return None;
    }

    match mouse.kind {
        MouseEventKind::ScrollUp => return Some(MouseOutcome::Action(TuiAction::VolumeUp)),
        MouseEventKind::ScrollDown => return Some(MouseOutcome::Action(TuiAction::VolumeDown)),
        MouseEventKind::Down(MouseButton::Left) => {}
        _ => return None,
    }

    // Row-aware transport hit-testing: the right column of the player
    // hosts three rows of buttons (primary / toggles / volume). Without
    // this, every click in that column collapsed to PlayPause.
    if let Some(action) = mouse_transport_action(app, player, mouse.column, mouse.row) {
        return Some(MouseOutcome::Action(action));
    }
    mouse_seek_position(app, player, mouse.column, mouse.row).map(MouseOutcome::Seek)
}

fn mouse_transport_action(app: &App, player: Rect, column: u16, row: u16) -> Option<TuiAction> {
    let inner = rect_inner(
        player,
        Margin {
            horizontal: 1,
            vertical: 1,
        },
    );
    let transport_width = 40u16.min(inner.width);
    if transport_width == 0 {
        return None;
    }
    let transport = Rect::new(
        inner
            .x
            .saturating_add(inner.width.saturating_sub(transport_width)),
        inner.y,
        transport_width,
        inner.height,
    );
    if !rect_contains(transport, column, row) {
        return None;
    }
    // Strip transport's 1-cell horizontal margin so columns match the
    // layout in `render_transport`.
    let local_col = column.saturating_sub(transport.x.saturating_add(1));
    let usable_width = transport.width.saturating_sub(2);
    if local_col >= usable_width {
        return None;
    }
    let local_row = row.saturating_sub(transport.y);
    match local_row {
        // Primary buttons row — prev / play / next split into 3 zones.
        2 => match local_col.saturating_mul(3) / usable_width.max(1) {
            0 => Some(TuiAction::Previous),
            1 => Some(TuiAction::PlayPause),
            _ => Some(TuiAction::Next),
        },
        // Toggles row — shuffle, repeat, like. Column ranges are
        // computed against the same chip widths `render_transport` uses
        // so clicks land on the chip the user sees.
        4 => {
            // Order: " " " SHUFFLE "/" shuffle " "  " " REPEAT … "/" repeat " "  " " LIKED "/" like "
            let shuffle_start: u16 = 1;
            let shuffle_end = shuffle_start.saturating_add(9);
            let repeat_len: u16 = match app.playback.repeat.as_str() {
                "track" | "context" | "on" => 12,
                _ => 8,
            };
            let repeat_start = shuffle_end.saturating_add(2);
            let repeat_end = repeat_start.saturating_add(repeat_len);
            let like_start = repeat_end.saturating_add(2);
            let like_len: u16 = {
                let liked = app.playback.item.as_ref().is_some_and(|i| {
                    app.marked_uris.contains(&i.uri)
                        || app.library_items.iter().any(|saved| saved.uri == i.uri)
                });
                if liked {
                    7
                } else {
                    6
                }
            };
            let like_end = like_start.saturating_add(like_len);
            if (shuffle_start..shuffle_end).contains(&local_col) {
                Some(TuiAction::ToggleShuffle)
            } else if (repeat_start..repeat_end).contains(&local_col) {
                Some(TuiAction::CycleRepeat)
            } else if (like_start..like_end).contains(&local_col) {
                Some(TuiAction::LikeSelection)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn mouse_tab_action(area: Rect, column: u16, row: u16) -> Option<TuiAction> {
    let tabs = body_tabs_area(area);
    if !rect_contains(tabs, column, row) || tabs.width == 0 {
        return None;
    }
    let relative = column.saturating_sub(tabs.x) as usize;
    let index = (relative * Screen::ALL.len()) / tabs.width as usize;
    Screen::ALL.get(index).copied().map(screen_action)
}

fn mouse_rail_outcome(
    mode: RightRailMode,
    rail: Rect,
    column: u16,
    row: u16,
) -> Option<MouseOutcome> {
    if row != rail.y {
        return None;
    }
    let action = match mode {
        RightRailMode::Queue if column >= rail.x.saturating_add(rail.width.saturating_sub(10)) => {
            TuiAction::ToggleQueueRail
        }
        RightRailMode::Lyrics if column >= rail.x.saturating_add(rail.width.saturating_sub(10)) => {
            TuiAction::ToggleLyricsRail
        }
        RightRailMode::Hints => TuiAction::ToggleHintsRail,
        RightRailMode::Queue | RightRailMode::Lyrics => TuiAction::ToggleRailFullscreen,
        RightRailMode::Hidden => return None,
    };
    Some(MouseOutcome::Action(action))
}

fn mouse_row_selection(app: &App, area: Rect, column: u16, row: u16) -> Option<usize> {
    if !rect_contains(area, column, row) {
        return None;
    }
    match app.screen {
        Screen::Search => mouse_search_selection(app, area, column, row),
        Screen::Library => list_index_from_row(area, 3, row, app.visible_items().len(), 1),
        Screen::Queue => list_index_from_row(area, 6, row, app.visible_items().len(), 1),
        Screen::Playlists if app.selected_playlist_id.is_some() => {
            list_index_from_row(area, 3, row, app.visible_items().len(), 1)
        }
        Screen::Playlists => list_index_from_row(area, 3, row, app.filtered_playlists().len(), 2),
        Screen::Devices => list_index_from_row(area, 3, row, app.filtered_devices().len(), 1),
        Screen::Diagnostics => diagnostics_log_index(app, area, column, row),
        Screen::Player => {
            let list = player_queue_list_area(app, area);
            list_index_from_row(list, 0, row, app.visible_items().len(), 1)
        }
        Screen::Lyrics => None,
    }
}

fn player_queue_list_area(app: &App, area: Rect) -> Rect {
    if app.player_large && app.viz_enabled {
        Rect::new(
            area.x,
            area.y.saturating_add(8),
            area.width,
            area.height.saturating_sub(8),
        )
    } else {
        area
    }
}

fn mouse_search_selection(app: &App, area: Rect, column: u16, row: u16) -> Option<usize> {
    let list = content_list_area(area, 3);
    let items = app.visible_items();
    if items.is_empty() {
        return None;
    }
    let row_index = list_index_from_row(area, 3, row, usize::MAX, 1)?;
    let groups = search_groups(&items);
    if groups.is_empty() {
        return (row_index < items.len()).then_some(row_index);
    }
    let relative_column = column.saturating_sub(list.x) as usize;
    let group_index = ((relative_column * groups.len()) / list.width.max(1) as usize)
        .min(groups.len().saturating_sub(1));
    let (_, group_items) = groups.get(group_index)?;
    let item = group_items.get(row_index)?;
    items.iter().position(|candidate| candidate.uri == item.uri)
}

fn diagnostics_log_index(app: &App, area: Rect, column: u16, row: u16) -> Option<usize> {
    let columns = split_percent(area, 45);
    let right = columns.1;
    if !rect_contains(right, column, row) {
        return None;
    }
    let lines = app.filtered_diagnostics_logs();
    list_index_from_row(right, 8, row, lines.len(), 1)
}

fn list_index_from_row(
    area: Rect,
    top_offset: u16,
    row: u16,
    len: usize,
    row_height: u16,
) -> Option<usize> {
    let list = content_list_area(area, top_offset);
    if !rect_contains(list, list.x, row) || row <= list.y {
        return None;
    }
    let index = (row.saturating_sub(list.y + 1) / row_height.max(1)) as usize;
    (index < len).then_some(index)
}

fn mouse_seek_position(app: &App, player: Rect, column: u16, row: u16) -> Option<u64> {
    let item = app.playback.item.as_ref().or(app.last_played.as_ref())?;
    let progress = player_progress_area(player);
    if !rect_contains(progress, column, row) || progress.width == 0 {
        return None;
    }
    let relative = column.saturating_sub(progress.x).min(progress.width);
    let ratio = relative as f64 / progress.width as f64;
    Some((item.duration_ms as f64 * ratio).round() as u64)
}

fn bottom_player_area(area: Rect) -> Rect {
    let chrome_height = ui::PLAYER_HEIGHT.saturating_add(ui::STATUS_HEIGHT);
    let y = area
        .y
        .saturating_add(area.height.saturating_sub(chrome_height));
    Rect::new(area.x, y, area.width, ui::PLAYER_HEIGHT.min(area.height))
}

fn player_progress_area(player: Rect) -> Rect {
    let inner = rect_inner(
        player,
        Margin {
            horizontal: 1,
            vertical: 1,
        },
    );
    let transport_width = 40.min(inner.width);
    let cover_width = 24.min(inner.width.saturating_sub(transport_width));
    let progress_width = inner.width.saturating_sub(cover_width + transport_width);
    Rect::new(
        inner.x.saturating_add(cover_width),
        player.y.saturating_add(player.height.saturating_sub(2)),
        progress_width,
        1,
    )
}

fn body_tabs_area(area: Rect) -> Rect {
    let body_height = area
        .height
        .saturating_sub(ui::PLAYER_HEIGHT.saturating_add(ui::STATUS_HEIGHT));
    let inner = rect_inner(
        Rect::new(area.x, area.y, area.width, body_height),
        Margin {
            horizontal: 1,
            vertical: 0,
        },
    );
    Rect::new(inner.x, inner.y, inner.width, 3.min(inner.height))
}

fn body_content_areas(area: Rect, rail: RightRailMode) -> (Rect, Option<Rect>) {
    let tabs = body_tabs_area(area);
    let content = Rect::new(
        tabs.x,
        tabs.y.saturating_add(tabs.height),
        tabs.width,
        area.height
            .saturating_sub(ui::PLAYER_HEIGHT.saturating_add(ui::STATUS_HEIGHT))
            .saturating_sub(tabs.height),
    );
    if rail == RightRailMode::Hidden || content.width < 96 {
        return (content, None);
    }
    let rail_width = 38.min(content.width);
    let main = Rect::new(
        content.x,
        content.y,
        content.width.saturating_sub(rail_width),
        content.height,
    );
    let rail = Rect::new(
        content.x.saturating_add(main.width),
        content.y,
        rail_width,
        content.height,
    );
    (main, Some(rail))
}

fn content_list_area(area: Rect, top_offset: u16) -> Rect {
    Rect::new(
        area.x,
        area.y.saturating_add(top_offset),
        area.width,
        area.height.saturating_sub(top_offset),
    )
}

fn split_percent(area: Rect, left_percent: u16) -> (Rect, Rect) {
    let left_width = (area.width as u32 * left_percent as u32 / 100) as u16;
    (
        Rect::new(area.x, area.y, left_width, area.height),
        Rect::new(
            area.x.saturating_add(left_width),
            area.y,
            area.width.saturating_sub(left_width),
            area.height,
        ),
    )
}

fn search_groups(items: &[MediaItem]) -> Vec<(MediaKind, Vec<MediaItem>)> {
    [
        MediaKind::Track,
        MediaKind::Artist,
        MediaKind::Album,
        MediaKind::Playlist,
        MediaKind::Show,
        MediaKind::Episode,
    ]
    .into_iter()
    .map(|kind| {
        let group_items = items
            .iter()
            .filter(|item| item.kind == kind)
            .cloned()
            .collect::<Vec<_>>();
        (kind, group_items)
    })
    .filter(|(_, group_items)| !group_items.is_empty())
    .collect()
}

/// Order library items so cursor navigation (a single `app.selected`
/// index into the flat Vec) matches the side-by-side Music / Podcasts
/// panel layout in the renderer. All non-podcast kinds keep their
/// relative order and come first; Show / Episode keep their relative
/// order and come last. Stable partition — no other sorting happens
/// here, so SQL's `ORDER BY fetched_at_ms DESC, name ASC` still
/// determines order within each panel.
pub(crate) fn partition_library_for_navigation(items: Vec<MediaItem>) -> Vec<MediaItem> {
    let (music, podcasts): (Vec<_>, Vec<_>) = items
        .into_iter()
        .partition(|item| !matches!(item.kind, MediaKind::Show | MediaKind::Episode));
    music.into_iter().chain(podcasts).collect()
}

/// Pick the index in `items` that the cursor should snap to when a
/// search hasn't yet been steered by the user. Priority order matches
/// `search_groups` and is also what `g t/r/b/p/s/e` would jump to:
/// Tracks first, then Artists, Albums, Playlists, Shows, Episodes.
/// Returns `None` only when `items` is empty.
pub(crate) fn preferred_search_index(items: &[MediaItem]) -> Option<usize> {
    const PRIORITY: [MediaKind; 6] = [
        MediaKind::Track,
        MediaKind::Artist,
        MediaKind::Album,
        MediaKind::Playlist,
        MediaKind::Show,
        MediaKind::Episode,
    ];
    PRIORITY
        .iter()
        .find_map(|kind| items.iter().position(|item| item.kind == *kind))
}

fn rect_inner(area: Rect, margin: Margin) -> Rect {
    let horizontal = margin.horizontal.saturating_mul(2);
    let vertical = margin.vertical.saturating_mul(2);
    Rect::new(
        area.x.saturating_add(margin.horizontal),
        area.y.saturating_add(margin.vertical),
        area.width.saturating_sub(horizontal),
        area.height.saturating_sub(vertical),
    )
}

fn rect_contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x
        && column < area.x.saturating_add(area.width)
        && row >= area.y
        && row < area.y.saturating_add(area.height)
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

fn handle_artist_view_key(
    app: &mut App,
    key: KeyEvent,
    async_tx: &mpsc::UnboundedSender<AsyncResult>,
) {
    let Some(view) = app.artist_view.as_mut() else {
        return;
    };
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) | (KeyCode::Char('q'), _) | (KeyCode::Char('b'), KeyModifiers::NONE) => {
            app.artist_view = None;
        }
        (KeyCode::Tab, _) | (KeyCode::Char('l'), KeyModifiers::NONE) => {
            view.focus = match view.focus {
                ArtistViewSide::Albums => ArtistViewSide::Tracks,
                ArtistViewSide::Tracks => ArtistViewSide::Albums,
            };
        }
        (KeyCode::BackTab, _) | (KeyCode::Char('h'), KeyModifiers::NONE) => {
            view.focus = match view.focus {
                ArtistViewSide::Albums => ArtistViewSide::Tracks,
                ArtistViewSide::Tracks => ArtistViewSide::Albums,
            };
        }
        (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => match view.focus {
            ArtistViewSide::Albums => {
                if !view.albums.is_empty() {
                    let last = view.albums.len() - 1;
                    let next = view.album_selected.saturating_add(1).min(last);
                    if next != view.album_selected {
                        view.album_selected = next;
                        let album_uri = view.albums[next].uri.clone();
                        load_album_tracks(app, async_tx, album_uri);
                    }
                }
            }
            ArtistViewSide::Tracks => {
                if !view.album_tracks.is_empty() {
                    let last = view.album_tracks.len() - 1;
                    view.track_selected = view.track_selected.saturating_add(1).min(last);
                }
            }
        },
        (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => match view.focus {
            ArtistViewSide::Albums => {
                if view.album_selected > 0 {
                    view.album_selected -= 1;
                    let album_uri = view.albums[view.album_selected].uri.clone();
                    load_album_tracks(app, async_tx, album_uri);
                }
            }
            ArtistViewSide::Tracks => {
                view.track_selected = view.track_selected.saturating_sub(1);
            }
        },
        (KeyCode::Enter, _) => match view.focus {
            ArtistViewSide::Albums => {
                if let Some(album) = view.albums.get(view.album_selected).cloned() {
                    let name = album.name.clone();
                    command_then_refresh(app, async_tx, CommandKind::PlayItem { item: album });
                    app.toast = Some(format!("Playing album {name}"));
                    app.artist_view = None;
                }
            }
            ArtistViewSide::Tracks => {
                if let Some(track) = view.album_tracks.get(view.track_selected).cloned() {
                    let name = track.name.clone();
                    command_then_refresh(app, async_tx, CommandKind::PlayItem { item: track });
                    app.toast = Some(format!("Playing {name}"));
                    app.artist_view = None;
                }
            }
        },
        (KeyCode::Char('e'), KeyModifiers::NONE) if view.focus == ArtistViewSide::Tracks => {
            if let Some(track) = view.album_tracks.get(view.track_selected).cloned() {
                let name = track.name.clone();
                command_then_refresh(app, async_tx, CommandKind::QueueItem { item: track });
                app.toast = Some(format!("Queued {name}"));
            }
        }
        _ => {}
    }
}

fn handle_playlist_picker_key(
    app: &mut App,
    key: KeyEvent,
    async_tx: &mpsc::UnboundedSender<AsyncResult>,
) {
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) | (KeyCode::Char('q'), _) => {
            app.playlist_picker = None;
            app.toast = Some("Canceled playlist add".to_string());
        }
        (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
            move_playlist_picker(app, 1);
        }
        (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
            move_playlist_picker(app, -1);
        }
        (KeyCode::Char(' '), KeyModifiers::NONE) => toggle_playlist_picker_selection(app),
        (KeyCode::Enter, _) | (KeyCode::Char('a'), KeyModifiers::NONE) => {
            let requests = playlist_picker_requests(app);
            app.playlist_picker = None;
            let count = requests.len();
            if count == 0 {
                app.toast = Some("No playlist selected".to_string());
            } else {
                requests_then_refresh(
                    app,
                    async_tx,
                    requests,
                    format!("Added item(s) to {count} playlist(s)"),
                );
            }
        }
        _ => {}
    }
}

fn move_playlist_picker(app: &mut App, delta: isize) {
    let len = app.filtered_playlists().len();
    let Some(picker) = app.playlist_picker.as_mut() else {
        return;
    };
    if len == 0 {
        picker.selected = 0;
        return;
    }
    let selected = picker.selected.min(len - 1);
    picker.selected = if delta < 0 {
        selected.saturating_sub(delta.unsigned_abs())
    } else {
        (selected + delta as usize).min(len - 1)
    };
}

fn toggle_playlist_picker_selection(app: &mut App) {
    let playlists = app.filtered_playlists();
    let Some(picker) = app.playlist_picker.as_mut() else {
        return;
    };
    let Some(playlist) = playlists.get(picker.selected) else {
        return;
    };
    if !picker.selected_playlist_ids.insert(playlist.id.clone()) {
        picker.selected_playlist_ids.remove(&playlist.id);
    }
}

/// Key handling for the auth re-login modal. State machine:
/// - `AwaitingConfirm`: Enter → kick off OAuth + transition to `InProgress`. Esc → dismiss.
/// - `InProgress`: ignore Enter (browser already open); Esc → dismiss
///   and surface a toast that the user can dismiss too (the OAuth task
///   keeps running in the background but its result is delivered to a
///   toast instead of the modal).
/// - `Failed`: Enter → retry. Esc → dismiss.
fn handle_login_modal_key(
    app: &mut App,
    key: KeyEvent,
    async_tx: &mpsc::UnboundedSender<AsyncResult>,
) {
    let Some(modal) = app.login_modal.as_mut() else {
        return;
    };
    match (modal.phase.clone(), key.code) {
        (LoginPhase::AwaitingConfirm, KeyCode::Enter) => {
            modal.phase = LoginPhase::InProgress;
            spawn_login_flow(async_tx.clone());
        }
        (LoginPhase::AwaitingConfirm, KeyCode::Esc) => {
            app.login_modal = None;
            app.toast = Some("Re-authentication dismissed".to_string());
        }
        (LoginPhase::InProgress, KeyCode::Esc) => {
            app.login_modal = None;
            app.toast = Some(
                "Re-login dismissed; browser may still be open. \
                 Result will land as a toast."
                    .to_string(),
            );
        }
        (LoginPhase::Failed(_), KeyCode::Enter) => {
            modal.phase = LoginPhase::InProgress;
            spawn_login_flow(async_tx.clone());
        }
        (LoginPhase::Failed(_), KeyCode::Esc) => {
            app.login_modal = None;
        }
        _ => {}
    }
}

fn handle_device_picker_key(
    app: &mut App,
    key: KeyEvent,
    async_tx: &mpsc::UnboundedSender<AsyncResult>,
) {
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) | (KeyCode::Char('q'), _) => {
            app.device_picker = None;
            app.toast = Some("Canceled device pick".to_string());
        }
        (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
            move_device_picker(app, 1);
        }
        (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
            move_device_picker(app, -1);
        }
        (KeyCode::Enter, _) => {
            transfer_device_picker_selection(app, async_tx);
        }
        _ => {}
    }
}

fn move_device_picker(app: &mut App, delta: isize) {
    let len = app.filtered_devices().len();
    let Some(picker) = app.device_picker.as_mut() else {
        return;
    };
    if len == 0 {
        picker.selected = 0;
        return;
    }
    let selected = picker.selected.min(len - 1);
    picker.selected = if delta < 0 {
        selected.saturating_sub(delta.unsigned_abs())
    } else {
        (selected + delta as usize).min(len - 1)
    };
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
        // `g` + kind letter jumps the cursor to the first item of
        // that kind on the Search screen. The visual focus follows
        // the cursor (focused_card_block highlights the panel
        // containing the selected item), so one keystroke after
        // `g` lands the user on the panel they want.
        if app.screen == Screen::Search {
            let kind = match (key.code, key.modifiers) {
                (KeyCode::Char('t'), KeyModifiers::NONE) => Some(MediaKind::Track),
                (KeyCode::Char('r'), KeyModifiers::NONE) => Some(MediaKind::Artist),
                (KeyCode::Char('b'), KeyModifiers::NONE) => Some(MediaKind::Album),
                (KeyCode::Char('p'), KeyModifiers::NONE) => Some(MediaKind::Playlist),
                (KeyCode::Char('s'), KeyModifiers::NONE) => Some(MediaKind::Show),
                (KeyCode::Char('e'), KeyModifiers::NONE) => Some(MediaKind::Episode),
                _ => None,
            };
            if let Some(target_kind) = kind {
                if let Some(idx) = app
                    .search_results
                    .iter()
                    .position(|item| item.kind == target_kind)
                {
                    app.selected = idx;
                    // Explicit pane pick — same intent as moving the
                    // cursor manually. Stop auto-snapping back to the
                    // preferred kind on the next streamed page.
                    app.search_user_steered = true;
                    return None;
                }
                app.toast = Some(format!(
                    "No {} results",
                    target_kind.label().to_ascii_lowercase()
                ));
                return None;
            }
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
        (KeyCode::Char('D'), _) => Some(TuiAction::OpenDevicePicker),
        (KeyCode::Char('7'), KeyModifiers::NONE) => Some(TuiAction::OpenDiagnostics),
        (KeyCode::Char('8'), KeyModifiers::NONE) => Some(TuiAction::OpenLyrics),
        (KeyCode::Char('Q'), _) => Some(TuiAction::ToggleQueueRail),
        (KeyCode::Char('L'), _) => Some(TuiAction::ToggleLyricsRail),
        (KeyCode::Char('H'), _) => Some(TuiAction::ToggleHintsRail),
        (KeyCode::Char('F'), _) => Some(TuiAction::ToggleRailFullscreen),
        // On Search, Tab cycles between the 6 result panels in place
        // of switching the global tab — the user is mid-search and
        // doesn't want to jump out. BackTab cycles the other way.
        // On Library, Tab swaps focus between the Music and Podcasts
        // panes so the user doesn't have to scroll through every saved
        // track to reach a subscribed show.
        // Everywhere else Tab still rotates tabs.
        (KeyCode::Tab, _) if app.screen == Screen::Search => {
            cycle_search_panel(app, 1);
            None
        }
        (KeyCode::BackTab, _) if app.screen == Screen::Search => {
            cycle_search_panel(app, -1);
            None
        }
        (KeyCode::Tab, _) | (KeyCode::BackTab, _) if app.screen == Screen::Library => {
            cycle_library_pane(app);
            None
        }
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
        (KeyCode::Left, _) | (KeyCode::Char('<'), _) => Some(TuiAction::SeekBack),
        (KeyCode::Right, _) | (KeyCode::Char('>'), _) => Some(TuiAction::SeekForward),
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
        (KeyCode::Char('u'), KeyModifiers::NONE) => {
            // Contextual: on Diagnostics, `u` undoes the last reversible
            // op (the safety-net key); everywhere else it refreshes.
            if app.screen == Screen::Diagnostics {
                Some(TuiAction::UndoLastOperation)
            } else {
                Some(TuiAction::Refresh)
            }
        }
        (KeyCode::Char('b'), KeyModifiers::NONE) => Some(TuiAction::Back),
        (KeyCode::Char('m'), KeyModifiers::NONE) => Some(TuiAction::ToggleMark),
        (KeyCode::Char('M'), _) => Some(TuiAction::MarkRange),
        (KeyCode::Char('z'), KeyModifiers::NONE) => Some(TuiAction::TogglePlayerMode),
        // Phase 17 — visualizer keybindings. `v` toggles enable; `V` cycles source.
        (KeyCode::Char('v'), KeyModifiers::NONE) => Some(TuiAction::ToggleViz),
        (KeyCode::Char('V'), _) => Some(TuiAction::CycleVizSource),
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
        TuiAction::OpenDevicePicker => open_device_picker(app),
        TuiAction::OpenDiagnostics => {
            switch_screen(app, Screen::Diagnostics);
            app.request_refresh();
        }
        TuiAction::OpenLyrics => {
            switch_screen(app, Screen::Lyrics);
            app.request_lyrics_if_visible();
        }
        TuiAction::MoveDown => {
            app.move_down();
            maybe_trigger_search_page(app, async_tx);
        }
        TuiAction::MoveUp => app.move_up(),
        TuiAction::PageDown => {
            app.page_down();
            maybe_trigger_search_page(app, async_tx);
        }
        TuiAction::PageUp => app.page_up(),
        TuiAction::JumpTop => app.move_top(),
        TuiAction::JumpBottom => {
            app.move_bottom();
            maybe_trigger_search_page(app, async_tx);
        }
        TuiAction::Back => app.back(),
        TuiAction::Refresh => {
            if app.screen == Screen::Lyrics || app.right_rail == RightRailMode::Lyrics {
                app.lyrics_failed_track_uri = None;
                app.lyrics_error = None;
                app.lyrics_loading = false;
                request_force_lyrics(app, async_tx);
            }
            app.request_refresh();
        }
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
        TuiAction::PlayPause if player_space_should_play_selected(app) => {
            activate_selected(app, async_tx);
        }
        TuiAction::PlayPause => {
            command_then_refresh_transport(
                app,
                async_tx,
                // Let the daemon decide from its playback clock/local
                // transport state. The TUI view can be stale, and Space
                // should not force a Spotify GET before toggling.
                CommandKind::TogglePlayback,
            )
        }
        TuiAction::Next => command_then_refresh_transport(app, async_tx, CommandKind::Next),
        TuiAction::Previous => command_then_refresh_transport(app, async_tx, CommandKind::Previous),
        TuiAction::SeekBack => {
            let position = app.playback.progress_ms.saturating_sub(15_000);
            command_then_refresh_transport(
                app,
                async_tx,
                CommandKind::Seek {
                    position_ms: position,
                },
            );
        }
        TuiAction::SeekForward => {
            let position = app.playback.progress_ms.saturating_add(15_000);
            command_then_refresh_transport(
                app,
                async_tx,
                CommandKind::Seek {
                    position_ms: position,
                },
            );
        }
        TuiAction::VolumeUp => adjust_volume(app, async_tx, 5),
        TuiAction::VolumeDown => adjust_volume(app, async_tx, -5),
        TuiAction::ToggleShuffle => command_then_refresh_transport(
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
            command_then_refresh_transport(
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
        TuiAction::ToggleQueueRail => toggle_right_rail(app, RightRailMode::Queue),
        TuiAction::ToggleLyricsRail => toggle_right_rail(app, RightRailMode::Lyrics),
        TuiAction::ToggleHintsRail => toggle_right_rail(app, RightRailMode::Hints),
        TuiAction::ToggleRailFullscreen => toggle_rail_fullscreen(app),
        TuiAction::UndoLastOperation => undo_last_operation(app, async_tx),
        TuiAction::ToggleViz => toggle_viz(app),
        TuiAction::CycleVizSource => cycle_viz_source(app),
    }
    Ok(false)
}

fn toggle_right_rail(app: &mut App, mode: RightRailMode) {
    app.right_rail = if app.right_rail == mode {
        RightRailMode::Hidden
    } else {
        mode
    };

    match app.right_rail {
        RightRailMode::Queue => app.request_refresh(),
        RightRailMode::Lyrics => app.request_lyrics_if_visible(),
        RightRailMode::Hidden | RightRailMode::Hints => {}
    }
}

fn toggle_rail_fullscreen(app: &mut App) {
    if app.fullscreen_panel.take().is_some() {
        return;
    }
    app.fullscreen_panel = match app.right_rail {
        RightRailMode::Queue => Some(FullscreenPanel::Queue),
        RightRailMode::Lyrics => Some(FullscreenPanel::Lyrics),
        RightRailMode::Hidden | RightRailMode::Hints => match app.screen {
            Screen::Queue => Some(FullscreenPanel::Queue),
            Screen::Lyrics => Some(FullscreenPanel::Lyrics),
            _ => {
                app.toast = Some("Open the queue or lyrics rail before expanding".to_string());
                None
            }
        },
    };
}

/// Phase 17 — toggle visualizer enabled/disabled. Optimistic UI: flip
/// the local flag immediately so the layout updates this frame, then
/// fire-and-forget the IPC request.
fn toggle_viz(app: &mut App) {
    app.viz_enabled = !app.viz_enabled;
    let enabled = app.viz_enabled;
    if enabled {
        app.toast = Some("Visualizer enabled".to_string());
    } else {
        app.toast = Some("Visualizer disabled".to_string());
        // Clear the cached frame so the next render shows silence.
        app.spectrum_bands = [0.0; 12];
        app.spectrum_peak = 0.0;
    }
    tokio::spawn(async move {
        if let Ok(mut client) = IpcClient::connect().await {
            let _ = client
                .request(spotuify_protocol::Request::SetVizEnabled { enabled })
                .await;
        }
    });
}

/// Phase 17 — cycle through the configured source kinds.
fn cycle_viz_source(app: &mut App) {
    use spotuify_protocol::VizSourceKindData;
    let next = match app.viz_configured_source {
        VizSourceKindData::Auto => VizSourceKindData::Sink,
        VizSourceKindData::Sink => VizSourceKindData::Loopback,
        VizSourceKindData::Loopback => VizSourceKindData::None,
        VizSourceKindData::None => VizSourceKindData::Auto,
    };
    app.viz_configured_source = next;
    app.toast = Some(format!("Viz source: {}", next.as_str()));
    let kind = next;
    tokio::spawn(async move {
        if let Ok(mut client) = IpcClient::connect().await {
            let _ = client
                .request(spotuify_protocol::Request::SetVizSource { kind })
                .await;
        }
    });
}

fn undo_last_operation(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    // The simplest "safety net" UX: kick off Request::OpsUndo with no
    // operation_id, which the daemon resolves to "last reversible op".
    // Refresh on completion so the diagnostics panel reflects the new
    // undo row. We do not gate behind a confirmation modal here — P13-L
    // adds destructive-action modals across the TUI; ops undo opts into
    // that flow once it lands.
    let async_tx_inner = async_tx.clone();
    app.toast = Some("Undoing last operation…".to_string());
    tokio::spawn(async move {
        let result = request_data(Request::OpsUndo {
            operation_id: None,
            dry_run: false,
            force: false,
            bulk_since_ms: None,
        })
        .await;
        let outcome = match result {
            Ok(ResponseData::OperationUndoResult {
                succeeded, errors, ..
            }) => {
                if errors.is_empty() {
                    Ok(CommandResult {
                        message: Some(format!("Undid {succeeded} op(s)")),
                        request_refresh: true,
                        ..Default::default()
                    })
                } else {
                    Err(errors.join("; "))
                }
            }
            Ok(_) => Err("unexpected undo response".to_string()),
            Err(err) => Err(short_error(err)),
        };
        let _ = async_tx_inner.send(AsyncResult::Command(Box::new(outcome)));
    });
}

fn start_search(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    let query = app.search_query.clone();
    if query.trim().is_empty() {
        app.search_results.clear();
        app.search_panes.clear();
        app.is_searching = false;
        app.screen = Screen::Search;
        app.selected = 0;
        app.toast = Some("Type a search query".to_string());
        app.error = None;
        return;
    }

    app.search_version = app.search_version.wrapping_add(1);
    let version = app.search_version;
    app.search_results.clear();
    app.search_panes.clear();
    // Fresh search: auto-snap selection back on, so the first batch of
    // results lands on the preferred kind (Tracks). The user gets a new
    // chance to steer the cursor on this query.
    app.search_user_steered = false;
    // Seed all six panes as loading; daemon will clear them as pages
    // resolve. This drives the per-pane "↓ loading more…" indicator
    // for the initial 18-request fanout.
    for kind in [
        MediaKind::Track,
        MediaKind::Artist,
        MediaKind::Album,
        MediaKind::Playlist,
        MediaKind::Show,
        MediaKind::Episode,
    ] {
        app.search_panes.insert(
            kind,
            SearchPaneState {
                loading: true,
                exhausted: false,
                error: None,
                next_offset: 0,
                pages: std::collections::BTreeMap::new(),
            },
        );
    }
    app.is_searching = true;
    app.screen = Screen::Search;
    app.selected = 0;
    app.toast = Some("Searching Spotify...".to_string());
    app.error = None;

    let async_tx = async_tx.clone();
    tokio::spawn(async move {
        // Fire-and-mostly-forget. The streaming response arrives as
        // DaemonEvent::SearchPage events; we just need the ack here to
        // surface daemon-connection failures. Short timeout because the
        // ack returns before any Spotify work is done.
        let result: Result<(), String> = match time::timeout(
            Duration::from_secs(5),
            request_data(Request::SearchStream {
                query: query.clone(),
                scope: SearchScopeData::All,
                source: spotuify_protocol::SearchSourceData::Spotify,
                version,
            }),
        )
        .await
        {
            Ok(Ok(ResponseData::SearchStarted { .. })) => Ok(()),
            Ok(Ok(_)) => Err("unexpected search response".to_string()),
            Ok(Err(err)) => Err(short_error(err)),
            Err(_) => Err("search request timed out".to_string()),
        };
        // We still reuse AsyncResult::Search to bubble up daemon-side
        // errors; the streaming events do the real work.
        let _ = async_tx.send(AsyncResult::Search {
            query,
            result: result.map(|_| Vec::new()),
        });
    });
}

/// Distance from the end of a pane at which scrolling triggers the
/// next page fetch. Three is enough headroom for the new page (10
/// items) to arrive before the user actually reaches the end at
/// normal `j` cadence.
const SEARCH_LOAD_MORE_THRESHOLD: usize = 3;

/// Called after each downward selection mutation in the search screen.
/// If the active pane's selection is within `SEARCH_LOAD_MORE_THRESHOLD`
/// of the pane's end AND the pane is neither loading nor exhausted,
/// fires the next `Request::SearchPage` for that pane.
fn maybe_trigger_search_page(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    if app.screen != Screen::Search {
        return;
    }
    let Some(selected_item) = app.search_results.get(app.selected) else {
        return;
    };
    let kind = selected_item.kind.clone();
    let (idx_within_pane, pane_count) = {
        let mut idx = 0usize;
        let mut count = 0usize;
        for (i, item) in app.search_results.iter().enumerate() {
            if item.kind == kind {
                if i == app.selected {
                    idx = count;
                }
                count += 1;
            }
        }
        (idx, count)
    };
    let near_end = idx_within_pane + SEARCH_LOAD_MORE_THRESHOLD >= pane_count;
    if !near_end {
        return;
    }
    let should_fire = app
        .search_panes
        .get(&kind)
        .map(|p| !p.loading && !p.exhausted)
        .unwrap_or(false);
    if should_fire {
        fetch_search_page(app, kind, async_tx);
    }
}

/// Issue a single-page fetch for `kind` at the pane's current
/// `next_offset`. Called by the scroll-trigger code when the user
/// approaches the end of a pane's loaded items.
fn fetch_search_page(
    app: &mut App,
    kind: MediaKind,
    async_tx: &mpsc::UnboundedSender<AsyncResult>,
) {
    let Some(pane) = app.search_panes.get_mut(&kind) else {
        return;
    };
    if pane.loading || pane.exhausted {
        return;
    }
    pane.loading = true;
    let query = app.search_query.clone();
    let version = app.search_version;
    let offset = pane.next_offset;
    let async_tx = async_tx.clone();
    tokio::spawn(async move {
        // Fire-and-forget on success; the daemon publishes the
        // SearchPage/SearchFailed event through the subscription. If the
        // IPC request itself times out, synthesize the same terminal event
        // locally so the pane never spins forever.
        let result = time::timeout(
            Duration::from_secs(5),
            request_data(Request::SearchPage {
                query: query.clone(),
                kind: kind.clone(),
                offset,
                version,
            }),
        )
        .await;
        match result {
            Ok(Ok(_)) => {}
            Ok(Err(err)) => {
                let _ = async_tx.send(AsyncResult::DaemonEvent(DaemonEvent::SearchFailed {
                    query,
                    version,
                    kind: Some(kind),
                    offset: Some(offset),
                    message: short_error(err),
                }));
            }
            Err(_) => {
                let _ = async_tx.send(AsyncResult::DaemonEvent(DaemonEvent::SearchFailed {
                    query,
                    version,
                    kind: Some(kind),
                    offset: Some(offset),
                    message: "search-page IPC request timed out after 5s".to_string(),
                }));
            }
        }
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
                // Enter on an Artist row opens the artist view:
                // their albums on the left, tracks of the selected
                // album on the right. Playback only fires when the
                // user picks a specific album or track from inside
                // the view.
                if matches!(item.kind, MediaKind::Artist) {
                    open_artist_view(app, async_tx, item);
                    return;
                }
                let item_name = item.name.clone();
                let toast = if app.screen == Screen::Player {
                    format!("Starting {item_name}")
                } else {
                    format!("Playing {item_name} (queue replaced · e to enqueue next time)")
                };
                command_then_refresh(app, async_tx, CommandKind::PlayItem { item });
                app.toast = Some(toast);
            }
        }
    }
}

fn open_artist_view(
    app: &mut App,
    async_tx: &mpsc::UnboundedSender<AsyncResult>,
    artist: MediaItem,
) {
    app.artist_view = Some(ArtistViewState {
        artist_uri: artist.uri.clone(),
        artist_name: artist.name.clone(),
        albums: Vec::new(),
        album_selected: 0,
        album_tracks: Vec::new(),
        track_selected: 0,
        focus: ArtistViewSide::Albums,
        loading_albums: true,
        loading_tracks: false,
        error: None,
    });
    let async_tx = async_tx.clone();
    let artist_uri = artist.uri.clone();
    tokio::spawn(async move {
        let result = request_data(Request::ArtistAlbums {
            artist: artist_uri.clone(),
        })
        .await;
        let _ = async_tx.send(AsyncResult::ArtistAlbums {
            artist_uri,
            result: match result {
                Ok(ResponseData::MediaItems { items }) => Ok(items),
                Ok(_) => Err("unexpected artist albums response".to_string()),
                Err(err) => Err(short_error(err)),
            },
        });
    });
}

fn load_album_tracks(
    app: &mut App,
    async_tx: &mpsc::UnboundedSender<AsyncResult>,
    album_uri: String,
) {
    if let Some(view) = app.artist_view.as_mut() {
        view.loading_tracks = true;
        view.album_tracks.clear();
        view.track_selected = 0;
    }
    let async_tx = async_tx.clone();
    let lookup_uri = album_uri.clone();
    tokio::spawn(async move {
        let result = request_data(Request::AlbumTracks {
            album: album_uri.clone(),
        })
        .await;
        let _ = async_tx.send(AsyncResult::AlbumTracks {
            album_uri: lookup_uri,
            result: match result {
                Ok(ResponseData::MediaItems { items }) => Ok(items),
                Ok(_) => Err("unexpected album tracks response".to_string()),
                Err(err) => Err(short_error(err)),
            },
        });
    });
}

fn open_playlist(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    let Some(playlist) = app.selected_playlist() else {
        return;
    };
    if !begin_action(app) {
        return;
    }

    spawn_playlist_tracks_request(
        async_tx.clone(),
        playlist.id,
        playlist.name,
        playlist.tracks_total,
    );
}

fn spawn_playlist_tracks_request(
    async_tx: mpsc::UnboundedSender<AsyncResult>,
    playlist_id: String,
    playlist_name: String,
    expected_total: u64,
) {
    let async_tx = async_tx.clone();
    tokio::spawn(async move {
        let result = match time::timeout(
            TUI_PLAYLIST_TIMEOUT,
            request_data(Request::PlaylistTracks {
                playlist: playlist_id.clone(),
                wait: false,
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
            playlist_id,
            playlist_name,
            expected_total,
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

fn open_device_picker(app: &mut App) {
    if app.devices.is_empty() {
        app.request_refresh();
        app.toast = Some("Loading devices…".to_string());
    }
    let devices = app.filtered_devices();
    let active_idx = devices.iter().position(|d| d.is_active);
    let playback_idx = app.playback.device.as_ref().and_then(|current| {
        devices
            .iter()
            .position(|d| d.id.is_some() && d.id == current.id)
    });
    let selected = active_idx.or(playback_idx).unwrap_or(0);
    app.device_picker = Some(DevicePickerModal { selected });
}

fn transfer_device_picker_selection(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    let Some(picker) = app.device_picker.as_ref() else {
        return;
    };
    let Some(device) = app.filtered_devices().get(picker.selected).cloned() else {
        return;
    };
    app.device_picker = None;
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
    let mut uris = app.selected_target_uris();
    if uris.is_empty() {
        let Some(item) = app.playback.item.as_ref() else {
            app.error =
                Some("Select an item or start playback before adding to a playlist".to_string());
            return;
        };
        uris.push(item.uri.clone());
    }

    open_playlist_picker(app, uris);
    let _ = async_tx;
}

fn open_playlist_picker(app: &mut App, uris: Vec<String>) {
    if app.playlists.is_empty() {
        app.request_refresh();
        app.toast = Some("Loading playlists...".to_string());
    } else {
        app.toast = Some(format!("Select playlist(s) for {} item(s)", uris.len()));
    }
    app.playlist_picker = Some(PlaylistPickerModal {
        uris,
        selected: app.playlist_selected,
        selected_playlist_ids: HashSet::new(),
    });
}

fn playlist_picker_requests(app: &App) -> Vec<Request> {
    let Some(picker) = &app.playlist_picker else {
        return Vec::new();
    };
    let playlists = app.filtered_playlists();
    let selected_ids = if picker.selected_playlist_ids.is_empty() {
        playlists
            .get(picker.selected)
            .map(|playlist| HashSet::from([playlist.id.clone()]))
            .unwrap_or_default()
    } else {
        picker.selected_playlist_ids.clone()
    };
    playlists
        .into_iter()
        .filter(|playlist| selected_ids.contains(&playlist.id))
        .map(|playlist| Request::PlaylistAddItems {
            playlist: playlist.id,
            uris: picker.uris.clone(),
        })
        .collect()
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

fn player_space_should_play_selected(app: &App) -> bool {
    app.screen == Screen::Player
        && app.queue.session_active
        && app.playback.item.is_none()
        && app.selected_item().is_some()
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

/// Transport-fast lane: bypass the `action_in_flight` gate. Pressing
/// Space (Toggle), Next, Previous, Seek, Volume, Shuffle, Repeat
/// should NEVER drop a keypress with a "Still working..." toast — the
/// daemon serialises transport via its `transport_mutation_lock` and
/// emits an optimistic `PlaybackChanged` before the Spotify call
/// returns, so the user always sees their keypress reflected at the
/// next render tick. Heavy mutations (PlayItem/QueueAdd/playlist
/// edits/library saves) keep the gated `command_then_refresh` path
/// where the toast genuinely helps signal in-flight work.
fn command_then_refresh_transport(
    _app: &mut App,
    async_tx: &mpsc::UnboundedSender<AsyncResult>,
    command: CommandKind,
) {
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
    spotuify_daemon::server::ensure_daemon_running().await?;
    request_data_without_daemon_start(request).await
}

async fn request_data_without_daemon_start(request: Request) -> Result<ResponseData> {
    let mut client =
        IpcClient::connect_with_source(spotuify_protocol::OperationSource::Tui).await?;
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
    command_then_refresh_transport(
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

/// Rotate the Search cursor to the first item of the next/previous
/// visible kind group. `delta = +1` moves forward, `-1` backward.
/// Library renders Music (Track/Album/Artist) on the left and Podcasts
/// (Show/Episode) on the right. The flat `app.library_items` vector
/// keeps all music first, then all podcasts (see
/// `partition_library_for_navigation`), so Tab is just a jump to the
/// first item in the other partition.
fn cycle_library_pane(app: &mut App) {
    let items = app.visible_items();
    if items.is_empty() {
        return;
    }
    let first_podcast = items
        .iter()
        .position(|i| matches!(i.kind, MediaKind::Show | MediaKind::Episode));
    let first_music = items
        .iter()
        .position(|i| !matches!(i.kind, MediaKind::Show | MediaKind::Episode));
    let selected_is_podcast = items
        .get(app.selected)
        .is_some_and(|i| matches!(i.kind, MediaKind::Show | MediaKind::Episode));
    let target = if selected_is_podcast {
        first_music
    } else {
        first_podcast
    };
    if let Some(idx) = target {
        app.set_active_selection(idx);
    }
}

fn cycle_search_panel(app: &mut App, delta: isize) {
    // Order matches `render_search_groups`.
    const ORDER: [MediaKind; 6] = [
        MediaKind::Track,
        MediaKind::Artist,
        MediaKind::Album,
        MediaKind::Playlist,
        MediaKind::Show,
        MediaKind::Episode,
    ];
    let visible: Vec<MediaKind> = ORDER
        .iter()
        .filter(|k| app.search_results.iter().any(|i| &i.kind == *k))
        .cloned()
        .collect();
    if visible.is_empty() {
        return;
    }
    let current_kind = app.search_results.get(app.selected).map(|i| i.kind.clone());
    let current_idx = current_kind
        .as_ref()
        .and_then(|kind| visible.iter().position(|k| k == kind))
        .unwrap_or(0);
    let next_idx = ((current_idx as isize + delta).rem_euclid(visible.len() as isize)) as usize;
    let target = visible[next_idx].clone();
    if let Some(idx) = app.search_results.iter().position(|i| i.kind == target) {
        app.selected = idx;
        // Cycling panels is an explicit user navigation — same as
        // arrow keys / `g <letter>`. Stop auto-snapping to the
        // preferred kind on the next streamed result page.
        app.search_user_steered = true;
    }
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
        Screen::Lyrics => TuiAction::OpenLyrics,
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
        TuiAction::OpenLyrics => switch_screen(app, Screen::Lyrics),
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
    execute!(
        stdout,
        EnterAlternateScreen,
        // Phase 17 — enable focus reporting so the TUI can throttle the
        // viz FFT broadcast rate when the terminal loses focus. Best-
        // effort: terminals that don't support `EnableFocusChange`
        // (older Windows console, some emulators) just won't fire the
        // events, and viz will keep running at 30 Hz.
        crossterm::event::EnableFocusChange,
        EnableMouseCapture,
    )?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).context("failed to create terminal")
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        crossterm::event::DisableFocusChange,
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn short_error(err: anyhow::Error) -> String {
    err.to_string()
}

fn playlist_tracks_forbidden(error: &str) -> bool {
    error.contains("Spotify API 403")
        && error.contains("GET /playlists/")
        && (error.contains("/items") || error.contains("/tracks"))
}

/// Returns `true` for daemon-emitted action strings that describe
/// background poll / cache-warm / optimistic-prediction flows rather
/// than a user-initiated mutation. The TUI uses this to skip the
/// toast notification — the user doesn't need a flash of "Playback
/// updated: synced" every 3 seconds while music is playing
/// normally.
/// Build the bottom-status toast for a `MutationFinalized` event.
/// Strips the `SpotifyError::Client` Display prefix (`"Spotify client
/// error: "`) — it's a logging convention, not something the user
/// needs in a one-line toast — and uses a verb that matches the
/// outcome ("Confirmed", "Failed") rather than the protocol enum's
/// Debug shape.
fn format_mutation_toast(status: spotuify_protocol::ReceiptStatus, message: &str) -> String {
    let trimmed = message
        .strip_prefix("Spotify client error: ")
        .unwrap_or(message);
    let label = match status {
        spotuify_protocol::ReceiptStatus::Confirmed => "Confirmed",
        spotuify_protocol::ReceiptStatus::Failed => "Failed",
        spotuify_protocol::ReceiptStatus::Pending => "Pending",
    };
    format!("{label}: {trimmed}")
}

fn is_background_event_action(action: &str) -> bool {
    action == "synced"
        || action == "snapshot"
        || action == "warmed"
        || action == "refreshed"
        || action.starts_with("optimistic-")
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
    use crossterm::event::{KeyCode, KeyModifiers};
    use spotuify_spotify::client::MediaKind;

    /// Build an empty `RefreshSnapshot` for tests that only care about
    /// a single field. Keeps tests insulated from `RefreshSnapshot`
    /// shape churn — when we add or drop a poll-only field, only this
    /// helper needs updating.
    fn empty_refresh_snapshot() -> RefreshSnapshot {
        RefreshSnapshot {
            playlists: None,
            library: None,
            recent: None,
            doctor: None,
            cache_status: None,
            logs: None,
            operations: None,
            lyrics: None,
            lyrics_error: None,
            lyrics_error_track_uri: None,
            library_refresh_attempted: false,
            errors: Vec::new(),
            elapsed_ms: 0,
        }
    }

    fn test_app() -> App {
        App {
            playback: Playback::default(),
            queue: Queue::default(),
            devices: Vec::new(),
            playlists: Vec::new(),
            inaccessible_playlist_ids: HashSet::new(),
            last_played: None,
            library_items: Vec::new(),
            playlist_tracks: Vec::new(),
            search_results: Vec::new(),
            search_version: 0,
            search_panes: std::collections::HashMap::new(),
            search_user_steered: false,
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
            awaiting_track_change_until: None,
            current_art_url: None,
            cover: None,
            playback_updated_at: None,
            queue_updated_at: None,
            devices_updated_at: None,
            playback_known: false,
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
            login_modal: None,
            operations: Vec::new(),
            operations_cursor: 0,
            pending_receipts: Vec::new(),
            banner: None,
            artist_view: None,
            refresh_requested: false,
            pending_g: false,
        }
    }

    #[test]
    fn refresh_plan_fetches_diagnostics_only_on_diagnostics_screen() {
        let mut app = test_app();
        app.last_library_sync = Some(Instant::now());

        let plan = refresh_plan(&app);
        assert!(!plan.diagnostics);

        app.screen = Screen::Diagnostics;
        let plan = refresh_plan(&app);
        assert!(plan.diagnostics);
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
            explicit: None,
            is_playable: None,
        }
    }

    fn device(id: &str, name: &str, active: bool, restricted: bool) -> Device {
        Device {
            id: Some(id.to_string()),
            name: name.to_string(),
            kind: "computer".to_string(),
            is_active: active,
            is_restricted: restricted,
            volume_percent: Some(50),
            supports_volume: true,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn mouse_left_click_on_play_chip_maps_to_play_pause() {
        let app = test_app();
        let area = Rect::new(0, 0, 120, 32);
        // Player at y=20..29; transport inner starts at x=80.
        // Primary row is y=22. Play chip is the middle third of the
        // transport's 38-cell usable width, so local_col ~20 → global 100.
        let event = mouse(MouseEventKind::Down(MouseButton::Left), 100, 22);

        assert_eq!(
            mouse_outcome(&app, area, event),
            Some(MouseOutcome::Action(TuiAction::PlayPause))
        );
    }

    #[test]
    fn mouse_left_click_on_prev_chip_maps_to_previous() {
        let app = test_app();
        let area = Rect::new(0, 0, 120, 32);
        // Local col 4 in primary row → Previous (left third of usable width).
        let event = mouse(MouseEventKind::Down(MouseButton::Left), 86, 22);

        assert_eq!(
            mouse_outcome(&app, area, event),
            Some(MouseOutcome::Action(TuiAction::Previous))
        );
    }

    #[test]
    fn mouse_left_click_on_next_chip_maps_to_next() {
        let app = test_app();
        let area = Rect::new(0, 0, 120, 32);
        // Local col 24 in primary row → Next (right third of usable width).
        let event = mouse(MouseEventKind::Down(MouseButton::Left), 106, 22);

        assert_eq!(
            mouse_outcome(&app, area, event),
            Some(MouseOutcome::Action(TuiAction::Next))
        );
    }

    #[test]
    fn mouse_left_click_on_shuffle_chip_toggles_shuffle() {
        let app = test_app();
        let area = Rect::new(0, 0, 120, 32);
        // Toggle row is y=24 (transport.y + 4). Shuffle chip occupies
        // local cols 1..10 → global 81..90.
        let event = mouse(MouseEventKind::Down(MouseButton::Left), 85, 24);

        assert_eq!(
            mouse_outcome(&app, area, event),
            Some(MouseOutcome::Action(TuiAction::ToggleShuffle))
        );
    }

    #[test]
    fn mouse_left_click_on_repeat_chip_cycles_repeat() {
        let app = test_app();
        let area = Rect::new(0, 0, 120, 32);
        // Default `repeat=""` → chip is " repeat " (8 wide) at local
        // cols 12..20 → global 92..100.
        let event = mouse(MouseEventKind::Down(MouseButton::Left), 96, 24);

        assert_eq!(
            mouse_outcome(&app, area, event),
            Some(MouseOutcome::Action(TuiAction::CycleRepeat))
        );
    }

    #[test]
    fn mouse_left_click_on_like_chip_likes_current_track() {
        let app = test_app();
        let area = Rect::new(0, 0, 120, 32);
        // With default repeat width, like chip is " like " at local
        // cols 22..28 → global 102..108.
        let event = mouse(MouseEventKind::Down(MouseButton::Left), 105, 24);

        assert_eq!(
            mouse_outcome(&app, area, event),
            Some(MouseOutcome::Action(TuiAction::LikeSelection))
        );
    }

    #[test]
    fn mouse_scroll_on_bottom_player_maps_to_volume() {
        let app = test_app();
        let area = Rect::new(0, 0, 120, 32);

        assert_eq!(
            mouse_outcome(&app, area, mouse(MouseEventKind::ScrollUp, 20, 23)),
            Some(MouseOutcome::Action(TuiAction::VolumeUp))
        );
        assert_eq!(
            mouse_outcome(&app, area, mouse(MouseEventKind::ScrollDown, 20, 23)),
            Some(MouseOutcome::Action(TuiAction::VolumeDown))
        );
    }

    #[test]
    fn mouse_click_outside_bottom_transport_is_ignored() {
        let app = test_app();
        let area = Rect::new(0, 0, 120, 32);

        assert_eq!(
            mouse_outcome(
                &app,
                area,
                mouse(MouseEventKind::Down(MouseButton::Left), 20, 12)
            ),
            None
        );
        assert_eq!(
            mouse_outcome(
                &app,
                area,
                mouse(MouseEventKind::Down(MouseButton::Left), 20, 23)
            ),
            None
        );
    }

    #[test]
    fn mouse_click_on_tab_switches_screen() {
        let app = test_app();
        let area = Rect::new(0, 0, 120, 32);

        assert_eq!(
            mouse_outcome(
                &app,
                area,
                mouse(MouseEventKind::Down(MouseButton::Left), 35, 1)
            ),
            Some(MouseOutcome::Action(TuiAction::OpenLibrary))
        );
    }

    #[test]
    fn mouse_click_on_search_group_row_selects_that_item() {
        let mut app = test_app();
        app.screen = Screen::Search;
        app.search_results = vec![
            item("spotify:track:first", "First"),
            item("spotify:artist:artist-one", "Artist One"),
        ];
        app.search_results[1].kind = MediaKind::Artist;
        let area = Rect::new(0, 0, 140, 32);

        assert_eq!(
            mouse_outcome(
                &app,
                area,
                mouse(MouseEventKind::Down(MouseButton::Left), 80, 7)
            ),
            Some(MouseOutcome::Select(1))
        );
    }

    #[test]
    fn mouse_click_on_progress_maps_to_seek_position() {
        let mut app = test_app();
        app.playback.item = Some(item("spotify:track:first", "First"));
        let area = Rect::new(0, 0, 120, 32);

        assert_eq!(
            mouse_outcome(
                &app,
                area,
                mouse(MouseEventKind::Down(MouseButton::Left), 60, 27)
            ),
            Some(MouseOutcome::Seek(116_667))
        );
    }

    #[test]
    fn mouse_click_on_rail_header_expands_or_hides_rail() {
        let mut app = test_app();
        app.right_rail = RightRailMode::Queue;
        let area = Rect::new(0, 0, 140, 32);

        assert_eq!(
            mouse_outcome(
                &app,
                area,
                mouse(MouseEventKind::Down(MouseButton::Left), 104, 3)
            ),
            Some(MouseOutcome::Action(TuiAction::ToggleRailFullscreen))
        );
        assert_eq!(
            mouse_outcome(
                &app,
                area,
                mouse(MouseEventKind::Down(MouseButton::Left), 134, 3)
            ),
            Some(MouseOutcome::Action(TuiAction::ToggleQueueRail))
        );
    }

    #[test]
    fn diagnostics_logs_are_filterable_and_keyboard_scrollable() {
        let mut app = test_app();
        app.screen = Screen::Diagnostics;
        app.diagnostics_logs = vec![
            "info startup complete".to_string(),
            "warn spotify retry".to_string(),
            "error device unavailable".to_string(),
        ];
        let (tx, _rx) = mpsc::unbounded_channel();

        handle_key(&mut app, key(KeyCode::Down), &tx).expect("down should scroll logs");
        assert_eq!(app.selected, 1);

        app.list_filter_query = "error".to_string();
        app.clamp_selection();

        assert_eq!(
            app.filtered_diagnostics_logs(),
            vec!["error device unavailable"]
        );
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn player_queue_is_keyboard_navigable() {
        let mut app = test_app();
        app.screen = Screen::Player;
        app.queue.session_active = true;
        app.queue.items = vec![
            item("spotify:track:first", "First"),
            item("spotify:track:second", "Second"),
        ];
        let (tx, _rx) = mpsc::unbounded_channel();

        handle_key(&mut app, key(KeyCode::Char('j')), &tx).expect("j should move down");
        assert_eq!(app.selected, 1);

        handle_key(&mut app, key(KeyCode::Char('k')), &tx).expect("k should move up");
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn player_space_on_idle_queue_prefers_selected_track() {
        let mut app = test_app();
        app.screen = Screen::Player;
        app.queue.session_active = true;
        app.queue.items = vec![item("spotify:track:first", "First")];

        assert!(player_space_should_play_selected(&app));

        app.playback.item = Some(item("spotify:track:current", "Current"));
        assert!(!player_space_should_play_selected(&app));
    }

    #[test]
    fn stale_player_queue_is_not_visible_or_playable() {
        let mut app = test_app();
        app.screen = Screen::Player;
        app.queue.session_active = false;
        app.queue.items = vec![item("spotify:track:first", "First")];

        assert!(app.visible_items().is_empty());
        assert!(!player_space_should_play_selected(&app));

        app.screen = Screen::Queue;
        assert!(app.visible_items().is_empty());
    }

    #[test]
    fn angle_brackets_seek_on_player() {
        let mut app = test_app();

        assert_eq!(
            action_from_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('<'), KeyModifiers::SHIFT)
            ),
            Some(TuiAction::SeekBack)
        );
        assert_eq!(
            action_from_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('>'), KeyModifiers::SHIFT)
            ),
            Some(TuiAction::SeekForward)
        );
    }

    #[test]
    fn text_input_captures_space_before_global_play_pause() {
        let mut app = test_app();
        app.search_input_active = true;
        let (tx, mut rx) = mpsc::unbounded_channel();

        let should_quit = handle_key(&mut app, key(KeyCode::Char(' ')), &tx)
            .expect("space key should handle while typing");

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
            snapshot_id: None,
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

    #[tokio::test]
    async fn enter_on_a_track_toast_surfaces_both_play_and_queue_shortcuts() {
        // User asked for queue/replace discoverability. After Enter
        // plays a track, the toast should name `e` so they learn the
        // append shortcut without reading docs.
        let mut app = test_app();
        app.screen = Screen::Search;
        app.search_results = vec![item("spotify:track:wonder", "Wonderwall")];
        let (tx, _rx) = mpsc::unbounded_channel();

        let _ = handle_key(&mut app, key(KeyCode::Enter), &tx)
            .expect("enter on a track should dispatch");

        let toast = app.toast.as_deref().unwrap_or_default();
        assert!(
            toast.contains("Wonderwall"),
            "toast should name the track that's now playing, got: {toast:?}"
        );
        assert!(
            toast.contains("queue replaced"),
            "toast should tell the user the queue was replaced, got: {toast:?}"
        );
        assert!(
            toast.contains('e'),
            "toast should hint at `e` as the alternative (append), got: {toast:?}"
        );
    }

    #[test]
    fn device_order_is_stable_across_two_refreshes_with_different_active_flags() {
        // User reported: "The device list keeps changing and re-sorting
        // itself." Root cause: sort key included `is_active` which
        // flips between polls. The list must order on device identity
        // alone so the cursor doesn't chase rows.
        let mut app = test_app();
        app.devices = vec![
            device("dev-a", "Phone", true, false),
            device("dev-b", "Laptop", false, false),
            device("dev-c", "Speaker", false, false),
        ];
        let first = app
            .filtered_devices()
            .into_iter()
            .map(|d| d.id.clone().unwrap_or_default())
            .collect::<Vec<_>>();

        // Second poll: active flag flipped to a different device, also
        // shuffled in the source list.
        app.devices = vec![
            device("dev-c", "Speaker", true, false),
            device("dev-a", "Phone", false, false),
            device("dev-b", "Laptop", false, false),
        ];
        let second = app
            .filtered_devices()
            .into_iter()
            .map(|d| d.id.clone().unwrap_or_default())
            .collect::<Vec<_>>();

        assert_eq!(
            first, second,
            "device ordering must stay identity-stable across refreshes (no jumping rows)"
        );
    }

    #[test]
    fn error_modal_persists_when_an_unrelated_background_command_succeeds() {
        // User reported that error modals "flash and close with barely
        // enough time to read." Root cause: any later successful
        // AsyncResult (a background refresh, an unrelated command)
        // silently wiped `app.error`. The contract is: errors stay
        // until the user dismisses them with Esc/Enter.
        let mut app = test_app();
        app.error = Some(
            "Spotify API 403 on POST /playlists/abc/tracks: scope playlist-modify-public required"
                .to_string(),
        );

        let success_result: CommandResult = CommandResult {
            message: Some("Pause confirmed".to_string()),
            request_refresh: false,
            ..CommandResult::default()
        };
        app.apply_async_result(AsyncResult::Command(Box::new(Ok(success_result))));

        assert!(
            app.error.is_some(),
            "Background success must NOT erase a still-unacknowledged error modal"
        );
        let surviving_error = app.error.as_ref().expect("error should still be set");
        assert!(
            surviving_error.contains("403"),
            "The original error text must remain readable: was {surviving_error:?}"
        );
        assert_eq!(
            app.toast.as_deref(),
            Some("Pause confirmed"),
            "The unrelated success should still apply its own toast/UI updates"
        );
    }

    #[test]
    fn error_modal_persists_when_a_later_search_succeeds() {
        let mut app = test_app();
        app.error = Some("Spotify API 411 on PUT /me/tracks".to_string());
        app.search_query = "wonderwall".to_string();
        app.search_version = 7;
        app.is_searching = true;

        // Under streaming search, results land via DaemonEvent::SearchPage,
        // not the synchronous AsyncResult::Search arm. The event handler
        // must not clear a pending error modal.
        app.apply_async_result(AsyncResult::DaemonEvent(DaemonEvent::SearchPage {
            query: "wonderwall".to_string(),
            kind: MediaKind::Track,
            offset: 0,
            version: 7,
            items: vec![item("spotify:track:wonder", "Wonderwall")],
        }));

        assert!(
            app.error.is_some(),
            "A successful search should not silently clear a pending error"
        );
        assert_eq!(
            app.search_results.len(),
            1,
            "The search results must still land"
        );
    }

    #[test]
    fn error_modal_clears_when_user_presses_esc() {
        let mut app = test_app();
        app.error = Some("Spotify API 411".to_string());
        let (tx, _rx) = mpsc::unbounded_channel();

        let should_quit = handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &tx,
        )
        .expect("Esc should handle");

        assert!(!should_quit);
        assert!(
            app.error.is_none(),
            "Esc must dismiss the error so the user can move on"
        );
    }

    fn search_item(uri: &str, name: &str, kind: MediaKind) -> MediaItem {
        let mut m = item(uri, name);
        m.kind = kind;
        m
    }

    /// Library renders Music and Podcasts in side-by-side panels.
    /// Navigation walks `library_items` as a flat list, so the flat
    /// order must match the panel layout: all music first (relative
    /// order preserved from the SQL query), then all podcasts.
    /// Otherwise `j/k` lurches between panels whenever a Show happens
    /// to sit between two music items in the underlying SQL ordering.
    #[test]
    fn partition_library_keeps_music_before_podcasts_with_stable_relative_order() {
        let interleaved = vec![
            search_item("spotify:track:a", "A", MediaKind::Track),
            search_item("spotify:show:1", "Show 1", MediaKind::Show),
            search_item("spotify:album:b", "B", MediaKind::Album),
            search_item("spotify:episode:1", "Ep 1", MediaKind::Episode),
            search_item("spotify:artist:c", "C", MediaKind::Artist),
            search_item("spotify:show:2", "Show 2", MediaKind::Show),
            search_item("spotify:track:d", "D", MediaKind::Track),
        ];
        let partitioned = partition_library_for_navigation(interleaved);
        let uris: Vec<&str> = partitioned.iter().map(|i| i.uri.as_str()).collect();
        assert_eq!(
            uris,
            vec![
                // Music block keeps fetched_at_ms / name ordering from SQL.
                "spotify:track:a",
                "spotify:album:b",
                "spotify:artist:c",
                "spotify:track:d",
                // Podcasts block likewise.
                "spotify:show:1",
                "spotify:episode:1",
                "spotify:show:2",
            ],
            "music kinds must come before podcasts, and relative order inside each \
             panel must match the input — otherwise the SQL `ORDER BY` is silently \
             undone, which would surprise anyone debugging via the daemon CLI"
        );
    }

    #[test]
    fn g_prefix_jumps_search_cursor_to_first_item_of_chosen_kind() {
        let mut app = test_app();
        app.screen = Screen::Search;
        app.search_results = vec![
            search_item("spotify:track:1", "Song A", MediaKind::Track),
            search_item("spotify:track:2", "Song B", MediaKind::Track),
            search_item("spotify:artist:1", "Artist A", MediaKind::Artist),
            search_item("spotify:album:1", "Album A", MediaKind::Album),
            search_item("spotify:playlist:1", "Playlist A", MediaKind::Playlist),
        ];
        app.selected = 0; // start on Song A
        let (tx, _rx) = mpsc::unbounded_channel();

        // g, then r → jump to first Artist row.
        let _ = handle_key(&mut app, key(KeyCode::Char('g')), &tx).expect("g");
        let _ = handle_key(&mut app, key(KeyCode::Char('r')), &tx).expect("r");
        assert_eq!(app.selected, 2, "cursor should be on Artist A");

        // g, then b → jump to first Album.
        let _ = handle_key(&mut app, key(KeyCode::Char('g')), &tx).expect("g");
        let _ = handle_key(&mut app, key(KeyCode::Char('b')), &tx).expect("b");
        assert_eq!(app.selected, 3, "cursor should be on Album A");

        // g, then p → jump to first Playlist.
        let _ = handle_key(&mut app, key(KeyCode::Char('g')), &tx).expect("g");
        let _ = handle_key(&mut app, key(KeyCode::Char('p')), &tx).expect("p");
        assert_eq!(app.selected, 4, "cursor should be on Playlist A");

        // g, then t → back to first Track.
        let _ = handle_key(&mut app, key(KeyCode::Char('g')), &tx).expect("g");
        let _ = handle_key(&mut app, key(KeyCode::Char('t')), &tx).expect("t");
        assert_eq!(app.selected, 0, "cursor should land on Song A");
    }

    /// The initial search fanout fires offsets [0, 10, 20] in
    /// parallel; the responses can land in any order. The flat list
    /// must show pages in offset order regardless of arrival order,
    /// so the user never sees items reshuffle mid-stream.
    #[test]
    fn search_pages_arriving_out_of_order_render_in_offset_order() {
        let mut app = test_app();
        app.search_query = "anything".to_string();
        app.search_version = 1;
        app.screen = Screen::Search;
        // Seed the pane the way start_search would.
        app.search_panes.insert(
            MediaKind::Track,
            SearchPaneState {
                loading: true,
                exhausted: false,
                error: None,
                next_offset: 0,
                pages: std::collections::BTreeMap::new(),
            },
        );

        // Offset 20 arrives FIRST (slow CDN, weird Spotify behaviour,
        // whatever — the fanout was parallel).
        app.apply_async_result(AsyncResult::DaemonEvent(
            spotuify_protocol::DaemonEvent::SearchPage {
                query: "anything".to_string(),
                kind: MediaKind::Track,
                offset: 20,
                version: 1,
                items: vec![
                    search_item("spotify:track:20", "Song 20", MediaKind::Track),
                    search_item("spotify:track:21", "Song 21", MediaKind::Track),
                ],
            },
        ));
        // Then offset 0.
        app.apply_async_result(AsyncResult::DaemonEvent(
            spotuify_protocol::DaemonEvent::SearchPage {
                query: "anything".to_string(),
                kind: MediaKind::Track,
                offset: 0,
                version: 1,
                items: vec![
                    search_item("spotify:track:0", "Song 0", MediaKind::Track),
                    search_item("spotify:track:1", "Song 1", MediaKind::Track),
                ],
            },
        ));
        // Then offset 10.
        app.apply_async_result(AsyncResult::DaemonEvent(
            spotuify_protocol::DaemonEvent::SearchPage {
                query: "anything".to_string(),
                kind: MediaKind::Track,
                offset: 10,
                version: 1,
                items: vec![
                    search_item("spotify:track:10", "Song 10", MediaKind::Track),
                    search_item("spotify:track:11", "Song 11", MediaKind::Track),
                ],
            },
        ));

        let uris: Vec<&str> = app.search_results.iter().map(|i| i.uri.as_str()).collect();
        assert_eq!(
            uris,
            vec![
                "spotify:track:0",
                "spotify:track:1",
                "spotify:track:10",
                "spotify:track:11",
                "spotify:track:20",
                "spotify:track:21",
            ],
            "items must be rendered in offset order regardless of which page arrived first \
             — otherwise the list reshuffles every time a slow page lands"
        );
    }

    /// Search results stream in from six parallel Spotify endpoints
    /// (tracks/artists/albums/playlists/shows/episodes). Whichever
    /// response arrived first used to land at index 0 — i.e. the
    /// focused pane was a coin flip. Fix: the cursor snaps to the
    /// first item of the highest-priority non-empty kind on every
    /// streamed page, until the user steers.
    #[test]
    fn streaming_search_pages_snap_cursor_to_tracks_regardless_of_arrival_order() {
        let mut app = test_app();
        app.search_query = "anything".to_string();
        app.search_version = 1;
        app.screen = Screen::Search;
        // First page arrives: Podcasts. Spotify's podcast endpoint was
        // fastest this run. Pre-fix cursor would point at the show.
        app.apply_async_result(AsyncResult::DaemonEvent(
            spotuify_protocol::DaemonEvent::SearchPage {
                query: "anything".to_string(),
                kind: MediaKind::Show,
                offset: 0,
                version: 1,
                items: vec![search_item("spotify:show:1", "Pod A", MediaKind::Show)],
            },
        ));
        // Second page: an Album. Cursor would jump to Album under the
        // pre-fix arrival-order behaviour (depending on insert order).
        app.apply_async_result(AsyncResult::DaemonEvent(
            spotuify_protocol::DaemonEvent::SearchPage {
                query: "anything".to_string(),
                kind: MediaKind::Album,
                offset: 0,
                version: 1,
                items: vec![search_item("spotify:album:1", "Album A", MediaKind::Album)],
            },
        ));
        // Third page: Tracks. Now Tracks exist, so the cursor MUST
        // snap to the first track regardless of when it arrived.
        app.apply_async_result(AsyncResult::DaemonEvent(
            spotuify_protocol::DaemonEvent::SearchPage {
                query: "anything".to_string(),
                kind: MediaKind::Track,
                offset: 0,
                version: 1,
                items: vec![
                    search_item("spotify:track:1", "Song A", MediaKind::Track),
                    search_item("spotify:track:2", "Song B", MediaKind::Track),
                ],
            },
        ));

        let selected_kind = app
            .search_results
            .get(app.selected)
            .map(|i| i.kind.clone())
            .expect("selection should index into results");
        assert_eq!(
            selected_kind,
            MediaKind::Track,
            "cursor must snap to the first Track once any track lands, no matter what kind \
             arrived first — that was the random-pane bug"
        );
    }

    /// After the user steers, a late-arriving earlier-offset page
    /// shifts everyone's index — but the cursor must keep tracking
    /// the same URI, not the same numeric index. `#[tokio::test]`
    /// because cursor-move actions can spawn pagination triggers on
    /// the current runtime.
    #[tokio::test]
    async fn user_steered_cursor_follows_uri_when_earlier_page_arrives_later() {
        let mut app = test_app();
        app.search_query = "anything".to_string();
        app.search_version = 1;
        app.screen = Screen::Search;
        app.search_panes.insert(
            MediaKind::Track,
            SearchPaneState {
                loading: true,
                exhausted: false,
                error: None,
                next_offset: 0,
                pages: std::collections::BTreeMap::new(),
            },
        );

        // Offset 10 lands first.
        app.apply_async_result(AsyncResult::DaemonEvent(
            spotuify_protocol::DaemonEvent::SearchPage {
                query: "anything".to_string(),
                kind: MediaKind::Track,
                offset: 10,
                version: 1,
                items: vec![
                    search_item("spotify:track:10", "Song 10", MediaKind::Track),
                    search_item("spotify:track:11", "Song 11", MediaKind::Track),
                ],
            },
        ));
        // User explicitly moves down to song 11.
        let (tx, _rx) = mpsc::unbounded_channel();
        let _ = handle_key(&mut app, key(KeyCode::Down), &tx).expect("down");
        assert_eq!(
            app.search_results[app.selected].uri, "spotify:track:11",
            "user moved cursor to song 11; precondition"
        );

        // Offset 0 lands AFTER. Rebuild shifts song 11 from index 1
        // to index 3 (now [song:0, song:1, song:10, song:11]).
        app.apply_async_result(AsyncResult::DaemonEvent(
            spotuify_protocol::DaemonEvent::SearchPage {
                query: "anything".to_string(),
                kind: MediaKind::Track,
                offset: 0,
                version: 1,
                items: vec![
                    search_item("spotify:track:0", "Song 0", MediaKind::Track),
                    search_item("spotify:track:1", "Song 1", MediaKind::Track),
                ],
            },
        ));
        assert_eq!(
            app.search_results[app.selected].uri, "spotify:track:11",
            "cursor must follow the URI the user picked, not the numeric index — \
             otherwise late-arriving earlier pages would silently jerk the cursor \
             to whatever happens to share the old index"
        );
    }

    #[test]
    fn search_failed_clears_pane_loading_without_exhausting_it() {
        let mut app = test_app();
        app.search_query = "anything".to_string();
        app.search_version = 1;
        app.screen = Screen::Search;
        app.search_panes.insert(
            MediaKind::Track,
            SearchPaneState {
                loading: true,
                exhausted: false,
                error: None,
                next_offset: 10,
                pages: std::collections::BTreeMap::new(),
            },
        );

        app.apply_async_result(AsyncResult::DaemonEvent(DaemonEvent::SearchFailed {
            query: "anything".to_string(),
            version: 1,
            kind: Some(MediaKind::Track),
            offset: Some(10),
            message: "search timed out".to_string(),
        }));

        let pane = app
            .search_panes
            .get(&MediaKind::Track)
            .expect("track search pane should exist");
        assert!(!pane.loading);
        assert!(!pane.exhausted);
        assert_eq!(pane.error.as_deref(), Some("search timed out"));
    }

    /// After the user has steered (arrow keys, `g <letter>`, or panel
    /// cycle), subsequent streamed pages must NOT yank the cursor back
    /// to Tracks. The user's last explicit choice wins.
    #[test]
    fn user_steered_cursor_survives_subsequent_streamed_pages() {
        let mut app = test_app();
        app.search_query = "anything".to_string();
        app.search_version = 1;
        app.screen = Screen::Search;

        // First page: tracks land.
        app.apply_async_result(AsyncResult::DaemonEvent(
            spotuify_protocol::DaemonEvent::SearchPage {
                query: "anything".to_string(),
                kind: MediaKind::Track,
                offset: 0,
                version: 1,
                items: vec![search_item("spotify:track:1", "Song A", MediaKind::Track)],
            },
        ));
        // Artists too.
        app.apply_async_result(AsyncResult::DaemonEvent(
            spotuify_protocol::DaemonEvent::SearchPage {
                query: "anything".to_string(),
                kind: MediaKind::Artist,
                offset: 0,
                version: 1,
                items: vec![search_item(
                    "spotify:artist:1",
                    "Artist A",
                    MediaKind::Artist,
                )],
            },
        ));
        // User picks Artists pane via `g r`.
        let (tx, _rx) = mpsc::unbounded_channel();
        let _ = handle_key(&mut app, key(KeyCode::Char('g')), &tx).expect("g");
        let _ = handle_key(&mut app, key(KeyCode::Char('r')), &tx).expect("r");
        assert_eq!(
            app.search_results[app.selected].kind,
            MediaKind::Artist,
            "user just steered to Artists; precondition"
        );

        // Now a slow Albums page lands. Snap-to-preferred would pull
        // the cursor back to Tracks. It must not.
        app.apply_async_result(AsyncResult::DaemonEvent(
            spotuify_protocol::DaemonEvent::SearchPage {
                query: "anything".to_string(),
                kind: MediaKind::Album,
                offset: 0,
                version: 1,
                items: vec![search_item("spotify:album:1", "Album A", MediaKind::Album)],
            },
        ));
        assert_eq!(
            app.search_results[app.selected].kind,
            MediaKind::Artist,
            "after the user has steered, new streaming pages must not yank the cursor — \
             auto-snap is a startup hint, not a recurring override"
        );
    }

    /// A brand-new search resets the steered flag — the next query
    /// gets the auto-snap behaviour back. `#[tokio::test]` because
    /// `start_search` spawns the request future on the current Tokio
    /// runtime; the spawned task itself isn't what we're checking
    /// (its first `await` never completes in tests) — we only need
    /// the in-process state mutation that happens before the spawn.
    #[tokio::test]
    async fn fresh_search_query_re_enables_auto_snap() {
        let mut app = test_app();
        app.search_query = "first".to_string();
        app.search_version = 1;
        app.screen = Screen::Search;
        // Initial page + user steers away.
        app.apply_async_result(AsyncResult::DaemonEvent(
            spotuify_protocol::DaemonEvent::SearchPage {
                query: "first".to_string(),
                kind: MediaKind::Track,
                offset: 0,
                version: 1,
                items: vec![search_item("spotify:track:1", "Song A", MediaKind::Track)],
            },
        ));
        let (tx, _rx) = mpsc::unbounded_channel();
        // Steer to Artist (will toast 'no Artist results' but flip the flag).
        // Easier: simulate by calling set_active_selection (mouse-click path).
        app.apply_async_result(AsyncResult::DaemonEvent(
            spotuify_protocol::DaemonEvent::SearchPage {
                query: "first".to_string(),
                kind: MediaKind::Artist,
                offset: 0,
                version: 1,
                items: vec![search_item(
                    "spotify:artist:1",
                    "Artist A",
                    MediaKind::Artist,
                )],
            },
        ));
        let _ = handle_key(&mut app, key(KeyCode::Char('g')), &tx).expect("g");
        let _ = handle_key(&mut app, key(KeyCode::Char('r')), &tx).expect("r");
        assert!(
            app.search_user_steered,
            "g+r should mark the search as steered (precondition)"
        );

        // Start a new search → flag resets. Use start_search internal.
        app.search_query = "second".to_string();
        start_search(&mut app, &tx);
        assert!(
            !app.search_user_steered,
            "new search must clear the steered flag so the next results land on Tracks"
        );
    }

    #[test]
    fn g_prefix_jump_to_missing_kind_shows_toast_and_keeps_cursor() {
        let mut app = test_app();
        app.screen = Screen::Search;
        app.search_results = vec![search_item("spotify:track:1", "Song A", MediaKind::Track)];
        app.selected = 0;
        let (tx, _rx) = mpsc::unbounded_channel();

        let _ = handle_key(&mut app, key(KeyCode::Char('g')), &tx).expect("g");
        let _ = handle_key(&mut app, key(KeyCode::Char('s')), &tx).expect("s");

        assert_eq!(
            app.selected, 0,
            "cursor should not move when target kind is absent"
        );
        assert!(
            app.toast.as_deref().is_some_and(|t| t.contains("No")),
            "should toast \"no <kind> results\""
        );
    }

    #[test]
    fn add_to_playlist_opens_picker_without_leaving_current_screen() {
        let mut app = test_app();
        app.screen = Screen::Search;
        app.search_results = vec![item("spotify:track:first", "First")];
        app.playlists = vec![Playlist {
            id: "playlist-1".to_string(),
            name: "Road Trip".to_string(),
            owner: "me".to_string(),
            tracks_total: 0,
            image_url: None,
            snapshot_id: None,
        }];
        let (tx, mut rx) = mpsc::unbounded_channel();

        let should_quit = handle_key(&mut app, key(KeyCode::Char('A')), &tx)
            .expect("add-to-playlist key should handle");

        assert!(!should_quit);
        assert_eq!(app.screen, Screen::Search);
        assert!(app.playlist_picker.is_some());
        let picker = app
            .playlist_picker
            .as_ref()
            .expect("add-to-playlist should open picker");
        assert_eq!(picker.uris, vec!["spotify:track:first"]);
        assert!(rx.try_recv().is_err(), "opening picker is local UI state");
    }

    #[test]
    fn playlist_picker_builds_one_add_request_per_selected_playlist() {
        let mut app = test_app();
        app.playlists = vec![
            Playlist {
                id: "playlist-1".to_string(),
                name: "Road Trip".to_string(),
                owner: "me".to_string(),
                tracks_total: 0,
                image_url: None,
                snapshot_id: None,
            },
            Playlist {
                id: "playlist-2".to_string(),
                name: "Gym".to_string(),
                owner: "me".to_string(),
                tracks_total: 0,
                image_url: None,
                snapshot_id: None,
            },
        ];
        app.playlist_picker = Some(PlaylistPickerModal {
            uris: vec!["spotify:track:first".to_string()],
            selected: 0,
            selected_playlist_ids: HashSet::from([
                "playlist-2".to_string(),
                "playlist-1".to_string(),
            ]),
        });

        let requests = playlist_picker_requests(&app);

        assert_eq!(
            requests,
            vec![
                Request::PlaylistAddItems {
                    playlist: "playlist-1".to_string(),
                    uris: vec!["spotify:track:first".to_string()],
                },
                Request::PlaylistAddItems {
                    playlist: "playlist-2".to_string(),
                    uris: vec!["spotify:track:first".to_string()],
                },
            ]
        );
    }

    #[test]
    fn command_palette_opens_with_current_device_context() {
        let mut app = test_app();
        app.screen = Screen::Devices;
        let (tx, _) = mpsc::unbounded_channel();

        let should_quit = apply_tui_action(&mut app, TuiAction::OpenCommandPalette, &tx)
            .expect("command palette action should handle");

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

    #[test]
    fn capital_d_opens_device_picker_preselecting_active_device() {
        let mut app = test_app();
        app.screen = Screen::Player;
        // filtered_devices() sorts by id; ensure preselect tracks
        // `is_active` after sorting, not the raw input order.
        app.devices = vec![
            device("dev-a", "Phone", false, false),
            device("dev-b", "Laptop", true, false),
            device("dev-c", "Speaker", false, false),
        ];
        let (tx, _rx) = mpsc::unbounded_channel();

        let should_quit =
            handle_key(&mut app, key(KeyCode::Char('D')), &tx).expect("D opens device picker");

        assert!(!should_quit);
        assert_eq!(
            app.screen,
            Screen::Player,
            "picker is an overlay, not a screen switch"
        );
        let picker = app
            .device_picker
            .as_ref()
            .expect("D should open the device picker");
        let sorted = app.filtered_devices();
        assert_eq!(
            sorted[picker.selected].id.as_deref(),
            Some("dev-b"),
            "active device should be preselected"
        );
    }

    #[tokio::test]
    async fn enter_on_device_picker_transfers_to_selected_device() {
        let mut app = test_app();
        app.devices = vec![
            device("dev-a", "Phone", false, false),
            device("dev-b", "Laptop", true, false),
        ];
        // Pretend the cursor moved to the second sorted entry.
        let sorted = app.filtered_devices();
        let target_idx = sorted
            .iter()
            .position(|d| d.id.as_deref() == Some("dev-b"))
            .expect("dev-b should be present in filtered devices");
        app.device_picker = Some(DevicePickerModal {
            selected: target_idx,
        });
        let (tx, _rx) = mpsc::unbounded_channel();

        let should_quit = handle_key(&mut app, key(KeyCode::Enter), &tx).expect("Enter transfers");

        assert!(!should_quit);
        assert!(
            app.device_picker.is_none(),
            "picker should close after transfer"
        );
        // `begin_action` is the synchronous gate inside `command_then_refresh`;
        // its flag flipping to true is proof that the transfer dispatched
        // (the async tokio::spawn races the test, so we can't observe the
        // channel send directly without a runtime tick).
        assert!(
            app.action_in_flight,
            "Enter on a picked device should dispatch a transfer command"
        );
    }

    #[test]
    fn esc_on_device_picker_cancels_without_transferring() {
        let mut app = test_app();
        app.devices = vec![device("dev-a", "Phone", false, false)];
        app.device_picker = Some(DevicePickerModal { selected: 0 });
        let (tx, mut rx) = mpsc::unbounded_channel();

        let _ = handle_key(&mut app, key(KeyCode::Esc), &tx).expect("Esc cancels");

        assert!(app.device_picker.is_none());
        assert!(rx.try_recv().is_err(), "cancel must not dispatch anything");
    }

    #[test]
    fn filtered_devices_order_by_identity_so_active_flag_changes_dont_jump_rows() {
        // Identity-first ordering (rather than `is_active` first) is
        // what the user requested. The active/restricted state still
        // appears in the row data; it just doesn't drive the sort key.
        let mut app = test_app();
        app.devices = vec![
            device("z", "Bedroom", false, false),
            device("r", "AirPlay", false, true),
            device("a", "Desk", true, false),
            device("b", "Basement", false, false),
        ];

        let ids = app
            .filtered_devices()
            .into_iter()
            .map(|device| device.id.clone().unwrap_or_default())
            .collect::<Vec<_>>();

        assert_eq!(
            ids,
            vec![
                "a".to_string(),
                "b".to_string(),
                "r".to_string(),
                "z".to_string()
            ],
            "devices should appear in id order, regardless of which one is active"
        );
    }

    #[test]
    fn app_starts_with_right_rail_hidden() {
        let app = test_app();

        assert_eq!(app.right_rail, RightRailMode::Hidden);
    }

    #[test]
    fn uppercase_l_toggles_lyrics_rail_without_leaving_current_screen() {
        let mut app = test_app();
        app.screen = Screen::Devices;
        app.last_library_sync = Some(Instant::now());
        app.playback.item = Some(item("spotify:track:lyrics", "Lyrics Track"));
        let (tx, mut rx) = mpsc::unbounded_channel();

        let should_quit =
            handle_key(&mut app, key(KeyCode::Char('L')), &tx).expect("lyrics key should handle");

        assert!(!should_quit);
        assert_eq!(app.screen, Screen::Devices);
        assert_eq!(app.right_rail, RightRailMode::Lyrics);
        assert!(!app.lyrics_loading);
        assert!(app.refresh_requested);
        assert!(
            refresh_plan(&app).lyrics,
            "opening lyrics rail should schedule the fetch instead of blocking it"
        );
        assert!(
            rx.try_recv().is_err(),
            "opening lyrics rail should reuse refresh path"
        );

        let should_quit =
            handle_key(&mut app, key(KeyCode::Char('L')), &tx).expect("lyrics key should handle");

        assert!(!should_quit);
        assert_eq!(app.screen, Screen::Devices);
        assert_eq!(app.right_rail, RightRailMode::Hidden);
    }

    #[test]
    fn uppercase_q_toggles_queue_rail_without_leaving_current_screen() {
        let mut app = test_app();
        app.screen = Screen::Search;
        let (tx, mut rx) = mpsc::unbounded_channel();

        let should_quit =
            handle_key(&mut app, key(KeyCode::Char('Q')), &tx).expect("queue key should handle");

        assert!(!should_quit);
        assert_eq!(app.screen, Screen::Search);
        assert_eq!(app.right_rail, RightRailMode::Queue);
        assert!(app.refresh_requested);
        assert!(
            rx.try_recv().is_err(),
            "opening queue rail should reuse refresh path"
        );

        let should_quit =
            handle_key(&mut app, key(KeyCode::Char('Q')), &tx).expect("queue key should handle");

        assert!(!should_quit);
        assert_eq!(app.screen, Screen::Search);
        assert_eq!(app.right_rail, RightRailMode::Hidden);
    }

    #[test]
    fn uppercase_h_toggles_hints_rail_without_requesting_refresh() {
        let mut app = test_app();
        app.screen = Screen::Library;
        let (tx, mut rx) = mpsc::unbounded_channel();

        let should_quit =
            handle_key(&mut app, key(KeyCode::Char('H')), &tx).expect("hints key should handle");

        assert!(!should_quit);
        assert_eq!(app.screen, Screen::Library);
        assert_eq!(app.right_rail, RightRailMode::Hints);
        assert!(!app.refresh_requested);
        assert!(
            rx.try_recv().is_err(),
            "opening hints rail is local TUI state only"
        );
    }

    #[test]
    fn uppercase_f_expands_and_closes_active_queue_rail() {
        let mut app = test_app();
        app.screen = Screen::Search;
        app.right_rail = RightRailMode::Queue;
        let (tx, _) = mpsc::unbounded_channel();

        handle_key(&mut app, key(KeyCode::Char('F')), &tx).expect("fullscreen key should handle");

        assert_eq!(app.screen, Screen::Search);
        assert_eq!(app.fullscreen_panel, Some(FullscreenPanel::Queue));

        handle_key(&mut app, key(KeyCode::Esc), &tx).expect("esc should close fullscreen panel");

        assert_eq!(app.fullscreen_panel, None);
        assert_eq!(app.right_rail, RightRailMode::Queue);
    }

    #[test]
    fn daemon_queue_event_writes_queue_directly_without_refresh() {
        let mut app = test_app();
        let queue = Queue {
            currently_playing: Some(track_with_image("spotify:track:first", None)),
            items: Vec::new(),
            ..Default::default()
        };

        app.apply_async_result(AsyncResult::DaemonEvent(DaemonEvent::QueueChanged {
            action: "queue".to_string(),
            uris: vec!["spotify:track:first".to_string()],
            queue: Some(queue.clone()),
        }));

        // Push-only contract: event is the sole writer for self.queue.
        // No refresh round-trip needed — that's the whole point.
        assert!(
            !app.refresh_requested,
            "queue event must not trigger a refresh (single-writer contract)"
        );
        assert_eq!(
            app.queue
                .currently_playing
                .as_ref()
                .map(|item| item.uri.as_str()),
            Some("spotify:track:first")
        );
        assert!(app.queue_updated_at.is_some());
        assert_eq!(app.toast.as_deref(), Some("Queue updated: 1 item(s)"));
    }

    #[test]
    fn successful_background_refresh_does_not_dismiss_command_error() {
        let mut app = test_app();
        app.error = Some("Spotify API 411 on PUT /me/player/seek".to_string());

        let mut snapshot = empty_refresh_snapshot();
        snapshot.elapsed_ms = 12;
        app.apply_refresh(snapshot);

        assert_eq!(
            app.error.as_deref(),
            Some("Spotify API 411 on PUT /me/player/seek")
        );
    }

    #[test]
    fn refresh_plan_loads_stale_library_without_manual_prompt() {
        let app = test_app();

        let plan = refresh_plan(&app);

        assert_eq!(
            plan,
            RefreshPlan {
                library: true,
                diagnostics: false,
                lyrics: false,
            }
        );
    }

    #[test]
    fn refresh_plan_loads_lyrics_when_lyrics_rail_is_open_on_any_screen() {
        let mut app = test_app();
        app.screen = Screen::Search;
        app.right_rail = RightRailMode::Lyrics;
        app.last_library_sync = Some(Instant::now());
        app.playback.item = Some(item("spotify:track:lyrics", "Lyrics Track"));

        let plan = refresh_plan(&app);

        assert_eq!(
            plan,
            RefreshPlan {
                library: false,
                diagnostics: false,
                lyrics: true,
            }
        );
    }

    #[test]
    fn requesting_visible_lyrics_does_not_pre_mark_loading_and_block_fetch() {
        let mut app = test_app();
        app.screen = Screen::Player;
        app.right_rail = RightRailMode::Lyrics;
        app.last_library_sync = Some(Instant::now());
        app.playback.item = Some(item("spotify:track:lyrics", "Lyrics Track"));

        app.request_lyrics_if_visible();

        assert!(!app.lyrics_loading);
        assert!(app.refresh_requested);
        assert!(
            refresh_plan(&app).lyrics,
            "refresh plan must still enqueue the lyrics request"
        );
    }

    #[test]
    fn refresh_plan_does_not_retry_failed_lyrics_for_same_track() {
        let mut app = test_app();
        app.screen = Screen::Search;
        app.right_rail = RightRailMode::Lyrics;
        app.last_library_sync = Some(Instant::now());
        app.playback.item = Some(item("spotify:track:lyrics", "Lyrics Track"));
        app.lyrics_failed_track_uri = Some("spotify:track:lyrics".to_string());
        app.lyrics_error = Some("operation timed out after 10s".to_string());

        assert_eq!(
            refresh_plan(&app),
            RefreshPlan {
                library: false,
                diagnostics: false,
                lyrics: false,
            }
        );

        app.lyrics_failed_track_uri = None;
        assert!(refresh_plan(&app).lyrics);
    }

    #[test]
    fn apply_refresh_marks_lyrics_failure_for_active_track() {
        let mut app = test_app();
        app.playback.item = Some(item("spotify:track:lyrics", "Lyrics Track"));

        let mut snapshot = empty_refresh_snapshot();
        snapshot.lyrics_error = Some("operation timed out after 10s".to_string());
        snapshot.lyrics_error_track_uri = Some("spotify:track:lyrics".to_string());
        app.apply_refresh(snapshot);

        assert_eq!(
            app.lyrics_failed_track_uri.as_deref(),
            Some("spotify:track:lyrics")
        );
        assert!(!app.lyrics_loading);
    }

    #[test]
    fn format_mutation_toast_strips_spotify_client_prefix_and_uses_outcome_label() {
        use spotuify_protocol::ReceiptStatus;

        assert_eq!(
            format_mutation_toast(ReceiptStatus::Confirmed, "Saved Wonderwall"),
            "Confirmed: Saved Wonderwall"
        );
        assert_eq!(
            format_mutation_toast(
                ReceiptStatus::Failed,
                "Spotify client error: Office Echo isn't available right now (Spotify 404); pick another device with [D]"
            ),
            "Failed: Office Echo isn't available right now (Spotify 404); pick another device with [D]"
        );
    }

    #[test]
    fn mutation_events_drive_pending_receipt_state() {
        let mut app = test_app();
        let receipt_id = spotuify_protocol::ReceiptId::new_v7();

        app.apply_async_result(AsyncResult::DaemonEvent(DaemonEvent::MutationAccepted {
            receipt_id,
            action: "playlist-add".to_string(),
        }));

        assert_eq!(app.pending_receipts.len(), 1);
        assert_eq!(app.pending_receipts[0].action, "playlist-add");

        app.apply_async_result(AsyncResult::DaemonEvent(DaemonEvent::MutationFinalized {
            receipt_id,
            status: spotuify_protocol::ReceiptStatus::Confirmed,
            message: "done".to_string(),
        }));

        assert!(app.pending_receipts.is_empty());
        assert!(app.refresh_requested);
    }

    #[test]
    fn rate_limit_and_auth_events_drive_banner_state() {
        let mut app = test_app();

        app.apply_async_result(AsyncResult::DaemonEvent(DaemonEvent::RateLimited {
            retry_after_secs: 7,
            scope: "GET /me/tracks".to_string(),
        }));

        assert!(matches!(
            app.banner,
            Some(BannerState::RateLimited {
                retry_after_secs: 7,
                ..
            })
        ));

        app.apply_async_result(AsyncResult::DaemonEvent(DaemonEvent::AuthError {
            kind: spotuify_protocol::AuthErrorKind::InvalidGrant,
        }));

        assert!(matches!(
            app.banner,
            Some(BannerState::Auth {
                kind: spotuify_protocol::AuthErrorKind::InvalidGrant
            })
        ));
    }

    // -----------------------------------------------------------------
    // Phase 6 — merge_playback / apply_refresh / optimistic-apply tests
    // -----------------------------------------------------------------
    //
    // These tests guard the contracts called out in the plan doc:
    // - Stale local progress is preserved across no-op refreshes
    // - Drift > 1500ms re-anchors
    // - PlayerEvent / CommandResult sources always re-anchor
    // - Cover art clears the instant the active art URL changes
    // - Lyrics fetched for a different URI are dropped
    // - Optimistic pause/play applies locally before daemon ack

    fn track_with_image(uri: &str, image_url: Option<&str>) -> MediaItem {
        MediaItem {
            id: Some(uri.rsplit(':').next().unwrap_or(uri).to_string()),
            uri: uri.to_string(),
            name: "Test".to_string(),
            subtitle: "Artist".to_string(),
            context: "Album".to_string(),
            duration_ms: 300_000,
            image_url: image_url.map(|s| s.to_string()),
            kind: MediaKind::Track,
            source: None,
            freshness: None,
            explicit: None,
            is_playable: None,
        }
    }

    fn playback_with(item: MediaItem, progress_ms: u64) -> spotuify_core::Playback {
        spotuify_core::Playback {
            item: Some(item),
            is_playing: true,
            progress_ms,
            ..Default::default()
        }
    }

    #[test]
    fn merge_playback_preserves_local_progress_on_minor_drift() {
        let mut app = test_app();
        // Bootstrap with track at 10s.
        let original = playback_with(track_with_image("spotify:track:a", None), 10_000);
        app.merge_playback(original.clone());
        // Local clock ticked forward to 10.5s.
        app.playback.progress_ms = 10_500;
        // Incoming refresh reports 10.2s — small drift, should preserve.
        let mut incoming = original.clone();
        incoming.progress_ms = 10_200;
        app.merge_playback(incoming);
        assert_eq!(
            app.playback.progress_ms, 10_500,
            "minor Web API drift must not clobber local extrapolation"
        );
    }

    #[test]
    fn merge_playback_reanchors_on_large_drift() {
        let mut app = test_app();
        let original = playback_with(track_with_image("spotify:track:a", None), 10_000);
        app.merge_playback(original.clone());
        app.playback.progress_ms = 10_500;
        // Big drift: remote seek to 60s on same track.
        let mut incoming = original.clone();
        incoming.progress_ms = 60_000;
        app.merge_playback(incoming);
        assert_eq!(
            app.playback.progress_ms, 60_000,
            "drift > 1500ms must re-anchor (catches remote seeks)"
        );
    }

    #[test]
    fn merge_playback_keeps_optimistic_track_during_activation_gap() {
        let mut app = test_app();
        app.playback = playback_with(track_with_image("spotify:track:new", None), 1_000);
        app.awaiting_track_change_until = Some(Instant::now() + Duration::from_secs(1));

        app.merge_playback(spotuify_core::Playback::default());

        assert_eq!(
            app.playback.item.as_ref().map(|item| item.uri.as_str()),
            Some("spotify:track:new")
        );
        assert_eq!(app.playback.progress_ms, 1_000);
        assert!(
            !app.playback.is_playing,
            "empty activation-gap poll may stop ticking, but must not clear the selected track"
        );
    }

    #[test]
    fn merge_playback_reanchors_on_player_event_source() {
        let mut app = test_app();
        let original = playback_with(track_with_image("spotify:track:a", None), 10_000);
        app.merge_playback(original.clone());
        app.playback.progress_ms = 10_500;
        // Tiny "drift" but marked PlayerEvent — authoritative.
        let mut incoming = original.clone();
        incoming.progress_ms = 10_200;
        incoming.source = Some(spotuify_core::PlaybackStateSource::PlayerEvent);
        app.merge_playback(incoming);
        assert_eq!(
            app.playback.progress_ms, 10_200,
            "PlayerEvent-sourced snapshot must always re-anchor regardless of drift size"
        );
    }

    #[test]
    fn apply_async_event_clears_cover_when_art_url_changes() {
        let mut app = test_app();
        app.current_art_url = Some("https://example.com/old.jpg".to_string());
        // Apply a PlaybackChanged event embedding a new image URL.
        // Push-only flow: the event arm itself derives the URL change
        // and clears cover (no refresh round-trip involved).
        let new_item = track_with_image("spotify:track:b", Some("https://example.com/new.jpg"));
        let event = DaemonEvent::PlaybackChanged {
            action: "track-change".to_string(),
            playback: Some(playback_with(new_item, 0)),
        };
        let (tx, _rx) = mpsc::unbounded_channel();
        app.apply_daemon_event(event, &tx);
        assert_eq!(
            app.current_art_url.as_deref(),
            Some("https://example.com/new.jpg"),
            "current_art_url must track the active playback's image URL"
        );
        assert!(
            app.cover.is_none(),
            "cover must be cleared when art URL changes"
        );
    }

    #[test]
    fn apply_refresh_drops_lyrics_for_stale_uri() {
        let mut app = test_app();
        // Active playback is track:a.
        app.merge_playback(playback_with(track_with_image("spotify:track:a", None), 0));
        // A lyrics fetch for track:b (the user already moved on) arrives.
        let stale_lyrics = SyncedLyrics {
            track_uri: "spotify:track:b".to_string(),
            provider: spotuify_core::LyricsProvider::SpotifyMercury,
            lines: vec![spotuify_core::LyricLine {
                start_ms: 0,
                text: "stale".to_string(),
                is_rtl: false,
            }],
            fetched_at_ms: 0,
            synced: true,
            language: None,
            source_url: None,
        };
        let mut snapshot = empty_refresh_snapshot();
        snapshot.lyrics = Some(LyricsSnapshot {
            lyrics: Some(stale_lyrics),
            track_uri: "spotify:track:b".to_string(),
            offset_ms: 0,
        });
        app.apply_refresh(snapshot);
        assert!(
            app.lyrics.is_none(),
            "Phase 6: lyrics fetched for a non-active URI must be dropped"
        );
        assert!(
            app.lyrics_track_uri.is_none(),
            "Phase 6: stale lyrics URI must not be recorded"
        );
    }

    /// Three rapid `PlaybackChanged` events advance `current_art_url`
    /// past each prior URL. The two earlier `CoverFetched` results
    /// arrive after the fact (the "stale cover ladder" the user
    /// reported) and must self-discard on URL mismatch. Only the most
    /// recent URL's cover should ever install.
    #[test]
    fn rapid_track_changes_drop_stale_cover_results() {
        let mut app = test_app();
        let (tx, _rx) = mpsc::unbounded_channel();
        // Track A → B → C in quick succession.
        for url in [
            "https://e.com/a.jpg",
            "https://e.com/b.jpg",
            "https://e.com/c.jpg",
        ] {
            let item = track_with_image(&format!("spotify:track:{url}"), Some(url));
            app.apply_daemon_event(
                DaemonEvent::PlaybackChanged {
                    action: "track-change".to_string(),
                    playback: Some(playback_with(item, 0)),
                },
                &tx,
            );
        }
        assert_eq!(
            app.current_art_url.as_deref(),
            Some("https://e.com/c.jpg"),
            "current_art_url tracks the most recent event"
        );
        // Two stale cover fetches resolve out of order. Both must drop.
        let blank = image::DynamicImage::new_rgb8(1, 1);
        app.apply_async_result(AsyncResult::CoverFetched {
            url: "https://e.com/a.jpg".to_string(),
            image: blank.clone(),
        });
        app.apply_async_result(AsyncResult::CoverFetched {
            url: "https://e.com/b.jpg".to_string(),
            image: blank.clone(),
        });
        assert!(
            app.cover.is_none(),
            "stale cover results must not install (URL drop-on-stale)"
        );
        // The matching result lands → cover installs.
        app.apply_async_result(AsyncResult::CoverFetched {
            url: "https://e.com/c.jpg".to_string(),
            image: blank,
        });
        assert!(app.cover.is_some(), "matching cover result must install");
    }

    /// `apply_refresh` is no longer authoritative for playback. Even
    /// if some legacy caller constructs a refresh, it must not clobber
    /// event-driven `self.playback`.
    #[test]
    fn refresh_does_not_overwrite_event_driven_playback() {
        let mut app = test_app();
        let (tx, _rx) = mpsc::unbounded_channel();
        let event_item = track_with_image("spotify:track:event", None);
        app.apply_daemon_event(
            DaemonEvent::PlaybackChanged {
                action: "play".to_string(),
                playback: Some(playback_with(event_item, 5_000)),
            },
            &tx,
        );
        let before = app.playback.item.as_ref().map(|i| i.uri.clone());

        // A refresh lands later (poll-only ancillary state).
        app.apply_refresh(empty_refresh_snapshot());

        let after = app.playback.item.as_ref().map(|i| i.uri.clone());
        assert_eq!(
            before, after,
            "refresh must not touch self.playback under the push-only contract"
        );
    }

    /// Seed is the bootstrap/recovery path. If an event has already
    /// updated state since the seed was issued, the seed must skip
    /// that field (tie-breaker by `fetched_at`).
    #[test]
    fn seed_skipped_when_newer_event_already_arrived() {
        let mut app = test_app();
        let (tx, _rx) = mpsc::unbounded_channel();
        // Event arrives first.
        let fresh_item = track_with_image("spotify:track:fresh", None);
        app.apply_daemon_event(
            DaemonEvent::PlaybackChanged {
                action: "play".to_string(),
                playback: Some(playback_with(fresh_item, 0)),
            },
            &tx,
        );

        // Seed result with an earlier `fetched_at` than the event.
        let stale_fetched_at = Instant::now() - Duration::from_millis(50);
        // The event's playback_updated_at was set to Instant::now() at
        // dispatch; sleep a tick to guarantee stale_fetched_at < it.
        std::thread::sleep(Duration::from_millis(2));
        let stale_item = track_with_image("spotify:track:stale", None);
        app.apply_async_result(AsyncResult::Seed {
            playback: Some(playback_with(stale_item, 0)),
            queue: None,
            devices: None,
            viz: None,
            recent: None,
            fetched_at: stale_fetched_at,
        });
        assert_eq!(
            app.playback.item.as_ref().map(|i| i.uri.as_str()),
            Some("spotify:track:fresh"),
            "seed older than event must be dropped"
        );
    }

    /// Seed applies when no prior event has touched the field — the
    /// startup / first-connect path.
    #[test]
    fn seed_applies_when_no_prior_event() {
        let mut app = test_app();
        let item = track_with_image("spotify:track:seeded", None);
        app.apply_async_result(AsyncResult::Seed {
            playback: Some(playback_with(item, 0)),
            queue: None,
            devices: None,
            viz: None,
            recent: None,
            fetched_at: Instant::now(),
        });
        assert_eq!(
            app.playback.item.as_ref().map(|i| i.uri.as_str()),
            Some("spotify:track:seeded"),
            "seed must apply when no event has written playback yet"
        );
        assert!(
            app.playback_updated_at.is_some(),
            "seed apply must stamp the updated_at timestamp"
        );
    }

    /// `EventStreamLagged` is the daemon's signal that we missed some
    /// events. The TUI must treat its push state as stale; the
    /// observable behavior is the toast / warn log + downstream
    /// `spawn_seed` (which we can't unit-test without a runtime, but
    /// can assert isn't a panic).
    #[test]
    fn event_stream_lagged_is_handled_without_panic() {
        let mut app = test_app();
        let (tx, _rx) = mpsc::unbounded_channel();
        app.apply_daemon_event(DaemonEvent::EventStreamLagged { skipped: 42 }, &tx);
        // No state assertions: spawn_seed runs in production but is
        // gated on a tokio runtime, which the unit-test harness lacks.
        // The contract under test is "handler accepts the variant
        // exhaustively without panic", which compiles + runs.
    }

    /// AuthError with the InvalidGrant kind must auto-open the
    /// LoginModal so the user doesn't have to remember `spotuify
    /// login`. Banner stays as a backstop.
    #[test]
    fn auth_error_invalid_grant_opens_login_modal() {
        let mut app = test_app();
        let (tx, _rx) = mpsc::unbounded_channel();
        assert!(app.login_modal.is_none());
        app.apply_daemon_event(
            DaemonEvent::AuthError {
                kind: spotuify_protocol::AuthErrorKind::InvalidGrant,
            },
            &tx,
        );
        let modal = app
            .login_modal
            .as_ref()
            .expect("modal must open on InvalidGrant");
        assert!(matches!(modal.phase, LoginPhase::AwaitingConfirm));
        // Banner kept as a backstop (in case user dismisses the modal).
        assert!(matches!(app.banner, Some(BannerState::Auth { .. })));
    }

    /// Softer auth issues (token works but is missing newer scopes)
    /// keep the banner-only treatment — the user can navigate
    /// read-only without an upfront modal.
    #[test]
    fn auth_error_scope_reauth_does_not_open_login_modal() {
        let mut app = test_app();
        let (tx, _rx) = mpsc::unbounded_channel();
        app.apply_daemon_event(
            DaemonEvent::AuthError {
                kind: spotuify_protocol::AuthErrorKind::ScopeReauthRequired,
            },
            &tx,
        );
        assert!(
            app.login_modal.is_none(),
            "ScopeReauthRequired stays banner-only"
        );
        assert!(matches!(app.banner, Some(BannerState::Auth { .. })));
    }

    #[test]
    fn playlist_tracks_not_logged_in_error_opens_login_modal() {
        let mut app = test_app();
        app.apply_async_result(AsyncResult::PlaylistTracks {
            playlist_id: "playlist".to_string(),
            playlist_name: "Playlist".to_string(),
            expected_total: 1,
            result: Err("not logged in; run `spotuify login`".to_string()),
        });

        assert!(
            app.login_modal.is_some(),
            "not-logged-in playlist loads should prompt reauth"
        );
        assert!(app.error.is_none());
    }

    #[test]
    fn forbidden_playlist_tracks_are_hidden_after_failure() {
        let mut app = test_app();
        app.playlists = vec![Playlist {
            id: "p1".to_string(),
            name: "Hidden Playlist".to_string(),
            owner: "owner".to_string(),
            tracks_total: 12,
            image_url: None,
            snapshot_id: None,
        }];

        app.apply_async_result(AsyncResult::PlaylistTracks {
            playlist_id: "p1".to_string(),
            playlist_name: "Hidden Playlist".to_string(),
            expected_total: 12,
            result: Err("Spotify API 403 on GET /playlists/p1/items: Forbidden".to_string()),
        });

        assert!(app.filtered_playlists().is_empty());
        assert!(app.error.is_none());
        assert_eq!(
            app.toast.as_deref(),
            Some("Hidden Playlist is unavailable to third-party apps")
        );
    }

    /// Cold-start grace: when an AuthError fires inside the 5s
    /// window, the modal is deferred and the auth_revoked_observed
    /// flag is set. The deferred OpenLoginModalIfStillNeeded handler
    /// then opens the modal (the flag is still set).
    #[tokio::test]
    async fn auth_error_in_cold_start_defers_then_opens_modal() {
        let mut app = test_app();
        app.started_at = Instant::now();
        let (tx, _rx) = mpsc::unbounded_channel();
        app.apply_daemon_event(
            DaemonEvent::AuthError {
                kind: spotuify_protocol::AuthErrorKind::InvalidGrant,
            },
            &tx,
        );
        assert!(
            app.login_modal.is_none(),
            "modal must not open immediately during cold-start grace window"
        );
        assert!(app.auth_revoked_observed);
        assert!(app.pending_auth_modal_until.is_some());
        // Simulate the deferred timer firing.
        app.apply_async_result(AsyncResult::OpenLoginModalIfStillNeeded);
        assert!(
            app.login_modal.is_some(),
            "deferred handler opens modal when auth_revoked still observed"
        );
    }

    #[tokio::test]
    async fn not_logged_in_auth_error_auto_opens_login_modal() {
        let mut app = test_app();
        app.started_at = Instant::now() - Duration::from_secs(10);
        let (tx, _rx) = mpsc::unbounded_channel();
        app.apply_daemon_event(
            DaemonEvent::AuthError {
                kind: spotuify_protocol::AuthErrorKind::NotLoggedIn,
            },
            &tx,
        );

        assert!(app.login_modal.is_some());
        assert!(matches!(app.banner, Some(BannerState::Auth { .. })));
    }

    /// Cold-start grace + success self-heal: a successful
    /// PlaybackChanged between the AuthError and the deferred
    /// timer fires clears the `auth_revoked_observed` flag, so the
    /// deferred handler skips opening the modal.
    #[tokio::test]
    async fn auth_success_during_grace_cancels_deferred_modal() {
        let mut app = test_app();
        app.started_at = Instant::now();
        let (tx, _rx) = mpsc::unbounded_channel();
        app.apply_daemon_event(
            DaemonEvent::AuthError {
                kind: spotuify_protocol::AuthErrorKind::InvalidGrant,
            },
            &tx,
        );
        assert!(app.auth_revoked_observed);
        app.apply_daemon_event(
            DaemonEvent::PlaybackChanged {
                action: "synced".to_string(),
                playback: Some(spotuify_core::Playback::default()),
            },
            &tx,
        );
        assert!(
            !app.auth_revoked_observed,
            "playback success clears the flag"
        );
        // Now the deferred wake-up fires; modal stays closed.
        app.apply_async_result(AsyncResult::OpenLoginModalIfStillNeeded);
        assert!(
            app.login_modal.is_none(),
            "auth self-heal during grace must cancel the deferred modal"
        );
    }

    /// AuthError outside the cold-start window opens the modal
    /// immediately. (Tested without a tokio runtime — the
    /// `can_defer` guard falls through to the immediate path.)
    #[test]
    fn auth_error_after_cold_start_opens_modal_immediately() {
        let mut app = test_app();
        // Pretend the TUI has been running for 10s — past the 5s
        // cold-start window.
        app.started_at = Instant::now() - Duration::from_secs(10);
        let (tx, _rx) = mpsc::unbounded_channel();
        app.apply_daemon_event(
            DaemonEvent::AuthError {
                kind: spotuify_protocol::AuthErrorKind::InvalidGrant,
            },
            &tx,
        );
        assert!(
            app.login_modal.is_some(),
            "post-grace AuthError opens modal immediately"
        );
    }

    /// LoginProgress events update `last_progress` on the modal so
    /// `render_login_modal` can paint the URL / status inside the
    /// frame instead of bleeding to stdout.
    #[test]
    fn login_progress_updates_modal_last_progress() {
        use spotuify_spotify::auth::LoginProgress;
        let mut app = test_app();
        app.login_modal = Some(LoginModal {
            phase: LoginPhase::InProgress,
            last_progress: None,
        });
        app.apply_async_result(AsyncResult::LoginProgress(
            LoginProgress::BrowserLaunchFailed {
                auth_url: "https://example/auth".to_string(),
                redirect_uri: "http://127.0.0.1:8888/callback".to_string(),
                error: "no DISPLAY".to_string(),
            },
        ));
        let modal = app.login_modal.as_ref().expect("modal");
        assert!(matches!(
            modal.last_progress.as_ref(),
            Some(LoginProgress::BrowserLaunchFailed { .. })
        ));
    }

    /// Successful re-login: modal closes, banner clears, toast
    /// confirms. (`spawn_reload_auth` no-ops when there's no runtime,
    /// so we don't observe the daemon round-trip in tests.)
    #[test]
    fn login_completed_ok_closes_modal_and_clears_banner() {
        let mut app = test_app();
        app.login_modal = Some(LoginModal {
            phase: LoginPhase::InProgress,
            last_progress: None,
        });
        app.banner = Some(BannerState::Auth {
            kind: spotuify_protocol::AuthErrorKind::InvalidGrant,
        });
        app.apply_async_result(AsyncResult::LoginCompleted { result: Ok(()) });
        assert!(app.login_modal.is_none());
        assert!(app.banner.is_none());
        assert_eq!(app.toast.as_deref(), Some("Logged in to Spotify"));
    }

    /// Failed re-login: modal transitions to `Failed(msg)` so the user
    /// can see the error and retry without losing the prompt.
    #[test]
    fn login_completed_err_transitions_modal_to_failed() {
        let mut app = test_app();
        app.login_modal = Some(LoginModal {
            phase: LoginPhase::InProgress,
            last_progress: None,
        });
        app.apply_async_result(AsyncResult::LoginCompleted {
            result: Err("user closed the browser".to_string()),
        });
        let modal = app
            .login_modal
            .as_ref()
            .expect("modal must persist on failure");
        assert!(
            matches!(&modal.phase, LoginPhase::Failed(msg) if msg.contains("closed the browser")),
            "expected Failed phase, got {:?}",
            modal.phase
        );
    }

    /// Failed login arriving after the user already dismissed the
    /// modal must not silently lose the error — surface it via toast.
    #[test]
    fn login_completed_err_with_no_modal_surfaces_via_toast() {
        let mut app = test_app();
        app.login_modal = None;
        app.apply_async_result(AsyncResult::LoginCompleted {
            result: Err("network down".to_string()),
        });
        assert!(app
            .toast
            .as_deref()
            .is_some_and(|t| t.contains("network down")));
    }

    #[tokio::test]
    async fn tui_does_not_mutate_playback_before_daemon_event() {
        let mut app = test_app();
        app.merge_playback(playback_with(
            track_with_image("spotify:track:a", None),
            5_000,
        ));
        assert!(app.playback.is_playing, "fixture should start playing");
        let before = app.playback.clone();
        let (tx, _rx) = mpsc::unbounded_channel();
        command_then_refresh_transport(
            &mut app,
            &tx,
            spotuify_spotify::actions::CommandKind::Pause,
        );
        assert_eq!(app.playback.is_playing, before.is_playing);
        assert_eq!(app.playback.progress_ms, before.progress_ms);
    }

    #[test]
    fn daemon_playback_event_updates_volume() {
        let mut app = test_app();
        app.merge_playback(playback_with(track_with_image("spotify:track:a", None), 0));
        app.playback.device = Some(Device {
            id: Some("dev-1".to_string()),
            name: "Speakers".to_string(),
            kind: "computer".to_string(),
            is_active: true,
            is_restricted: false,
            volume_percent: Some(50),
            supports_volume: true,
        });
        app.devices = vec![Device {
            id: Some("dev-1".to_string()),
            name: "Speakers".to_string(),
            kind: "computer".to_string(),
            is_active: true,
            is_restricted: false,
            volume_percent: Some(50),
            supports_volume: true,
        }];
        let mut incoming = app.playback.clone();
        incoming
            .device
            .as_mut()
            .expect("incoming playback should have a device")
            .volume_percent = Some(80);
        app.merge_playback(incoming);
        assert_eq!(
            app.playback.device.as_ref().and_then(|d| d.volume_percent),
            Some(80),
            "daemon PlaybackChanged should update playback.device"
        );
    }
}
