//! Per-category request dispatchers, split out of the original
//! `handler::dispatch` god-function. `categorize` routes each
//! `Request` to its module; the arm bodies moved here verbatim.

use spotuify_protocol::Request;

pub(crate) mod admin;
pub(crate) mod analytics;
pub(crate) mod library;
pub(crate) mod media;
pub(crate) mod ops;
pub(crate) mod playback;
pub(crate) mod playlists;
pub(crate) mod reminders;
pub(crate) mod search;
pub(crate) mod viz;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cat {
    Admin,
    Playback,
    Search,
    Library,
    Playlists,
    Analytics,
    Ops,
    Reminders,
    Viz,
    Media,
}

pub(crate) fn categorize(request: &Request) -> Cat {
    match request {
        Request::Ping
        | Request::SubscribeEvents { .. }
        | Request::GetDaemonStatus
        | Request::GetDoctorReport
        | Request::ClientSeed
        | Request::ProvidersList
        | Request::ResolveTarget { .. }
        | Request::ListAudioOutputs
        | Request::CacheStatus
        | Request::LogsTail { .. }
        | Request::CheckUpdate { .. }
        | Request::Reindex
        | Request::Reload
        | Request::ReloadAuth
        | Request::AuthStart { .. }
        | Request::AuthPoll { .. }
        | Request::AuthCancel { .. }
        | Request::AuthStatus { .. }
        | Request::AuthLogout { .. }
        | Request::WebApiToken { .. }
        | Request::Shutdown
        | Request::Sync { .. }
        | Request::SearchCachePrune { .. } => Cat::Admin,
        Request::PlaybackGet
        | Request::PlaybackCommand { .. }
        | Request::Reconnect
        | Request::SetAudioOutput { .. }
        | Request::DevicesList
        | Request::DeviceTransfer { .. }
        | Request::QueueAdd { .. }
        | Request::QueueAddMany { .. }
        | Request::QueueGet
        | Request::RecentlyPlayed { .. } => Cat::Playback,
        Request::Search { .. } | Request::SearchStream { .. } | Request::SearchPage { .. } => {
            Cat::Search
        }
        Request::LibraryList { .. }
        | Request::LibrarySave { .. }
        | Request::LibraryUnsave { .. }
        | Request::SavedTracks { .. }
        | Request::SavedShows { .. }
        | Request::FollowedArtists { .. }
        | Request::ArtistFollow { .. }
        | Request::ArtistUnfollow { .. }
        | Request::ArtistAlbums { .. }
        | Request::AlbumTracks { .. }
        | Request::ShowEpisodes { .. }
        | Request::EpisodeFeed { .. }
        | Request::RelatedArtists { .. }
        | Request::RadioStart { .. } => Cat::Library,
        Request::PlaylistsList { .. }
        | Request::PlaylistTracks { .. }
        | Request::PlaylistItemsPreview { .. }
        | Request::PlaylistAddItems { .. }
        | Request::PlaylistRemoveItems { .. }
        | Request::PlaylistCreate { .. }
        | Request::PlaylistCreatePreview { .. }
        | Request::PlaylistUnfollow { .. }
        | Request::PlaylistSetImage { .. } => Cat::Playlists,
        Request::AnalyticsRebuild { .. }
        | Request::AnalyticsTop { .. }
        | Request::AnalyticsHabits { .. }
        | Request::AnalyticsSearch { .. }
        | Request::AnalyticsRediscovery { .. }
        | Request::AnalyticsExport { .. }
        | Request::AnalyticsImport { .. }
        | Request::AnalyticsImportStatus { .. }
        | Request::AnalyticsImportUnresolved { .. }
        | Request::AnalyticsImportUndo { .. }
        | Request::AnalyticsPrune { .. }
        | Request::ListenSessions { .. } => Cat::Analytics,
        Request::OpsLog { .. }
        | Request::OpsShow { .. }
        | Request::OpsUndo { .. }
        | Request::OpsRedo { .. } => Cat::Ops,
        Request::ReminderCreate { .. }
        | Request::RemindersList { .. }
        | Request::ReminderCancel { .. }
        | Request::NotificationsList { .. }
        | Request::NotificationAct { .. } => Cat::Reminders,
        Request::SetVizEnabled { .. }
        | Request::SetVizSource { .. }
        | Request::GetVizStatus
        | Request::SetVizFocus { .. } => Cat::Viz,
        Request::Image { .. }
        | Request::CoverArt { .. }
        | Request::LyricsGet { .. }
        | Request::LyricsOffsetSet { .. } => Cat::Media,
    }
}
