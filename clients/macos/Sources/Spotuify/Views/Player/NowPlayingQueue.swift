import SwiftUI
import SpotuifyKit

/// The up-next queue as a companion to the record: the current track, then
/// what follows. Click an upcoming row to play it. Also used by the global side
/// rail, which passes the regular text colour instead of the stage's.
struct NowPlayingQueue: View {
    @Environment(AppModel.self) private var model
    let accent: Color
    var textColor: Color = .white

    private var current: MediaItem? { model.player.currentItem }
    private var upcoming: [MediaItem] { model.player.queue?.items ?? [] }

    var body: some View {
        if current == nil && upcoming.isEmpty {
            VStack(spacing: 10) {
                Image(systemName: "list.bullet")
                    .font(.system(size: 30, weight: .light)).foregroundStyle(textColor.opacity(0.45))
                Text("Nothing queued")
                    .font(.displayTitle(20)).foregroundStyle(textColor.opacity(0.8))
                Text("Songs you queue show up here.")
                    .font(.callout).foregroundStyle(textColor.opacity(0.55))
                    .multilineTextAlignment(.center)
            }
            .frame(maxWidth: 260)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            ScrollView(showsIndicators: false) {
                LazyVStack(alignment: .leading, spacing: 2) {
                    if let current {
                        header("Now Playing")
                        row(current, isCurrent: true)
                    }
                    if !upcoming.isEmpty {
                        header("Up Next")
                        ForEach(Array(upcoming.enumerated()), id: \.offset) { _, item in
                            row(item, isCurrent: false)
                        }
                    }
                }
                .padding(.vertical, 6)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .mask(
                LinearGradient(
                    stops: [
                        .init(color: .clear, location: 0),
                        .init(color: .black, location: 0.04),
                        .init(color: .black, location: 0.94),
                        .init(color: .clear, location: 1),
                    ],
                    startPoint: .top, endPoint: .bottom))
        }
    }

    private func header(_ text: String) -> some View {
        Text(text.uppercased())
            .font(.system(size: 10.5, weight: .semibold))
            .tracking(1.4)
            .foregroundStyle(textColor.opacity(0.6))
            .padding(.horizontal, 12)
            .padding(.top, 16).padding(.bottom, 4)
    }

    private func row(_ item: MediaItem, isCurrent: Bool) -> some View {
        QueueCompanionRow(item: item, isCurrent: isCurrent, accent: accent, textColor: textColor) {
            if !isCurrent { model.play(uri: item.uri) }
        }
    }
}

private struct QueueCompanionRow: View {
    @Environment(AppModel.self) private var model
    let item: MediaItem
    let isCurrent: Bool
    let accent: Color
    let textColor: Color
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 12) {
                AsyncCoverImage(url: item.imageURL, cornerRadius: 5)
                    .frame(width: 40, height: 40)
                    .overlay {
                        if isCurrent || hovering {
                            RoundedRectangle(cornerRadius: 5).fill(.black.opacity(0.45))
                            Group {
                                if isCurrent {
                                    LevelMeter(isPlaying: model.player.isPlaying, size: 13)
                                } else {
                                    Image(systemName: "play.fill").font(.system(size: 13, weight: .bold))
                                }
                            }
                            .foregroundStyle(.white)
                        }
                    }
                VStack(alignment: .leading, spacing: 2) {
                    Text(item.name)
                        .font(.system(size: 13.5, weight: isCurrent ? .semibold : .medium))
                        .foregroundStyle(textColor).lineLimit(1)
                    if !item.subtitle.isEmpty {
                        Text(item.subtitle)
                            .font(.caption).foregroundStyle(textColor.opacity(0.62)).lineLimit(1)
                    }
                }
                Spacer(minLength: 8)
                Text(Theme.timeString(item.durationMs))
                    .font(.caption.monospacedDigit()).foregroundStyle(textColor.opacity(0.5))
            }
            .padding(.horizontal, 10).padding(.vertical, 6)
            .background(
                RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous)
                    .fill(textColor.opacity(isCurrent ? 0.12 : (hovering ? 0.07 : 0))))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        // Not `.disabled`: that greyed out the one row that matters most.
        .allowsHitTesting(!isCurrent)
        .onHover { hovering = $0 }
        .accessibilityLabel(isCurrent ? "Now playing: \(item.name)" : "Play \(item.name)")
    }
}
