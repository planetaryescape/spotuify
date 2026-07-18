import SwiftUI
import SpotuifyKit

/// The always-visible bottom transport bar (Spotify-style), shown under every
/// destination so playback control is one click away from anywhere.
struct NowPlayingBar: View {
    @Environment(AppModel.self) private var model
    @Environment(ArtworkTheme.self) private var theme
    @AppStorage("globalSidePanel") private var globalPanelRaw = GlobalPanel.none.rawValue

    private var item: MediaItem? { model.player.currentItem }
    private var globalPanel: GlobalPanel { GlobalPanel(rawValue: globalPanelRaw) ?? .none }

    private func togglePanel(_ target: GlobalPanel) {
        globalPanelRaw = (globalPanel == target ? GlobalPanel.none : target).rawValue
    }

    var body: some View {
        VStack(spacing: 0) {
            // A thin seek line spanning the whole bar.
            SeekBar(
                progress: model.player.progressFraction,
                durationMs: model.player.durationMs,
                onSeek: { model.seek(toFraction: $0) },
                height: 4)
                .disabled(!model.canSeek)
                .padding(.horizontal, 14)
                .padding(.top, 6)

            HStack(spacing: 12) {
                trackCell
                    .layoutPriority(1)
                Spacer(minLength: 8)
                controls
                    .layoutPriority(3) // transport never compresses
                Spacer(minLength: 8)
                trailing
                    .layoutPriority(2)
            }
            .padding(.horizontal, 16)
            .padding(.top, 8)
            .padding(.bottom, 20)
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
        .frame(maxWidth: 280, alignment: .leading)
    }

    private var controls: some View {
        HStack(spacing: 14) {
            TransportButton(systemName: "shuffle", size: 12) { model.toggleShuffle() }
                .foregroundStyle(model.player.shuffle ? AnyShapeStyle(.tint) : AnyShapeStyle(.secondary))
                .disabled(!model.canSetShuffle)
            TransportButton(systemName: "backward.fill", size: 14) { model.previous() }
                .disabled(!model.canSkipPrevious)
            TransportButton(
                systemName: model.player.isPlaying ? "pause.fill" : "play.fill",
                size: 16, prominent: true) { model.togglePlayPause() }
                .disabled(!model.canTogglePlayPause)
            TransportButton(systemName: "forward.fill", size: 14) { model.next() }
                .disabled(!model.canSkipNext)
            TransportButton(
                systemName: model.player.repeatMode == .track ? "repeat.1" : "repeat",
                size: 12) { model.cycleRepeat() }
                .foregroundStyle(model.player.repeatMode == .off ? AnyShapeStyle(.secondary) : AnyShapeStyle(.tint))
                .disabled(!model.canSetRepeat)
        }
    }

    private var trailing: some View {
        HStack(spacing: 10) {
            TransportButton(systemName: "quote.bubble", size: 13) { togglePanel(.lyrics) }
                .foregroundStyle(globalPanel == .lyrics ? AnyShapeStyle(.tint) : AnyShapeStyle(.secondary))
                .help("Lyrics")
            TransportButton(systemName: "list.bullet", size: 13) { togglePanel(.queue) }
                .foregroundStyle(globalPanel == .queue ? AnyShapeStyle(.tint) : AnyShapeStyle(.secondary))
                .help("Up next")
                .disabled(!model.canReadQueue)
            Text("\(Theme.timeString(model.player.displayProgressMs)) / \(Theme.timeString(model.player.durationMs))")
                .font(.caption2.monospacedDigit())
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .fixedSize()
            DeviceMenu(showsActiveName: false)
            VolumeControl().frame(width: 96).disabled(!model.canSetVolume)
        }
        .frame(maxWidth: 340, alignment: .trailing)
    }
}
