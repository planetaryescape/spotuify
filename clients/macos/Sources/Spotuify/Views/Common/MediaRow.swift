import SwiftUI
import SpotuifyKit

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

/// A reusable result/list row: artwork, title/subtitle, optional album +
/// date-added columns, hover play & queue actions, and a right-click menu.
/// Double-click plays (or plays the context for albums/playlists).
struct MediaRow: View {
    @Environment(AppModel.self) private var model
    let item: MediaItem
    var showsArtwork = true
    /// Show album + date-added columns (for track tables).
    var detailed = false

    @State private var hovering = false
    @State private var showReminderPicker = false
    @State private var justQueued = false

    var body: some View {
        HStack(spacing: 10) {
            if showsArtwork {
                AsyncCoverImage(url: item.imageURL, cornerRadius: item.kind == .artist ? 20 : 6)
                    .frame(width: 40, height: 40)
            }
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    if item.kind == .episode, item.isFullyPlayed {
                        Image(systemName: "checkmark.circle.fill")
                            .font(.caption2).foregroundStyle(.tertiary)
                    } else if item.kind == .episode, item.isInProgress {
                        Image(systemName: "circle.lefthalf.filled")
                            .font(.caption2).foregroundStyle(.tint)
                    }
                    Text(item.name).font(.system(size: 13, weight: .medium)).lineLimit(1)
                }
                subtitleView
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            if detailed {
                albumColumn
                dateAddedColumn
            }
            HStack(spacing: 8) {
                if item.explicit == true {
                    Image(systemName: "e.square.fill").font(.caption2).foregroundStyle(.tertiary)
                } else {
                    Color.clear.frame(width: 14, height: 1)
                }
                Button {
                    model.queueAdd(uri: item.uri)
                    justQueued = true
                    Task { try? await Task.sleep(for: .seconds(1.2)); justQueued = false }
                } label: {
                    Image(systemName: justQueued ? "checkmark" : "text.append")
                        .foregroundStyle(justQueued ? AnyShapeStyle(.tint) : AnyShapeStyle(.primary))
                        .contentTransition(.symbolEffect(.replace))
                }
                .buttonStyle(.plain).help("Add to queue")
                .opacity((hovering || justQueued) && item.kind.isQueueable ? 1 : 0)
                .allowsHitTesting(hovering && item.kind.isQueueable)
                Button { model.play(uri: item.uri) } label: {
                    Image(systemName: "play.circle.fill").font(.title3)
                }
                .buttonStyle(.plain)
                .opacity(hovering ? 1 : 0)
                .allowsHitTesting(hovering)
                .help("Play")
                Menu {
                    MediaItemMenu(item: item, onRemind: { showReminderPicker = true })
                } label: {
                    Image(systemName: "ellipsis").font(.body)
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
                .opacity(hovering ? 1 : 0.35)
                .help("More actions")
            }
            .frame(width: 100)
            Text(item.durationMs > 0 ? durationLabel : "")
                .font(.caption2.monospacedDigit()).foregroundStyle(.secondary)
                .frame(width: 48, alignment: .trailing)
        }
        .padding(.vertical, 4)
        .padding(.horizontal, 8)
        .background {
            RoundedRectangle(cornerRadius: 8)
                .fill(hovering ? AnyShapeStyle(.primary.opacity(0.06)) : AnyShapeStyle(.clear))
        }
        .contentShape(Rectangle())
        .onTapGesture(count: 2) { model.play(uri: item.uri) }
        .onHover { hovering = $0 }
        .contextMenu {
            MediaItemMenu(item: item, onRemind: { showReminderPicker = true })
        }
        .sheet(isPresented: $showReminderPicker) {
            ReminderPickerView(item: item)
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
                        NavLinkLabel(name: artist.name).font(.caption).lineLimit(1)
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
            .frame(width: 180, alignment: .leading)
        } else if let album = item.albumLabel {
            Text(album)
                .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                .frame(width: 180, alignment: .leading)
        } else {
            Color.clear.frame(width: 180, height: 1)
        }
    }

    @ViewBuilder
    private var dateAddedColumn: some View {
        Text(relativeDate(item.addedAtMs) ?? "")
            .font(.caption2).foregroundStyle(.secondary)
            .frame(width: 72, alignment: .trailing)
    }

    private var durationLabel: String {
        if item.kind == .episode, item.isInProgress, let resume = item.resumePositionMs {
            let left = item.durationMs > resume ? item.durationMs - resume : 0
            return "\(Theme.timeString(left)) left"
        }
        return Theme.timeString(item.durationMs)
    }

    private func relativeDate(_ ms: Int64?) -> String? {
        guard let ms, ms > 0 else { return nil }
        let date = Date(timeIntervalSince1970: Double(ms) / 1000)
        let formatter = DateFormatter()
        formatter.dateFormat = "MMM yyyy"
        return formatter.string(from: date)
    }
}
