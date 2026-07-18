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
                EditorialPageHeader("Playlists")
                Divider()
                if !model.canListPlaylists {
                    ContentUnavailableView(
                        "Playlists unavailable", systemImage: "music.note.list",
                        description: Text("The current provider does not expose playlists."))
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if model.library.loadingPlaylists && model.library.playlists.isEmpty {
                    SkeletonTiles()
                } else if model.library.playlists.isEmpty {
                    ContentUnavailableView("No playlists", systemImage: "music.note.list")
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    CollectionView(items: items, storageKey: "playlistsLayout")
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .mediaDetailDestinations()
        }
        .background(.background)
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
            HStack(spacing: 16) {
                AsyncCoverImage(url: playlist.imageURL, cornerRadius: 10)
                    .frame(width: 120, height: 120)
                    .shadow(radius: 8, y: 4)
                VStack(alignment: .leading, spacing: 8) {
                    Text(playlist.name).font(.displayHero(32)).lineLimit(2).minimumScaleFactor(0.6)
                    Text("\(playlist.tracksTotal) tracks · \(playlist.owner)")
                        .foregroundStyle(.secondary)
                    HStack(spacing: 10) {
                        Button {
                            if let playlistURI { model.play(uri: playlistURI) }
                        } label: { Label("Play", systemImage: "play.fill") }
                            .buttonStyle(.borderedProminent).controlSize(.large)
                            .disabled(!canPlayPlaylist)
                        Button { model.shufflePlay(uris: tracks.map(\.uri)) } label: { Label("Shuffle", systemImage: "shuffle") }
                            .buttonStyle(.bordered).controlSize(.large)
                            .disabled(tracks.isEmpty || !tracks.allSatisfy { model.canQueue(uri: $0.uri) })
                        Button {
                            if let playlistURI { model.queueAdd(uri: playlistURI) }
                        } label: { Label("Add to Queue", systemImage: "text.append") }
                            .buttonStyle(.bordered).controlSize(.large)
                            .disabled(!canQueuePlaylist)
                    }
                }
                Spacer()
            }
            .padding(20)
            Divider()

            if model.library.loadingTracksFor == playlist.id && tracks.isEmpty {
                SkeletonRows()
            } else {
                TrackListView(tracks: tracks)
            }
        }
        .background(.background)
        .navigationTitle(playlist.name)
        .task(id: playlist.id) { await model.library.loadTracks(for: playlist) }
    }
}
