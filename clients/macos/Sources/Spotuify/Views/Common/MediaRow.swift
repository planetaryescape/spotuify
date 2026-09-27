import SwiftUI
import SpotuifyKit

/// Display affordances for a `MediaKind`: its section heading and whether
/// items of this kind can be queued.
extension MediaKind {
    var sectionTitle: String {
        switch self {
        case .track: "Songs"
        case .artist: "Artists"
        case .album: "Albums"
        case .playlist: "Playlists"
        case .show: "Podcasts"
        case .episode: "Episodes"
        case .other: "Other"
        }
    }

    var isQueueable: Bool { self == .track || self == .episode }
}

/// A reusable result/list row: an optional track number, artwork,
/// title/subtitle, album + date-added columns when the table has room, and
/// hover actions. The number turns into a live meter on the playing row and a
/// play glyph on hover. Double-click plays (or plays the context for
/// albums/playlists).
struct MediaRow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.room) private var room
    @Environment(\.trackColumns) private var columns
    let item: MediaItem
    var showsArtwork = true
    /// Show album + date-added columns (for track tables).
    var detailed = false
    var fallbackImageURL: String?
    /// Collection context this row plays inside of (album/playlist URI, or
    /// ``AppModel/likedContext``). `nil` keeps single-track play behaviour.
    var contextURI: String?
    /// 1-based position for numbered track lists; `nil` hides the number column.
    var index: Int?

    @State private var hovering = false
    @State private var showReminderPicker = false
    @State private var justQueued = false

    private var isCurrent: Bool { model.player.currentItem?.uri == item.uri }

    var body: some View {
        HStack(spacing: TrackColumnLayout.spacing) {
            if let index {
                leadingNumber(index)
            }
            if showsArtwork {
                artwork
            }
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    if item.kind == .episode, item.isFullyPlayed {
                        Image(systemName: "checkmark.circle.fill")
                            .font(.caption2).foregroundStyle(.tertiary)
                    } else if item.kind == .episode, item.isInProgress {
                        Image(systemName: "circle.lefthalf.filled")
                            .font(.caption2).foregroundStyle(room.accent)
                    }
                    Text(item.name)
                        .font(.system(size: 13.5, weight: isCurrent ? .semibold : .medium))
                        .foregroundStyle(isCurrent ? room.accent : room.ink)
                        .lineLimit(1)
                    if item.explicit == true {
                        Text("E")
                            .font(.mono(8.5, weight: .bold))
                            .foregroundStyle(room.base)
                            .frame(width: 13, height: 13)
                            .background(room.inkFaint, in: RoundedRectangle(cornerRadius: 3))
                            .accessibilityLabel("Explicit")
                    }
                }
                subtitleView
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if detailed {
                if columns.showsAlbum { albumColumn }
                if columns.showsDateAdded { dateAddedColumn }
            }
            HStack(spacing: 10) {
                Button {
                    model.queueAdd(uri: item.uri)
                    justQueued = true
                    Task { try? await Task.sleep(for: .seconds(1.2)); justQueued = false }
                } label: {
                    Image(systemName: justQueued ? "checkmark" : "text.append")
                        .foregroundStyle(justQueued ? room.accent : room.inkMuted)
                        .contentTransition(.symbolEffect(.replace))
                }
                .buttonStyle(.plain).help("Add to queue")
                .disabled(!model.canQueue(uri: item.uri))
                .opacity((hovering || justQueued) && item.kind.isQueueable ? 1 : 0)
                .allowsHitTesting(hovering && item.kind.isQueueable)
                Menu {
                    MediaItemMenu(item: item, contextURI: contextURI, onRemind: { showReminderPicker = true })
                } label: {
                    Image(systemName: "ellipsis").font(.body).foregroundStyle(room.inkMuted)
                }
                .menuStyle(.button)
                .buttonStyle(.plain)
                .menuIndicator(.hidden)
                .fixedSize()
                .opacity(hovering ? 1 : 0)
                .help("More actions")
            }
            .frame(width: Theme.TrackColumn.actions, alignment: .trailing)
            Text(item.durationMs > 0 ? durationLabel : "")
                .font(.mono(11))
                .foregroundStyle(room.inkFaint)
                .frame(width: Theme.TrackColumn.duration, alignment: .leading)
        }
        .padding(.vertical, 5)
        .padding(.horizontal, TrackColumnLayout.horizontalPadding)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background {
            RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous)
                .fill(room.ink.opacity(isCurrent ? 0.07 : (hovering ? 0.045 : 0)))
        }
        .contentShape(Rectangle())
        .onTapGesture(count: 2) { play() }
        .onHover { hovering = $0 }
        .contextMenu {
            MediaItemMenu(item: item, contextURI: contextURI, onRemind: { showReminderPicker = true })
        }
        .sheet(isPresented: $showReminderPicker) {
            ReminderPickerView(item: item)
        }
    }

    private func play() { model.play(uri: item.uri, contextURI: contextURI) }

    /// "07" at rest, a live meter when playing, a play glyph under the pointer.
    private func leadingNumber(_ index: Int) -> some View {
        ZStack {
            if hovering && !isCurrent {
                Button(action: play) {
                    Image(systemName: "play.fill").font(.system(size: 11, weight: .bold))
                        .foregroundStyle(room.ink)
                        .frame(width: 26, height: 26)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(!model.canPlay(uri: item.uri))
                .help("Play")
            } else if isCurrent {
                LevelMeter(isPlaying: model.player.isPlaying, size: 11)
                    .foregroundStyle(room.accent)
            } else {
                Text(String(format: "%02d", index))
                    .font(.mono(11))
                    .foregroundStyle(room.inkFaint)
            }
        }
        .frame(width: 26)
    }

    /// Without a number column the artwork carries the play affordance.
    private var artwork: some View {
        AsyncCoverImage(url: item.imageURL ?? fallbackImageURL, cornerRadius: item.kind == .artist ? 20 : 6)
            .frame(width: Theme.TrackColumn.artwork, height: Theme.TrackColumn.artwork)
            .overlay {
                if index == nil && (hovering || isCurrent) {
                    RoundedRectangle(cornerRadius: item.kind == .artist ? 20 : 6).fill(.black.opacity(0.45))
                    if isCurrent && !hovering {
                        LevelMeter(isPlaying: model.player.isPlaying, size: 12).foregroundStyle(.white)
                    } else {
                        Button(action: play) {
                            Image(systemName: "play.fill").font(.system(size: 13, weight: .bold))
                                .foregroundStyle(.white)
                                .frame(width: 40, height: 40)
                                .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .disabled(!model.canPlay(uri: item.uri))
                        .help("Play")
                    }
                }
            }
    }

    /// Subtitle line: clickable artist links when the item carries navigable
    /// artist refs, else the plain subtitle text.
    @ViewBuilder
    private var subtitleView: some View {
        let artistItems = item.artistNavItems
        if !artistItems.isEmpty {
            HStack(spacing: 3) {
                ForEach(Array(artistItems.enumerated()), id: \.element.id) { index, artist in
                    if index > 0 {
                        Text(",").font(.caption).foregroundStyle(.secondary)
                    }
                    NavigationLink(value: artist) {
                        NavLinkLabel(name: artist.name).font(.system(size: 12)).lineLimit(1)
                    }
                    .buttonStyle(.plain)
                }
            }
        } else if !item.subtitle.isEmpty {
            Text(item.subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1)
        }
    }

    @ViewBuilder
    private var albumColumn: some View {
        if let album = item.albumLabel, let albumNav = item.albumNavItem {
            NavigationLink(value: albumNav) {
                NavLinkLabel(name: album).font(.caption).lineLimit(1)
            }
            .buttonStyle(.plain)
            .frame(width: columns.albumWidth, alignment: .leading)
        } else if let album = item.albumLabel {
            Text(album)
                .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                .frame(width: columns.albumWidth, alignment: .leading)
        } else {
            Color.clear.frame(width: columns.albumWidth, height: 1)
        }
    }

    @ViewBuilder
    private var dateAddedColumn: some View {
        Text(relativeDate(item.addedAtMs) ?? "")
            .font(.mono(10.5)).foregroundStyle(room.inkFaint)
            .frame(width: Theme.TrackColumn.dateAdded, alignment: .leading)
    }

    private var durationLabel: String {
        if item.kind == .episode, item.isInProgress, let resume = item.resumePositionMs {
            let left = item.durationMs > resume ? item.durationMs - resume : 0
            return "\(Theme.timeString(left)) left"
        }
        return Theme.timeString(item.durationMs)
    }

    private static let addedAtFormatter: DateFormatter = {
        let f = DateFormatter()
        f.dateFormat = "MMM yyyy"
        return f
    }()

    private func relativeDate(_ ms: Int64?) -> String? {
        guard let ms, ms > 0 else { return nil }
        let date = Date(timeIntervalSince1970: Double(ms) / 1000)
        return Self.addedAtFormatter.string(from: date)
    }
}
