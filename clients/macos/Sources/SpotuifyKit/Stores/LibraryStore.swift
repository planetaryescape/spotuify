import Foundation
import Observation

/// Loads the user's library collections (playlists, liked songs, saved albums,
/// subscribed podcasts) and per-playlist track lists on demand, refreshing when
/// the daemon reports library/playlist changes.
@MainActor
@Observable
public final class LibraryStore {
    public private(set) var playlists: [Playlist] = []
    public private(set) var likedSongs: [MediaItem] = []
    public private(set) var savedAlbums: [MediaItem] = []
    public private(set) var savedShows: [MediaItem] = []
    public private(set) var playlistTracks: [String: [MediaItem]] = [:]
    public private(set) var loadingPlaylists = false
    public private(set) var loadingLiked = false
    public private(set) var loadingAlbums = false
    public private(set) var loadingShows = false
    public private(set) var loadingTracksFor: String?

    private weak var model: AppModel?

    public init() {}

    func connect(_ model: AppModel) {
        self.model = model
        model.addEventObserver { [weak self] event in
            guard let self else { return }
            switch event {
            case .playlistsChanged: Task { await self.loadPlaylists(force: true) }
            case .libraryChanged:
                Task {
                    await self.loadLiked(force: true)
                    await self.loadAlbums(force: true)
                }
            default: break
            }
        }
    }

    public func loadPlaylists(force: Bool = false) async {
        guard let model else { return }
        if !force && !playlists.isEmpty { return }
        loadingPlaylists = true
        defer { loadingPlaylists = false }
        if case .playlists(let result) = try? await model.request(.playlistsList, timeout: .seconds(20)) {
            playlists = result
        }
    }

    /// Liked songs — real saved tracks (`/me/tracks`) with date added.
    public func loadLiked(force: Bool = false) async {
        guard let model else { return }
        if !force && !likedSongs.isEmpty { return }
        loadingLiked = true
        defer { loadingLiked = false }
        if case .mediaItems(let items) = try? await model.request(
            .savedTracks(limit: 50, offset: 0), timeout: .seconds(20)) {
            likedSongs = items
        }
    }

    /// Saved albums — from the synced library, filtered to album rows.
    public func loadAlbums(force: Bool = false) async {
        guard let model else { return }
        if !force && !savedAlbums.isEmpty { return }
        loadingAlbums = true
        defer { loadingAlbums = false }
        if case .mediaItems(let items) = try? await model.request(.libraryList(limit: 200), timeout: .seconds(20)) {
            savedAlbums = items.filter { $0.kind == .album }
        }
    }

    /// Subscribed podcasts (saved shows).
    public func loadShows(force: Bool = false) async {
        guard let model else { return }
        if !force && !savedShows.isEmpty { return }
        loadingShows = true
        defer { loadingShows = false }
        if case .mediaItems(let items) = try? await model.request(.savedShows(limit: 200), timeout: .seconds(20)) {
            savedShows = items.filter { $0.kind == .show }
        }
    }

    public func loadTracks(for playlist: Playlist) async {
        guard let model else { return }
        loadingTracksFor = playlist.id
        defer { loadingTracksFor = nil }
        if case .mediaItems(let items) = try? await model.request(
            .playlistTracks(playlist: playlist.id, wait: true), timeout: .seconds(30)) {
            playlistTracks[playlist.id] = items
        }
    }

    public func tracks(for playlist: Playlist) -> [MediaItem] {
        playlistTracks[playlist.id] ?? []
    }
}
