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
    /// The user's full liked-songs count reported by the daemon, so the header
    /// and scroll trigger know the true size before every page has loaded.
    public private(set) var likedTotal = 0
    /// How far into the server list we've paged (the next request's `offset`).
    public private(set) var likedLoadedOffset = 0
    /// Guards `loadMoreLiked()` re-entrancy so overlapping scroll events don't
    /// fetch the same page twice.
    public private(set) var loadingLikedPage = false
    public private(set) var savedAlbums: [MediaItem] = []
    public private(set) var savedShows: [MediaItem] = []
    public private(set) var followedArtists: [MediaItem] = []
    public private(set) var historySessions: [ListenSession] = []
    public private(set) var playlistTracks: [String: [MediaItem]] = [:]
    public private(set) var loadingPlaylists = false
    public private(set) var loadingLiked = false
    public private(set) var loadingAlbums = false
    public private(set) var loadingShows = false
    public private(set) var loadingArtists = false
    public private(set) var loadingHistory = false
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
                    await self.loadFollowedArtists(force: true)
                }
            default: break
            }
        }
    }

    public func loadPlaylists(force: Bool = false) async {
        guard let model, model.canListPlaylists else { return }
        if !force && !playlists.isEmpty { return }
        loadingPlaylists = true
        defer { loadingPlaylists = false }
        if case .playlists(let result) = try? await model.request(.playlistsList(), timeout: .seconds(20)) {
            playlists = result
        }
    }

    /// Liked songs — real saved tracks (`/me/tracks`) with date added. Loads
    /// only the first page; later pages lazy-load via `loadMoreLiked()` as the
    /// user scrolls. The paged response carries the library `total`.
    public func loadLiked(force: Bool = false) async {
        guard let model, model.canReadLibrary(kind: .track) else { return }
        if !force && !likedSongs.isEmpty { return }
        loadingLiked = true
        defer { loadingLiked = false }
        guard case .savedTracksPage(let items, let total, _) = try? await model.request(
            .savedTracks(limit: 50, offset: 0), timeout: .seconds(45))
        else { return }
        likedSongs = items
        likedTotal = total
        likedLoadedOffset = items.count
    }

    /// Fetch the next page of liked songs and append it. Called when the user
    /// scrolls near the end of the list. Re-entrant-safe via `loadingLikedPage`;
    /// stops once we've paged through the whole library (or hit Spotify's
    /// 1000-item offset wall, which the daemon reports as an empty page).
    public func loadMoreLiked() async {
        guard let model, model.canReadLibrary(kind: .track) else { return }
        guard !loadingLikedPage, likedLoadedOffset < likedTotal else { return }
        loadingLikedPage = true
        defer { loadingLikedPage = false }
        guard case .savedTracksPage(let items, let total, _) = try? await model.request(
            .savedTracks(limit: 50, offset: UInt32(likedLoadedOffset)), timeout: .seconds(45))
        else { return }
        likedTotal = total
        if items.isEmpty {
            // Reached the end (or the 1000-item wall): pin total so we stop.
            likedTotal = likedLoadedOffset
            return
        }
        // Dedupe by uri so an overlapping page can never double-insert a track.
        let known = Set(likedSongs.map(\.uri))
        likedSongs.append(contentsOf: items.filter { !known.contains($0.uri) })
        likedLoadedOffset += items.count
    }

    /// Saved albums — from the synced library, filtered to album rows.
    public func loadAlbums(force: Bool = false) async {
        guard let model, model.canReadLibrary(kind: .album) else { return }
        if !force && !savedAlbums.isEmpty { return }
        loadingAlbums = true
        defer { loadingAlbums = false }
        if case .mediaItems(let items) = try? await model.request(.libraryList(limit: 200), timeout: .seconds(20)) {
            savedAlbums = items.filter { $0.kind == .album }
        }
    }

    /// Subscribed podcasts (saved shows).
    public func loadShows(force: Bool = false) async {
        guard let model, model.canReadLibrary(kind: .show) else { return }
        if !force && !savedShows.isEmpty { return }
        loadingShows = true
        defer { loadingShows = false }
        if case .mediaItems(let items) = try? await model.request(.savedShows(limit: 200), timeout: .seconds(20)) {
            savedShows = items.filter { $0.kind == .show }
        }
    }

    /// Followed artists — the discography browser's entry point.
    public func loadFollowedArtists(force: Bool = false) async {
        guard let model, model.canReadLibrary(kind: .artist) else { return }
        if !force && !followedArtists.isEmpty { return }
        loadingArtists = true
        defer { loadingArtists = false }
        if case .mediaItems(let items) = try? await model.request(
            .followedArtists(limit: 500), timeout: .seconds(20)) {
            followedArtists = items.filter { $0.kind == .artist }
        }
    }

    /// Listening history grouped into sessions (merged local + recently-played).
    public func loadHistory(force: Bool = false) async {
        guard let model else { return }
        if !force && !historySessions.isEmpty { return }
        loadingHistory = true
        defer { loadingHistory = false }
        if case .listenSessions(let sessions) = try? await model.request(
            .listenSessions(limit: 50), timeout: .seconds(20)) {
            historySessions = sessions
        }
    }

    public func loadTracks(for playlist: Playlist) async {
        guard let model, model.canReadPlaylistItems else { return }
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
