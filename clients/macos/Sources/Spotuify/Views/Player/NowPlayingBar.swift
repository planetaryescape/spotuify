import SwiftUI
import SpotuifyKit

/// The always-visible bottom transport bar (Spotify-style), shown under every
/// destination so playback control is one click away from anywhere.
struct NowPlayingBar: View {
    @Environment(AppModel.self) private var model
    @Environment(ArtworkTheme.self) private var theme

    private var item: MediaItem? { model.player.currentItem }

    var body: some View {
        VStack(spacing: 0) {
            // A thin seek line spanning the whole bar.
            SeekBar(progress: model.player.progressFraction, onSeek: { model.seek(toFraction: $0) }, height: 4)
                .padding(.horizontal, 14)
                .padding(.top, 6)

            HStack(spacing: 14) {
                trackCell
                Spacer(minLength: 12)
                controls
                Spacer(minLength: 12)
                trailing
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
        }
        .frame(height: Theme.nowPlayingBarHeight)
        .background {
            ZStack {
                Rectangle().fill(.bar)
                LinearGradient(
                    colors: [theme.accent.opacity(0.10), .clear],
                    startPoint: .leading, endPoint: .trailing)
            }
        }
        .overlay(alignment: .top) {
            LinearGradient(
                colors: [theme.accent.opacity(0.55), theme.accent.opacity(0.0)],
                startPoint: .leading, endPoint: .trailing)
                .frame(height: 1)
        }
    }

    private var trackCell: some View {
        HStack(spacing: 10) {
            AsyncCoverImage(url: item?.imageURL, cornerRadius: 6)
                .frame(width: 44, height: 44)
            VStack(alignment: .leading, spacing: 2) {
                Text(item?.name ?? "Nothing playing")
                    .font(.system(size: 13, weight: .semibold))
                    .lineLimit(1)
                Text(item?.subtitle ?? "")
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
        }
        .frame(width: 240, alignment: .leading)
    }

    private var controls: some View {
        HStack(spacing: 14) {
            TransportButton(systemName: "shuffle", size: 12) { model.toggleShuffle() }
                .foregroundStyle(model.player.shuffle ? AnyShapeStyle(.tint) : AnyShapeStyle(.secondary))
            TransportButton(systemName: "backward.fill", size: 14) { model.previous() }
            TransportButton(
                systemName: model.player.isPlaying ? "pause.fill" : "play.fill",
                size: 16, prominent: true) { model.togglePlayPause() }
            TransportButton(systemName: "forward.fill", size: 14) { model.next() }
            TransportButton(
                systemName: model.player.repeatMode == .track ? "repeat.1" : "repeat",
                size: 12) { model.cycleRepeat() }
                .foregroundStyle(model.player.repeatMode == .off ? AnyShapeStyle(.secondary) : AnyShapeStyle(.tint))
        }
    }

    private var trailing: some View {
        HStack(spacing: 10) {
            Text("\(Theme.timeString(model.player.displayProgressMs)) / \(Theme.timeString(model.player.durationMs))")
                .font(.caption2.monospacedDigit())
                .foregroundStyle(.secondary)
            VolumeControl().frame(width: 96)
        }
        .frame(width: 240, alignment: .trailing)
    }
}
