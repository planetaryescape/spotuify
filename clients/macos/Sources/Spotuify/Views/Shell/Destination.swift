import Observation
import SwiftUI

/// Shared current-destination state, so both the sidebar (`AppShell`) and the
/// app-level keyboard `Commands` (⌘1–0) can drive view navigation.
@MainActor
@Observable
final class Navigator {
    var selection: Destination = .nowPlaying

    init() {
        #if DEBUG
        // `-SpotuifyInitialDestination albums` opens a page directly, so agents
        // can capture any screen without driving the keyboard.
        if let raw = UserDefaults.standard.string(forKey: "SpotuifyInitialDestination"),
           let destination = Destination(rawValue: raw) {
            selection = destination
        }
        #endif
    }

    /// Shortcut order for ⌘1…⌘9, ⌘0, then ⌘⇧0. The final chord preserves all
    /// existing numeric mappings while making Notifications reachable too.
    static let numbered: [Destination] = [
        .nowPlaying, .queue, .search, .likedSongs, .albums,
        .artists, .podcasts, .playlists, .history, .devices, .notifications,
    ]
}

/// Sidebar destinations.
enum Destination: String, CaseIterable, Identifiable {
    case nowPlaying
    case queue
    case search
    case likedSongs
    case albums
    case artists
    case podcasts
    case playlists
    case history
    case notifications
    case bookmarks
    case devices

    var id: String { rawValue }

    /// Sidebar grouping: what you do now, what you keep, what you've done.
    enum Section: String, CaseIterable, Identifiable {
        case listen = "Listen"
        case library = "Library"
        case you = "You"
        var id: String { rawValue }
    }

    var section: Section {
        switch self {
        case .nowPlaying, .search, .queue: .listen
        case .likedSongs, .albums, .artists, .playlists, .podcasts: .library
        case .history, .bookmarks, .notifications, .devices: .you
        }
    }

    /// Display order within each sidebar section.
    static let sidebarOrder: [Destination] = [
        .nowPlaying, .search, .queue,
        .likedSongs, .playlists, .albums, .artists, .podcasts,
        .history, .bookmarks, .notifications, .devices,
    ]

    var title: String {
        switch self {
        case .nowPlaying: "Now Playing"
        case .search: "Search"
        case .likedSongs: "Liked Songs"
        case .albums: "Albums"
        case .artists: "Artists"
        case .podcasts: "Podcasts"
        case .playlists: "Playlists"
        case .queue: "Queue"
        case .history: "History"
        case .notifications: "Notifications"
        case .bookmarks: "Bookmarks"
        case .devices: "Devices"
        }
    }

    var icon: String {
        switch self {
        case .nowPlaying: "play.circle.fill"
        case .search: "magnifyingglass"
        case .likedSongs: "heart.fill"
        case .albums: "square.stack.fill"
        case .artists: "music.mic"
        case .podcasts: "mic.fill"
        case .playlists: "music.note.list"
        case .queue: "list.bullet"
        case .history: "clock.arrow.circlepath"
        case .notifications: "bell.fill"
        case .bookmarks: "bookmark.fill"
        case .devices: "hifispeaker.2.fill"
        }
    }
}
