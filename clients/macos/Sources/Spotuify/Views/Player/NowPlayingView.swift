import SwiftUI
import SpotuifyKit

/// Which companion sits beside the record on the Now Playing stage. `artwork`
/// means none: the record takes the room alone.
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
    var title: String {
        switch self {
        case .artwork: "Artwork"
        case .visualizer: "Visualizer"
        case .lyrics: "Lyrics"
        case .queue: "Up Next"
        }
    }
}

/// The listening room. The whole cover sits on a blurred wash of itself. Wide
/// windows put the record on the left and a companion (lyrics, queue,
/// visualizer) on the right; in artwork mode the record's liner notes take the
/// right instead. Narrow windows stack. "Cinema" hides everything but the art.
struct NowPlayingView: View {
    @Environment(AppModel.self) private var model
    @Environment(ArtworkTheme.self) private var theme
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @AppStorage("nowPlayingMode") private var modeRaw = NowPlayingMode.artwork.rawValue
    @AppStorage("nowPlayingMinimized") private var minimized = false
    @AppStorage("vizStyle") private var vizStyleRaw = VizStyle.bars.rawValue

    private var mode: NowPlayingMode { NowPlayingMode(rawValue: modeRaw) ?? .artwork }
    private var vizStyle: VizStyle { VizStyle(rawValue: vizStyleRaw) ?? .bars }
    private var item: MediaItem? { model.player.currentItem }
    private var isPlaying: Bool { model.player.isPlaying }
    private var text: Color { theme.immersiveText }

    var body: some View {
        // The stage is the root of its own navigation stack so the album and
        // artist links can push detail pages without leaving Now Playing.
        NavigationStack {
            stage
                .mediaDetailDestinations()
                .toolbar(.hidden, for: .windowToolbar)
        }
    }

    private var stage: some View {
        GeometryReader { geo in
            let size = geo.size
            Group {
                if minimized {
                    cinema(size)
                } else if size.width >= 760 {
                    wide(size)
                } else {
                    narrow(size)
                }
            }
            .frame(width: size.width, height: size.height)
        }
        .safeAreaInset(edge: .top, spacing: 0) { topBar }
        .background {
            NowPlayingBackdrop(imageURL: item?.imageURL, palette: theme.palette, isLight: theme.immersiveIsLight)
                .ignoresSafeArea()
        }
        .animation(reduceMotion ? nil : .spring(response: 0.5, dampingFraction: 0.86), value: mode)
        .animation(reduceMotion ? nil : .easeInOut(duration: 0.4), value: minimized)
        .environment(\.colorScheme, theme.immersiveIsLight ? .light : .dark)
    }

    // MARK: Layouts

    /// Two columns. Artwork mode: cover left, liner notes and transport right.
    /// Companion modes: cover, notes and transport stack on the left; the
    /// companion fills the right.
    @ViewBuilder
    private func wide(_ size: CGSize) -> some View {
        let hPad: CGFloat = 56
        if mode == .artwork {
            let notesWidth = min(420, max(320, size.width * 0.34))
            let art = max(200, min(620, size.height - 120, size.width - notesWidth - hPad * 2 - 64))
            HStack(alignment: .center, spacing: 64) {
                RecordArtwork(item: item, size: art, isPlaying: isPlaying)
                VStack(alignment: .leading, spacing: 28) {
                    RecordInfo(alignment: .leading, titleSize: 46)
                    VStack(alignment: .leading, spacing: 18) {
                        SeekRow(barHeight: 5, fill: AnyShapeStyle(text), textColor: text.opacity(0.7), layout: .stacked)
                        stageTransport(.large)
                            .frame(maxWidth: .infinity)
                        utilityRow
                    }
                }
                .frame(width: notesWidth)
            }
            .padding(.horizontal, hPad)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            let column = min(400, max(300, size.width * 0.36))
            let art = max(160, min(column, size.height - 360))
            HStack(alignment: .center, spacing: 48) {
                VStack(spacing: 22) {
                    RecordArtwork(item: item, size: art, isPlaying: isPlaying)
                    RecordInfo(alignment: .center, titleSize: 28, showsLike: false)
                    SeekRow(barHeight: 4, fill: AnyShapeStyle(text), textColor: text.opacity(0.7), layout: .stacked)
                    stageTransport(.regular)
                    utilityRow
                }
                .frame(width: column)
                companion
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .transition(.opacity.combined(with: .offset(x: 24)))
            }
            .padding(.horizontal, hPad)
            .padding(.vertical, 28)
        }
    }

    /// One column. Artwork mode shows the cover over the notes; companion modes
    /// shrink the record to a header row so the companion gets the height.
    @ViewBuilder
    private func narrow(_ size: CGSize) -> some View {
        VStack(spacing: 20) {
            if mode == .artwork {
                Spacer(minLength: 8)
                RecordArtwork(item: item, size: max(160, min(size.width - 80, size.height - 330)), isPlaying: isPlaying)
                RecordInfo(alignment: .center, titleSize: 32)
                Spacer(minLength: 8)
            } else {
                HStack(spacing: 16) {
                    RecordArtwork(item: item, size: 72, isPlaying: isPlaying)
                    RecordInfo(alignment: .leading, titleSize: 22, showsLike: false)
                }
                companion
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            VStack(spacing: 14) {
                SeekRow(barHeight: 4, fill: AnyShapeStyle(text), textColor: text.opacity(0.7), layout: .stacked)
                stageTransport(.regular)
                utilityRow
            }
            .frame(maxWidth: 440)
        }
        .padding(.horizontal, 32)
        .padding(.vertical, 24)
    }

    /// Cover only, as large as the room allows. Click anywhere to bring the
    /// controls back; the dock carries the transport meanwhile.
    private func cinema(_ size: CGSize) -> some View {
        RecordArtwork(item: item, size: max(160, min(size.width, size.height) - 72), isPlaying: isPlaying)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .contentShape(Rectangle())
            .onTapGesture { minimized = false }
            .accessibilityAddTraits(.isButton)
            .accessibilityHint("Shows the player controls")
    }

    // MARK: Pieces

    private func stageTransport(_ scale: TransportCluster.Scale) -> some View {
        TransportCluster(
            scale: scale,
            color: text,
            onColor: theme.palette.accent,
            playFill: AnyShapeStyle(text),
            playGlyph: theme.immersivePillGlyph)
    }

    /// Device on the left, volume on the right: the "where" and "how loud".
    private var utilityRow: some View {
        HStack(spacing: 16) {
            DeviceMenu()
                .foregroundStyle(text.opacity(0.8))
            Spacer(minLength: 8)
            VolumeControl(fill: AnyShapeStyle(text), iconColor: text.opacity(0.7))
                .frame(width: 130)
                .disabled(!model.canSetVolume)
        }
    }

    @ViewBuilder
    private var companion: some View {
        switch mode {
        case .artwork:
            EmptyView()
        case .visualizer:
            VisualizerView(style: vizStyle, tint: theme.palette.accent)
                .padding(.vertical, 48)
                .overlay(alignment: .bottom) {
                    RoomTabs(
                        options: VizStyle.allCases.map { (value: $0.rawValue, title: $0.rawValue) },
                        selection: $vizStyleRaw,
                        color: text)
                }
        case .lyrics:
            LyricsView(textColor: text)
        case .queue:
            NowPlayingQueue(accent: theme.palette.accent, textColor: text)
        }
    }

    /// Companion tabs, centred; the cinema toggle on the right.
    private var topBar: some View {
        ZStack {
            if !minimized {
                RoomTabs(
                    options: NowPlayingMode.allCases.map { (value: $0.rawValue, title: $0.title) },
                    selection: $modeRaw,
                    color: text)
                    .transition(.opacity)
            }
            HStack {
                Spacer()
                Button {
                    minimized.toggle()
                } label: {
                    Image(systemName: minimized ? "arrow.down.right.and.arrow.up.left" : "arrow.up.left.and.arrow.down.right")
                        .font(.system(size: 11, weight: .bold))
                        .foregroundStyle(text.opacity(0.75))
                        .frame(width: 30, height: 30)
                        .background(Circle().fill(text.opacity(0.08)))
                        .contentShape(Circle())
                }
                .buttonStyle(PressableButtonStyle())
                .help(minimized ? "Show the player controls" : "Show only the artwork")
                .accessibilityLabel(minimized ? "Show controls" : "Cinema")
            }
        }
        .padding(.horizontal, 20)
        .padding(.top, 16)
        .frame(height: 52)
    }
}
