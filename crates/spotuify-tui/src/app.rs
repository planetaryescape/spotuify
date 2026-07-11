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
use crate::widgets::style::UiPalette;
use spotuify_cli::actions::{CommandKind, CommandResult};
use spotuify_core::{Notification, Recurrence, Reminder, SyncedLyrics};
use spotuify_protocol::ipc_client::IpcClient;
use spotuify_protocol::{
    CacheStatus, DaemonEvent, DoctorReport, ListenSession, NotificationAction, PlaybackCommand,
    Request, Response, ResponseData, SearchScopeData, SearchSortData,
};
use spotuify_spotify::client::{Device, MediaItem, MediaKind, Playback, Playlist, Queue};
use spotuify_spotify::config::Config;

const TUI_PLAYLIST_TIMEOUT: Duration = Duration::from_secs(30);
const TUI_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const SYSTEM_AUDIO_OUTPUT_LABEL: &str = "System Default";
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
    History,
    Devices,
    Diagnostics,
    Lyrics,
    Notifications,
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
    pub const ALL: [Self; 10] = [
        Self::Player,
        Self::Search,
        Self::Library,
        Self::Playlists,
        Self::Queue,
        Self::History,
        Self::Devices,
        Self::Diagnostics,
        Self::Lyrics,
        Self::Notifications,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Player => "Home",
            Self::Search => "Search",
            Self::Library => "Library",
            Self::Playlists => "Playlists",
            Self::Queue => "Queue",
            Self::History => "History",
            Self::Devices => "Devices",
            Self::Diagnostics => "Diagnostics",
            Self::Lyrics => "Lyrics",
            Self::Notifications => "Notifications",
        }
    }

    /// Abbreviated tab label for narrow terminals, where the full set of
    /// ten tabs doesn't fit on one row.
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Player => "Home",
            Self::Search => "Srch",
            Self::Library => "Lib",
            Self::Playlists => "Lists",
            Self::Queue => "Queue",
            Self::History => "Hist",
            Self::Devices => "Dev",
            Self::Diagnostics => "Diag",
            Self::Lyrics => "Lyr",
            Self::Notifications => "Notif",
        }
    }

    /// The number key that jumps to this screen (History is `0`, since 1–9 are
    /// taken). Used by the tab bar so the chip matches the real keybinding
    /// rather than the screen's position in `ALL`.
    pub fn key_label(self) -> &'static str {
        match self {
            Self::Player => "1",
            Self::Search => "2",
            Self::Library => "3",
            Self::Playlists => "4",
            Self::Queue => "5",
            Self::Devices => "6",
            Self::Diagnostics => "7",
            Self::Lyrics => "8",
            Self::Notifications => "9",
            Self::History => "0",
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
            // History is a track list; reuse the Library hint set (play / queue
            // / like / go-to all apply).
            Self::History => ActionContext::Library,
            Self::Devices => ActionContext::Devices,
            Self::Diagnostics => ActionContext::Diagnostics,
            Self::Lyrics => ActionContext::Lyrics,
            Self::Notifications => ActionContext::Notifications,
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
    /// A newer spotuify binary was installed while this TUI was open, so
    /// the running daemon is now stale. Driven by the `update_available`
    /// flag (not stored here) and surfaced as a banner with a restart key.
    UpdateAvailable,
    /// A newer spotuify *release* exists on GitHub (from the daemon's
    /// `UpdateAvailable` event). Distinct from the binary-changed signal
    /// above: this tells the user a newer version is downloadable and how.
    UpgradeAvailable {
        latest_version: String,
        /// Pre-rendered action, e.g. "run: brew upgrade …" or "download: <url>".
        action: String,
    },
    /// The daemon resolved to first-party-only Spotify auth, which Spotify
    /// rate-limits heavily. Dismissible advisory (not a modal): the user
    /// migrates off it with `spotuify login --dev-app` (or `spotuify
    /// onboard` when no BYO client_id is configured — `can_login_dev_app`
    /// is `false`).
    AuthMigration {
        can_login_dev_app: bool,
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

/// Modal listing the local audio output devices the embedded player can
/// render to (the Mac speakers/headphones). Opened with `O`; Enter sets
/// `player.audio_output_device` and asks the daemon to rebind its sink
/// live (playback resumes; no daemon restart). Carries its own snapshot
/// of the device list since it isn't part of normal app state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioOutputPickerModal {
    pub outputs: Vec<String>,
    pub selected: usize,
}

/// Quick-pick reminder scheduling modal opened by `R` on a selected item(s).
/// A preset (or a typed custom offset) chooses the time; `Tab` cycles the
/// recurrence; Enter schedules a `ReminderCreate` for every target URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderPickerModal {
    pub uris: Vec<String>,
    pub label: String,
    pub preset: usize,
    pub recurrence: Recurrence,
    /// Offset text (e.g. `+3d`, `+2w`) used when the Custom preset is selected.
    pub custom: String,
}

/// Preset labels in display order. The last entry is the custom-offset entry.
pub const REMINDER_PRESETS: [&str; 6] = [
    "In 1 hour",
    "This evening (7pm)",
    "Tomorrow 9am",
    "This weekend (Sat 10am)",
    "Next week (Mon 9am)",
    "Custom offset…",
];

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
    pub recent_items: Vec<MediaItem>,
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
    /// Inbox of fired reminder notifications (newest first). Populated from
    /// `ReminderDue` events + a `notifications-list` fetch on connect.
    pub notifications: Vec<Notification>,
    /// Scheduled reminders, shown on the Notifications screen below the inbox.
    pub reminders: Vec<Reminder>,
    /// Listening history sessions (newest first), shown on the History screen.
    pub history_sessions: Vec<ListenSession>,
    pub history_loading: bool,
    pub history_error: Option<String>,
    /// Search-results view refinement (client-side, like the macOS list sort):
    /// ordering + an optional single-kind filter applied in `visible_items`.
    pub search_sort: SearchSortData,
    pub search_kind_filter: Option<MediaKind>,
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
    pub palette: UiPalette,
    pub selected_art_url: Option<String>,
    pub selected_art_cover: Option<StatefulProtocol>,
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
    /// within ~5s of launch, defer the modal so startup auth recovery
    /// can settle without a second modal stacking on top.
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
    pub audio_output_picker: Option<AudioOutputPickerModal>,
    pub reminder_picker: Option<ReminderPickerModal>,
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
    /// (inode, mtime) of our own binary captured at launch. Compared on
    /// the heartbeat to detect an in-place upgrade (`brew upgrade`/`cargo
    /// install`) so the live TUI can offer to restart the now-stale
    /// daemon. `None` when the binary couldn't be stat'd at startup.
    pub binary_fingerprint: Option<BinaryFingerprint>,
    /// Set once the binary on disk changes from `binary_fingerprint`.
    /// Drives the `UpdateAvailable` banner + the `R` restart key.
    pub update_available: bool,
    /// When Enter is pressed on an artist, we open a two-column view:
    /// albums on the left, tracks of the focused album on the right.
    pub artist_view: Option<ArtistViewState>,
    pub(crate) refresh_requested: bool,
    pub(crate) pending_g: bool,
    /// Per-frame click targets, recorded by renderers. RefCell because
    /// most renderers take `&App`; rendering and mouse handling are
    /// both on the event-loop thread, so borrows never overlap.
    pub(crate) hit_map: std::cell::RefCell<crate::hit::HitMap>,
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
    /// When set, only show albums already in the library (saved). Toggled
    /// with `L`; a pure client-side filter over the daemon-tagged list.
    pub library_only: bool,
    /// Whether the user follows this artist. Seeded from the opening item's
    /// `in_library` flag (None = unknown); flipped optimistically on F.
    pub is_followed: Option<bool>,
    /// When the view is opened by navigating from a track to its album, the
    /// album to auto-select once the discography loads (else the first album).
    pub pending_album_uri: Option<String>,
}

impl ArtistViewState {
    /// Albums to display: filtered by the library toggle and ordered into
    /// Spotify's discography sections (albums → singles → compilations →
    /// appears-on). `album_selected` indexes into this list.
    pub fn visible_albums(&self) -> Vec<&MediaItem> {
        let mut out: Vec<&MediaItem> = self
            .albums
            .iter()
            .filter(|album| !self.library_only || album.in_library == Some(true))
            .collect();
        // Stable sort preserves Spotify's within-group newest-first ordering.
        out.sort_by_key(|album| album_group_rank(album.album_group.as_deref()));
        out
    }

    /// Count of visible albums that are in the library (for the mode badge).
    pub fn in_library_count(&self) -> usize {
        self.albums
            .iter()
            .filter(|album| album.in_library == Some(true))
            .count()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtistViewSide {
    Albums,
    Tracks,
}

/// Discography section order, keyed by Spotify's `album_group`.
pub const ARTIST_ALBUM_GROUPS: &[(&str, &str)] = &[
    ("album", "Albums"),
    ("single", "Singles & EPs"),
    ("compilation", "Compilations"),
    ("appears_on", "Appears On"),
];

/// Sort rank for an `album_group`; unknown/None groups sink to the bottom.
pub fn album_group_rank(group: Option<&str>) -> usize {
    group
        .and_then(|g| ARTIST_ALBUM_GROUPS.iter().position(|(key, _)| *key == g))
        .unwrap_or(ARTIST_ALBUM_GROUPS.len())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ArtworkSubject {
    pub uri: String,
    pub title: String,
    pub subtitle: String,
    pub detail: String,
    pub image_url: Option<String>,
    pub label: String,
}

impl ArtworkSubject {
    fn from_playlist(playlist: &Playlist) -> Self {
        Self {
            uri: format!("spotify:playlist:{}", playlist.id),
            title: playlist.name.clone(),
            subtitle: playlist.owner.clone(),
            detail: format!("{} tracks", playlist.tracks_total),
            image_url: playlist.image_url.clone(),
            label: artwork_label(&playlist.name, "≣"),
        }
    }

    fn from_media_item(item: &MediaItem) -> Self {
        let detail = match item.kind {
            MediaKind::Album => {
                if item.context.is_empty() {
                    "album".to_string()
                } else {
                    format!("album · {}", item.context)
                }
            }
            MediaKind::Playlist => {
                if item.context.is_empty() {
                    "playlist".to_string()
                } else {
                    format!("playlist · {}", item.context)
                }
            }
            _ => item.context.clone(),
        };
        Self {
            uri: item.uri.clone(),
            title: item.name.clone(),
            subtitle: item.subtitle.clone(),
            detail,
            image_url: item.image_url.clone(),
            label: artwork_label(&item.name, kind_icon_fallback(&item.kind)),
        }
    }
}

fn artwork_label(name: &str, fallback: &str) -> String {
    name.chars().next().map_or_else(
        || fallback.to_string(),
        |c| c.to_ascii_uppercase().to_string(),
    )
}

fn kind_icon_fallback(kind: &MediaKind) -> &'static str {
    match kind {
        MediaKind::Album => "◉",
        MediaKind::Playlist => "≣",
        MediaKind::Artist => "A",
        MediaKind::Show => "S",
        MediaKind::Episode => "E",
        MediaKind::Track => "♪",
    }
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
    /// Listen-history sessions for the History screen.
    ListenHistory {
        result: std::result::Result<Vec<ListenSession>, String>,
    },
    /// Cover-art fetch result. URL is the version — `apply_async_result`
    /// accepts iff `self.current_art_url == Some(url)`. Stale fetches
    /// (track advanced before our fetch completed) self-discard.
    CoverFetched {
        url: String,
        image: image::DynamicImage,
    },
    SelectedArtFetched {
        url: String,
        image: image::DynamicImage,
    },
    /// Audio-output list for the Shift+O picker, enumerated off the
    /// event loop (CoreAudio device listing + config read can block).
    AudioOutputs {
        outputs: Vec<String>,
        current: Option<String>,
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
    /// Result of fetching reminder schedules + inbox notifications for the
    /// Notifications screen (on screen-open, connect, and `RemindersChanged`).
    RemindersLoaded {
        reminders: Vec<Reminder>,
        notifications: Vec<Notification>,
    },
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
        let viz_color_scheme = loaded_config.as_ref().map_or_else(
            || "spotify-green".to_string(),
            |c| c.viz.color_scheme.clone(),
        );
        let viz_enabled_default = loaded_config.as_ref().is_none_or(|c| c.viz.enabled);

        Ok(Self {
            playback: Playback::default(),
            queue: Queue::default(),
            devices: Vec::new(),
            playlists: Vec::new(),
            inaccessible_playlist_ids: HashSet::new(),
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
            notifications: Vec::new(),
            reminders: Vec::new(),
            history_sessions: Vec::new(),
            history_loading: false,
            history_error: None,
            search_sort: SearchSortData::Relevance,
            search_kind_filter: None,
            error: None,
            last_progress_tick: Instant::now(),
            awaiting_track_change_until: None,
            current_art_url: None,
            cover: None,
            palette: UiPalette::default(),
            selected_art_url: None,
            selected_art_cover: None,
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
            audio_output_picker: None,
            reminder_picker: None,
            login_modal: None,
            operations: Vec::new(),
            operations_cursor: 0,
            pending_receipts: Vec::new(),
            banner: None,
            binary_fingerprint: current_binary_fingerprint(),
            update_available: false,
            artist_view: None,
            refresh_requested: false,
            pending_g: false,
            hit_map: std::cell::RefCell::new(crate::hit::HitMap::default()),
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
        let items: Vec<MediaItem> = match self.screen {
            Screen::Player => self.home_items(),
            Screen::Queue if self.queue.session_active => self.queue.items.clone(),
            Screen::Queue => Vec::new(),
            // History flattens its sessions (newest first) into a track list so
            // the standard selection / play / queue / go-to actions just work.
            Screen::History => self
                .history_sessions
                .iter()
                .flat_map(|session| session.tracks.iter().cloned())
                .collect(),
            Screen::Search => {
                // Client-side view refinement (the macOS search uses the
                // daemon's Request::Search sort/kinds; the TUI streams into
                // per-kind panes, so we refine the streamed set for display).
                let mut results = self.search_results.clone();
                if let Some(kind) = self.search_kind_filter.as_ref() {
                    results.retain(|item| &item.kind == kind);
                }
                match self.search_sort {
                    SearchSortData::Relevance => {}
                    SearchSortData::Name => results.sort_by_key(|item| item.name.to_lowercase()),
                    SearchSortData::Duration => results.sort_by_key(|item| item.duration_ms),
                    SearchSortData::Artist => {
                        results.sort_by_key(|item| item.subtitle.to_lowercase())
                    }
                    SearchSortData::Date => {
                        results.sort_by(|a, b| b.release_date.cmp(&a.release_date))
                    }
                }
                results
            }
            Screen::Library => self.library_items.clone(),
            Screen::Playlists if self.selected_playlist_id.is_some() => {
                self.playlist_tracks.clone()
            }
            _ => Vec::new(),
        };
        items
            .into_iter()
            .filter(|item| matches_filter(&self.list_filter_query, media_item_filter_text(item)))
            .collect()
    }

    /// The item under the cursor. `selected` indexes the FILTERED /
    /// SORTED visible list — never index the raw backing arrays with
    /// it (with a filter or sort active the raw index lands on a
    /// different item entirely).
    pub(crate) fn selected_visible_item(&self) -> Option<MediaItem> {
        self.visible_items().into_iter().nth(self.selected)
    }

    /// Cycle the search-results sort (client-side display order). Resets the
    /// cursor so the user lands at the top of the re-ordered list.
    pub(crate) fn cycle_search_sort(&mut self) {
        self.search_sort = match self.search_sort {
            SearchSortData::Relevance => SearchSortData::Name,
            SearchSortData::Name => SearchSortData::Duration,
            SearchSortData::Duration => SearchSortData::Artist,
            SearchSortData::Artist => SearchSortData::Date,
            SearchSortData::Date => SearchSortData::Relevance,
        };
        self.selected = 0;
        self.toast = Some(format!("Sort: {}", search_sort_label(self.search_sort)));
    }

    /// Cycle the search type filter through All → each kind → All.
    pub(crate) fn cycle_search_kind_filter(&mut self) {
        self.search_kind_filter = match &self.search_kind_filter {
            None => Some(MediaKind::Track),
            Some(MediaKind::Track) => Some(MediaKind::Artist),
            Some(MediaKind::Artist) => Some(MediaKind::Album),
            Some(MediaKind::Album) => Some(MediaKind::Playlist),
            Some(MediaKind::Playlist) => Some(MediaKind::Show),
            Some(MediaKind::Show) => Some(MediaKind::Episode),
            _ => None,
        };
        self.selected = 0;
        let label = self
            .search_kind_filter
            .as_ref()
            .map_or("All", |kind| kind.label());
        self.toast = Some(format!("Filter: {label}"));
    }

    pub(crate) fn home_items(&self) -> Vec<MediaItem> {
        let mut items = playable_home_items(&self.library_items);
        if items.is_empty() {
            items = playable_home_items(&self.recent_items);
        }
        if items.is_empty() && self.queue.session_active {
            items = self.queue.items.clone();
        }
        dedupe_media_items(items)
    }

    fn selected_item(&self) -> Option<MediaItem> {
        self.visible_items().get(self.selected).cloned()
    }

    fn selected_playlist(&self) -> Option<Playlist> {
        self.filtered_playlists()
            .get(self.playlist_selected)
            .cloned()
    }

    pub(crate) fn selected_artwork_subject(&self) -> Option<ArtworkSubject> {
        match self.screen {
            Screen::Playlists if self.selected_playlist_id.is_none() => self
                .selected_playlist()
                .map(|playlist| ArtworkSubject::from_playlist(&playlist)),
            Screen::Library | Screen::Search => self.selected_item().and_then(|item| {
                matches!(
                    item.kind,
                    MediaKind::Album | MediaKind::Playlist | MediaKind::Show | MediaKind::Episode
                )
                .then(|| ArtworkSubject::from_media_item(&item))
            }),
            _ => None,
        }
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
        match action {
            TuiAction::QueueSelection => self
                .selected_queue_target_uris()
                .into_iter()
                .map(|uri| Request::QueueAdd { uri })
                .collect(),
            TuiAction::LikeSelection => self
                .selected_target_uris()
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
                let uris = self.selected_target_uris();
                if uris.is_empty() {
                    Vec::new()
                } else {
                    vec![Request::PlaylistAddItems { playlist, uris }]
                }
            }
            TuiAction::DeleteSelectedPlaylist => self
                .selected_playlist_target()
                .map(|(playlist, _)| vec![Request::PlaylistUnfollow { playlist }])
                .unwrap_or_default(),
            TuiAction::UnsaveSelection => self
                .selected_target_uris()
                .into_iter()
                .map(|uri| Request::LibraryUnsave { uri })
                .collect(),
            TuiAction::RemindMe => {
                // TUI quick-schedule: remind about the selection in 1 day. Rich
                // scheduling (presets/recurrence/custom date) lives in the macOS
                // picker and the CLI `spotuify reminder create … --at`.
                let anchor_at_ms = chrono::Local::now().timestamp_millis() + 86_400_000;
                let tz = "UTC".to_string();
                self.selected_target_uris()
                    .into_iter()
                    .map(|uri| Request::ReminderCreate {
                        media_uri: uri,
                        anchor_at_ms,
                        recurrence: spotuify_core::Recurrence::None,
                        tz: tz.clone(),
                        message: None,
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    fn selected_queue_target_uris(&self) -> Vec<String> {
        if self.screen == Screen::Playlists && self.selected_playlist_id.is_none() {
            return self
                .selected_playlist()
                .map(|playlist| vec![format!("spotify:playlist:{}", playlist.id)])
                .unwrap_or_default();
        }
        self.selected_target_uris()
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
            Screen::Search | Screen::Library | Screen::Queue | Screen::History => {
                self.visible_items().len()
            }
            Screen::Playlists if self.selected_playlist_id.is_some() => self.visible_items().len(),
            Screen::Playlists => self.filtered_playlists().len(),
            Screen::Devices => self.filtered_devices().len(),
            Screen::Notifications => self.notifications.len() + self.reminders.len(),
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
            self.recent_items = recent.clone();
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
                // The user changed tracks while this fetch was in flight.
                // Drop the stale result, but clear the loading flag and ask
                // for another refresh so the now-active track's lyrics get
                // fetched — otherwise `refresh_plan` (gated on
                // `!lyrics_loading`) never re-fetches and the spinner sticks
                // on "Fetching…" forever.
                self.lyrics_loading = false;
                self.refresh_requested = true;
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
        let should_sync_selected_art = !matches!(
            &result,
            AsyncResult::CoverFetched { .. }
                | AsyncResult::SelectedArtFetched { .. }
                | AsyncResult::LoginProgress(_)
                | AsyncResult::OpenLoginModalIfStillNeeded
        );
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
                                "Tracks for {playlist_name} are restricted by Spotify for third-party apps"
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
                            let new_art_url =
                                playback.item.as_ref().and_then(|i| i.image_url.clone());
                            self.merge_playback(playback);
                            self.playback_updated_at = Some(Instant::now());
                            self.handle_art_url_change(new_art_url, async_tx);
                            self.request_lyrics_if_visible();
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
                            view.error = None;
                            // If we arrived here by navigating to a specific
                            // album, select it; otherwise the first album.
                            let target = view.pending_album_uri.take();
                            let (idx, uri) = {
                                let visible = view.visible_albums();
                                let idx = target
                                    .as_deref()
                                    .and_then(|t| visible.iter().position(|a| a.uri == t))
                                    .unwrap_or(0);
                                (idx, visible.get(idx).map(|a| a.uri.clone()))
                            };
                            view.album_selected = idx;
                            auto_load = uri;
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
                    let expected_uri = view
                        .visible_albums()
                        .get(view.album_selected)
                        .map(|a| a.uri.clone());
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
            AsyncResult::ListenHistory { result } => {
                self.history_loading = false;
                match result {
                    Ok(sessions) => {
                        self.history_sessions = sessions;
                        self.history_error = None;
                        if self.screen == Screen::History {
                            self.selected = 0;
                            self.clamp_selection();
                        }
                    }
                    Err(err) => self.history_error = Some(err),
                }
            }
            AsyncResult::DaemonEvent(event) => self.apply_daemon_event(event, async_tx),
            AsyncResult::CoverFetched { url, image } => {
                if self.current_art_url.as_deref() == Some(url.as_str()) {
                    self.palette = if terminal_color_enabled() {
                        UiPalette::from_cover(&image).unwrap_or_default()
                    } else {
                        UiPalette::default()
                    };
                    self.cover = Some(self.picker.new_resize_protocol(image));
                } else {
                    tracing::debug!(
                        target: "spotuify_tui::merge",
                        stale_url = %url,
                        "tui_cover_stale_dropped"
                    );
                }
            }
            AsyncResult::SelectedArtFetched { url, image } => {
                if self.selected_art_url.as_deref() == Some(url.as_str()) {
                    self.selected_art_cover = Some(self.picker.new_resize_protocol(image));
                } else {
                    tracing::debug!(
                        target: "spotuify_tui::merge",
                        stale_url = %url,
                        "tui_selected_art_stale_dropped"
                    );
                }
            }
            AsyncResult::AudioOutputs {
                mut outputs,
                current,
            } => {
                outputs.insert(0, SYSTEM_AUDIO_OUTPUT_LABEL.to_string());
                let selected = current
                    .as_deref()
                    .and_then(|name| outputs.iter().position(|o| o == name))
                    .unwrap_or(0);
                self.audio_output_picker = Some(AudioOutputPickerModal { outputs, selected });
            }
            AsyncResult::Seed {
                playback,
                queue,
                devices,
                viz,
                recent,
                fetched_at,
            } => {
                self.apply_seed(playback, queue, devices, viz, recent, fetched_at, async_tx);
                // Bootstrap the reminders inbox so the Notifications badge/screen
                // is populated as soon as the client connects.
                spawn_load_reminders(async_tx);
            }
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
            AsyncResult::RemindersLoaded {
                reminders,
                notifications,
            } => {
                self.reminders = reminders;
                self.notifications = notifications;
                if self.screen == Screen::Notifications {
                    self.clamp_selection();
                }
            }
        }
        if should_sync_selected_art {
            self.sync_selected_artwork(async_tx);
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
            self.recent_items = items.clone();
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
        self.palette = UiPalette::default();
        if let Some(url) = new_url {
            spawn_cover_fetch(url, async_tx.clone());
        }
    }

    fn sync_selected_artwork(&mut self, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
        let new_url = self
            .selected_artwork_subject()
            .and_then(|subject| subject.image_url);
        if new_url.as_deref() == self.selected_art_url.as_deref() {
            return;
        }
        self.selected_art_url = new_url.clone();
        self.selected_art_cover = None;
        if let Some(url) = new_url {
            spawn_selected_art_fetch(url, async_tx.clone());
        }
    }

    fn apply_daemon_event(
        &mut self,
        event: DaemonEvent,
        async_tx: &mpsc::UnboundedSender<AsyncResult>,
    ) {
        match event {
            // Forward-compat: a newer daemon emitted an event this build
            // doesn't know. Ignoring it is the whole point.
            DaemonEvent::Unknown => {}
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
                    // Arm the stale-poll guard on an optimistic track
                    // change: until Spotify actually transitions, web
                    // polls still report the OLD track and would snap
                    // the display back. (The guard's setter was lost in
                    // the daemon-optimistic refactor — `merge_playback`
                    // checked a deadline nothing ever armed.)
                    let incoming_uri = pb.item.as_ref().map(|i| i.uri.as_str());
                    let current_uri = self.playback.item.as_ref().map(|i| i.uri.as_str());
                    if action.starts_with("optimistic-")
                        && incoming_uri.is_some()
                        && incoming_uri != current_uri
                    {
                        self.awaiting_track_change_until =
                            Some(Instant::now() + Duration::from_secs(3));
                    }
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
                let selected_uri = self.selected_visible_item().map(|i| i.uri);

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
                let visible = self.visible_items();
                if !self.search_user_steered {
                    if let Some(idx) = preferred_search_index(&visible) {
                        self.selected = idx;
                    }
                } else if let Some(uri) = selected_uri {
                    if let Some(idx) = visible.iter().position(|i| i.uri == uri) {
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
                        // ~3.5s. If the daemon self-heals
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
            DaemonEvent::AuthMigrationRecommended { can_login_dev_app } => {
                // Banner-only, dismissible (mirrors the softer
                // ScopeReauthRequired handling — the user is logged in and
                // can keep working; this just nudges them off the
                // rate-limited first-party path). Never a modal.
                self.banner = Some(BannerState::AuthMigration { can_login_dev_app });
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
            DaemonEvent::AnalyticsImportProgress { phase, .. } => {
                if matches!(phase.as_str(), "completed" | "failed") {
                    self.request_refresh();
                }
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
            DaemonEvent::ReminderDue { notification } => {
                self.toast = Some(format!("⏰ Reminder: {}", notification.name));
                self.notifications.insert(0, notification);
                // Pull the authoritative inbox + schedules (a recurring reminder
                // just advanced its next-due).
                spawn_load_reminders(async_tx);
            }
            DaemonEvent::RemindersChanged { .. } => {
                spawn_load_reminders(async_tx);
            }
            DaemonEvent::UpdateAvailable {
                latest_version,
                release_url,
                upgrade,
            } => {
                // Prefer the upgrade command (terminal-friendly); fall back to a
                // download URL for DMG/manual installs.
                let action = upgrade
                    .command
                    .clone()
                    .map(|cmd| format!("run: {cmd}"))
                    .or_else(|| {
                        upgrade
                            .url
                            .clone()
                            .or(release_url)
                            .map(|url| format!("download: {url}"))
                    })
                    .unwrap_or_else(|| "see the releases page".to_string());
                self.banner = Some(BannerState::UpgradeAvailable {
                    latest_version,
                    action,
                });
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
    // Self-heal the daemon's per-client focus vote: a previous TUI that
    // exited while unfocused leaves a stale "tui = unfocused" vote that
    // would keep the viz throttled. We're clearly focused at launch.
    spawn_viz_focus(true);
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
            _ = heartbeat.tick() => {
                // Detect an in-place upgrade (brew/cargo) so we can offer
                // to restart the now-stale daemon without the user having
                // to quit and relaunch.
                if !app.update_available
                    && binary_changed(app.binary_fingerprint, current_binary_fingerprint())
                {
                    app.update_available = true;
                }
                app.request_refresh();
            }
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
        // `connect_with_source(Tui)` so the daemon files this vote in
        // the TUI's focus bucket — focus throttling is per-client now,
        // and a source-less connect would land in the shared "unknown"
        // bucket alongside the macOS app.
        if let Ok(mut client) =
            IpcClient::connect_with_source(spotuify_protocol::OperationSource::Tui).await
        {
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
    let library_visible = matches!(app.screen, Screen::Library | Screen::Playlists);
    RefreshPlan {
        library: library_visible
            && app
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
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
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

fn request_force_media(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    request_force_cover(app, async_tx);
    request_force_lyrics(app, async_tx);
}

fn request_force_cover(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    let Some(url) = app
        .playback
        .item
        .as_ref()
        .and_then(|item| item.image_url.clone())
    else {
        return;
    };
    app.current_art_url = Some(url.clone());
    app.cover = None;
    app.palette = UiPalette::default();
    spawn_cover_fetch(url, async_tx.clone());
}

/// Fire a single cover-art fetch for `url`, decode the response, and
/// post the result as `AsyncResult::CoverFetched`. The URL itself is
/// the version: when the result arrives, `apply_async_result_with`
/// accepts the image iff `app.current_art_url == Some(url)`. Stale
/// fetches (track advanced past the URL while we were in flight) drop
/// silently on arrival.
///
/// Failures are logged and swallowed — cover just stays cleared.
fn terminal_color_enabled() -> bool {
    std::env::var_os("NO_COLOR").is_none()
}

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

fn spawn_selected_art_fetch(url: String, async_tx: mpsc::UnboundedSender<AsyncResult>) {
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
                _ => anyhow::bail!("unexpected selected artwork response"),
            }) {
            Ok(image) => {
                let _ = async_tx.send(AsyncResult::SelectedArtFetched { url, image });
            }
            Err(err) => {
                tracing::warn!(error = %err, url = %url, "selected artwork fetch failed");
            }
        }
    });
}

/// One-shot seed of push-driven state (playback / queue / devices).
/// Issued on TUI startup, after a daemon event-stream reconnect, and
/// when the broadcast subscription returns `RecvError::Lagged`.
///
/// Uses the cached-only `ClientSeed` request: startup seeding must not
/// trigger Spotify reads. The daemon's warm/sync loops own live refreshes
/// so opening the TUI doesn't spend provider budget before playback.
fn spawn_seed(async_tx: mpsc::UnboundedSender<AsyncResult>) {
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    tokio::spawn(async move {
        let fetched_at = Instant::now();
        let seed = match request_data_without_daemon_start(Request::ClientSeed).await {
            Ok(ResponseData::ClientSeed {
                playback,
                queue,
                devices,
                recent,
                viz,
            }) => (
                Some(playback),
                Some(queue),
                Some(devices),
                Some(recent),
                Some(viz),
            ),
            Ok(other) => {
                tracing::debug!(?other, "seed: unexpected ClientSeed response");
                (None, None, None, None, None)
            }
            Err(err) => {
                if let Some(kind) = auth_error_kind_from_error(&err.to_string()) {
                    let _ =
                        async_tx.send(AsyncResult::DaemonEvent(DaemonEvent::AuthError { kind }));
                }
                tracing::debug!(error = %err, "seed: ClientSeed failed");
                (None, None, None, None, None)
            }
        };
        let _ = async_tx.send(AsyncResult::Seed {
            playback: seed.0,
            queue: seed.1,
            devices: seed.2,
            recent: seed.3,
            viz: seed.4,
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

/// Platform fingerprint of our own executable, or `None` if it can't
/// be stat'd. Used to detect an in-place upgrade while the TUI is open.
#[cfg(unix)]
type BinaryFingerprint = (u64, i64);

#[cfg(not(unix))]
type BinaryFingerprint = (u64, Option<std::time::SystemTime>);

#[cfg(unix)]
fn current_binary_fingerprint() -> Option<BinaryFingerprint> {
    use std::os::unix::fs::MetadataExt;
    let exe = std::env::current_exe().ok()?;
    let meta = std::fs::metadata(&exe).ok()?;
    Some((meta.ino(), meta.mtime()))
}

#[cfg(not(unix))]
fn current_binary_fingerprint() -> Option<BinaryFingerprint> {
    let exe = std::env::current_exe().ok()?;
    let meta = std::fs::metadata(&exe).ok()?;
    Some((meta.len(), meta.modified().ok()))
}

/// True when the executable on disk differs from the launch fingerprint:
/// replaced in place (inode/mtime change) or removed (`brew` cleanup of
/// the old Cellar). Unknown start fingerprint → can't tell → false.
fn binary_changed(start: Option<BinaryFingerprint>, now: Option<BinaryFingerprint>) -> bool {
    match (start, now) {
        (Some(start), Some(now)) => start != now,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Restart the (now-stale) daemon so it picks up the freshly-installed
/// binary. Fire-and-forget; the daemon's event stream re-seeds the TUI.
/// Enumerate audio outputs + read the configured device off the event
/// loop, then open the picker via `AsyncResult::AudioOutputs`.
fn spawn_audio_output_picker(async_tx: mpsc::UnboundedSender<AsyncResult>) {
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    tokio::spawn(async move {
        let listed = tokio::task::spawn_blocking(|| {
            let outputs = spotuify_daemon::server::list_audio_outputs();
            let current = Config::load()
                .ok()
                .and_then(|c| c.player.audio_output_device);
            (outputs, current)
        })
        .await;
        if let Ok((outputs, current)) = listed {
            let _ = async_tx.send(AsyncResult::AudioOutputs { outputs, current });
        }
    });
}

fn spawn_restart_daemon(async_tx: mpsc::UnboundedSender<AsyncResult>) {
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    tokio::spawn(async move {
        if let Err(err) = spotuify_daemon::server::restart_daemon().await {
            tracing::warn!(error = %err, "daemon restart from update banner failed");
        }
        let _ = async_tx;
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

    if app.audio_output_picker.is_some() {
        handle_audio_output_picker_key(app, key, async_tx);
        return Ok(false);
    }

    if app.reminder_picker.is_some() {
        handle_reminder_picker_key(app, key, async_tx);
        return Ok(false);
    }

    if app.command_palette.visible {
        if let Some(action) = handle_palette_key(app, key) {
            return apply_tui_action(app, action, async_tx);
        }
        return Ok(false);
    }

    if app.artist_view.is_some() {
        handle_artist_view_key(app, key, async_tx);
        app.sync_selected_artwork(async_tx);
        return Ok(false);
    }

    // Text input MUST outrank every single-character global intercept
    // below ('R' restart, 'O' output picker, 'D' delete confirm, the
    // notifications action keys): typing "Oasis" in the search box used
    // to open the audio-output picker and typing "Daily" into the
    // playlist filter popped a delete-playlist confirm.
    if app.search_input_active || app.list_filter_active {
        handle_text_input(app, key, async_tx);
        app.sync_selected_artwork(async_tx);
        return Ok(false);
    }

    // Notifications screen: contextual action keys (Enter play, s snooze,
    // d dismiss, x cancel) act on the selected inbox notification or scheduled
    // reminder. Nav keys (j/k, digits, Esc) fall through to the generic map.
    if app.screen == Screen::Notifications && handle_notifications_key(app, key, async_tx) {
        return Ok(false);
    }

    // Update banner: Shift+R restarts the stale daemon onto the new
    // binary. Contextual — only bound while the banner is showing, so it
    // never shadows the per-page `r` actions.
    if app.update_available && matches!(key.code, KeyCode::Char('R')) {
        spawn_restart_daemon(async_tx.clone());
        app.update_available = false;
        app.toast = Some("Restarting daemon to apply update…".to_string());
        return Ok(false);
    }

    // Shift+O opens the local audio-output picker (which Mac speaker the
    // embedded player renders to). Uppercase + contextual-free so it
    // doesn't clash with lowercase per-page keys. Device enumeration is
    // spawned off the event loop — CoreAudio listing can block.
    if matches!(key.code, KeyCode::Char('O')) {
        spawn_audio_output_picker(async_tx.clone());
        return Ok(false);
    }

    // Shift+D: destructive remove on the current screen, behind a confirm
    // modal. Only intercepts when there's a target (a selected playlist /
    // marked liked tracks); otherwise it falls through to the keymap so it
    // never shadows other `D` bindings.
    if matches!(key.code, KeyCode::Char('D')) {
        if let Some(modal) = delete_confirm_for_screen(app) {
            app.confirm_modal = Some(modal);
            return Ok(false);
        }
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
    /// Absolute volume from a click on the transport's volume bar.
    Volume(u8),
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
            app.sync_selected_artwork(async_tx);
            Ok(false)
        }
        MouseOutcome::Volume(percent) => {
            command_then_refresh_transport(
                app,
                async_tx,
                CommandKind::Volume {
                    volume_percent: percent,
                },
            );
            Ok(false)
        }
    }
}

/// True while any modal/overlay owns the screen. The mouse path must
/// honor the same gates as the keyboard path — without this, clicks
/// kept seeking, switching tabs, and firing transport actions (and the
/// scroll wheel kept changing volume) UNDERNEATH an open delete-confirm
/// or login modal.
fn modal_blocks_input(app: &App) -> bool {
    app.error.is_some()
        || app.fullscreen_panel.is_some()
        || app.login_modal.is_some()
        || app.confirm_modal.is_some()
        || app.playlist_picker.is_some()
        || app.device_picker.is_some()
        || app.audio_output_picker.is_some()
        || app.reminder_picker.is_some()
        || app.artist_view.is_some()
        || app.command_palette.visible
        || app.show_help
}

fn mouse_outcome(app: &App, area: Rect, mouse: MouseEvent) -> Option<MouseOutcome> {
    if modal_blocks_input(app) {
        return None;
    }
    if let Some(outcome) = mouse_player_outcome(app, area, mouse) {
        return Some(outcome);
    }
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        return None;
    }
    if let Some(action) = mouse_tab_action(app, area, mouse.column, mouse.row) {
        return Some(MouseOutcome::Action(action));
    }
    let (main, rail) = body_content_areas(area, app.right_rail);
    if rail.is_some_and(|rail| rect_contains(rail, mouse.column, mouse.row)) {
        return mouse_rail_outcome(app, app.right_rail, mouse.column, mouse.row);
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
    if let Some(outcome) = mouse_transport_outcome(app, player, mouse.column, mouse.row) {
        return Some(outcome);
    }
    mouse_seek_position(app, player, mouse.column, mouse.row).map(MouseOutcome::Seek)
}

fn mouse_transport_outcome(app: &App, player: Rect, column: u16, row: u16) -> Option<MouseOutcome> {
    let inner = rect_inner(
        player,
        Margin {
            horizontal: 1,
            vertical: 1,
        },
    );
    // Same layout call as `render_now_playing`, so the transport rect
    // and chip widths track the responsive collapse exactly.
    let layout = ui::now_playing_layout(inner);
    let transport = layout.transport;
    if transport.width == 0 || !rect_contains(transport, column, row) {
        return None;
    }
    // Strip transport's 1-cell horizontal margin so columns match the
    // layout in `render_transport`.
    let local_col = column.saturating_sub(transport.x.saturating_add(1));
    let usable_width = transport.width.saturating_sub(2);
    if local_col >= usable_width {
        return None;
    }
    let compact = layout.compact_transport;
    let local_row = row.saturating_sub(transport.y);
    match local_row {
        // Primary buttons row — prev / play / next. Ranges come from
        // the same helper that lays out the rendered chips (the old
        // equal-thirds split fired PlayPause for clicks on ⏭).
        2 => {
            let [prev, play, next] = ui::transport_primary_ranges(compact);
            if prev.contains(&local_col) {
                Some(MouseOutcome::Action(TuiAction::Previous))
            } else if play.contains(&local_col) {
                Some(MouseOutcome::Action(TuiAction::PlayPause))
            } else if next.contains(&local_col) {
                Some(MouseOutcome::Action(TuiAction::Next))
            } else {
                None
            }
        }
        // Toggles row — shuffle, repeat, like. Ranges come from the
        // same helper `render_transport` uses for its chip labels, so
        // clicks land on the chip the user sees in either layout.
        4 => {
            let liked = app.playback.item.as_ref().is_some_and(|i| {
                app.marked_uris.contains(&i.uri)
                    || app.library_items.iter().any(|saved| saved.uri == i.uri)
            });
            let [shuffle, repeat, like] = ui::transport_toggle_ranges(
                app.playback.repeat.as_str(),
                app.playback.shuffle,
                liked,
                compact,
            );
            if shuffle.contains(&local_col) {
                Some(MouseOutcome::Action(TuiAction::ToggleShuffle))
            } else if repeat.contains(&local_col) {
                Some(MouseOutcome::Action(TuiAction::CycleRepeat))
            } else if like.contains(&local_col) {
                Some(MouseOutcome::Action(TuiAction::LikeSelection))
            } else {
                None
            }
        }
        // Volume row — click-to-set on the rendered bar.
        6 => {
            let bar = ui::transport_volume_bar_range(compact);
            if !bar.contains(&local_col) {
                return None;
            }
            let width = bar.end.saturating_sub(bar.start).max(1);
            let filled = local_col.saturating_sub(bar.start) + 1;
            let percent = (u32::from(filled) * 100 / u32::from(width)).min(100) as u8;
            Some(MouseOutcome::Volume(percent))
        }
        _ => None,
    }
}

fn mouse_tab_action(app: &App, area: Rect, column: u16, row: u16) -> Option<TuiAction> {
    let tabs = body_tabs_area(area);
    if !rect_contains(tabs, column, row) || tabs.width == 0 {
        return None;
    }
    // Same layout call as `render_body`, so clicks land on the tab the
    // user sees regardless of which responsive mode is active.
    let selected = Screen::ALL
        .iter()
        .position(|screen| *screen == app.screen)
        .unwrap_or(0);
    let (_, ranges) = ui::tab_strip_layout(selected, tabs.width);
    let relative = column.saturating_sub(tabs.x);
    ranges
        .iter()
        .find(|(_, range)| range.contains(&relative))
        .and_then(|(index, _)| Screen::ALL.get(*index))
        .copied()
        .map(screen_action)
}

fn mouse_rail_outcome(
    app: &App,
    mode: RightRailMode,
    column: u16,
    row: u16,
) -> Option<MouseOutcome> {
    // Targets come from the renderer: the hide chip rect is where the
    // "Q hide" text is actually drawn (the old hotspot was the
    // rightmost 10 columns while the text sat on the left).
    use crate::hit::HitTarget;
    let target = app.hit_map.borrow().target_at(column, row)?;
    let action = match (target, mode) {
        (HitTarget::RailToggle, RightRailMode::Queue) => TuiAction::ToggleQueueRail,
        (HitTarget::RailToggle, RightRailMode::Lyrics) => TuiAction::ToggleLyricsRail,
        (HitTarget::RailToggle, RightRailMode::Hints) => TuiAction::ToggleHintsRail,
        (HitTarget::RailFullscreen, RightRailMode::Queue | RightRailMode::Lyrics) => {
            TuiAction::ToggleRailFullscreen
        }
        _ => return None,
    };
    Some(MouseOutcome::Action(action))
}

fn mouse_row_selection(app: &App, area: Rect, column: u16, row: u16) -> Option<usize> {
    if !rect_contains(area, column, row) {
        return None;
    }
    // Diagnostics' log pane keeps its custom mapping (variable-height
    // wrapped lines). Every other screen resolves through the hit map
    // the renderers populated THIS frame — exact rows, exact scroll
    // offsets, exact column splits, because the renderer registered
    // what it drew. (The old per-screen math here assumed 1-line rows
    // against 2-line rendered lists and ignored scrolling: clicks
    // selected the wrong row on most screens.)
    if app.screen == Screen::Diagnostics {
        return diagnostics_log_index(app, area, column, row);
    }
    match app.hit_map.borrow().target_at(column, row) {
        Some(crate::hit::HitTarget::Row { index }) => Some(index),
        _ => None,
    }
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
    // Same solver the renderer uses — at tiny heights the chrome
    // shrinks and fixed offsets drifted off the drawn player.
    ui::root_chrome_layout(area)[1]
}

fn player_progress_area(player: Rect) -> Rect {
    let inner = rect_inner(
        player,
        Margin {
            horizontal: 1,
            vertical: 1,
        },
    );
    // The progress gauge lives in the track panel; when the responsive
    // layout drops that panel there is nothing to click (zero width).
    // `track_gauge_rect` is the SAME geometry the renderer draws the
    // gauge with — row and x-origin included.
    match ui::now_playing_layout(inner).track {
        Some(track) => ui::track_gauge_rect(track),
        None => Rect::new(inner.x, inner.y, 0, 0),
    }
}

fn body_tabs_area(area: Rect) -> Rect {
    let body = ui::root_chrome_layout(area)[0];
    // Horizontal margin 2 = the body block's LEFT/RIGHT border plus its
    // 1-cell inner margin, so columns here line up with `render_body`'s
    // tab strip exactly (the strip's ranges are relative to this x).
    let inner = rect_inner(
        body,
        Margin {
            horizontal: 2,
            vertical: 0,
        },
    );
    // Only the strip row itself (rows[1] of the body stack) — the
    // padding rows above/below are not tab hotspots.
    Rect::new(
        inner.x,
        inner.y.saturating_add(1),
        inner.width,
        1.min(inner.height),
    )
}

fn body_content_areas(area: Rect, rail: RightRailMode) -> (Rect, Option<Rect>) {
    let body = ui::root_chrome_layout(area)[0];
    let inner = rect_inner(
        body,
        Margin {
            horizontal: 2,
            vertical: 0,
        },
    );
    // Content sits below the 3-row tab band (pad, strip, pad) — the
    // body stack `render_body` lays out.
    let content = Rect::new(
        inner.x,
        inner.y.saturating_add(3),
        inner.width,
        inner.height.saturating_sub(3),
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
        (KeyCode::Char('L'), _) => {
            // Toggle the in-library filter. Selection resets to the first
            // visible album; no refetch — the daemon already tagged each one.
            view.library_only = !view.library_only;
            view.album_selected = 0;
            let mode = if view.library_only { "library" } else { "all" };
            let next_uri = view.visible_albums().first().map(|a| a.uri.clone());
            if let Some(album_uri) = next_uri {
                load_album_tracks(app, async_tx, album_uri);
            } else if let Some(view) = app.artist_view.as_mut() {
                view.album_tracks.clear();
                view.track_selected = 0;
            }
            app.toast = Some(format!("Showing {mode} releases"));
        }
        (KeyCode::Char('F'), _) => {
            // Toggle follow. Fire-and-forget; the daemon emits LibraryChanged
            // and the toast + optimistic state flip give instant feedback.
            let uri = view.artist_uri.clone();
            let name = view.artist_name.clone();
            let was_following = view.is_followed == Some(true);
            view.is_followed = Some(!was_following);
            let async_tx = async_tx.clone();
            tokio::spawn(async move {
                let request = if was_following {
                    Request::ArtistUnfollow { artist: uri }
                } else {
                    Request::ArtistFollow { artist: uri }
                };
                if request_data(request).await.is_err() {
                    let _ = async_tx;
                }
            });
            app.toast = Some(if was_following {
                format!("Unfollowed {name}")
            } else {
                format!("Followed {name}")
            });
        }
        (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => match view.focus {
            ArtistViewSide::Albums => {
                let visible = view.visible_albums();
                if !visible.is_empty() {
                    let last = visible.len() - 1;
                    let next = view.album_selected.saturating_add(1).min(last);
                    let next_uri = (next != view.album_selected).then(|| visible[next].uri.clone());
                    drop(visible);
                    if let Some(album_uri) = next_uri {
                        view.album_selected = next;
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
                    let prev = view.album_selected - 1;
                    let prev_uri = view.visible_albums().get(prev).map(|a| a.uri.clone());
                    if let Some(album_uri) = prev_uri {
                        view.album_selected = prev;
                        load_album_tracks(app, async_tx, album_uri);
                    }
                }
            }
            ArtistViewSide::Tracks => {
                view.track_selected = view.track_selected.saturating_sub(1);
            }
        },
        (KeyCode::Enter, _) => match view.focus {
            ArtistViewSide::Albums => {
                let album = view
                    .visible_albums()
                    .get(view.album_selected)
                    .map(|album| (*album).clone());
                if let Some(album) = album {
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

fn handle_audio_output_picker_key(
    app: &mut App,
    key: KeyEvent,
    async_tx: &mpsc::UnboundedSender<AsyncResult>,
) {
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) | (KeyCode::Char('q'), _) => {
            app.audio_output_picker = None;
        }
        (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
            move_audio_output_picker(app, 1);
        }
        (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
            move_audio_output_picker(app, -1);
        }
        (KeyCode::Enter, _) => apply_audio_output_picker_selection(app, async_tx),
        _ => {}
    }
}

fn move_audio_output_picker(app: &mut App, delta: isize) {
    let Some(picker) = app.audio_output_picker.as_mut() else {
        return;
    };
    let len = picker.outputs.len();
    if len == 0 {
        return;
    }
    picker.selected = (picker.selected as isize + delta).rem_euclid(len as isize) as usize;
}

fn apply_audio_output_picker_selection(
    app: &mut App,
    async_tx: &mpsc::UnboundedSender<AsyncResult>,
) {
    let Some(picker) = app.audio_output_picker.take() else {
        return;
    };
    let Some(name) = picker.outputs.get(picker.selected).cloned() else {
        return;
    };
    let value = audio_output_config_value(&name);
    match spotuify_spotify::config::set_config_value(
        spotuify_spotify::config::ConfigKey::PlayerAudioOutputDevice,
        value,
    ) {
        // Live rebind: the daemon swaps its sink in-process and resumes
        // the interrupted track. No daemon restart, so the TUI's IPC
        // connection and event stream stay up.
        Ok(_) => {
            let device = (!value.is_empty()).then(|| value.to_string());
            let async_tx_inner = async_tx.clone();
            tokio::spawn(async move {
                let outcome = match request_data(Request::SetAudioOutput { device }).await {
                    Ok(ResponseData::Ack { message }) => Ok(CommandResult {
                        message: Some(message),
                        request_refresh: true,
                        ..Default::default()
                    }),
                    Ok(_) => Err("unexpected response to set-audio-output".to_string()),
                    Err(err) => Err(short_error(err)),
                };
                let _ = async_tx_inner.send(AsyncResult::Command(Box::new(outcome)));
            });
            app.toast = Some(format!(
                "Audio output → {}…",
                audio_output_toast_label(&name)
            ));
        }
        Err(err) => {
            app.toast = Some(format!("Couldn't set audio output: {err}"));
        }
    }
}

fn audio_output_config_value(selection: &str) -> &str {
    if selection == SYSTEM_AUDIO_OUTPUT_LABEL {
        ""
    } else {
        selection
    }
}

fn audio_output_toast_label(selection: &str) -> &str {
    if selection == SYSTEM_AUDIO_OUTPUT_LABEL {
        "system default"
    } else {
        selection
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

fn search_sort_label(sort: SearchSortData) -> &'static str {
    match sort {
        SearchSortData::Relevance => "Relevance",
        SearchSortData::Name => "Name",
        SearchSortData::Duration => "Duration",
        SearchSortData::Artist => "Artist",
        SearchSortData::Date => "Date",
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
        (KeyCode::Char('9'), KeyModifiers::NONE) => Some(TuiAction::OpenNotifications),
        (KeyCode::Char('0'), KeyModifiers::NONE) => Some(TuiAction::OpenHistory),
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
        // Search-screen result refinements (client-side): cycle sort / type
        // filter. Only when results are focused — typing handles letters first.
        (KeyCode::Char('S'), _) if app.screen == Screen::Search => {
            app.cycle_search_sort();
            None
        }
        (KeyCode::Char('T'), _) if app.screen == Screen::Search => {
            app.cycle_search_kind_filter();
            None
        }
        (KeyCode::Char('l'), KeyModifiers::NONE) => Some(TuiAction::LikeSelection),
        (KeyCode::Char('o'), KeyModifiers::NONE) => Some(TuiAction::OpenSelectedArtist),
        (KeyCode::Char('O'), _) => Some(TuiAction::OpenSelectedAlbum),
        (KeyCode::Char('R'), _) => Some(TuiAction::RemindMe),
        (KeyCode::Char('U'), _) => Some(TuiAction::RefreshMedia),
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
        TuiAction::OpenNotifications => {
            switch_screen(app, Screen::Notifications);
            spawn_load_reminders(async_tx);
        }
        TuiAction::OpenHistory => {
            switch_screen(app, Screen::History);
            app.history_loading = true;
            app.history_error = None;
            spawn_load_history(async_tx);
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
        TuiAction::RefreshMedia => request_force_media(app, async_tx),
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
        TuiAction::OpenSelectedArtist => open_selected_artist(app, async_tx),
        TuiAction::OpenSelectedAlbum => open_selected_album(app, async_tx),
        TuiAction::PlaySelected => activate_selected(app, async_tx),
        TuiAction::QueueSelection => queue_selection(app, async_tx),
        TuiAction::LikeSelection => like_selection(app, async_tx),
        TuiAction::RemindMe => remind_selection(app, async_tx),
        TuiAction::AddSelectionToPlaylist => add_selection_to_playlist(app, async_tx),
        TuiAction::DeleteSelectedPlaylist => delete_selected_playlist(app, async_tx),
        TuiAction::UnsaveSelection => unsave_selection(app, async_tx),
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
    app.sync_selected_artwork(async_tx);
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
    // VISIBLE list, not `search_results`: with a sort/kind-filter
    // active the raw index points at a different item and the trigger
    // fires for the wrong pane (or not at all near the real end).
    let items = app.visible_items();
    let Some(selected_item) = items.get(app.selected) else {
        return;
    };
    let kind = selected_item.kind.clone();
    let (idx_within_pane, pane_count) = {
        let mut idx = 0usize;
        let mut count = 0usize;
        for (i, item) in items.iter().enumerate() {
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
        .is_some_and(|p| !p.loading && !p.exhausted);
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
            if let Some((playlist_id, playlist_name)) = app.selected_playlist_target() {
                command_then_refresh(
                    app,
                    async_tx,
                    CommandKind::PlayUri {
                        uri: format!("spotify:playlist:{playlist_id}"),
                        context: None,
                    },
                );
                app.toast = Some(format!("Playing playlist {playlist_name}"));
            }
        }
        Screen::Playlists if app.selected_playlist_id.is_some() => {
            if let Some((playlist_id, playlist_name)) = app.selected_playlist_target() {
                command_then_refresh(
                    app,
                    async_tx,
                    CommandKind::PlayUri {
                        uri: format!("spotify:playlist:{playlist_id}"),
                        context: None,
                    },
                );
                app.toast = Some(format!("Playing playlist {playlist_name}"));
            }
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
                    open_artist_view(app, async_tx, item, None);
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

/// Navigate from the selected item to its primary artist (the TUI mirror of the
/// macOS click-through). An artist row opens itself; anything else uses its
/// first artist ref.
fn open_selected_artist(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    let Some(item) = app.selected_item() else {
        return;
    };
    if matches!(item.kind, MediaKind::Artist) {
        open_artist_view(app, async_tx, item, None);
        return;
    }
    match item.artists.first() {
        Some(artist) if !artist.uri.is_empty() => {
            let synthetic = MediaItem {
                uri: artist.uri.clone(),
                name: artist.name.clone(),
                kind: MediaKind::Artist,
                ..Default::default()
            };
            open_artist_view(app, async_tx, synthetic, None);
        }
        _ => app.toast = Some("No artist link for this item".to_string()),
    }
}

/// Navigate from the selected track/album to its album. The TUI has no
/// standalone album page, so this opens the artist view focused on that album
/// (its discography includes appears-on, so the album is present).
fn open_selected_album(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    let Some(item) = app.selected_item() else {
        return;
    };
    let (album_uri, artist) = match item.kind {
        MediaKind::Album => (Some(item.uri.clone()), item.artists.first().cloned()),
        _ => (item.album_uri.clone(), item.artists.first().cloned()),
    };
    match (album_uri, artist) {
        (Some(album_uri), Some(artist)) if !album_uri.is_empty() && !artist.uri.is_empty() => {
            let synthetic = MediaItem {
                uri: artist.uri.clone(),
                name: artist.name.clone(),
                kind: MediaKind::Artist,
                ..Default::default()
            };
            open_artist_view(app, async_tx, synthetic, Some(album_uri));
        }
        _ => app.toast = Some("No album link for this item".to_string()),
    }
}

fn open_artist_view(
    app: &mut App,
    async_tx: &mpsc::UnboundedSender<AsyncResult>,
    artist: MediaItem,
    focus_album: Option<String>,
) {
    app.artist_view = Some(ArtistViewState {
        artist_uri: artist.uri.clone(),
        artist_name: artist.name.clone(),
        albums: Vec::new(),
        album_selected: 0,
        album_tracks: Vec::new(),
        track_selected: 0,
        // Jump straight to the Tracks pane when navigating to a specific album.
        focus: if focus_album.is_some() {
            ArtistViewSide::Tracks
        } else {
            ArtistViewSide::Albums
        },
        loading_albums: true,
        loading_tracks: false,
        error: None,
        library_only: false,
        is_followed: artist.in_library,
        pending_album_uri: focus_album,
    });
    let async_tx = async_tx.clone();
    let artist_uri = artist.uri;
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
    let async_tx = async_tx;
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

/// Contextual key handling for the Notifications screen. Returns true if the
/// key was consumed as an action; false lets nav/global keys through. The
/// combined list is `[inbox notifications…, scheduled reminders…]`.
fn handle_notifications_key(
    app: &mut App,
    key: KeyEvent,
    async_tx: &mpsc::UnboundedSender<AsyncResult>,
) -> bool {
    let n_count = app.notifications.len();
    if app.selected < n_count {
        let notification = app.notifications[app.selected].clone();
        let act = |action, snooze_until_ms| Request::NotificationAct {
            id: notification.id.clone(),
            action,
            snooze_until_ms,
        };
        match (key.code, key.modifiers) {
            (KeyCode::Enter, _) => {
                requests_then_refresh(
                    app,
                    async_tx,
                    vec![act(NotificationAction::Play, None)],
                    format!("Playing {}", notification.name),
                );
                true
            }
            (KeyCode::Char('s'), KeyModifiers::NONE) => {
                let until = chrono::Local::now().timestamp_millis() + 3_600_000;
                requests_then_refresh(
                    app,
                    async_tx,
                    vec![act(NotificationAction::Snooze, Some(until))],
                    "Snoozed 1h".to_string(),
                );
                true
            }
            (KeyCode::Char('d'), KeyModifiers::NONE) => {
                requests_then_refresh(
                    app,
                    async_tx,
                    vec![act(NotificationAction::Dismiss, None)],
                    "Dismissed reminder".to_string(),
                );
                true
            }
            _ => false,
        }
    } else if let Some(reminder) = app.reminders.get(app.selected - n_count).cloned() {
        match (key.code, key.modifiers) {
            (KeyCode::Enter, _) => {
                requests_then_refresh(
                    app,
                    async_tx,
                    vec![Request::PlaybackCommand {
                        command: PlaybackCommand::PlayUri {
                            uri: reminder.media_uri.clone(),
                            context_uri: None,
                        },
                    }],
                    format!("Playing {}", reminder.name),
                );
                true
            }
            (KeyCode::Char('x'), KeyModifiers::NONE) | (KeyCode::Char('c'), KeyModifiers::NONE) => {
                requests_then_refresh(
                    app,
                    async_tx,
                    vec![Request::ReminderCancel { id: reminder.id }],
                    "Reminder cancelled".to_string(),
                );
                true
            }
            _ => false,
        }
    } else {
        false
    }
}

fn remind_selection(app: &mut App, _async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    let uris = app.selected_target_uris();
    if uris.is_empty() {
        app.toast = Some("Select a track to set a reminder".to_string());
        return;
    }
    let label = app
        .selected_item()
        .map(|item| item.name)
        .unwrap_or_else(|| format!("{} item(s)", uris.len()));
    app.reminder_picker = Some(ReminderPickerModal {
        uris,
        label,
        preset: 2, // default: Tomorrow 9am
        recurrence: Recurrence::None,
        custom: "+3d".to_string(),
    });
}

fn cycle_recurrence(recurrence: Recurrence) -> Recurrence {
    match recurrence {
        Recurrence::None => Recurrence::Daily,
        Recurrence::Daily => Recurrence::Weekly,
        Recurrence::Weekly => Recurrence::Monthly,
        Recurrence::Monthly => Recurrence::None,
    }
}

/// Parse a bare offset like `+3d` / `2w` / `6h` into milliseconds.
fn parse_offset_ms(input: &str) -> Option<i64> {
    let s = input.trim().trim_start_matches('+');
    if s.len() < 2 {
        return None;
    }
    let (num, unit) = s.split_at(s.len() - 1);
    let n: i64 = num.trim().parse().ok()?;
    let mult = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        "w" => 604_800_000,
        _ => return None,
    };
    Some(n * mult)
}

/// Resolve a preset index (+ custom offset text) to an absolute epoch (ms) in
/// the local timezone. Returns None for an unparseable custom offset.
fn resolve_reminder_preset(index: usize, custom: &str) -> Option<i64> {
    use chrono::{Datelike, Duration as ChronoDuration, Local, TimeZone};
    let now = Local::now();
    let at_hour = |hour: u32, day: chrono::DateTime<Local>| -> Option<chrono::DateTime<Local>> {
        let naive = day.date_naive().and_hms_opt(hour, 0, 0)?;
        Local.from_local_datetime(&naive).single()
    };
    let ms = match index {
        0 => (now + ChronoDuration::hours(1)).timestamp_millis(),
        1 => {
            let today = at_hour(19, now)?;
            let chosen = if today > now {
                today
            } else {
                at_hour(19, now + ChronoDuration::days(1))?
            };
            chosen.timestamp_millis()
        }
        2 => at_hour(9, now + ChronoDuration::days(1))?.timestamp_millis(),
        3 => {
            // Next Saturday 10am (Mon=0…Sat=5…Sun=6).
            let weekday = now.weekday().num_days_from_monday() as i64;
            let days = (5 - weekday).rem_euclid(7);
            let mut sat = at_hour(10, now + ChronoDuration::days(days))?;
            if sat <= now {
                sat = at_hour(10, now + ChronoDuration::days(days + 7))?;
            }
            sat.timestamp_millis()
        }
        4 => {
            // Next Monday 9am (always at least next week).
            let weekday = now.weekday().num_days_from_monday() as i64;
            let days = if weekday == 0 { 7 } else { 7 - weekday };
            at_hour(9, now + ChronoDuration::days(days))?.timestamp_millis()
        }
        _ => now.timestamp_millis() + parse_offset_ms(custom)?,
    };
    Some(ms)
}

fn handle_reminder_picker_key(
    app: &mut App,
    key: KeyEvent,
    async_tx: &mpsc::UnboundedSender<AsyncResult>,
) {
    let custom_idx = REMINDER_PRESETS.len() - 1;
    let Some(picker) = app.reminder_picker.as_mut() else {
        return;
    };
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => {
            app.reminder_picker = None;
            app.toast = Some("Canceled reminder".to_string());
        }
        (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
            picker.preset = (picker.preset + 1) % REMINDER_PRESETS.len();
        }
        (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
            picker.preset = (picker.preset + REMINDER_PRESETS.len() - 1) % REMINDER_PRESETS.len();
        }
        (KeyCode::Tab, _) => picker.recurrence = cycle_recurrence(picker.recurrence),
        (KeyCode::Backspace, _) if picker.preset == custom_idx => {
            picker.custom.pop();
        }
        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT)
            if picker.preset == custom_idx && !c.is_control() =>
        {
            picker.custom.push(c);
        }
        (KeyCode::Enter, _) => match resolve_reminder_preset(picker.preset, &picker.custom) {
            Some(anchor_at_ms) => {
                let recurrence = picker.recurrence;
                let uris = picker.uris.clone();
                let count = uris.len();
                app.reminder_picker = None;
                let requests = uris
                    .into_iter()
                    .map(|uri| Request::ReminderCreate {
                        media_uri: uri,
                        anchor_at_ms,
                        recurrence,
                        tz: "UTC".to_string(),
                        message: None,
                    })
                    .collect();
                requests_then_refresh(
                    app,
                    async_tx,
                    requests,
                    format!("Reminder set for {count} item(s)"),
                );
            }
            None => {
                app.toast = Some("Bad custom offset — try +3d, +2w, +6h".to_string());
            }
        },
        _ => {}
    }
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

fn delete_selected_playlist(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    let requests = app.requests_for_action(TuiAction::DeleteSelectedPlaylist);
    if requests.is_empty() {
        return;
    }
    requests_then_refresh(app, async_tx, requests, "Removed playlist".to_string());
}

fn unsave_selection(app: &mut App, async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    let requests = app.requests_for_action(TuiAction::UnsaveSelection);
    if requests.is_empty() {
        return;
    }
    let count = requests.len();
    requests_then_refresh(app, async_tx, requests, format!("Unsaved {count} track(s)"));
}

/// Build the confirm modal for the destructive action available on the
/// current screen: remove-playlist (Playlists) or bulk-unsave (Library).
/// `None` when there's nothing to act on, so the `D` key falls through.
fn delete_confirm_for_screen(app: &App) -> Option<ConfirmModal> {
    match app.screen {
        Screen::Playlists => {
            let (_, name) = app.selected_playlist_target()?;
            Some(ConfirmModal {
                title: "Remove playlist".to_string(),
                body: format!(
                    "Remove \"{name}\" from your library? Undo with `spotuify ops undo`."
                ),
                on_confirm: TuiAction::DeleteSelectedPlaylist,
            })
        }
        Screen::Library => {
            let count = app.selected_target_uris().len();
            (count > 0).then(|| ConfirmModal {
                title: "Unsave tracks".to_string(),
                body: format!(
                    "Remove {count} track(s) from Liked Songs? Undo with `spotuify ops undo`."
                ),
                on_confirm: TuiAction::UnsaveSelection,
            })
        }
        _ => None,
    }
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
    if app.playback.is_playing || playback_can_resume_current(&app.playback) {
        return false;
    }

    match app.screen {
        Screen::Player | Screen::Search | Screen::Library => app.selected_item().is_some(),
        Screen::Playlists if app.selected_playlist_id.is_none() => {
            app.selected_playlist().is_some()
        }
        Screen::Playlists => app.selected_item().is_some(),
        _ => false,
    }
}

fn playback_can_resume_current(playback: &spotuify_core::Playback) -> bool {
    let Some(item) = playback.item.as_ref() else {
        return false;
    };
    item.duration_ms == 0 || playback.progress_ms.saturating_add(750) < item.duration_ms
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

/// Fetch reminder schedules + inbox notifications and deliver them via
/// `AsyncResult::RemindersLoaded`. Fire-and-forget; failures yield empty lists.
fn spawn_load_history(async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    // See `spawn_load_reminders`: tests apply paths without a Tokio runtime.
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    let async_tx = async_tx.clone();
    tokio::spawn(async move {
        let result = request_data(Request::ListenSessions { limit: 50 }).await;
        let _ = async_tx.send(AsyncResult::ListenHistory {
            result: match result {
                Ok(ResponseData::ListenSessions { sessions }) => Ok(sessions),
                Ok(_) => Err("unexpected listen-sessions response".to_string()),
                Err(err) => Err(short_error(err)),
            },
        });
    });
}

fn spawn_load_reminders(async_tx: &mpsc::UnboundedSender<AsyncResult>) {
    // Tests drive apply paths synchronously without a Tokio runtime; skip the
    // background fetch there rather than panic on `tokio::spawn`.
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    let async_tx = async_tx.clone();
    tokio::spawn(async move {
        let reminders = match request_data(Request::RemindersList {
            include_inactive: false,
        })
        .await
        {
            Ok(ResponseData::Reminders { reminders }) => reminders,
            _ => Vec::new(),
        };
        let notifications = match request_data(Request::NotificationsList {
            include_archived: false,
        })
        .await
        {
            Ok(ResponseData::Notifications { notifications }) => notifications,
            _ => Vec::new(),
        };
        let _ = async_tx.send(AsyncResult::RemindersLoaded {
            reminders,
            notifications,
        });
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
            // Mutations and reminder/notification acks are all "fire + refresh"
            // commands — we only care that they succeeded, not the payload.
            ResponseData::Mutation { .. }
            | ResponseData::Ack { .. }
            | ResponseData::ReminderCreated { .. } => {}
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
            command: PlaybackCommand::PlayUri {
                uri: item.uri,
                context_uri: None,
            },
        },
        CommandKind::PlayUri { uri, context } => Request::PlaybackCommand {
            command: PlaybackCommand::PlayUri {
                uri,
                // TUI-originated plays only ever carry a Spotify context
                // URI (album/playlist); the daemon owns Liked-Songs
                // resolution, so no explicit track list reaches here.
                context_uri: context.and_then(|c| c.context_uri),
            },
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
    // Operate on the VISIBLE list: `app.selected` indexes the
    // filtered/sorted view, not `search_results`.
    let items = app.visible_items();
    let visible: Vec<MediaKind> = ORDER
        .iter()
        .filter(|k| items.iter().any(|i| &i.kind == *k))
        .cloned()
        .collect();
    if visible.is_empty() {
        return;
    }
    let current_kind = items.get(app.selected).map(|i| i.kind.clone());
    let current_idx = current_kind
        .as_ref()
        .and_then(|kind| visible.iter().position(|k| k == kind))
        .unwrap_or(0);
    let next_idx = ((current_idx as isize + delta).rem_euclid(visible.len() as isize)) as usize;
    let target = visible[next_idx].clone();
    if let Some(idx) = items.iter().position(|i| i.kind == target) {
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
        Screen::History => TuiAction::OpenHistory,
        Screen::Devices => TuiAction::OpenDevices,
        Screen::Diagnostics => TuiAction::OpenDiagnostics,
        Screen::Lyrics => TuiAction::OpenLyrics,
        Screen::Notifications => TuiAction::OpenNotifications,
    }
}

fn apply_screen_switch(app: &mut App, action: TuiAction) -> bool {
    match action {
        TuiAction::OpenPlayer => switch_screen(app, Screen::Player),
        TuiAction::OpenSearch => switch_screen(app, Screen::Search),
        TuiAction::OpenLibrary => {
            switch_screen(app, Screen::Library);
            app.request_refresh();
        }
        TuiAction::OpenPlaylists => {
            switch_screen(app, Screen::Playlists);
            app.request_refresh();
        }
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

fn playable_home_items(items: &[MediaItem]) -> Vec<MediaItem> {
    items
        .iter()
        .filter(|item| {
            matches!(
                item.kind,
                MediaKind::Track | MediaKind::Album | MediaKind::Show | MediaKind::Episode
            )
        })
        .cloned()
        .collect()
}

fn dedupe_media_items(items: Vec<MediaItem>) -> Vec<MediaItem> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(item.uri.clone()))
        .collect()
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
            recent_items: Vec::new(),
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
            notifications: Vec::new(),
            reminders: Vec::new(),
            history_sessions: Vec::new(),
            history_loading: false,
            history_error: None,
            search_sort: SearchSortData::Relevance,
            search_kind_filter: None,
            error: None,
            last_progress_tick: Instant::now(),
            awaiting_track_change_until: None,
            current_art_url: None,
            cover: None,
            palette: UiPalette::default(),
            selected_art_url: None,
            selected_art_cover: None,
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

    #[test]
    fn binary_changed_detects_replace_and_removal() {
        // Same fingerprint → no upgrade.
        assert!(!binary_changed(Some((10, 100)), Some((10, 100))));
        // Inode change (in-place replace / cargo install).
        assert!(binary_changed(Some((10, 100)), Some((11, 100))));
        // mtime change.
        assert!(binary_changed(Some((10, 100)), Some((10, 200))));
        // File removed (brew cleanup of the old Cellar).
        assert!(binary_changed(Some((10, 100)), None));
        // Couldn't fingerprint at launch → can't tell → never nags.
        assert!(!binary_changed(None, Some((10, 100))));
        assert!(!binary_changed(None, None));
    }

    #[test]
    fn shift_r_restarts_daemon_only_when_update_available() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = test_app();
        // No update pending: Shift+R is not the restart key (falls through
        // to normal handling, banner flag untouched).
        app.update_available = false;
        let _ = handle_key(&mut app, key(KeyCode::Char('R')), &tx);
        assert!(!app.update_available);
        // Update pending: Shift+R consumes it and clears the banner flag.
        app.update_available = true;
        let _ = handle_key(&mut app, key(KeyCode::Char('R')), &tx);
        assert!(!app.update_available, "Shift+R should dismiss the banner");
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// Render a full frame so renderers populate the hit map, and
    /// return the buffer for locating drawn text.
    fn render_frame(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| crate::ui::render(frame, app))
            .expect("render");
        terminal.backend().buffer().clone()
    }

    /// Top-left cell of the first occurrence of `needle` at or below
    /// `min_y`. Compares cell-by-cell so wide glyphs can't shift the
    /// column math.
    fn find_text(buffer: &ratatui::buffer::Buffer, needle: &str, min_y: u16) -> (u16, u16) {
        let found = try_find_text(buffer, needle, min_y);
        assert!(
            found.is_some(),
            "text {needle:?} not found in rendered frame"
        );
        found.unwrap_or_default()
    }

    fn try_find_text(
        buffer: &ratatui::buffer::Buffer,
        needle: &str,
        min_y: u16,
    ) -> Option<(u16, u16)> {
        let area = *buffer.area();
        for y in min_y..area.height {
            'col: for x in 0..area.width {
                let mut cx = x;
                for ch in needle.chars() {
                    if cx >= area.width || buffer[(cx, y)].symbol() != ch.to_string() {
                        continue 'col;
                    }
                    cx += 1;
                }
                return Some((x, y));
            }
        }
        None
    }

    fn left_click(app: &App, area: Rect, column: u16, row: u16) -> Option<MouseOutcome> {
        mouse_outcome(
            app,
            area,
            mouse(MouseEventKind::Down(MouseButton::Left), column, row),
        )
    }

    #[test]
    fn transport_clicks_land_on_the_drawn_chips() {
        let mut app = test_app();
        let area = Rect::new(0, 0, 120, 32);
        let buffer = render_frame(&mut app, 120, 32);
        let player_top = 32 - 13;
        // Anchor on the drawn ⏮ glyph; chips sit 10 columns apart
        // (chip starts 1/11/21, glyph 3 cells into each chip).
        let (prev_x, row) = find_text(&buffer, "⏮", player_top);
        assert_eq!(
            left_click(&app, area, prev_x, row),
            Some(MouseOutcome::Action(TuiAction::Previous))
        );
        assert_eq!(
            left_click(&app, area, prev_x + 10, row),
            Some(MouseOutcome::Action(TuiAction::PlayPause))
        );
        // The ⏭ chip used to fire PlayPause under the equal-thirds
        // split — the marquee regression this suite guards against.
        assert_eq!(
            left_click(&app, area, prev_x + 20, row),
            Some(MouseOutcome::Action(TuiAction::Next))
        );
        // The gap between chips is dead, not a misfire.
        assert_eq!(left_click(&app, area, prev_x + 5, row), None);
    }

    #[test]
    fn volume_bar_click_sets_absolute_volume() {
        let mut app = test_app();
        let area = Rect::new(0, 0, 120, 32);
        let buffer = render_frame(&mut app, 120, 32);
        let player_top = 32 - 13;
        let (prev_x, primary_row) = find_text(&buffer, "⏮", player_top);
        // prev glyph sits at transport local col 4 → transport.x = prev_x - 5.
        let transport_x = prev_x - 5;
        let bar = crate::ui::transport_volume_bar_range(false);
        let mid = transport_x + 1 + bar.start + (bar.end - bar.start) / 2;
        let outcome = left_click(&app, area, mid, primary_row + 4);
        assert!(
            matches!(outcome, Some(MouseOutcome::Volume(_))),
            "expected Volume, got {outcome:?}"
        );
        if let Some(MouseOutcome::Volume(percent)) = outcome {
            assert!((40..=60).contains(&percent), "got {percent}%");
        }
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

        // Resolve Library's drawn cell through the same layout the
        // renderer and hit-test share, so the click targets the chip
        // wherever the responsive tab strip puts it.
        let tabs = body_tabs_area(area);
        let (_, ranges) = ui::tab_strip_layout(0, tabs.width);
        let library = Screen::ALL
            .iter()
            .position(|screen| *screen == Screen::Library)
            .expect("library screen exists");
        let range = ranges
            .iter()
            .find(|(index, _)| *index == library)
            .map(|(_, range)| range.clone())
            .expect("library tab visible at 120 cols");
        let column = tabs.x + range.start + (range.end - range.start) / 2;

        assert_eq!(
            mouse_outcome(
                &app,
                area,
                mouse(MouseEventKind::Down(MouseButton::Left), column, 1)
            ),
            Some(MouseOutcome::Action(TuiAction::OpenLibrary))
        );
    }

    #[test]
    fn mouse_click_on_search_group_row_selects_that_item() {
        let mut app = test_app();
        app.screen = Screen::Search;
        app.search_results = vec![
            item("spotify:track:first", "First Song"),
            item("spotify:track:second", "Second Song"),
            item("spotify:artist:artist-one", "Artist One"),
        ];
        app.search_results[2].kind = MediaKind::Artist;
        let area = Rect::new(0, 0, 140, 32);
        let buffer = render_frame(&mut app, 140, 32);

        // Click the drawn rows; the registered targets carry the
        // FULL-list index, including across group panes.
        let (x, y) = find_text(&buffer, "Second Song", 0);
        assert_eq!(left_click(&app, area, x, y), Some(MouseOutcome::Select(1)));
        let (x, y) = find_text(&buffer, "Artist One", 0);
        assert_eq!(left_click(&app, area, x, y), Some(MouseOutcome::Select(2)));
    }

    #[test]
    fn mouse_click_on_home_feed_selects_item_without_selecting_queue_panel() {
        let mut app = test_app();
        app.screen = Screen::Player;
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
        let area = Rect::new(0, 0, 140, 32);
        let buffer = render_frame(&mut app, 140, 32);

        // Clicking a feed row selects ITS index in the visible list,
        // wherever the responsive layout put the podcasts column.
        let (x, y) = find_text(&buffer, "Saved Show", 0);
        let expected = app
            .visible_items()
            .iter()
            .position(|i| i.name == "Saved Show")
            .expect("show in home feed");
        assert_eq!(
            left_click(&app, area, x, y),
            Some(MouseOutcome::Select(expected))
        );
        // The home queue panel is informational — clicks there must
        // not move the feed selection.
        let (qx, qy) = find_text(&buffer, "Next Queue Track", 0);
        assert_eq!(left_click(&app, area, qx, qy), None);
    }

    #[test]
    fn mouse_click_on_progress_maps_to_seek_position() {
        let mut app = test_app();
        app.playback.item = Some(item("spotify:track:first", "First"));
        let area = Rect::new(0, 0, 120, 32);
        let buffer = render_frame(&mut app, 120, 32);

        // The hit-test and the renderer share `track_gauge_rect`;
        // prove the gauge really is drawn there (its time label sits
        // on that row), then click it.
        let player = bottom_player_area(area);
        let inner = rect_inner(
            player,
            Margin {
                horizontal: 1,
                vertical: 1,
            },
        );
        let track = crate::ui::now_playing_layout(inner)
            .track
            .expect("track panel visible at 120 cols");
        let gauge = crate::ui::track_gauge_rect(track);
        let (_, label_row) = find_text(&buffer, "/ 3:00", 0);
        assert_eq!(label_row, gauge.y, "gauge drawn on the hit-test row");

        let outcome = left_click(&app, area, gauge.x + gauge.width / 2, gauge.y);
        assert!(
            matches!(outcome, Some(MouseOutcome::Seek(_))),
            "expected Seek, got {outcome:?}"
        );
        if let Some(MouseOutcome::Seek(ms)) = outcome {
            // 3-minute track, mid-gauge click ≈ halfway.
            assert!((60_000..=120_000).contains(&ms), "{ms}");
        }
        // The row above the gauge is NOT a seek target (the old
        // hit-test sat one row off and made the visible gauge dead).
        assert_eq!(
            left_click(&app, area, gauge.x + gauge.width / 2, gauge.y - 1),
            None
        );
    }

    #[test]
    fn devices_clicks_select_the_clicked_device() {
        let mut app = test_app();
        app.screen = Screen::Devices;
        app.devices = vec![
            device("d1", "Device One", true, false),
            device("d2", "Device Two", false, false),
            device("d3", "Device Three", false, false),
        ];
        let area = Rect::new(0, 0, 120, 32);
        let buffer = render_frame(&mut app, 120, 32);

        // Devices render 2 table rows apiece; the old 1-row mapping
        // selected device 2k for a click on device k (and Enter then
        // transferred playback to the WRONG device).
        let (x, y) = find_text(&buffer, "Device Two", 0);
        assert_eq!(left_click(&app, area, x, y), Some(MouseOutcome::Select(1)));
        let (x, y) = find_text(&buffer, "Device Three", 0);
        assert_eq!(left_click(&app, area, x, y), Some(MouseOutcome::Select(2)));
    }

    #[test]
    fn scrolled_queue_clicks_select_the_clicked_row() {
        let mut app = test_app();
        app.screen = Screen::Queue;
        app.queue.session_active = true;
        app.queue.items = (0..30)
            .map(|i| item(&format!("spotify:track:q{i:02}"), &format!("Qrow {i:02}")))
            .collect();
        app.selected = 20;
        let area = Rect::new(0, 0, 100, 40);
        let buffer = render_frame(&mut app, 100, 40);

        // With the cursor deep in the list the List widget scrolls;
        // the hit map records what was ACTUALLY drawn, so a click on a
        // visible row selects that row, not row-minus-offset.
        let (x, y) = find_text(&buffer, "Qrow 20", 0);
        assert_eq!(left_click(&app, area, x, y), Some(MouseOutcome::Select(20)));
        // One row above the cursor is always inside the scroll window.
        let (x, y) = find_text(&buffer, "Qrow 19", 0);
        assert_eq!(left_click(&app, area, x, y), Some(MouseOutcome::Select(19)));
    }

    #[test]
    fn playlist_list_clicks_select_the_clicked_playlist() {
        let mut app = test_app();
        app.screen = Screen::Playlists;
        app.playlists = (0..3)
            .map(|i| Playlist {
                id: format!("pl{i}"),
                name: format!("Playlist Number {i}"),
                owner: "me".to_string(),
                tracks_total: 5,
                image_url: None,
                snapshot_id: None,
            })
            .collect();
        let area = Rect::new(0, 0, 120, 32);
        let buffer = render_frame(&mut app, 120, 32);

        let (x, y) = find_text(&buffer, "Playlist Number 1", 0);
        assert_eq!(left_click(&app, area, x, y), Some(MouseOutcome::Select(1)));
    }

    #[test]
    fn modal_blocks_all_mouse_input() {
        let mut app = test_app();
        let area = Rect::new(0, 0, 120, 32);
        let buffer = render_frame(&mut app, 120, 32);
        let player_top = 32 - 13;
        let (prev_x, row) = find_text(&buffer, "⏮", player_top);
        app.confirm_modal = Some(ConfirmModal {
            title: "Delete playlist?".to_string(),
            body: "really?".to_string(),
            on_confirm: TuiAction::DeleteSelectedPlaylist,
        });

        // With a destructive confirm open, clicks (and scroll-wheel
        // volume) must not act on the screen underneath.
        assert_eq!(left_click(&app, area, prev_x + 10, row), None);
        assert_eq!(
            mouse_outcome(&app, area, mouse(MouseEventKind::ScrollUp, 60, row)),
            None
        );
    }

    #[test]
    fn mouse_click_on_rail_header_expands_or_hides_rail() {
        let mut app = test_app();
        app.right_rail = RightRailMode::Queue;
        let area = Rect::new(0, 0, 140, 32);
        let buffer = render_frame(&mut app, 140, 32);

        // Clicking the DRAWN "Q hide" text hides the rail (the old
        // hotspot was the rightmost 10 columns while the text sat on
        // the left of the title). min_y=2 skips the tab strip's
        // "Queue" tab.
        let (hide_x, hide_y) = find_text(&buffer, "Q hide", 2);
        assert_eq!(
            left_click(&app, area, hide_x, hide_y),
            Some(MouseOutcome::Action(TuiAction::ToggleQueueRail))
        );
        // Title text outside the chip expands to fullscreen.
        assert_eq!(
            left_click(&app, area, hide_x.saturating_sub(5), hide_y),
            Some(MouseOutcome::Action(TuiAction::ToggleRailFullscreen))
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
        app.library_items = vec![item("spotify:track:first", "First")];

        assert!(player_space_should_play_selected(&app));

        app.playback.item = Some(item("spotify:track:current", "Current"));
        assert!(!player_space_should_play_selected(&app));

        if let Some(current) = app.playback.item.as_mut() {
            current.duration_ms = 180_000;
        }
        app.playback.progress_ms = 180_000;
        assert!(player_space_should_play_selected(&app));
    }

    #[test]
    fn playlist_space_can_start_selected_playlist_when_idle() {
        let mut app = test_app();
        app.screen = Screen::Playlists;
        app.playlists = vec![Playlist {
            id: "quiet-storm".to_string(),
            name: "Quiet Storm".to_string(),
            owner: "me".to_string(),
            tracks_total: 12,
            image_url: None,
            snapshot_id: None,
        }];

        assert!(player_space_should_play_selected(&app));
    }

    #[test]
    fn player_home_uses_saved_music_without_a_live_queue() {
        let mut app = test_app();
        app.screen = Screen::Player;
        app.queue.session_active = false;
        app.queue.items = vec![item("spotify:track:stale", "Stale Queue Track")];
        app.library_items = vec![
            item("spotify:track:first", "First Saved Track"),
            item_kind("spotify:album:album", "Saved Album", MediaKind::Album),
            item_kind("spotify:show:show", "Saved Show", MediaKind::Show),
            item_kind(
                "spotify:episode:episode",
                "Saved Episode",
                MediaKind::Episode,
            ),
            item_kind(
                "spotify:artist:artist",
                "Followed Artist",
                MediaKind::Artist,
            ),
        ];

        let visible = app.visible_items();
        let uris = visible
            .iter()
            .map(|item| item.uri.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            uris,
            vec![
                "spotify:track:first",
                "spotify:album:album",
                "spotify:show:show",
                "spotify:episode:episode",
            ]
        );
        assert!(player_space_should_play_selected(&app));
    }

    #[test]
    fn stale_queue_screen_is_not_visible_or_playable() {
        let mut app = test_app();
        app.queue.session_active = false;
        app.queue.items = vec![item("spotify:track:first", "First")];

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
    fn playlist_list_queue_request_targets_whole_playlist() {
        let mut app = test_app();
        app.screen = Screen::Playlists;
        app.playlists = vec![Playlist {
            id: "quiet-storm".to_string(),
            name: "Quiet Storm".to_string(),
            owner: "me".to_string(),
            tracks_total: 12,
            image_url: None,
            snapshot_id: None,
        }];

        let requests = app.requests_for_action(TuiAction::QueueSelection);

        assert_eq!(
            requests,
            vec![Request::QueueAdd {
                uri: "spotify:playlist:quiet-storm".to_string(),
            }]
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

    #[test]
    fn system_default_audio_output_clears_config_override() {
        assert_eq!(audio_output_config_value(SYSTEM_AUDIO_OUTPUT_LABEL), "");
        assert_eq!(
            audio_output_toast_label(SYSTEM_AUDIO_OUTPUT_LABEL),
            "system default"
        );
        assert_eq!(
            audio_output_config_value("MacBook Pro Speakers"),
            "MacBook Pro Speakers"
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
            .map(|d| d.id.unwrap_or_default())
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
            .map(|d| d.id.unwrap_or_default())
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
            .map(|device| device.id.unwrap_or_default())
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
            queue: Some(queue),
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
    fn refresh_plan_loads_stale_library_when_library_visible() {
        let mut app = test_app();
        app.screen = Screen::Library;

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
    fn refresh_plan_skips_library_on_player_startup() {
        let app = test_app();

        let plan = refresh_plan(&app);

        assert_eq!(
            plan,
            RefreshPlan {
                library: false,
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
    fn force_media_refresh_requests_cover_and_lyrics_for_current_track() {
        let mut app = test_app();
        let mut current = item("spotify:track:lyrics", "Lyrics Track");
        current.image_url = Some("https://i.scdn.co/image/current".to_string());
        app.playback.item = Some(current);
        let (tx, _rx) = mpsc::unbounded_channel();

        request_force_media(&mut app, &tx);

        assert_eq!(
            app.current_art_url.as_deref(),
            Some("https://i.scdn.co/image/current")
        );
        assert!(app.lyrics_loading);
        assert!(app.lyrics_error.is_none());
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
            ..Default::default()
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
        let mut incoming = original;
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
        let mut incoming = original;
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
        let mut incoming = original;
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
    fn selected_artwork_tracks_playlist_selection_url() {
        let mut app = test_app();
        let (tx, _rx) = mpsc::unbounded_channel();
        app.screen = Screen::Playlists;
        app.playlists = vec![
            Playlist {
                id: "plain".to_string(),
                name: "Plain".to_string(),
                owner: "Me".to_string(),
                tracks_total: 3,
                image_url: None,
                snapshot_id: None,
            },
            Playlist {
                id: "art".to_string(),
                name: "Art".to_string(),
                owner: "Me".to_string(),
                tracks_total: 9,
                image_url: Some("https://example.com/art.jpg".to_string()),
                snapshot_id: None,
            },
        ];

        app.playlist_selected = 1;
        app.sync_selected_artwork(&tx);
        assert_eq!(
            app.selected_art_url.as_deref(),
            Some("https://example.com/art.jpg")
        );

        app.playlist_selected = 0;
        app.selected_art_cover = Some(
            app.picker
                .new_resize_protocol(image::DynamicImage::new_rgb8(1, 1)),
        );
        app.sync_selected_artwork(&tx);
        assert!(app.selected_art_url.is_none());
        assert!(
            app.selected_art_cover.is_none(),
            "cover protocol must clear when selected item has no image"
        );
    }

    #[test]
    fn selected_artwork_drops_stale_fetch_results() {
        let mut app = test_app();
        app.selected_art_url = Some("https://example.com/current.jpg".to_string());
        let image = image::DynamicImage::new_rgb8(1, 1);

        app.apply_async_result(AsyncResult::SelectedArtFetched {
            url: "https://example.com/old.jpg".to_string(),
            image: image.clone(),
        });
        assert!(
            app.selected_art_cover.is_none(),
            "stale selected-art fetch must not install"
        );

        app.apply_async_result(AsyncResult::SelectedArtFetched {
            url: "https://example.com/current.jpg".to_string(),
            image,
        });
        assert!(
            app.selected_art_cover.is_some(),
            "matching selected-art fetch should install"
        );
    }

    #[test]
    fn selected_artwork_subject_includes_albums_playlists_and_podcasts() {
        let mut app = test_app();
        app.screen = Screen::Library;
        app.library_items = vec![
            item_kind("spotify:track:t", "Track", MediaKind::Track),
            {
                let mut album = item_kind("spotify:album:a", "Album", MediaKind::Album);
                album.image_url = Some("https://example.com/album.jpg".to_string());
                album
            },
            {
                let mut show = item_kind("spotify:show:s", "Show", MediaKind::Show);
                show.image_url = Some("https://example.com/show.jpg".to_string());
                show
            },
            {
                let mut episode = item_kind("spotify:episode:e", "Episode", MediaKind::Episode);
                episode.image_url = Some("https://example.com/episode.jpg".to_string());
                episode
            },
        ];

        app.selected = 0;
        assert!(app.selected_artwork_subject().is_none());

        app.selected = 1;
        let subject = app
            .selected_artwork_subject()
            .expect("album selection should expose artwork subject");
        assert_eq!(subject.title, "Album");
        assert_eq!(
            subject.image_url.as_deref(),
            Some("https://example.com/album.jpg")
        );

        app.selected = 2;
        let subject = app
            .selected_artwork_subject()
            .expect("show selection should expose artwork subject");
        assert_eq!(subject.title, "Show");
        assert_eq!(
            subject.image_url.as_deref(),
            Some("https://example.com/show.jpg")
        );

        app.selected = 3;
        let subject = app
            .selected_artwork_subject()
            .expect("episode selection should expose artwork subject");
        assert_eq!(subject.title, "Episode");
        assert_eq!(
            subject.image_url.as_deref(),
            Some("https://example.com/episode.jpg")
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

    #[test]
    fn palette_updates_only_for_matching_active_cover_url() {
        let mut app = test_app();
        app.current_art_url = Some("https://e.com/current.jpg".to_string());
        let stale = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            2,
            2,
            image::Rgb([240, 20, 20]),
        ));
        app.apply_async_result(AsyncResult::CoverFetched {
            url: "https://e.com/old.jpg".to_string(),
            image: stale,
        });
        assert_eq!(app.palette, UiPalette::default());

        let current = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            2,
            2,
            image::Rgb([20, 80, 240]),
        ));
        app.apply_async_result(AsyncResult::CoverFetched {
            url: "https://e.com/current.jpg".to_string(),
            image: current,
        });
        if terminal_color_enabled() {
            assert_ne!(app.palette, UiPalette::default());
        } else {
            assert_eq!(app.palette, UiPalette::default());
        }
    }

    #[test]
    fn art_url_change_resets_palette_until_new_cover_decodes() {
        let mut app = test_app();
        app.palette = UiPalette {
            accent: ratatui::style::Color::Rgb(240, 20, 20),
            ..UiPalette::default()
        };
        let (tx, _rx) = mpsc::unbounded_channel();
        app.handle_art_url_change(Some("https://e.com/new.jpg".to_string()), &tx);
        assert_eq!(app.palette, UiPalette::default());
    }

    #[test]
    fn command_result_playback_updates_active_cover_url() {
        let mut app = test_app();
        let result = CommandResult {
            playback: Some(playback_with(
                track_with_image("spotify:track:command", Some("https://e.com/command.jpg")),
                0,
            )),
            ..CommandResult::default()
        };

        app.apply_async_result(AsyncResult::Command(Box::new(Ok(result))));

        assert_eq!(
            app.current_art_url.as_deref(),
            Some("https://e.com/command.jpg"),
            "command result playback must use the same cover path as daemon events"
        );
        assert!(
            app.playback_updated_at.is_some(),
            "command result playback must stamp event freshness"
        );
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
    fn forbidden_playlist_tracks_hide_only_that_playlist_after_failure() {
        let mut app = test_app();
        app.playlists = vec![
            Playlist {
                id: "p1".to_string(),
                name: "Hidden Playlist".to_string(),
                owner: "owner".to_string(),
                tracks_total: 12,
                image_url: None,
                snapshot_id: None,
            },
            Playlist {
                id: "followed".to_string(),
                name: "Followed Playlist".to_string(),
                owner: "third-party".to_string(),
                tracks_total: 5,
                image_url: None,
                snapshot_id: None,
            },
        ];

        app.apply_async_result(AsyncResult::PlaylistTracks {
            playlist_id: "p1".to_string(),
            playlist_name: "Hidden Playlist".to_string(),
            expected_total: 12,
            result: Err("Spotify API 403 on GET /playlists/p1/items: Forbidden".to_string()),
        });

        let visible = app.filtered_playlists();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, "followed");
        assert!(app.error.is_none());
        assert_eq!(
            app.toast.as_deref(),
            Some("Tracks for Hidden Playlist are restricted by Spotify for third-party apps")
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

    #[test]
    fn parse_offset_ms_handles_units_and_rejects_garbage() {
        assert_eq!(parse_offset_ms("+2h"), Some(7_200_000));
        assert_eq!(parse_offset_ms("3d"), Some(259_200_000));
        assert_eq!(parse_offset_ms("1w"), Some(604_800_000));
        assert_eq!(parse_offset_ms("+90m"), Some(5_400_000));
        assert_eq!(parse_offset_ms("nonsense"), None);
        assert_eq!(parse_offset_ms("+5y"), None);
        assert_eq!(parse_offset_ms("+"), None);
    }

    #[test]
    fn cycle_recurrence_wraps_through_all_modes() {
        assert_eq!(cycle_recurrence(Recurrence::None), Recurrence::Daily);
        assert_eq!(cycle_recurrence(Recurrence::Daily), Recurrence::Weekly);
        assert_eq!(cycle_recurrence(Recurrence::Weekly), Recurrence::Monthly);
        assert_eq!(cycle_recurrence(Recurrence::Monthly), Recurrence::None);
    }

    #[test]
    fn resolve_custom_preset_applies_offset_and_rejects_bad_input() {
        let custom = REMINDER_PRESETS.len() - 1;
        let now = chrono::Local::now().timestamp_millis();
        let at = resolve_reminder_preset(custom, "+1h").expect("custom offset resolves");
        assert!((at - now - 3_600_000).abs() < 5_000, "≈ now + 1h");
        assert!(resolve_reminder_preset(custom, "garbage").is_none());
        // A non-custom preset always yields a concrete future time.
        assert!(resolve_reminder_preset(2, "").is_some());
    }
}
