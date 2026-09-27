import SwiftUI
import SpotuifyKit

/// Registers detail destinations so any `NavigationLink(value: MediaItem)`
/// (album/artist/show) opens the right page. Apply to a NavigationStack root.
extension View {
    func mediaDetailDestinations() -> some View {
        navigationDestination(for: MediaItem.self) { item in
            switch item.kind {
            case .album: AlbumDetailView(album: item)
            case .artist: ArtistDetailView(artist: item)
            case .show: ShowDetailView(show: item)
            case .playlist: PlaylistItemDetailView(playlist: item)
            default: SingleItemDetailView(item: item)
            }
        }
    }
}

/// Hero shared by detail pages: artwork, eyebrow, title, credits, and the
/// play / shuffle / queue actions, plus a page-specific `accessory` (save an
/// album, follow an artist).
struct DetailHeader<Accessory: View>: View {
    @Environment(AppModel.self) private var model
    let item: MediaItem
    let subtitle: String
    let contextURI: String?
    let trackURIs: [String]
    var artworkIsCircle = false
    @ViewBuilder var accessory: () -> Accessory

    var body: some View {
        HeroHeader(eyebrow: eyebrow, title: item.name, showsBack: true) {
            AsyncCoverImage(url: item.imageURL, cornerRadius: artworkIsCircle ? 0 : Theme.tileCornerRadius)
                .circularArtwork(artworkIsCircle)
        } credits: {
            subtitleView
        } actions: {
            RoomPlayButton(label: "Play \(item.name)") { play() }
                .disabled(!canPlay || (trackURIs.isEmpty && contextURI == nil))
            if !trackURIs.isEmpty {
                RoomIconButton(systemName: "shuffle", label: "Shuffle") { model.shufflePlay(uris: trackURIs) }
                    .disabled(!canPlayTracks)
            }
            RoomIconButton(systemName: "text.append", label: "Add to Queue") { queue() }
                .disabled(!canQueue || (trackURIs.isEmpty && contextURI == nil))
            accessory()
        }
    }

    private var eyebrow: String {
        switch item.kind {
        case .album: "Album"
        case .artist: "Artist"
        case .playlist: "Playlist"
        case .show: "Podcast"
        case .episode: "Episode"
        default: "Track"
        }
    }

    /// Artist line: clickable links to each artist when the item carries artist
    /// refs (e.g. an album → its artist), else the plain subtitle text.
    @ViewBuilder
    private var subtitleView: some View {
        let artists = item.artistNavItems
        if !artists.isEmpty {
            HStack(spacing: 4) {
                ForEach(Array(artists.enumerated()), id: \.element.id) { index, artist in
                    if index > 0 { Text(",").foregroundStyle(.secondary) }
                    NavigationLink(value: artist) {
                        NavLinkLabel(name: artist.name)
                    }
                    .buttonStyle(.plain)
                }
            }
        } else {
            Text(subtitle).foregroundStyle(.secondary)
        }
    }

    private func play() {
        if let contextURI { model.play(uri: contextURI) } else { model.playAll(uris: trackURIs) }
    }
    private func queue() {
        if let contextURI { model.queueAdd(uri: contextURI) } else { model.queueAll(uris: trackURIs) }
    }

    private var canPlay: Bool {
        if let contextURI { return model.canPlay(uri: contextURI) }
        return canPlayTracks
    }

    private var canPlayTracks: Bool {
        guard let first = trackURIs.first, model.canPlay(uri: first) else { return false }
        return trackURIs.dropFirst().allSatisfy { model.canQueue(uri: $0) }
    }

    private var canQueue: Bool {
        if let contextURI { return model.canQueue(uri: contextURI) }
        return !trackURIs.isEmpty && trackURIs.allSatisfy { model.canQueue(uri: $0) }
    }
}

extension DetailHeader where Accessory == EmptyView {
    init(item: MediaItem, subtitle: String, contextURI: String?, trackURIs: [String], artworkIsCircle: Bool = false) {
        self.init(
            item: item, subtitle: subtitle, contextURI: contextURI, trackURIs: trackURIs,
            artworkIsCircle: artworkIsCircle, accessory: { EmptyView() })
    }
}

/// Album detail page: editorial header plus the album's track list.
struct AlbumDetailView: View {
    @Environment(AppModel.self) private var model
    let album: MediaItem
    @State private var tracks: [MediaItem] = []
    @State private var loading = true
    @State private var loadError: String?
    @State private var savedOverride: Bool?

    private var isSaved: Bool {
        savedOverride ?? model.library.savedAlbums.contains { $0.uri == album.uri }
    }

    var body: some View {
        VStack(spacing: 0) {
            DetailHeader(
                item: album,
                subtitle: album.subtitle,
                contextURI: album.uri,
                trackURIs: tracks.map(\.uri)
            ) {
                RoomIconButton(
                    systemName: isSaved ? "checkmark" : "plus",
                    label: isSaved ? "Remove from Library" : "Add to Library",
                    isOn: isSaved
                ) { toggleSaved() }
                    .disabled(!model.canSave(uri: album.uri))
            }
            if loading && tracks.isEmpty {
                LoadingStateView(label: "Loading album tracks", style: .rows)
            } else if let loadError {
                ErrorStateView(message: loadError) { Task { await load() } }
            } else {
                TrackListView(tracks: tracks, detailed: false, fallbackImageURL: album.imageURL, contextURI: album.uri)
            }
        }
        .navigationTitle(album.name)
        .task(id: album.uri) { await load() }
    }

    private func toggleSaved() {
        let nowSaved = !isSaved
        savedOverride = nowSaved
        let request: DaemonRequest = nowSaved
            ? .librarySave(uri: album.uri, current: false)
            : .libraryUnsave(uri: album.uri)
        Task { @MainActor in
            do {
                _ = try await model.request(request)
                model.showToast(nowSaved ? "Added to Library" : "Removed from Library")
            } catch {
                savedOverride = nil
                model.showToast("Couldn't update library")
            }
        }
    }

    private func load() async {
        loading = true
        loadError = nil
        defer { loading = false }
        do {
            guard case .mediaItems(let items) = try await model.request(.albumTracks(album: album.uri)) else {
                loadError = "The app received an unexpected response."
                return
            }
            tracks = items
        } catch {
            loadError = error.localizedDescription
        }
    }
}

/// Artist detail page: header plus the artist's albums, with a library-only
/// filter toggle.
struct ArtistDetailView: View {
    @Environment(AppModel.self) private var model
    let artist: MediaItem
    @State private var albums: [MediaItem] = []
    @State private var loading = true
    @State private var loadError: String?
    @State private var libraryOnly = false
    /// Optimistic follow state; nil = derive from the library.
    @State private var followingOverride: Bool?

    private var isFollowing: Bool {
        followingOverride ?? model.library.followedArtists.contains { $0.uri == artist.uri }
    }

    private let columns = [GridItem(.adaptive(minimum: 150, maximum: 200), spacing: 16)]

    /// Discography section order, keyed by Spotify's `album_group`.
    private static let groupOrder: [(key: String, label: String)] = [
        ("album", "Albums"),
        ("single", "Singles & EPs"),
        ("compilation", "Compilations"),
        ("appears_on", "Appears On"),
    ]

    /// Albums shown for the current toggle (the daemon already tagged each
    /// one's `inLibrary`, so flipping is instant — no refetch).
    private var visible: [MediaItem] {
        libraryOnly ? albums.filter { $0.inLibrary == true } : albums
    }

    private var inLibraryCount: Int {
        albums.filter { $0.inLibrary == true }.count
    }

    /// Visible albums split into ordered, non-empty sections.
    private var sections: [(label: String, items: [MediaItem])] {
        let shown = visible
        let known = Set(Self.groupOrder.map(\.key))
        var result = Self.groupOrder.map { group in
            (label: group.label, items: shown.filter { $0.albumGroup == group.key })
        }
        let other = shown.filter { item in
            guard let group = item.albumGroup else { return true }
            return !known.contains(group)
        }
        if !other.isEmpty { result.append((label: "Other", items: other)) }
        return result.filter { !$0.items.isEmpty }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            DetailHeader(
                item: artist,
                subtitle: artist.context.isEmpty ? "Artist" : artist.context,
                contextURI: artist.uri,
                trackURIs: [],
                artworkIsCircle: true
            ) {
                Button {
                    let nowFollowing = !isFollowing
                    followingOverride = nowFollowing
                    if nowFollowing {
                        model.followArtist(uri: artist.uri)
                    } else {
                        model.unfollowArtist(uri: artist.uri)
                    }
                } label: {
                    Label(isFollowing ? "Following" : "Follow", systemImage: isFollowing ? "checkmark" : "plus")
                }
                .buttonStyle(RoomButtonStyle())
                .disabled(!model.canFollow(uri: artist.uri))
            }
            HStack(alignment: .firstTextBaseline) {
                RoomTabs(options: [(value: false, title: "All"), (value: true, title: "In Library")], selection: $libraryOnly)
                Spacer()
                MonoCaps("\(visible.count) releases · \(inLibraryCount) in library", size: 9.5)
            }
            .padding(.horizontal, 32).padding(.bottom, 8)
            if loading && albums.isEmpty {
                LoadingStateView(label: "Loading artist releases", style: .tiles)
            } else if let loadError {
                ErrorStateView(message: loadError) { Task { await load() } }
            } else if visible.isEmpty {
                EmptyState(
                    "No albums", systemImage: "square.stack",
                    description: Text(libraryOnly
                        ? "None of this artist's albums are in your library."
                        : "No releases found."))
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 16, pinnedViews: [.sectionHeaders]) {
                        ForEach(sections, id: \.label) { section in
                            Section {
                                LazyVGrid(columns: columns, spacing: 16) {
                                    ForEach(section.items) { album in
                                        NavigationLink(value: album) { ArtworkTile(item: album) }
                                            .buttonStyle(.plain)
                                    }
                                }
                            } header: {
                                Text(section.label)
                                    .editorialSectionHeader()
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .padding(.vertical, 4)
                                    .roomPinnedBackground()
                            }
                        }
                    }
                    .padding(16)
                }
            }
        }
        .navigationTitle(artist.name)
        .task(id: artist.uri) { await load() }
    }

    private func load() async {
        loading = true
        loadError = nil
        defer { loading = false }
        do {
            guard case .mediaItems(let items) = try await model.request(.artistAlbums(artist: artist.uri)) else {
                loadError = "The app received an unexpected response."
                return
            }
            albums = items
        } catch {
            loadError = error.localizedDescription
        }
    }
}

/// Podcast show detail page: header plus the show's episodes, with an
/// unplayed-only filter toggle.
struct ShowDetailView: View {
    @Environment(AppModel.self) private var model
    let show: MediaItem
    @State private var episodes: [MediaItem] = []
    @State private var loading = true
    @State private var loadError: String?
    @State private var unplayedOnly = false
    @State private var newestFirst = true

    private var visible: [MediaItem] {
        var result = episodes
        if unplayedOnly { result = result.filter { !$0.isFullyPlayed } }
        result.sort {
            let a = $0.releaseDate ?? ""
            let b = $1.releaseDate ?? ""
            return newestFirst ? a > b : a < b
        }
        return result
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            DetailHeader(
                item: show,
                subtitle: show.subtitle,
                contextURI: nil,
                trackURIs: visible.map(\.uri))
            HStack(alignment: .firstTextBaseline) {
                RoomTabs(options: [(value: true, title: "Newest first"), (value: false, title: "Oldest first")],
                         selection: $newestFirst)
                Spacer()
                Toggle("Unplayed only", isOn: $unplayedOnly).toggleStyle(.switch).controlSize(.small)
            }
            .padding(.horizontal, 32).padding(.bottom, 8)
            if loading && episodes.isEmpty {
                LoadingStateView(label: "Loading episodes", style: .rows)
            } else if let loadError {
                ErrorStateView(message: loadError) { Task { await load() } }
            } else if visible.isEmpty {
                EmptyState("No episodes", systemImage: "mic",
                    description: Text(unplayedOnly ? "All caught up." : "No episodes found."))
            } else {
                ScrollView {
                    LazyVStack(spacing: 2) {
                        ForEach(Array(visible.enumerated()), id: \.offset) { _, episode in
                            MediaRow(item: episode, detailed: false)
                        }
                    }
                    .padding(10)
                }
            }
        }
        .navigationTitle(show.name)
        .task(id: show.uri) { await load() }
    }

    private func load() async {
        loading = true
        loadError = nil
        defer { loading = false }
        do {
            guard case .mediaItems(let items) = try await model.request(
                .showEpisodes(show: show.uri, limit: 50, offset: 0), timeout: .seconds(25)) else {
                loadError = "The app received an unexpected response."
                return
            }
            episodes = items
        } catch {
            loadError = error.localizedDescription
        }
    }
}

/// Detail for a playlist arrived at as a search/grid result (a `MediaItem`
/// of kind playlist, as opposed to the sidebar's `Playlist` model).
private struct PlaylistTracksLoadIdentity: Hashable {
    let uri: String
    let canReadItems: Bool
}

struct PlaylistItemDetailView: View {
    @Environment(AppModel.self) private var model
    let playlist: MediaItem
    @State private var tracks: [MediaItem] = []
    @State private var loading = true
    @State private var loadError: String?

    var body: some View {
        VStack(spacing: 0) {
            DetailHeader(
                item: playlist,
                subtitle: playlist.subtitle,
                contextURI: playlist.uri,
                trackURIs: tracks.map(\.uri))
            if !model.canReadPlaylistItems(uri: playlist.uri) {
                EmptyState(
                    "Playlist unavailable",
                    systemImage: "lock",
                    description: Text("This provider does not expose playlist items."))
            } else if loading && tracks.isEmpty {
                LoadingStateView(label: "Loading playlist tracks", style: .rows)
            } else if let loadError {
                ErrorStateView(message: loadError) { Task { await load() } }
            } else {
                TrackListView(tracks: tracks, contextURI: playlist.uri)
            }
        }
        .navigationTitle(playlist.name)
        .task(id: PlaylistTracksLoadIdentity(
            uri: playlist.uri,
            canReadItems: model.canReadPlaylistItems(uri: playlist.uri)
        )) { await load() }
    }

    private func load() async {
        guard model.canReadPlaylistItems(uri: playlist.uri) else {
            loading = false
            loadError = nil
            tracks = []
            return
        }
        loading = true
        loadError = nil
        defer { loading = false }
        do {
            guard case .mediaItems(let items) = try await model.request(
                .playlistTracks(playlist: playlist.uri, wait: true), timeout: .seconds(30)) else {
                loadError = "The app received an unexpected response."
                return
            }
            tracks = items
        } catch {
            loadError = error.localizedDescription
        }
    }
}

/// Fallback detail for a single item (e.g. a lone track/episode result).
struct SingleItemDetailView: View {
    let item: MediaItem
    var body: some View {
        TrackListView(tracks: [item], detailed: false)
            .navigationTitle(item.name)
    }
}
