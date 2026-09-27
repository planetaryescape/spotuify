import SwiftUI
import SpotuifyKit

/// Liked Songs — the user's real saved tracks (`/me/tracks`), with filter,
/// sort, and Play-all / Shuffle / Queue-all actions.
struct LikedSongsView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let liked = model.library.likedSongs
        NavigationStack {
            Group {
                if model.library.loadingLiked && liked.isEmpty {
                    SkeletonRows()
                } else if liked.isEmpty {
                    EmptyState("No liked songs", systemImage: "heart",
                        description: Text("Songs you like on Spotify show up here."))
                } else {
                    TrackListView(
                        tracks: liked,
                        storageKey: "likedLayout",
                        onReachEnd: { Task { await model.library.loadMoreLiked() } },
                        contextURI: AppModel.likedContext,
                        totalCount: model.library.likedTotal
                    ) {
                        CollectionHeader(
                            icon: "heart.fill",
                            title: "Liked Songs",
                            // The daemon-reported library total, so the count is
                            // right even before every page has lazy-loaded.
                            subtitle: model.library.likedTotal == 1 ? "1 song" : "\(model.library.likedTotal) songs",
                            uris: liked.map(\.uri),
                            playContextURI: AppModel.likedContext)
                    }
                }
            }
            .mediaDetailDestinations()
        }
        .task { await model.library.loadLiked() }
    }
}

/// Saved albums → album detail, as a card grid or list (toggle persisted).
struct AlbumsView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 0) {
                EditorialPageHeader("Albums", eyebrow: "Library")
                if model.library.loadingAlbums && model.library.savedAlbums.isEmpty {
                    SkeletonTiles()
                } else if model.library.savedAlbums.isEmpty {
                    EmptyState("No saved albums", systemImage: "square.stack")
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    CollectionView(items: model.library.savedAlbums, storageKey: "albumsLayout")
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .mediaDetailDestinations()
        }
        .task { await model.library.loadAlbums() }
    }
}

/// Followed artists → artist discography, as a card grid or list (toggle persisted).
struct ArtistsView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 0) {
                EditorialPageHeader("Artists", eyebrow: "Library")
                if model.library.loadingArtists && model.library.followedArtists.isEmpty {
                    SkeletonTiles(minTile: 150)
                } else if model.library.followedArtists.isEmpty {
                    EmptyState("No followed artists", systemImage: "music.mic",
                        description: Text("Artists you follow on Spotify show up here."))
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    CollectionView(
                        items: model.library.followedArtists,
                        storageKey: "artistsLayout", minTile: 150, maxTile: 190)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .mediaDetailDestinations()
        }
        .task { await model.library.loadFollowedArtists() }
    }
}

/// The hero for a cover-less collection of tracks (Liked Songs): Play /
/// Shuffle / Queue-all over a glyph cover.
struct CollectionHeader: View {
    @Environment(AppModel.self) private var model
    let icon: String
    let title: String
    let subtitle: String
    let uris: [String]
    /// When set, the header Play button starts the whole collection at its
    /// first track inside this context (e.g. ``AppModel/likedContext``), so
    /// header + row taps converge on the same in-context playback. `nil`
    /// keeps the play-first-then-queue-rest fallback.
    var playContextURI: String?

    var body: some View {
        HeroHeader(eyebrow: "Collection", title: title) {
            GlyphCoverArt(systemName: icon)
        } credits: {
            Text(subtitle).font(.mono(12))
        } actions: {
            RoomPlayButton(label: "Play \(title)") {
                if let playContextURI, let first = uris.first {
                    model.play(uri: first, contextURI: playContextURI)
                } else {
                    model.playAll(uris: uris)
                }
            }
            .disabled(uris.isEmpty || !model.canPlay(uri: uris[0]))
            RoomIconButton(systemName: "shuffle", label: "Shuffle") { model.shufflePlay(uris: uris) }
                .disabled(
                    uris.isEmpty || !model.canPlay(uri: uris[0])
                        || !uris.dropFirst().allSatisfy { model.canQueue(uri: $0) })
            RoomIconButton(systemName: "text.append", label: "Queue All") { model.queueAll(uris: uris) }
                .disabled(uris.isEmpty || !uris.allSatisfy { model.canQueue(uri: $0) })
        }
    }
}

/// Square artwork tile for album/show grids. Lifts and deepens its shadow on
/// hover so the cover art reads as the hero of the grid.
struct ArtworkTile: View {
    let item: MediaItem
    @Environment(\.room) private var room
    @State private var hovering = false

    private var isCircle: Bool { item.kind == .artist }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            AsyncCoverImage(url: item.imageURL, cornerRadius: isCircle ? 0 : Theme.tileCornerRadius)
                .circularArtwork(isCircle)
                .aspectRatio(1, contentMode: .fit)
                .overlay {
                    // Hairline edge so dark covers keep their shape in a dark room.
                    if isCircle {
                        Circle().strokeBorder(room.hairline)
                    } else {
                        RoundedRectangle(cornerRadius: Theme.tileCornerRadius, style: .continuous).strokeBorder(room.hairline)
                    }
                }
                .shadow(color: .black.opacity(hovering ? 0.45 : 0.25),
                        radius: hovering ? 22 : 10, y: hovering ? 14 : 5)
                .offset(y: hovering ? -4 : 0)
            Text(item.name)
                .font(.displayTitle(15.5))
                .lineLimit(1)
                .padding(.top, 4)
            if !item.subtitle.isEmpty {
                Text(item.subtitle).font(.system(size: 12, weight: .medium)).foregroundStyle(.secondary).lineLimit(1)
            }
            if let meta = item.metaLine {
                MonoCaps(meta, size: 9)
            }
        }
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .animation(.spring(response: 0.3, dampingFraction: 0.7), value: hovering)
    }
}
