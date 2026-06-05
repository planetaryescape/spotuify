import SwiftUI
import SpotuifyKit

struct PlaylistsView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        NavigationStack {
            Group {
                if model.library.loadingPlaylists && model.library.playlists.isEmpty {
                    ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if model.library.playlists.isEmpty {
                    ContentUnavailableView("No playlists", systemImage: "music.note.list")
                } else {
                    ScrollView {
                        LazyVStack(spacing: 2) {
                            ForEach(model.library.playlists) { playlist in
                                NavigationLink(value: playlist) {
                                    PlaylistRow(playlist: playlist)
                                }
                                .buttonStyle(.plain)
                            }
                        }
                        .padding(10)
                    }
                }
            }
            .navigationTitle("Playlists")
            .navigationDestination(for: Playlist.self) { playlist in
                PlaylistDetailView(playlist: playlist)
            }
        }
        .background(.background)
        .task { await model.library.loadPlaylists() }
    }
}

private struct PlaylistRow: View {
    let playlist: Playlist
    @State private var hovering = false

    var body: some View {
        HStack(spacing: 10) {
            AsyncCoverImage(url: playlist.imageURL, cornerRadius: 6)
                .frame(width: 44, height: 44)
            VStack(alignment: .leading, spacing: 2) {
                Text(playlist.name).font(.system(size: 13, weight: .medium)).lineLimit(1)
                Text("\(playlist.tracksTotal) tracks · \(playlist.owner)")
                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
            Spacer()
            Image(systemName: "chevron.right").font(.caption).foregroundStyle(.tertiary)
        }
        .padding(.vertical, 4).padding(.horizontal, 8)
        .background {
            RoundedRectangle(cornerRadius: 8).fill(hovering ? AnyShapeStyle(.primary.opacity(0.06)) : AnyShapeStyle(.clear))
        }
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
    }
}

struct PlaylistDetailView: View {
    @Environment(AppModel.self) private var model
    let playlist: Playlist

    private var tracks: [MediaItem] { model.library.tracks(for: playlist) }

    private var playlistURI: String { "spotify:playlist:\(playlist.id)" }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 16) {
                AsyncCoverImage(url: playlist.imageURL, cornerRadius: 10)
                    .frame(width: 120, height: 120)
                    .shadow(radius: 8, y: 4)
                VStack(alignment: .leading, spacing: 8) {
                    Text(playlist.name).font(.title.bold()).lineLimit(2)
                    Text("\(playlist.tracksTotal) tracks · \(playlist.owner)")
                        .foregroundStyle(.secondary)
                    HStack(spacing: 10) {
                        Button { model.play(uri: playlistURI) } label: { Label("Play", systemImage: "play.fill") }
                            .buttonStyle(.borderedProminent).controlSize(.large)
                        Button { model.shufflePlay(uris: tracks.map(\.uri)) } label: { Label("Shuffle", systemImage: "shuffle") }
                            .buttonStyle(.bordered).controlSize(.large)
                        Button { model.queueAdd(uri: playlistURI) } label: { Label("Add to Queue", systemImage: "text.append") }
                            .buttonStyle(.bordered).controlSize(.large)
                    }
                }
                Spacer()
            }
            .padding(20)
            Divider()

            if model.library.loadingTracksFor == playlist.id && tracks.isEmpty {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                TrackListView(tracks: tracks)
            }
        }
        .background(.background)
        .navigationTitle(playlist.name)
        .task(id: playlist.id) { await model.library.loadTracks(for: playlist) }
    }
}
