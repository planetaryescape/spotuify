import SwiftUI
import SpotuifyKit

enum NowPlayingMode: String, CaseIterable, Identifiable {
    case artwork, visualizer, lyrics, queue
    var id: String { rawValue }
    var icon: String {
        switch self {
        case .artwork: "photo"
        case .visualizer: "waveform"
        case .lyrics: "quote.bubble"
        case .queue: "list.bullet"
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
    @AppStorage("nowPlayingMinimized") private var minimized = false
    @AppStorage("vizStyle") private var vizStyleRaw = VizStyle.bars.rawValue

    private var mode: NowPlayingMode { NowPlayingMode(rawValue: modeRaw) ?? .artwork }
    private var vizStyle: VizStyle { VizStyle(rawValue: vizStyleRaw) ?? .bars }
    private var item: MediaItem? { model.player.currentItem }
    private var palette: ArtworkPalette { theme.palette }

    var body: some View {
        // The full-bleed player is the root of its own navigation stack so the
        // album eyebrow and artist line can push their detail pages (with a
        // back button) without leaving the Now Playing destination.
        NavigationStack {
            playerStage.mediaDetailDestinations()
        }
    }

    /// A deterministic top→bottom column: top controls, a flexible middle,
    /// then the transport pinned as the LAST row. The cover is a *background*
    /// (not a layout participant), so it can't push anything around and the
    /// controls can never be shoved off-screen on resize.
    private var playerStage: some View {
        VStack(spacing: 0) {
            topControls
            middle
            if !minimized {
                controlsOverlay
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(backgroundLayer)
        .clipped()
        .contentShape(Rectangle())
        // Fallback to the pill: click anywhere on the full-art view to restore.
        .onTapGesture { if minimized { minimized = false } }
        .animation(.easeInOut(duration: 0.3), value: minimized)
    }

    /// Flexible middle between the top controls and the bottom transport. Artwork
    /// shows the cover (the background) through empty space; visualizer/lyrics
    /// render their feature here, width-capped so a wider window never changes
    /// their vertical size.
    @ViewBuilder
    private var middle: some View {
        if minimized || mode == .artwork {
            Color.clear.frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            featureContent
                .frame(maxWidth: 600)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .padding(.horizontal, 32)
                .padding(.vertical, 12)
        }
    }

    /// Top bar: the mode switch (centered) + minimise toggle, or the labelled
    /// restore pill when minimised. Fixed height — the first row of the column.
    private var topControls: some View {
        Group {
            if minimized {
                Button { minimized = false } label: {
                    Label("Show controls", systemImage: "chevron.up")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.white)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 8)
                        .background(.ultraThinMaterial, in: Capsule())
                        .shadow(color: .black.opacity(0.3), radius: 6, y: 2)
                }
                .buttonStyle(.plain)
                .help("Show the player controls")
            } else {
                ZStack(alignment: .top) {
                    modePill
                    HStack {
                        // Visualizer-style switch lives up here (viz mode only) so
                        // the visualizer itself owns the full middle of the stage.
                        if mode == .visualizer { vizStylePill }
                        Spacer()
                        Button { minimized = true } label: {
                            Image(systemName: "chevron.down")
                                .font(.system(size: 12, weight: .bold))
                                .foregroundStyle(.white)
                                .padding(8)
                                .background(.ultraThinMaterial, in: Circle())
                                .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .help("Hide controls for full art")
                    }
                }
                .padding(.horizontal, 16)
            }
        }
        .padding(.top, 18)
        .padding(.bottom, 4)
    }

    /// Visualizer-style switch — same glass-pill language as the mode pill,
    /// shown below the visualizer (not stacked with the top toggles).
    private var vizStylePill: some View {
        GlassEffectContainer(spacing: 4) {
            HStack(spacing: 4) {
                ForEach(VizStyle.allCases) { vizStyleButton($0) }
            }
            .padding(5)
            .glassEffect(.regular.tint(palette.accent.opacity(0.18)).interactive(), in: .capsule)
        }
    }

    private func vizStyleButton(_ target: VizStyle) -> some View {
        let active = vizStyle == target
        return Button {
            withAnimation(.easeInOut(duration: 0.25)) { vizStyleRaw = target.rawValue }
        } label: {
            Image(systemName: target.icon)
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(active ? AnyShapeStyle(palette.background) : AnyShapeStyle(.white))
                .frame(width: 30, height: 30)
                .background(active ? AnyShapeStyle(.white) : AnyShapeStyle(Color.clear), in: Circle())
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    // MARK: Layers

    /// Full-bleed art: the sharp cover fills the whole stage in artwork mode; a
    /// blurred ambient wash backs the visualizer/lyrics stages so they still sit
    /// on the album's colour.
    @ViewBuilder
    private var backgroundLayer: some View {
        // Minimised always shows the sharp cover (the whole point is to see the
        // art) regardless of the active mode.
        if mode == .artwork || minimized {
            ZStack {
                palette.background
                AsyncCoverImage(url: item?.imageURL, cornerRadius: 0)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .clipped()
                    .id(item?.uri)
                    .animation(.easeInOut(duration: 0.5), value: item?.uri)
                // Slight top darkening so the mode picker reads on bright covers.
                LinearGradient(
                    colors: [.black.opacity(0.4), .clear],
                    startPoint: .top, endPoint: .center)
            }
        } else {
            ZStack {
                palette.background
                if !reduceTransparency {
                    AsyncCoverImage(url: item?.imageURL, cornerRadius: 0)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .clipped()
                        .blur(radius: 80).opacity(0.55).saturation(1.4)
                }
                // Darken so the visualizer / lyrics read clearly over the art.
                Color.black.opacity(0.4)
            }
        }
    }

    /// The non-artwork feature: the spectrum over the blurred+darkened cover, or
    /// the lyrics. Sits in the flexible region above the controls (see `content`).
    @ViewBuilder
    private var featureContent: some View {
        switch mode {
        case .artwork:
            EmptyView()
        case .visualizer:
            // Full middle; its style switch lives in the top bar (see topControls).
            VisualizerView(style: vizStyle, tint: palette.accent)
        case .lyrics:
            LyricsView()
        case .queue:
            NowPlayingQueue(accent: palette.accent)
        }
    }

    /// Transport + metadata floated over a palette-tinted scrim pinned to the
    /// bottom — the cover stays visible above; the controls stay legible below.
    private var controlsOverlay: some View {
        VStack(spacing: 14) {
            trackInfo
            seekSection
            transportBar
        }
        .padding(.horizontal, 24)
        // Cap to a tidy centered column so the controls don't stretch across a
        // maximized window; the scrim still spans full width behind them.
        .frame(maxWidth: 680)
        .frame(maxWidth: .infinity)
        .padding(.top, 64)
        .padding(.bottom, 40)
        // The scrim must be genuinely dark *where the text sits* — not just at
        // the very bottom — so the white title/artist/eyebrow stay legible over
        // ANY cover (including a white one). It ramps to an album-tinted dark by
        // ~22% down (where the eyebrow begins); the top half of the art above
        // stays bright. `palette.background` is always dark (≤0.30 brightness),
        // so it darkens while keeping the album's hue.
        .background(
            LinearGradient(
                stops: [
                    .init(color: .clear, location: 0),
                    .init(color: palette.background.opacity(0.82), location: 0.22),
                    .init(color: palette.background.opacity(0.96), location: 0.5),
                    .init(color: .black.opacity(0.96), location: 1),
                ],
                startPoint: .top, endPoint: .bottom)
            .allowsHitTesting(false))
    }

    /// One unified glass-pill mode switch for all four modes (artwork /
    /// visualizer / lyrics / queue) — a single consistent style rather than a
    /// segmented control up top and a separate pill below.
    private var modePill: some View {
        GlassEffectContainer(spacing: 4) {
            HStack(spacing: 4) {
                modeButton(.artwork, help: "Artwork")
                modeButton(.visualizer, help: "Visualizer")
                modeButton(.lyrics, help: "Lyrics")
                modeButton(.queue, help: "Up next")
            }
            .padding(5)
            .glassEffect(.regular.tint(palette.accent.opacity(0.18)).interactive(), in: .capsule)
        }
    }

    private func modeButton(_ target: NowPlayingMode, help: String) -> some View {
        let active = mode == target
        return Button {
            withAnimation(.easeInOut(duration: 0.25)) { modeRaw = target.rawValue }
        } label: {
            Image(systemName: target.icon)
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(active ? AnyShapeStyle(palette.background) : AnyShapeStyle(.white))
                .frame(width: 34, height: 34)
                .background(active ? AnyShapeStyle(.white) : AnyShapeStyle(Color.clear), in: Circle())
                // The whole 34x34 cell is the hit target — without this an
                // inactive button is only tappable on the glyph itself (a clear
                // background doesn't hit-test).
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(help)
    }

    private var trackInfo: some View {
        // Over the dark scrim, force light text (palette text roles adapt to the
        // *background* luminance, which is wrong against the scrim); the album
        // colour still comes through the accent eyebrow + controls.
        VStack(spacing: 8) {
            eyebrowLabel
            Text(item?.name ?? "Nothing playing")
                .font(.displayHero(42))
                .foregroundStyle(.white)
                .multilineTextAlignment(.center)
                .lineLimit(2)
                .minimumScaleFactor(0.5)
            artistLabel
            if let item {
                NowPlayingLikeButton(item: item, accent: palette.accent) { model.likeCurrent() }
                    .padding(.top, 2)
            }
        }
        .frame(maxWidth: 560)
        .shadow(color: .black.opacity(0.35), radius: 8, y: 2)
    }

    /// Album eyebrow — links to the album detail when the track carries an
    /// album URI, else plain text. Near-white (not the palette accent): the
    /// accent is *derived from the cover*, so over the cover it has too little
    /// luminance contrast to read. The accent still anchors the seek bar,
    /// transport, and chrome.
    @ViewBuilder
    private var eyebrowLabel: some View {
        if let album = item?.albumNavItem {
            NavigationLink(value: album) {
                NowPlayingLink(text: eyebrow, font: .displayAccent(15), color: .white.opacity(0.92))
            }
            .buttonStyle(.plain)
        } else {
            Text(eyebrow)
                .font(.displayAccent(15))
                .foregroundStyle(.white.opacity(0.92))
                .lineLimit(1)
        }
    }

    /// Artist line — one link per artist when the track carries artist refs,
    /// else the plain subtitle.
    @ViewBuilder
    private var artistLabel: some View {
        let artists = item?.artistNavItems ?? []
        if !artists.isEmpty {
            HStack(spacing: 4) {
                ForEach(Array(artists.enumerated()), id: \.element.id) { index, artist in
                    if index > 0 {
                        Text(",").font(.title3).foregroundStyle(.white.opacity(0.8))
                    }
                    NavigationLink(value: artist) {
                        NowPlayingLink(text: artist.name, font: .title3, color: .white.opacity(0.8))
                    }
                    .buttonStyle(.plain)
                }
            }
        } else {
            Text(item?.subtitle ?? "")
                .font(.title3)
                .foregroundStyle(.white.opacity(0.8))
                .lineLimit(1)
        }
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
            .font(.caption.monospacedDigit()).foregroundStyle(.white.opacity(0.7)).frame(maxWidth: 460)
        }
    }

    private var transportRow: some View {
        GlassEffectContainer(spacing: 12) {
            HStack(spacing: 22) {
                TransportButton(systemName: "shuffle", size: 14) { model.toggleShuffle() }
                    .foregroundStyle(model.player.shuffle ? AnyShapeStyle(.tint) : AnyShapeStyle(.white.opacity(0.6)))
                TransportButton(systemName: "backward.fill", size: 18) { model.previous() }
                TransportButton(
                    systemName: model.player.isPlaying ? "pause.fill" : "play.fill",
                    size: 20, prominent: true) { model.togglePlayPause() }
                TransportButton(systemName: "forward.fill", size: 18) { model.next() }
                TransportButton(systemName: model.player.repeatMode == .track ? "repeat.1" : "repeat", size: 14) { model.cycleRepeat() }
                    .foregroundStyle(model.player.repeatMode == .off ? AnyShapeStyle(.white.opacity(0.6)) : AnyShapeStyle(.tint))
            }
            .padding(.horizontal, 26)
            .padding(.vertical, 12)
            .glassEffect(.regular.tint(palette.accent.opacity(0.22)).interactive(), in: .capsule)
        }
    }

    /// One row: device picker (left), the glass transport pill (center), volume
    /// (right). Folding device + volume onto the transport line frees vertical
    /// space for the feature content above (visualizer / lyrics / queue).
    private var transportBar: some View {
        HStack(spacing: 16) {
            DeviceMenu()
                .frame(maxWidth: .infinity, alignment: .leading)
            transportRow
                .fixedSize()
            VolumeControl()
                .frame(width: 130)
                .frame(maxWidth: .infinity, alignment: .trailing)
        }
        .frame(maxWidth: 640)
    }
}

/// The up-next queue rendered for the immersive player stage: a compact,
/// dark-on-art list (the standard chrome `MediaRow` is built for the light
/// surfaces, not the cover backdrop). Tap an upcoming row to play that track.
/// Reused by the global side rail (`GlobalSidePanel`).
struct NowPlayingQueue: View {
    @Environment(AppModel.self) private var model
    let accent: Color

    private var current: MediaItem? { model.player.currentItem }
    private var upcoming: [MediaItem] { model.player.queue?.items ?? [] }

    var body: some View {
        if current == nil && upcoming.isEmpty {
            VStack(spacing: 10) {
                Image(systemName: "list.bullet")
                    .font(.system(size: 34)).foregroundStyle(.white.opacity(0.5))
                Text("Queue is empty")
                    .font(.title3).foregroundStyle(.white.opacity(0.7))
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            ScrollView(showsIndicators: false) {
                LazyVStack(alignment: .leading, spacing: 4) {
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
        }
    }

    private func header(_ text: String) -> some View {
        Text(text.uppercased())
            .font(.caption.weight(.semibold))
            .foregroundStyle(accent)
            .padding(.horizontal, 12)
            .padding(.top, 14).padding(.bottom, 2)
    }

    private func row(_ item: MediaItem, isCurrent: Bool) -> some View {
        Button {
            if !isCurrent { model.play(uri: item.uri) }
        } label: {
            HStack(spacing: 12) {
                AsyncCoverImage(url: item.imageURL, cornerRadius: 5)
                    .frame(width: 40, height: 40)
                VStack(alignment: .leading, spacing: 2) {
                    Text(item.name)
                        .font(.system(size: 14, weight: isCurrent ? .semibold : .regular))
                        .foregroundStyle(.white).lineLimit(1)
                    if !item.subtitle.isEmpty {
                        Text(item.subtitle)
                            .font(.caption).foregroundStyle(.white.opacity(0.7)).lineLimit(1)
                    }
                }
                Spacer(minLength: 8)
                if isCurrent {
                    Image(systemName: "speaker.wave.2.fill")
                        .font(.caption).foregroundStyle(accent)
                } else {
                    Text(Theme.timeString(item.durationMs))
                        .font(.caption.monospacedDigit()).foregroundStyle(.white.opacity(0.5))
                }
            }
            .padding(.horizontal, 12).padding(.vertical, 6)
            .background(
                RoundedRectangle(cornerRadius: 8)
                    .fill(isCurrent ? AnyShapeStyle(.white.opacity(0.12)) : AnyShapeStyle(.clear)))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(isCurrent)
    }
}

/// Heart toggle for the now-playing track. Fills + accent-tints the instant it's
/// tapped (optimistic local state) and bounces, so the user feels the action land
/// without waiting for the daemon round-trip. The optimistic override is dropped
/// once the authoritative `inLibrary` catches up or the track changes.
private struct NowPlayingLikeButton: View {
    let item: MediaItem
    let accent: Color
    let action: () -> Void
    @State private var bounce = 0
    @State private var optimistic: Bool?

    private var liked: Bool { optimistic ?? (item.inLibrary == true) }

    var body: some View {
        Button {
            optimistic = !liked
            bounce += 1
            action()
        } label: {
            Image(systemName: liked ? "heart.fill" : "heart")
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(liked ? AnyShapeStyle(accent) : AnyShapeStyle(.white.opacity(0.85)))
                .frame(width: 38, height: 38)
                .background(.white.opacity(0.12), in: Circle())
                .contentShape(Circle())
                .contentTransition(.symbolEffect(.replace))
                .symbolEffect(.bounce, value: bounce)
        }
        .buttonStyle(.plain)
        .help(liked ? "Remove from Liked Songs" : "Add to Liked Songs")
        .onChange(of: item.inLibrary) { optimistic = nil }
        .onChange(of: item.uri) { optimistic = nil }
    }
}

/// A tappable album/artist label floated over the player's dark scrim. Underlines
/// on hover so it reads as clickable against the full-bleed cover.
private struct NowPlayingLink: View {
    let text: String
    let font: Font
    let color: Color
    @State private var hovering = false

    var body: some View {
        Text(text)
            .font(font)
            .underline(hovering, color: color)
            .foregroundStyle(color)
            .lineLimit(1)
            .contentShape(Rectangle())
            .onHover { hovering = $0 }
            .pointerStyle(.link)
    }
}
