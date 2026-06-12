import SwiftUI
import SpotuifyKit

/// Secondary-styled inline link label: underlines on hover. Inherits ambient font; wrap in a `NavigationLink`.
struct NavLinkLabel: View {
    let name: String
    @State private var hovering = false

    var body: some View {
        Text(name)
            .foregroundStyle(.secondary)
            .underline(hovering, pattern: .solid)
            .onHover { hovering = $0 }
            .animation(.easeOut(duration: 0.1), value: hovering)
    }
}

/// The canonical action set for a media item, shared by the right-click
/// `.contextMenu` and the visible `⋯` button so every surface offers the same
/// discoverable actions: play, queue, follow the artist(s), and (where wired) a
/// reminder. Navigation ("go to artist/album") stays as inline links on the row
/// itself — context menus can't push onto a NavigationStack reliably.
struct MediaItemMenu: View {
    @Environment(AppModel.self) private var model
    let item: MediaItem
    /// Optional reminder hook; when nil the "Remind me…" item is hidden.
    var onRemind: (() -> Void)?

    var body: some View {
        Button { model.play(uri: item.uri) } label: {
            Label("Play", systemImage: "play.fill")
        }
        if item.kind.isQueueable {
            Button { model.queueAdd(uri: item.uri) } label: {
                Label("Add to Queue", systemImage: "text.append")
            }
        }
        if item.kind != .artist && item.kind != .playlist {
            let liked = item.inLibrary == true
            Button { model.toggleLike(item) } label: {
                Label(liked ? "Remove from Library" : "Add to Library",
                      systemImage: liked ? "heart.fill" : "heart")
            }
        }
        followSection
        if let onRemind {
            Divider()
            Button { onRemind() } label: { Label("Remind me…", systemImage: "bell") }
        }
    }

    @ViewBuilder
    private var followSection: some View {
        if item.kind == .artist {
            Divider()
            Button { model.followArtist(uri: item.uri) } label: {
                Label("Follow \(item.name)", systemImage: "person.badge.plus")
            }
        } else {
            let artists = item.artists.filter { !$0.uri.isEmpty }
            if !artists.isEmpty {
                Divider()
                ForEach(artists) { artist in
                    Button { model.followArtist(uri: artist.uri) } label: {
                        Label("Follow \(artist.name)", systemImage: "person.badge.plus")
                    }
                }
            }
        }
    }
}
