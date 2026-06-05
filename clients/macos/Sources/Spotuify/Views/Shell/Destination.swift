import SwiftUI

/// Sidebar destinations.
enum Destination: String, CaseIterable, Identifiable {
    case nowPlaying
    case search
    case likedSongs
    case albums
    case podcasts
    case playlists
    case queue
    case devices

    var id: String { rawValue }

    var title: String {
        switch self {
        case .nowPlaying: "Now Playing"
        case .search: "Search"
        case .likedSongs: "Liked Songs"
        case .albums: "Albums"
        case .podcasts: "Podcasts"
        case .playlists: "Playlists"
        case .queue: "Queue"
        case .devices: "Devices"
        }
    }

    var icon: String {
        switch self {
        case .nowPlaying: "play.circle.fill"
        case .search: "magnifyingglass"
        case .likedSongs: "heart.fill"
        case .albums: "square.stack.fill"
        case .podcasts: "mic.fill"
        case .playlists: "music.note.list"
        case .queue: "list.bullet"
        case .devices: "hifispeaker.2.fill"
        }
    }
}
