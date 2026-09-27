import SwiftUI
import SpotuifyKit

struct PlaylistsView: View {
    @Environment(AppModel.self) private var model

    /// Sidebar `Playlist`s as `MediaItem`s so they flow through the shared
    /// grid/list `CollectionView` and open via `mediaDetailDestinations`.
    private var items: [MediaItem] {
        model.library.playlists.compactMap { playlist in
            guard let uri = model.playlistResourceURI(for: playlist) else { return nil }
            return MediaItem(
                uri: uri,
                name: playlist.name,
                subtitle: playlist.owner,
                context: "\(playlist.tracksTotal) tracks",
                imageURL: playlist.imageURL,
                kind: .playlist)
        }
    }

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 0) {
                EditorialPageHeader("Playlists", eyebrow: "Library")
                if !model.canListPlaylists {
                    EmptyState(
                        "Playlists unavailable", systemImage: "music.note.list",
                        description: Text("The current provider does not expose playlists."))
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if model.library.loadingPlaylists && model.library.playlists.isEmpty {
                    SkeletonTiles()
                } else if model.library.playlists.isEmpty {
                    EmptyState("No playlists", systemImage: "music.note.list")
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    CollectionView(items: items, storageKey: "playlistsLayout")
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .mediaDetailDestinations()
        }
        .task { await model.library.loadPlaylists() }
    }
}

struct PlaylistDetailView: View {
    @Environment(AppModel.self) private var model
    let playlist: Playlist

    private var tracks: [MediaItem] { model.library.tracks(for: playlist) }

    private var playlistURI: String? { model.playlistResourceURI(for: playlist) }

    private var canPlayPlaylist: Bool {
        guard let playlistURI else { return false }
        return model.canPlay(uri: playlistURI)
    }

    private var canQueuePlaylist: Bool {
        guard let playlistURI else { return false }
        return model.canQueue(uri: playlistURI)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HeroHeader(eyebrow: "Playlist", title: playlist.name, showsBack: true) {
                AsyncCoverImage(url: playlist.imageURL, cornerRadius: Theme.tileCornerRadius)
            } credits: {
                Text("\(playlist.tracksTotal) track\(playlist.tracksTotal == 1 ? "" : "s") · \(playlist.owner)")
                    .font(.mono(12))
            } actions: {
                RoomPlayButton(label: "Play \(playlist.name)") {
                    if let playlistURI { model.play(uri: playlistURI) }
                }
                .disabled(!canPlayPlaylist)
                RoomIconButton(systemName: "shuffle", label: "Shuffle") { model.shufflePlay(uris: tracks.map(\.uri)) }
                    .disabled(tracks.isEmpty || !tracks.allSatisfy { model.canQueue(uri: $0.uri) })
                RoomIconButton(systemName: "text.append", label: "Add to Queue") {
                    if let playlistURI { model.queueAdd(uri: playlistURI) }
                }
                .disabled(!canQueuePlaylist)
            }

            if model.library.loadingTracksFor == playlist.id && tracks.isEmpty {
                SkeletonRows()
            } else {
                TrackListView(tracks: tracks)
            }
        }
        .navigationTitle(playlist.name)
        .task(id: playlist.id) { await model.library.loadTracks(for: playlist) }
    }
}
