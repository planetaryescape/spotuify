import SwiftUI
import SpotuifyKit

enum NowPlayingMode: String, CaseIterable, Identifiable {
    case artwork, visualizer, lyrics
    var id: String { rawValue }
    var icon: String {
        switch self {
        case .artwork: "photo"
        case .visualizer: "waveform"
        case .lyrics: "quote.bubble"
        }
    }
}

/// Immersive Now Playing: an artwork-tinted backdrop, a mode switch
/// (Artwork / Visualizer / Lyrics) for the main area, and full transport.
struct NowPlayingView: View {
    @Environment(AppModel.self) private var model
    @Environment(ArtworkTheme.self) private var theme
    @AppStorage("nowPlayingMode") private var modeRaw = NowPlayingMode.artwork.rawValue

    private var mode: NowPlayingMode { NowPlayingMode(rawValue: modeRaw) ?? .artwork }
    private var item: MediaItem? { model.player.currentItem }

    var body: some View {
        ZStack {
            backdrop
            VStack(spacing: 18) {
                modePicker
                mainArea
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                trackInfo
                seekSection
                transportRow
                bottomRow
            }
            .padding(.horizontal, 40)
            .padding(.vertical, 20)
        }
    }

    // MARK: Backdrop (dynamic, artwork-tinted)

    private var backdrop: some View {
        ZStack {
            AsyncCoverImage(url: item?.imageURL, cornerRadius: 0)
                .blur(radius: 90).opacity(0.45).saturation(1.5)
            LinearGradient(
                colors: [theme.background.opacity(0.85), theme.background.opacity(0.55), .black.opacity(0.5)],
                startPoint: .top, endPoint: .bottom)
            Rectangle().fill(.ultraThinMaterial).opacity(0.25)
        }
        .ignoresSafeArea()
    }

    private var modePicker: some View {
        Picker("Mode", selection: Binding(get: { mode }, set: { modeRaw = $0.rawValue })) {
            ForEach(NowPlayingMode.allCases) { Image(systemName: $0.icon).tag($0) }
        }
        .pickerStyle(.segmented)
        .frame(width: 200)
        .labelsHidden()
    }

    @ViewBuilder
    private var mainArea: some View {
        switch mode {
        case .artwork:
            AsyncCoverImage(url: item?.imageURL)
                .frame(width: 320, height: 320)
                .shadow(color: .black.opacity(0.4), radius: 24, y: 12)
        case .visualizer:
            VStack(spacing: 18) {
                AsyncCoverImage(url: item?.imageURL)
                    .frame(width: 120, height: 120)
                    .shadow(radius: 10, y: 5)
                VisualizerView().frame(maxWidth: 460, maxHeight: 180)
            }
        case .lyrics:
            HStack(alignment: .top, spacing: 20) {
                AsyncCoverImage(url: item?.imageURL)
                    .frame(width: 96, height: 96)
                LyricsView().frame(maxWidth: 520)
            }
        }
    }

    private var trackInfo: some View {
        VStack(spacing: 6) {
            Text(item?.name ?? "Nothing playing")
                .font(.system(size: 22, weight: .bold)).multilineTextAlignment(.center).lineLimit(2)
            Text(item?.subtitle ?? "")
                .font(.title3).foregroundStyle(.secondary).lineLimit(1)
        }
        .frame(maxWidth: 460)
    }

    private var seekSection: some View {
        VStack(spacing: 6) {
            SeekBar(progress: model.player.progressFraction) { model.seek(toFraction: $0) }
                .frame(maxWidth: 460)
            HStack {
                Text(Theme.timeString(model.player.displayProgressMs))
                Spacer()
                Text(Theme.timeString(model.player.durationMs))
            }
            .font(.caption.monospacedDigit()).foregroundStyle(.secondary).frame(maxWidth: 460)
        }
    }

    private var transportRow: some View {
        HStack(spacing: 20) {
            TransportButton(systemName: "shuffle", size: 14) { model.toggleShuffle() }
                .foregroundStyle(model.player.shuffle ? AnyShapeStyle(.tint) : AnyShapeStyle(.secondary))
            TransportButton(systemName: "backward.fill", size: 18) { model.previous() }
            TransportButton(
                systemName: model.player.isPlaying ? "pause.fill" : "play.fill",
                size: 20, prominent: true) { model.togglePlayPause() }
            TransportButton(systemName: "forward.fill", size: 18) { model.next() }
            TransportButton(systemName: model.player.repeatMode == .track ? "repeat.1" : "repeat", size: 14) { model.cycleRepeat() }
                .foregroundStyle(model.player.repeatMode == .off ? AnyShapeStyle(.secondary) : AnyShapeStyle(.tint))
        }
    }

    private var bottomRow: some View {
        HStack(spacing: 16) {
            DeviceMenu()
            Spacer()
            VolumeControl().frame(width: 140)
        }
        .frame(maxWidth: 460)
    }
}
