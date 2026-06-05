import SwiftUI
import SpotuifyKit

/// The menubar popover companion. Shares the same AppModel as the main window
/// so they stay perfectly in sync. Enriched further in Phase 5.
struct MenuBarView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openWindow) private var openWindow

    private var item: MediaItem? { model.player.currentItem }

    var body: some View {
        VStack(spacing: 12) {
            HStack(spacing: 12) {
                AsyncCoverImage(url: item?.imageURL, cornerRadius: 8)
                    .frame(width: 64, height: 64)
                VStack(alignment: .leading, spacing: 3) {
                    Text(item?.name ?? "Nothing playing")
                        .font(.system(size: 14, weight: .semibold))
                        .lineLimit(2)
                    Text(item?.subtitle ?? "")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer(minLength: 0)
            }

            SeekBar(progress: model.player.progressFraction, onSeek: { model.seek(toFraction: $0) }, height: 4)

            HStack(spacing: 18) {
                TransportButton(systemName: "shuffle", size: 12) { model.toggleShuffle() }
                    .foregroundStyle(model.player.shuffle ? AnyShapeStyle(.tint) : AnyShapeStyle(.secondary))
                TransportButton(systemName: "backward.fill", size: 16) { model.previous() }
                TransportButton(
                    systemName: model.player.isPlaying ? "pause.fill" : "play.fill",
                    size: 18, prominent: true) { model.togglePlayPause() }
                TransportButton(systemName: "forward.fill", size: 16) { model.next() }
                TransportButton(
                    systemName: model.player.repeatMode == .track ? "repeat.1" : "repeat",
                    size: 12) { model.cycleRepeat() }
                    .foregroundStyle(model.player.repeatMode == .off ? AnyShapeStyle(.secondary) : AnyShapeStyle(.tint))
            }

            Divider()

            HStack {
                DeviceMenu()
                Spacer()
                Button("Open Player") { openWindow(id: "player") }
                    .controlSize(.small)
                Button("Mini") { openWindow(id: "mini-player") }
                    .controlSize(.small)
                Button("Quit") { NSApplication.shared.terminate(nil) }
                    .controlSize(.small)
            }
            .font(.caption)
        }
        .padding(14)
        .frame(width: 300)
    }
}
