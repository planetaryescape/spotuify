import SwiftUI
import SpotuifyKit

/// The menubar popover companion — now cover-art-first to match the main
/// window: a palette-tinted header with the artwork as hero, the metadata over
/// a scrim, and the transport below. Shares the same AppModel + ArtworkTheme as
/// the main window so they stay perfectly in sync.
struct MenuBarView: View {
    @Environment(AppModel.self) private var model
    @Environment(ArtworkTheme.self) private var theme
    @Environment(\.openWindow) private var openWindow

    private var item: MediaItem? { model.player.currentItem }
    private var palette: ArtworkPalette { theme.palette }

    var body: some View {
        VStack(spacing: 0) {
            header
            controls
        }
        .frame(width: 320)
        .task(id: item?.imageURL) { await theme.update(for: item?.imageURL) }
    }

    private var header: some View {
        ZStack(alignment: .bottom) {
            ZStack {
                palette.background
                AsyncCoverImage(url: item?.imageURL, cornerRadius: 0)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .clipped()
                    .blur(radius: 28).opacity(0.5)
                LinearGradient(
                    colors: [.clear, palette.background.opacity(0.55), .black.opacity(0.85)],
                    startPoint: .top, endPoint: .bottom)
            }
            VStack(spacing: 10) {
                AsyncCoverImage(url: item?.imageURL, cornerRadius: 10)
                    .frame(width: 132, height: 132)
                    .shadow(color: palette.accent.opacity(0.45), radius: 18, y: 8)
                    .shadow(color: .black.opacity(0.4), radius: 12, y: 6)
                VStack(spacing: 3) {
                    Text(item?.name ?? "Nothing playing")
                        .font(.displayTitle(17)).foregroundStyle(.white)
                        .lineLimit(1).minimumScaleFactor(0.7)
                    Text(item?.subtitle ?? "")
                        .font(.caption).foregroundStyle(.white.opacity(0.78)).lineLimit(1)
                    if let album = item?.albumLabel {
                        Text(album)
                            .font(.caption2).foregroundStyle(.white.opacity(0.5)).lineLimit(1)
                    }
                }
                SeekBar(
                    progress: model.player.progressFraction,
                    onSeek: { model.seek(toFraction: $0) },
                    height: 3)
                .tint(palette.accent)
            }
            .padding(.top, 20).padding(.bottom, 14).padding(.horizontal, 14)
        }
        .frame(height: 264)
        .clipped()
    }

    private var controls: some View {
        VStack(spacing: 12) {
            GlassEffectContainer(spacing: 10) {
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
                .padding(.horizontal, 18)
                .padding(.vertical, 9)
                .glassEffect(.regular.tint(palette.accent.opacity(0.20)).interactive(), in: .capsule)
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
    }
}
