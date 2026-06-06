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

/// Immersive, editorial Now Playing: a palette-flood backdrop derived from the
/// cover, a hero artwork with a color-matched glow, a Fraunces display title,
/// and a Liquid Glass transport. The mode switch swaps the main stage between
/// Artwork / Visualizer / Lyrics.
struct NowPlayingView: View {
    @Environment(AppModel.self) private var model
    @Environment(ArtworkTheme.self) private var theme
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    @AppStorage("nowPlayingMode") private var modeRaw = NowPlayingMode.artwork.rawValue

    private var mode: NowPlayingMode { NowPlayingMode(rawValue: modeRaw) ?? .artwork }
    private var item: MediaItem? { model.player.currentItem }
    private var palette: ArtworkPalette { theme.palette }

    var body: some View {
        GeometryReader { geo in
            let heroSize = min(geo.size.height * 0.46, geo.size.width * 0.42, 420)
            ZStack {
                backdrop
                VStack(spacing: 20) {
                    modePicker
                    mainArea(heroSize: heroSize)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                    trackInfo
                    seekSection
                    transportRow
                    bottomRow
                }
                .padding(.horizontal, 44)
                .padding(.vertical, 22)
            }
        }
    }

    // MARK: Backdrop (palette flood + soft artwork ambience)

    private var backdrop: some View {
        ZStack {
            // Bold color field from the cover — calm and large, the magazine flood.
            LinearGradient(
                colors: [
                    palette.background,
                    palette.background.opacity(0.82),
                    palette.accent.opacity(0.18),
                ],
                startPoint: .top, endPoint: .bottom)
            // A whisper of the actual artwork for texture, not detail.
            if !reduceTransparency {
                AsyncCoverImage(url: item?.imageURL, cornerRadius: 0)
                    .blur(radius: 120).opacity(0.30).saturation(1.4)
            }
            // Vignette to seat the controls.
            RadialGradient(
                colors: [.clear, .black.opacity(0.35)],
                center: .center, startRadius: 120, endRadius: 620)
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
    private func mainArea(heroSize: CGFloat) -> some View {
        switch mode {
        case .artwork:
            AsyncCoverImage(url: item?.imageURL)
                .frame(width: heroSize, height: heroSize)
                .shadow(color: palette.accent.opacity(0.45), radius: 40, y: 18)
                .shadow(color: .black.opacity(0.45), radius: 24, y: 14)
                .id(item?.uri)
                .transition(.scale(scale: 0.92).combined(with: .opacity))
                .animation(.spring(response: 0.5, dampingFraction: 0.82), value: item?.uri)
        case .visualizer:
            VStack(spacing: 18) {
                AsyncCoverImage(url: item?.imageURL)
                    .frame(width: 120, height: 120)
                    .shadow(color: palette.accent.opacity(0.4), radius: 18, y: 8)
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
        VStack(spacing: 8) {
            Text(eyebrow)
                .font(.displayAccent(15))
                .foregroundStyle(palette.accent)
                .lineLimit(1)
            Text(item?.name ?? "Nothing playing")
                .font(.displayHero(44))
                .foregroundStyle(palette.primary)
                .multilineTextAlignment(.center)
                .lineLimit(2)
                .minimumScaleFactor(0.5)
            Text(item?.subtitle ?? "")
                .font(.title3)
                .foregroundStyle(palette.secondary)
                .lineLimit(1)
        }
        .frame(maxWidth: 520)
    }

    private var eyebrow: String {
        if let album = item?.albumLabel, !album.isEmpty { return album }
        return item == nil ? "Spotuify" : "Now Playing"
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
            .font(.caption.monospacedDigit()).foregroundStyle(palette.secondary).frame(maxWidth: 460)
        }
    }

    private var transportRow: some View {
        GlassEffectContainer(spacing: 12) {
            HStack(spacing: 22) {
                TransportButton(systemName: "shuffle", size: 14) { model.toggleShuffle() }
                    .foregroundStyle(model.player.shuffle ? AnyShapeStyle(.tint) : AnyShapeStyle(palette.secondary))
                TransportButton(systemName: "backward.fill", size: 18) { model.previous() }
                TransportButton(
                    systemName: model.player.isPlaying ? "pause.fill" : "play.fill",
                    size: 20, prominent: true) { model.togglePlayPause() }
                TransportButton(systemName: "forward.fill", size: 18) { model.next() }
                TransportButton(systemName: model.player.repeatMode == .track ? "repeat.1" : "repeat", size: 14) { model.cycleRepeat() }
                    .foregroundStyle(model.player.repeatMode == .off ? AnyShapeStyle(palette.secondary) : AnyShapeStyle(.tint))
            }
            .padding(.horizontal, 26)
            .padding(.vertical, 12)
            .glassEffect(.regular.tint(palette.accent.opacity(0.22)).interactive(), in: .capsule)
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
