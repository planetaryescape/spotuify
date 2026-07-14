#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TuiAction {
    Quit,
    Help,
    OpenCommandPalette,
    OpenPlayer,
    OpenSearch,
    OpenLibrary,
    OpenPlaylists,
    OpenPodcasts,
    OpenQueue,
    OpenHistory,
    OpenDevicePicker,
    OpenDiagnostics,
    OpenLyrics,
    OpenNotifications,
    MoveDown,
    MoveUp,
    PageDown,
    PageUp,
    JumpTop,
    JumpBottom,
    Back,
    Refresh,
    RefreshMedia,
    StartSearchInput,
    StartListFilter,
    SubmitSearch,
    CancelInput,
    PlayPause,
    Next,
    Previous,
    SeekBack,
    SeekForward,
    VolumeUp,
    VolumeDown,
    ToggleShuffle,
    CycleRepeat,
    OpenSelected,
    /// Navigate from the selected item to its artist (track/album → artist).
    OpenSelectedArtist,
    /// Navigate from the selected item to its album (track → album).
    OpenSelectedAlbum,
    PlaySelected,
    QueueSelection,
    LikeSelection,
    RemindMe,
    AddSelectionToPlaylist,
    /// Remove the selected playlist from the library (confirm-gated).
    DeleteSelectedPlaylist,
    /// Unsave the marked/selected liked tracks (confirm-gated).
    UnsaveSelection,
    ToggleMark,
    MarkRange,
    ClearMarks,
    TogglePlayerMode,
    /// Phase 12 (P12-F) — undo the most-recent reversible operation
    /// from the Diagnostics ops panel.
    UndoLastOperation,
    /// Phase 17 — cycle the visualizer source (Auto → Sink → Loopback →
    /// None → Auto). Sends `Request::SetVizSource` to the daemon.
    CycleVizSource,
    /// Phase 17 — toggle the visualizer on/off. Sends
    /// `Request::SetVizEnabled` to the daemon.
    ToggleViz,
    ToggleQueueRail,
    ToggleLyricsRail,
    ToggleHintsRail,
    ToggleRailFullscreen,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ActionContext {
    Player,
    SearchInput,
    SearchResults,
    Library,
    Playlists,
    PlaylistTracks,
    Podcasts,
    PodcastEpisodes,
    Queue,
    Diagnostics,
    Lyrics,
    Notifications,
    MultiSelect,
}

impl ActionContext {
    pub fn label(self) -> &'static str {
        match self {
            Self::Player => "Home",
            Self::SearchInput => "Search input",
            Self::SearchResults => "Search results",
            Self::Library => "Library",
            Self::Playlists => "Playlists",
            Self::PlaylistTracks => "Playlist tracks",
            Self::Podcasts => "Podcasts",
            Self::PodcastEpisodes => "Podcast episodes",
            Self::Queue => "Queue",
            Self::Diagnostics => "Diagnostics",
            Self::Lyrics => "Lyrics",
            Self::Notifications => "Notifications",
            Self::MultiSelect => "Multi-select",
        }
    }
}

const ALL_CONTEXTS: &[ActionContext] = &[
    ActionContext::Player,
    ActionContext::SearchInput,
    ActionContext::SearchResults,
    ActionContext::Library,
    ActionContext::Playlists,
    ActionContext::PlaylistTracks,
    ActionContext::Podcasts,
    ActionContext::PodcastEpisodes,
    ActionContext::Queue,
    ActionContext::Diagnostics,
    ActionContext::Lyrics,
    ActionContext::Notifications,
    ActionContext::MultiSelect,
];

const BROWSABLE_CONTEXTS: &[ActionContext] = &[
    ActionContext::SearchResults,
    ActionContext::Library,
    ActionContext::PlaylistTracks,
    ActionContext::PodcastEpisodes,
    ActionContext::Queue,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActionSpec {
    pub id: TuiAction,
    pub label: &'static str,
    pub shortcut: &'static str,
    pub contexts: &'static [ActionContext],
    pub category: &'static str,
    pub cli: Option<&'static str>,
}

impl ActionSpec {
    fn matches_context(self, context: ActionContext) -> bool {
        self.contexts.contains(&context)
    }
}

pub fn default_actions() -> Vec<ActionSpec> {
    use ActionContext as C;
    use TuiAction as A;

    vec![
        ActionSpec {
            id: A::OpenPlayer,
            label: "Home",
            shortcut: "1",
            contexts: ALL_CONTEXTS,
            category: "Navigation",
            cli: Some("spotuify status"),
        },
        ActionSpec {
            id: A::OpenSearch,
            label: "Search",
            shortcut: "2",
            contexts: ALL_CONTEXTS,
            category: "Navigation",
            cli: Some("spotuify search QUERY"),
        },
        ActionSpec {
            id: A::OpenLibrary,
            label: "Library",
            shortcut: "3",
            contexts: ALL_CONTEXTS,
            category: "Navigation",
            cli: Some("spotuify library tracks"),
        },
        ActionSpec {
            id: A::OpenPlaylists,
            label: "Playlists",
            shortcut: "4",
            contexts: ALL_CONTEXTS,
            category: "Navigation",
            cli: Some("spotuify playlists"),
        },
        ActionSpec {
            id: A::OpenPodcasts,
            label: "Podcasts",
            shortcut: "5",
            contexts: ALL_CONTEXTS,
            category: "Navigation",
            cli: Some("spotuify library shows"),
        },
        ActionSpec {
            id: A::OpenHistory,
            label: "History",
            shortcut: "6",
            contexts: ALL_CONTEXTS,
            category: "Navigation",
            cli: Some("spotuify history"),
        },
        ActionSpec {
            id: A::OpenNotifications,
            label: "Notifications",
            shortcut: "7",
            contexts: ALL_CONTEXTS,
            category: "Reminders",
            cli: Some("spotuify notifications list"),
        },
        ActionSpec {
            id: A::OpenQueue,
            label: "Queue Fullscreen",
            shortcut: "Alt-q",
            contexts: ALL_CONTEXTS,
            category: "View",
            cli: Some("spotuify queue"),
        },
        ActionSpec {
            id: A::OpenDevicePicker,
            label: "Devices",
            shortcut: "D",
            contexts: ALL_CONTEXTS,
            category: "Navigation",
            cli: Some("spotuify devices"),
        },
        ActionSpec {
            id: A::OpenDiagnostics,
            label: "Diagnostics",
            shortcut: "Ctrl-p",
            contexts: ALL_CONTEXTS,
            category: "Diagnostics",
            cli: Some("spotuify doctor"),
        },
        ActionSpec {
            id: A::OpenLyrics,
            label: "Lyrics Fullscreen",
            shortcut: "Ctrl-p",
            contexts: ALL_CONTEXTS,
            category: "View",
            cli: Some("spotuify lyrics show"),
        },
        ActionSpec {
            id: A::ToggleQueueRail,
            label: "Queue Rail",
            shortcut: "Q",
            contexts: ALL_CONTEXTS,
            category: "View",
            cli: Some("spotuify queue"),
        },
        ActionSpec {
            id: A::ToggleLyricsRail,
            label: "Lyrics Rail",
            shortcut: "L",
            contexts: ALL_CONTEXTS,
            category: "View",
            cli: Some("spotuify lyrics show"),
        },
        ActionSpec {
            id: A::ToggleHintsRail,
            label: "Hints Rail",
            shortcut: "H",
            contexts: ALL_CONTEXTS,
            category: "View",
            cli: Some("spotuify --help"),
        },
        ActionSpec {
            id: A::ToggleRailFullscreen,
            label: "Expand Rail",
            shortcut: "F",
            contexts: ALL_CONTEXTS,
            category: "View",
            cli: None,
        },
        ActionSpec {
            id: A::OpenCommandPalette,
            label: "Command Palette",
            shortcut: "Ctrl-p",
            contexts: ALL_CONTEXTS,
            category: "Navigation",
            cli: None,
        },
        ActionSpec {
            id: A::Help,
            label: "Help",
            shortcut: "?",
            contexts: ALL_CONTEXTS,
            category: "Help",
            cli: None,
        },
        ActionSpec {
            id: A::Quit,
            label: "Quit TUI",
            shortcut: "q",
            contexts: ALL_CONTEXTS,
            category: "Navigation",
            cli: None,
        },
        ActionSpec {
            id: A::Refresh,
            label: "Refresh",
            shortcut: "u",
            contexts: ALL_CONTEXTS,
            category: "Sync",
            cli: Some("spotuify sync playback"),
        },
        ActionSpec {
            id: A::RefreshMedia,
            label: "Refresh Media",
            shortcut: "U",
            contexts: ALL_CONTEXTS,
            category: "Sync",
            cli: Some("spotuify refresh-media"),
        },
        ActionSpec {
            id: A::MoveDown,
            label: "Move Down",
            shortcut: "j",
            contexts: ALL_CONTEXTS,
            category: "Navigation",
            cli: None,
        },
        ActionSpec {
            id: A::MoveUp,
            label: "Move Up",
            shortcut: "k",
            contexts: ALL_CONTEXTS,
            category: "Navigation",
            cli: None,
        },
        ActionSpec {
            id: A::PageDown,
            label: "Page Down",
            shortcut: "Ctrl-d",
            contexts: ALL_CONTEXTS,
            category: "Navigation",
            cli: None,
        },
        ActionSpec {
            id: A::PageUp,
            label: "Page Up",
            shortcut: "Ctrl-u",
            contexts: ALL_CONTEXTS,
            category: "Navigation",
            cli: None,
        },
        ActionSpec {
            id: A::JumpTop,
            label: "Jump Top",
            shortcut: "gg",
            contexts: ALL_CONTEXTS,
            category: "Navigation",
            cli: None,
        },
        ActionSpec {
            id: A::JumpBottom,
            label: "Jump Bottom",
            shortcut: "G",
            contexts: ALL_CONTEXTS,
            category: "Navigation",
            cli: None,
        },
        ActionSpec {
            id: A::Back,
            label: "Back",
            shortcut: "Esc",
            contexts: ALL_CONTEXTS,
            category: "Navigation",
            cli: None,
        },
        ActionSpec {
            id: A::StartSearchInput,
            label: "Global Search",
            shortcut: "/",
            contexts: ALL_CONTEXTS,
            category: "Search",
            cli: Some("spotuify search QUERY"),
        },
        ActionSpec {
            id: A::StartListFilter,
            label: "Filter Current List",
            shortcut: "Ctrl-f",
            contexts: &[
                C::SearchResults,
                C::Library,
                C::Playlists,
                C::PlaylistTracks,
                C::Podcasts,
                C::PodcastEpisodes,
                C::Queue,
            ],
            category: "Search",
            cli: None,
        },
        ActionSpec {
            id: A::SubmitSearch,
            label: "Search",
            shortcut: "Enter",
            contexts: &[C::SearchInput],
            category: "Search",
            cli: Some("spotuify search QUERY"),
        },
        ActionSpec {
            id: A::CancelInput,
            label: "Cancel Input",
            shortcut: "Esc",
            contexts: &[C::SearchInput],
            category: "Navigation",
            cli: None,
        },
        ActionSpec {
            id: A::PlayPause,
            label: "Play/Pause",
            shortcut: "Space",
            contexts: ALL_CONTEXTS,
            category: "Player",
            cli: Some("spotuify toggle"),
        },
        ActionSpec {
            id: A::Next,
            label: "Next",
            shortcut: "n",
            contexts: &[C::Player, C::Queue],
            category: "Player",
            cli: Some("spotuify next"),
        },
        ActionSpec {
            id: A::Previous,
            label: "Previous",
            shortcut: "p",
            contexts: &[C::Player, C::Queue],
            category: "Player",
            cli: Some("spotuify previous"),
        },
        ActionSpec {
            id: A::SeekBack,
            label: "Seek Back 15s",
            shortcut: "Left",
            contexts: &[C::Player, C::Queue],
            category: "Player",
            cli: Some("spotuify seek -15s"),
        },
        ActionSpec {
            id: A::SeekForward,
            label: "Seek Forward 15s",
            shortcut: "Right",
            contexts: &[C::Player, C::Queue],
            category: "Player",
            cli: Some("spotuify seek +15s"),
        },
        ActionSpec {
            id: A::VolumeUp,
            label: "Volume Up",
            shortcut: "+",
            contexts: &[C::Player, C::Queue],
            category: "Player",
            cli: Some("spotuify volume PERCENT"),
        },
        ActionSpec {
            id: A::VolumeDown,
            label: "Volume Down",
            shortcut: "-",
            contexts: &[C::Player, C::Queue],
            category: "Player",
            cli: Some("spotuify volume PERCENT"),
        },
        ActionSpec {
            id: A::ToggleShuffle,
            label: "Shuffle",
            shortcut: "s",
            contexts: &[C::Player, C::Queue],
            category: "Player",
            cli: Some("spotuify shuffle toggle"),
        },
        ActionSpec {
            id: A::CycleRepeat,
            label: "Repeat",
            shortcut: "r",
            contexts: &[C::Player, C::Queue],
            category: "Player",
            cli: Some("spotuify repeat off|context|track"),
        },
        ActionSpec {
            id: A::PlaySelected,
            label: "Play Selected",
            shortcut: "Enter",
            contexts: BROWSABLE_CONTEXTS,
            category: "Player",
            cli: Some("spotuify play-uri URI"),
        },
        ActionSpec {
            id: A::OpenSelected,
            label: "Open Selected",
            shortcut: "Enter",
            contexts: &[C::Playlists, C::Podcasts],
            category: "Navigation",
            cli: Some("spotuify playlist tracks PLAYLIST | spotuify show episodes SHOW"),
        },
        ActionSpec {
            id: A::QueueSelection,
            label: "Queue Selected",
            shortcut: "e",
            contexts: &[
                C::Player,
                C::SearchResults,
                C::Library,
                C::Playlists,
                C::PlaylistTracks,
                C::PodcastEpisodes,
                C::Queue,
                C::MultiSelect,
            ],
            category: "Queue",
            cli: Some("spotuify queue add URI"),
        },
        ActionSpec {
            id: A::LikeSelection,
            label: "Like Selected",
            shortcut: "l",
            contexts: &[
                C::Player,
                C::SearchResults,
                C::Library,
                C::PlaylistTracks,
                C::PodcastEpisodes,
                C::Queue,
                C::MultiSelect,
            ],
            category: "Library",
            cli: Some("spotuify like URI"),
        },
        ActionSpec {
            id: A::OpenSelectedArtist,
            label: "Go To Artist",
            shortcut: "o",
            contexts: &[
                C::SearchResults,
                C::Library,
                C::PlaylistTracks,
                C::PodcastEpisodes,
                C::Queue,
            ],
            category: "Navigate",
            cli: Some("spotuify artist albums URI"),
        },
        ActionSpec {
            id: A::OpenSelectedAlbum,
            label: "Go To Album",
            shortcut: "O",
            contexts: &[
                C::SearchResults,
                C::Library,
                C::PlaylistTracks,
                C::PodcastEpisodes,
                C::Queue,
            ],
            category: "Navigate",
            cli: Some("spotuify album tracks URI"),
        },
        ActionSpec {
            id: A::RemindMe,
            label: "Remind Me",
            shortcut: "R",
            contexts: &[
                C::Player,
                C::SearchResults,
                C::Library,
                C::PlaylistTracks,
                C::PodcastEpisodes,
                C::Queue,
                C::MultiSelect,
            ],
            category: "Reminders",
            cli: Some("spotuify reminder create URI --at +1d"),
        },
        ActionSpec {
            id: A::AddSelectionToPlaylist,
            label: "Add To Playlist",
            shortcut: "a",
            contexts: &[
                C::Player,
                C::SearchResults,
                C::Library,
                C::PlaylistTracks,
                C::PodcastEpisodes,
                C::Queue,
                C::Playlists,
                C::MultiSelect,
            ],
            category: "Playlists",
            cli: Some("spotuify playlist add PLAYLIST URI"),
        },
        ActionSpec {
            id: A::ToggleMark,
            label: "Mark Item",
            shortcut: "m",
            contexts: BROWSABLE_CONTEXTS,
            category: "Selection",
            cli: None,
        },
        ActionSpec {
            id: A::MarkRange,
            label: "Mark Range",
            shortcut: "M",
            contexts: BROWSABLE_CONTEXTS,
            category: "Selection",
            cli: None,
        },
        ActionSpec {
            id: A::ClearMarks,
            label: "Clear Marks",
            shortcut: "Esc",
            contexts: &[C::MultiSelect],
            category: "Selection",
            cli: None,
        },
        ActionSpec {
            id: A::TogglePlayerMode,
            label: "Toggle Player Size",
            shortcut: "z",
            contexts: &[C::Player],
            category: "View",
            cli: None,
        },
        ActionSpec {
            id: A::ToggleViz,
            label: "Toggle Visualizer",
            shortcut: "v",
            contexts: &[C::Player],
            category: "View",
            cli: Some("spotuify viz enable"),
        },
        ActionSpec {
            id: A::CycleVizSource,
            label: "Visualizer Source",
            shortcut: "V",
            contexts: &[C::Player],
            category: "View",
            cli: Some("spotuify viz source auto|sink|loopback|none"),
        },
        ActionSpec {
            id: A::UndoLastOperation,
            label: "Undo Last Operation",
            shortcut: "u",
            contexts: &[C::Diagnostics],
            category: "Diagnostics",
            cli: Some("spotuify ops undo"),
        },
    ]
}

pub fn effective_context(context: ActionContext, selected_count: usize) -> ActionContext {
    if selected_count > 0
        && matches!(
            context,
            ActionContext::SearchResults
                | ActionContext::Library
                | ActionContext::PlaylistTracks
                | ActionContext::PodcastEpisodes
                | ActionContext::Queue
        )
    {
        ActionContext::MultiSelect
    } else {
        context
    }
}

pub fn actions_for_context(context: ActionContext, selected_count: usize) -> Vec<ActionSpec> {
    let context = effective_context(context, selected_count);
    default_actions()
        .into_iter()
        .filter(|spec| spec.matches_context(context))
        .collect()
}

pub fn action_spec(action: TuiAction) -> Option<ActionSpec> {
    default_actions().into_iter().find(|spec| spec.id == action)
}

#[allow(dead_code)]
pub fn tui_only_reason(action: TuiAction) -> Option<&'static str> {
    match action {
        TuiAction::OpenCommandPalette => Some("client discovery surface"),
        TuiAction::Help => Some("client help overlay"),
        TuiAction::Quit => Some("closes the TUI client only"),
        TuiAction::MoveDown
        | TuiAction::MoveUp
        | TuiAction::PageDown
        | TuiAction::PageUp
        | TuiAction::JumpTop
        | TuiAction::JumpBottom
        | TuiAction::Back => Some("client navigation state"),
        TuiAction::StartListFilter => Some("client-side visible-list filter"),
        TuiAction::CancelInput => Some("client text input state"),
        TuiAction::ToggleMark | TuiAction::MarkRange | TuiAction::ClearMarks => {
            Some("client multi-select state")
        }
        TuiAction::TogglePlayerMode => Some("client layout preference"),
        TuiAction::ToggleViz => Some("client visualizer toggle"),
        TuiAction::CycleVizSource => Some("client visualizer source picker"),
        TuiAction::ToggleRailFullscreen => Some("client layout preference"),
        TuiAction::OpenPlayer
        | TuiAction::OpenSearch
        | TuiAction::OpenLibrary
        | TuiAction::OpenPlaylists
        | TuiAction::OpenPodcasts
        | TuiAction::OpenQueue
        | TuiAction::OpenDevicePicker
        | TuiAction::OpenHistory
        | TuiAction::OpenDiagnostics
        | TuiAction::OpenLyrics
        | TuiAction::OpenNotifications
        | TuiAction::Refresh
        | TuiAction::RefreshMedia
        | TuiAction::StartSearchInput
        | TuiAction::SubmitSearch
        | TuiAction::PlayPause
        | TuiAction::Next
        | TuiAction::Previous
        | TuiAction::SeekBack
        | TuiAction::SeekForward
        | TuiAction::VolumeUp
        | TuiAction::VolumeDown
        | TuiAction::ToggleShuffle
        | TuiAction::CycleRepeat
        | TuiAction::OpenSelected
        | TuiAction::OpenSelectedArtist
        | TuiAction::OpenSelectedAlbum
        | TuiAction::PlaySelected
        | TuiAction::QueueSelection
        | TuiAction::LikeSelection
        | TuiAction::RemindMe
        | TuiAction::AddSelectionToPlaylist
        | TuiAction::DeleteSelectedPlaylist
        | TuiAction::UnsaveSelection
        | TuiAction::UndoLastOperation
        | TuiAction::ToggleQueueRail
        | TuiAction::ToggleLyricsRail
        | TuiAction::ToggleHintsRail => None,
    }
}

pub fn top_hints(context: ActionContext, selected_count: usize) -> Vec<ActionSpec> {
    use ActionContext as C;
    use TuiAction as A;

    let context = effective_context(context, selected_count);
    let priority = match context {
        C::Player => &[
            A::PlayPause,
            A::QueueSelection,
            A::LikeSelection,
            A::ToggleQueueRail,
        ][..],
        C::SearchInput => &[A::SubmitSearch, A::CancelInput, A::OpenDevicePicker][..],
        C::SearchResults => &[
            A::PlaySelected,
            A::ToggleMark,
            A::QueueSelection,
            A::LikeSelection,
            A::OpenDevicePicker,
        ][..],
        C::Library => &[
            A::PlaySelected,
            A::ToggleMark,
            A::QueueSelection,
            A::LikeSelection,
            A::OpenDevicePicker,
        ][..],
        C::Podcasts => &[
            A::OpenSelected,
            A::StartListFilter,
            A::Refresh,
            A::OpenDevicePicker,
        ][..],
        C::PodcastEpisodes => &[
            A::PlaySelected,
            A::QueueSelection,
            A::ToggleMark,
            A::LikeSelection,
            A::OpenDevicePicker,
        ][..],
        C::Playlists => &[
            A::OpenSelected,
            A::QueueSelection,
            A::AddSelectionToPlaylist,
            A::StartListFilter,
            A::Refresh,
            A::OpenDevicePicker,
        ][..],
        C::PlaylistTracks => &[
            A::PlaySelected,
            A::QueueSelection,
            A::ToggleMark,
            A::LikeSelection,
            A::AddSelectionToPlaylist,
            A::OpenDevicePicker,
        ][..],
        C::Queue => &[
            A::PlaySelected,
            A::ToggleMark,
            A::QueueSelection,
            A::PlayPause,
            A::OpenDevicePicker,
        ][..],
        C::Diagnostics => &[
            A::Refresh,
            A::OpenDevicePicker,
            A::OpenCommandPalette,
            A::Quit,
        ][..],
        C::Lyrics => &[
            A::Refresh,
            A::ToggleRailFullscreen,
            A::OpenPlayer,
            A::OpenDevicePicker,
        ][..],
        C::Notifications => &[
            A::Refresh,
            A::OpenCommandPalette,
            A::OpenDevicePicker,
            A::Quit,
        ][..],
        C::MultiSelect => &[
            A::QueueSelection,
            A::LikeSelection,
            A::AddSelectionToPlaylist,
            A::ClearMarks,
            A::OpenDevicePicker,
        ][..],
    };

    // Priority actions first, then every other action available in
    // this context (registry order) so the hint bar can fill whatever
    // width the terminal has — a fixed five-hint cap left most of the
    // keymap invisible. Tab-bar navigation actions are skipped (the
    // tab strip already shows them) and Help is forced last so the
    // renderer can guarantee a trailing "? Help" escape hatch.
    let mut hints: Vec<ActionSpec> = priority
        .iter()
        .filter_map(|action| action_spec(*action))
        .filter(|spec| spec.matches_context(context))
        .collect();
    for spec in default_actions() {
        if !spec.matches_context(context)
            || hints.iter().any(|hint| hint.id == spec.id)
            || matches!(spec.id, A::Help)
            || is_tab_navigation(spec.id)
        {
            continue;
        }
        hints.push(spec);
    }
    if let Some(help) = action_spec(A::Help) {
        hints.push(help);
    }
    // The playlist LIST queues the whole playlist on `e` (the daemon
    // expands the playlist URI) — say so, "Queue" alone reads as a
    // single-track action and nobody finds it.
    if context == C::Playlists {
        for hint in &mut hints {
            if hint.id == A::QueueSelection {
                hint.label = "Queue playlist";
            }
        }
    }
    hints
}

/// Actions the tab strip already advertises (number keys); repeating
/// them in the hint bar wastes the row.
fn is_tab_navigation(action: TuiAction) -> bool {
    use TuiAction as A;
    matches!(
        action,
        A::OpenPlayer
            | A::OpenSearch
            | A::OpenLibrary
            | A::OpenPlaylists
            | A::OpenPodcasts
            | A::OpenHistory
            | A::OpenNotifications
    )
}

pub fn palette_commands(
    context: ActionContext,
    selected_count: usize,
    query: &str,
    recent_actions: &[TuiAction],
) -> Vec<ActionSpec> {
    let query = query.trim().to_ascii_lowercase();
    let recent_position = |action: TuiAction| -> usize {
        recent_actions
            .iter()
            .position(|recent| *recent == action)
            .unwrap_or(usize::MAX)
    };
    let mut scored = actions_for_context(context, selected_count)
        .into_iter()
        .enumerate()
        .filter_map(|(index, command)| {
            palette_match_score(command, &query)
                .map(|score| (command, score, recent_position(command.id), index))
        })
        .collect::<Vec<_>>();
    scored.sort_by_key(|(_, score, recency, index)| (*score, *recency, *index));
    scored
        .into_iter()
        .map(|(command, _, _, _)| command)
        .collect()
}

fn palette_match_score(command: ActionSpec, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    let label = command.label.to_ascii_lowercase();
    if label == query {
        return Some(0);
    }
    if label.starts_with(query) {
        return Some(1);
    }
    if label.split_whitespace().any(|word| word.starts_with(query)) {
        return Some(2);
    }
    if label.contains(query) {
        return Some(3);
    }
    let category = command.category.to_ascii_lowercase();
    let shortcut = command.shortcut.to_ascii_lowercase();
    let cli = command.cli.unwrap_or_default().to_ascii_lowercase();
    if category.contains(query) || shortcut.contains(query) || cli.contains(query) {
        return Some(4);
    }
    None
}

#[derive(Clone, Debug)]
pub struct CommandPalette {
    pub visible: bool,
    pub input: String,
    pub selected: usize,
    pub context: ActionContext,
    pub selected_count: usize,
    pub recent_actions: Vec<TuiAction>,
}

impl Default for CommandPalette {
    fn default() -> Self {
        Self {
            visible: false,
            input: String::new(),
            selected: 0,
            context: ActionContext::Player,
            selected_count: 0,
            recent_actions: Vec::new(),
        }
    }
}

impl CommandPalette {
    pub fn open(&mut self, context: ActionContext, selected_count: usize) {
        self.visible = true;
        self.input.clear();
        self.selected = 0;
        self.context = context;
        self.selected_count = selected_count;
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.input.clear();
        self.selected = 0;
    }

    pub fn on_char(&mut self, c: char) {
        let selected = self.selected_action();
        self.input.push(c);
        self.preserve_selected(selected);
    }

    pub fn on_backspace(&mut self) {
        let selected = self.selected_action();
        self.input.pop();
        self.preserve_selected(selected);
    }

    pub fn select_next(&mut self) {
        let len = self.visible_commands().len();
        if len > 0 {
            self.selected = (self.selected + 1) % len;
        }
    }

    pub fn select_prev(&mut self) {
        let len = self.visible_commands().len();
        if len > 0 {
            self.selected = self.selected.checked_sub(1).unwrap_or(len - 1);
        }
    }

    pub fn confirm(&mut self) -> Option<TuiAction> {
        let action = self.selected_action()?;
        self.visible = false;
        self.record_recent(action);
        Some(action)
    }

    pub fn visible_commands(&self) -> Vec<ActionSpec> {
        palette_commands(
            self.context,
            self.selected_count,
            &self.input,
            &self.recent_actions,
        )
    }

    fn selected_action(&self) -> Option<TuiAction> {
        self.visible_commands()
            .get(self.selected)
            .map(|command| command.id)
    }

    fn preserve_selected(&mut self, selected: Option<TuiAction>) {
        let commands = self.visible_commands();
        if let Some(action) = selected {
            if let Some(index) = commands.iter().position(|command| command.id == action) {
                self.selected = index;
                return;
            }
        }
        self.selected = self.selected.min(commands.len().saturating_sub(1));
    }

    fn record_recent(&mut self, action: TuiAction) {
        self.recent_actions.retain(|existing| *existing != action);
        self.recent_actions.insert(0, action);
        self.recent_actions.truncate(8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_hints_lead_with_priority_actions_and_end_with_help() {
        let hints = top_hints(ActionContext::Player, 0);

        assert_eq!(hints[0].id, TuiAction::PlayPause);
        assert_eq!(hints[1].id, TuiAction::QueueSelection);
        assert_eq!(hints[2].id, TuiAction::LikeSelection);
        assert_eq!(hints[3].id, TuiAction::ToggleQueueRail);
        // The tail carries the rest of the context's keymap (the
        // renderer fits to width) and Help is always the closer.
        assert!(hints.len() > 5, "hints should cover the full keymap");
        assert_eq!(hints.last().map(|hint| hint.id), Some(TuiAction::Help));
        // Tab-bar navigation is already on the tab strip — no repeats.
        assert!(hints.iter().all(|hint| !is_tab_navigation(hint.id)));
        // No duplicates.
        let mut seen = std::collections::HashSet::new();
        assert!(hints.iter().all(|hint| seen.insert(hint.id)));
    }

    #[test]
    fn playlist_list_hints_surface_queue_whole_playlist() {
        let hints = top_hints(ActionContext::Playlists, 0);
        let queue = hints
            .iter()
            .find(|hint| hint.id == TuiAction::QueueSelection)
            .expect("playlist list must hint the queue action");
        assert_eq!(queue.label, "Queue playlist");
        // It must be in the first few hints, not buried in the tail —
        // narrow terminals only render the head of the list.
        let position = hints
            .iter()
            .position(|hint| hint.id == TuiAction::QueueSelection)
            .expect("position exists");
        assert!(position <= 2, "queue hint buried at {position}");
        // Inside an open playlist the generic label is correct.
        let track_hints = top_hints(ActionContext::PlaylistTracks, 0);
        let track_queue = track_hints
            .iter()
            .find(|hint| hint.id == TuiAction::QueueSelection)
            .expect("playlist tracks hint the queue action");
        assert_ne!(track_queue.label, "Queue playlist");
    }

    #[test]
    fn right_rail_actions_are_global_palette_commands() {
        let player = actions_for_context(ActionContext::Player, 0)
            .into_iter()
            .map(|action| action.id)
            .collect::<Vec<_>>();
        let podcasts = actions_for_context(ActionContext::Podcasts, 0)
            .into_iter()
            .map(|action| action.id)
            .collect::<Vec<_>>();

        for action in [
            TuiAction::ToggleQueueRail,
            TuiAction::ToggleLyricsRail,
            TuiAction::ToggleHintsRail,
        ] {
            assert!(player.contains(&action));
            assert!(podcasts.contains(&action));
        }
    }

    #[test]
    fn multi_select_context_prioritizes_bulk_actions() {
        let hints = top_hints(ActionContext::SearchResults, 2);
        let ids = hints.iter().map(|hint| hint.id).collect::<Vec<_>>();

        // Bulk actions lead the bar; the rest of the keymap follows
        // for wide terminals, with Help always closing the list.
        assert_eq!(
            &ids[..5],
            &[
                TuiAction::QueueSelection,
                TuiAction::LikeSelection,
                TuiAction::AddSelectionToPlaylist,
                TuiAction::ClearMarks,
                TuiAction::OpenDevicePicker,
            ]
        );
        assert_eq!(ids.last(), Some(&TuiAction::Help));
    }

    #[test]
    fn diagnostics_is_a_global_palette_command() {
        let search_labels = palette_commands(ActionContext::SearchResults, 0, "diagnostics", &[])
            .into_iter()
            .map(|command| command.label)
            .collect::<Vec<_>>();
        assert_eq!(search_labels, vec!["Diagnostics"]);
    }

    #[test]
    fn command_palette_confirm_returns_selected_action_and_records_recent() {
        let mut palette = CommandPalette::default();
        palette.open(ActionContext::SearchResults, 0);
        for ch in "queue selected".chars() {
            palette.on_char(ch);
        }

        let action = palette.confirm();

        assert_eq!(action, Some(TuiAction::QueueSelection));
        assert!(!palette.visible);
        assert_eq!(
            palette.recent_actions.first(),
            Some(&TuiAction::QueueSelection)
        );
    }

    #[test]
    fn action_registry_covers_keyboard_actions() {
        let actions = [
            TuiAction::Quit,
            TuiAction::Help,
            TuiAction::OpenCommandPalette,
            TuiAction::OpenPlayer,
            TuiAction::OpenSearch,
            TuiAction::OpenLibrary,
            TuiAction::OpenPlaylists,
            TuiAction::OpenPodcasts,
            TuiAction::OpenQueue,
            TuiAction::OpenDevicePicker,
            TuiAction::OpenDiagnostics,
            TuiAction::OpenLyrics,
            TuiAction::OpenHistory,
            TuiAction::OpenNotifications,
            TuiAction::MoveDown,
            TuiAction::MoveUp,
            TuiAction::PageDown,
            TuiAction::PageUp,
            TuiAction::JumpTop,
            TuiAction::JumpBottom,
            TuiAction::Back,
            TuiAction::Refresh,
            TuiAction::RefreshMedia,
            TuiAction::StartSearchInput,
            TuiAction::StartListFilter,
            TuiAction::SubmitSearch,
            TuiAction::CancelInput,
            TuiAction::PlayPause,
            TuiAction::Next,
            TuiAction::Previous,
            TuiAction::SeekBack,
            TuiAction::SeekForward,
            TuiAction::VolumeUp,
            TuiAction::VolumeDown,
            TuiAction::ToggleShuffle,
            TuiAction::CycleRepeat,
            TuiAction::OpenSelected,
            TuiAction::PlaySelected,
            TuiAction::QueueSelection,
            TuiAction::LikeSelection,
            TuiAction::AddSelectionToPlaylist,
            TuiAction::ToggleMark,
            TuiAction::MarkRange,
            TuiAction::ClearMarks,
            TuiAction::TogglePlayerMode,
            TuiAction::ToggleQueueRail,
            TuiAction::ToggleLyricsRail,
            TuiAction::ToggleHintsRail,
            TuiAction::ToggleRailFullscreen,
        ];

        for action in actions {
            assert!(
                action_spec(action).is_some(),
                "missing action registry spec for {action:?}"
            );
        }
    }

    #[test]
    fn tui_actions_have_cli_equivalent_or_client_only_reason() {
        for action in default_actions() {
            assert!(
                action.cli.is_some() || tui_only_reason(action.id).is_some(),
                "{} must define a CLI equivalent or TUI-only reason",
                action.label
            );
        }
    }

    #[test]
    fn tui_only_actions_are_documented_in_decision_log() {
        let decision_log = include_str!("../../../docs/blueprint/13-decision-log.md");

        for action in default_actions()
            .into_iter()
            .filter(|action| action.cli.is_none())
        {
            let reason = tui_only_reason(action.id).unwrap_or("missing TUI-only reason");
            assert_ne!(
                reason, "missing TUI-only reason",
                "{} must define a TUI-only reason",
                action.label
            );
            assert!(
                decision_log.contains(action.label) && decision_log.contains(reason),
                "{} must be documented with reason `{reason}` in decision log",
                action.label
            );
        }
    }
}
