import SwiftUI
import SpotuifyKit

/// Liked Songs — the user's real saved tracks (`/me/tracks`), with filter,
/// sort, and Play-all / Shuffle / Queue-all actions.
struct LikedSongsView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let liked = model.library.likedSongs
        Group {
            if model.library.loadingLiked && liked.isEmpty {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if liked.isEmpty {
                ContentUnavailableView("No liked songs", systemImage: "heart",
                    description: Text("Songs you like on Spotify show up here."))
            } else {
                TrackListView(tracks: liked) {
                    CollectionHeader(
                        icon: "heart.fill",
                        title: "Liked Songs",
                        subtitle: "\(liked.count) songs",
                        uris: liked.map(\.uri))
                }
            }
        }
        .background(.background)
        .task { await model.library.loadLiked() }
    }
}

/// Saved albums grid → album detail.
struct AlbumsView: View {
    @Environment(AppModel.self) private var model

    private let columns = [GridItem(.adaptive(minimum: 150, maximum: 200), spacing: 16)]

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 0) {
                EditorialPageHeader("Albums")
                Divider()
                if model.library.loadingAlbums && model.library.savedAlbums.isEmpty {
                    ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if model.library.savedAlbums.isEmpty {
                    ContentUnavailableView("No saved albums", systemImage: "square.stack")
                } else {
                    ScrollView {
                        LazyVGrid(columns: columns, spacing: 16) {
                            ForEach(model.library.savedAlbums) { album in
                                NavigationLink(value: album) {
                                    ArtworkTile(item: album)
                                }
                                .buttonStyle(.plain)
                            }
                        }
                        .padding(16)
                    }
                }
            }
            .mediaDetailDestinations()
        }
        .background(.background)
        .task { await model.library.loadAlbums() }
    }
}

/// Followed artists grid → artist discography (with the All / In-Library toggle).
struct ArtistsView: View {
    @Environment(AppModel.self) private var model

    private let columns = [GridItem(.adaptive(minimum: 140, maximum: 180), spacing: 16)]

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 0) {
                EditorialPageHeader("Artists")
                Divider()
                if model.library.loadingArtists && model.library.followedArtists.isEmpty {
                    ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if model.library.followedArtists.isEmpty {
                    ContentUnavailableView("No followed artists", systemImage: "music.mic",
                        description: Text("Artists you follow on Spotify show up here."))
                } else {
                    ScrollView {
                        LazyVGrid(columns: columns, spacing: 16) {
                            ForEach(model.library.followedArtists) { artist in
                                NavigationLink(value: artist) {
                                    ArtworkTile(item: artist)
                                }
                                .buttonStyle(.plain)
                            }
                        }
                        .padding(16)
                    }
                }
            }
            .mediaDetailDestinations()
        }
        .background(.background)
        .task { await model.library.loadFollowedArtists() }
    }
}

/// A header bar with Play / Shuffle / Queue-all for a collection of tracks.
struct CollectionHeader: View {
    @Environment(AppModel.self) private var model
    let icon: String
    let title: String
    let subtitle: String
    let uris: [String]

    var body: some View {
        HStack(spacing: 14) {
            Image(systemName: icon)
                .font(.system(size: 30))
                .foregroundStyle(.tint)
                .frame(width: 56, height: 56)
                .background(.tint.opacity(0.15), in: RoundedRectangle(cornerRadius: 10))
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.displayTitle(26))
                Text(subtitle).font(.callout).foregroundStyle(.secondary)
            }
            Spacer()
            Button { model.playAll(uris: uris) } label: { Label("Play", systemImage: "play.fill") }
                .buttonStyle(.borderedProminent)
            Button { model.shufflePlay(uris: uris) } label: { Label("Shuffle", systemImage: "shuffle") }
                .buttonStyle(.bordered)
            Button { model.queueAll(uris: uris) } label: { Label("Queue All", systemImage: "text.append") }
                .buttonStyle(.bordered)
        }
        .padding(16)
    }
}

/// Square artwork tile for album/show grids. Lifts and deepens its shadow on
/// hover so the cover art reads as the hero of the grid.
struct ArtworkTile: View {
    let item: MediaItem
    @State private var hovering = false

    private var isCircle: Bool { item.kind == .artist }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            AsyncCoverImage(url: item.imageURL, cornerRadius: isCircle ? 200 : Theme.tileCornerRadius)
                .aspectRatio(1, contentMode: .fit)
                .shadow(color: .black.opacity(hovering ? 0.4 : 0.22),
                        radius: hovering ? 18 : 8, y: hovering ? 10 : 4)
                .scaleEffect(hovering ? 1.03 : 1)
            Text(item.name)
                .font(.system(size: 13, weight: .semibold))
                .lineLimit(1)
            if !item.subtitle.isEmpty {
                Text(item.subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
        }
        .padding(6)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .animation(.spring(response: 0.3, dampingFraction: 0.7), value: hovering)
    }
}
