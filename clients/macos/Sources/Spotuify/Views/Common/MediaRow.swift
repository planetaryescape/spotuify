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
                if !item.subtitle.isEmpty {
                    Text(item.subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            Spacer(minLength: 8)
            if detailed, let album = item.albumLabel {
                Text(album)
                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    .frame(width: 160, alignment: .leading)
            }
            if detailed, let added = relativeDate(item.addedAtMs) {
                Text(added)
                    .font(.caption2).foregroundStyle(.secondary)
                    .frame(width: 84, alignment: .trailing)
            }
            if item.explicit == true {
                Image(systemName: "e.square.fill").font(.caption2).foregroundStyle(.tertiary)
            }
            if hovering, item.kind.isQueueable {
                Button { model.queueAdd(uri: item.uri) } label: {
                    Image(systemName: "text.append")
                }
                .buttonStyle(.plain).help("Add to queue")
            }
            Button { model.play(uri: item.uri) } label: {
                Image(systemName: "play.circle.fill").font(.title3)
            }
            .buttonStyle(.plain)
            .opacity(hovering ? 1 : 0)
            .help("Play")
            if item.durationMs > 0 {
                Text(durationLabel)
                    .font(.caption2.monospacedDigit()).foregroundStyle(.secondary)
                    .frame(width: 48, alignment: .trailing)
            }
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
            Button("Play") { model.play(uri: item.uri) }
            if item.kind.isQueueable {
                Button("Add to Queue") { model.queueAdd(uri: item.uri) }
            }
        }
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
