import SwiftUI
import AppKit
import SpotuifyKit

enum MiniSize: String, CaseIterable {
    case full, compact, tiny
    var next: MiniSize {
        switch self {
        case .full: .compact
        case .compact: .tiny
        case .tiny: .full
        }
    }
}

/// Sets the hosting NSWindow to a floating (always-on-top) panel that shows
/// across Spaces, with a transparent titlebar so the content can fill it.
private struct FloatingWindowAccessor: NSViewRepresentable {
    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        DispatchQueue.main.async {
            guard let window = view.window else { return }
            window.level = .floating
            window.collectionBehavior.insert(.canJoinAllSpaces)
            window.collectionBehavior.insert(.fullScreenAuxiliary)
            window.titlebarAppearsTransparent = true
            window.titleVisibility = .hidden
            window.isMovableByWindowBackground = true
            window.standardWindowButton(.zoomButton)?.isHidden = true
            window.standardWindowButton(.miniaturizeButton)?.isHidden = true
        }
        return view
    }
    func updateNSView(_ nsView: NSView, context: Context) {}
}

/// A compact, always-on-top now-playing HUD with three graduated sizes. The
/// cover is the whole face; controls appear over it on hover, so at rest it's
/// just the record and a hairline of progress.
struct MiniPlayerView: View {
    @Environment(AppModel.self) private var model
    @Environment(ArtworkTheme.self) private var theme
    @Environment(\.openWindow) private var openWindow
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @AppStorage("miniSize") private var sizeRaw = MiniSize.full.rawValue
    @State private var hovering = false

    private var size: MiniSize { MiniSize(rawValue: sizeRaw) ?? .full }
    private var item: MediaItem? { model.player.currentItem }
    private var room: Room { theme.room }

    var body: some View {
        content
            .frame(width: width, height: height)
            // Run under the (hidden) title bar: the cover is the whole face.
            .ignoresSafeArea()
            .background { ZStack { room.base; Grain() } }
            .environment(\.room, room)
            .environment(\.colorScheme, room.isLight ? .light : .dark)
            .tint(room.accent)
            .background(FloatingWindowAccessor())
            .onHover { hovering = $0 }
            .animation(reduceMotion ? nil : .easeOut(duration: 0.18), value: hovering)
            .task(id: "\(theme.adaptiveEnabled)#\(item?.imageURL ?? "")") {
                await theme.update(for: item?.imageURL, reduceMotion: reduceMotion)
            }
    }

    private var width: CGFloat { size == .tiny ? 280 : 320 }
    private var height: CGFloat {
        switch size {
        case .full: 392
        case .compact: 96
        case .tiny: 44
        }
    }

    @ViewBuilder
    private var content: some View {
        switch size {
        case .full: fullContent
        case .compact: compactContent
        case .tiny: tinyContent
        }
    }

    /// Cover on top, liner strip beneath; transport floats over the cover on hover.
    private var fullContent: some View {
        VStack(spacing: 0) {
            ZStack {
                AsyncCoverImage(url: item?.imageURL, cornerRadius: 0)
                    .frame(width: 320, height: 320)
                    .clipped()
                if hovering {
                    LinearGradient(colors: [.black.opacity(0.55), .black.opacity(0.15), .black.opacity(0.6)],
                                   startPoint: .top, endPoint: .bottom)
                    VStack {
                        HStack {
                            sizeButton
                            Spacer()
                            openMainButton
                        }
                        Spacer()
                        TransportCluster(
                            scale: .regular, showsModes: false, color: .white,
                            onColor: room.accent, playFill: AnyShapeStyle(.white), playGlyph: .black)
                        Spacer()
                    }
                    .padding(12)
                    .transition(.opacity)
                }
            }
            .frame(width: 320, height: 320)
            .overlay(alignment: .bottom) { progressLine }
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 3) {
                    Text(item?.name ?? "Nothing playing")
                        .font(.displayTitle(16))
                        .foregroundStyle(room.ink)
                        .lineLimit(1)
                    Text(item?.subtitle ?? "")
                        .font(.system(size: 11.5, weight: .medium))
                        .foregroundStyle(room.inkMuted)
                        .lineLimit(1)
                }
                Spacer(minLength: 4)
                LevelMeter(isPlaying: model.player.isPlaying, size: 11)
                    .foregroundStyle(room.accent)
                    .opacity(item == nil ? 0 : 1)
            }
            .padding(.horizontal, 14)
            .frame(height: 72)
        }
    }

    private var compactContent: some View {
        HStack(spacing: 12) {
            AsyncCoverImage(url: item?.imageURL, cornerRadius: 0)
                .frame(width: 96, height: 96)
                .clipped()
            VStack(alignment: .leading, spacing: 4) {
                Text(item?.name ?? "Nothing playing")
                    .font(.displayTitle(14.5)).foregroundStyle(room.ink).lineLimit(1)
                Text(item?.subtitle ?? "")
                    .font(.system(size: 11, weight: .medium)).foregroundStyle(room.inkMuted).lineLimit(1)
                TransportCluster(
                    scale: .compact, showsModes: false, color: room.ink,
                    onColor: room.accent, playFill: AnyShapeStyle(room.ink), playGlyph: room.base)
                    .padding(.leading, -6)
            }
            Spacer(minLength: 0)
            VStack {
                sizeButton
                Spacer()
            }
            .padding(.vertical, 10)
            .opacity(hovering ? 1 : 0.35)
        }
        .padding(.trailing, 10)
        .overlay(alignment: .bottom) { progressLine }
    }

    private var tinyContent: some View {
        HStack(spacing: 10) {
            LevelMeter(isPlaying: model.player.isPlaying, size: 10)
                .foregroundStyle(room.accent)
            Text(item?.name ?? "—")
                .font(.displayTitle(13)).foregroundStyle(room.ink).lineLimit(1)
            Spacer(minLength: 4)
            TransportGlyph(systemName: model.player.isPlaying ? "pause.fill" : "play.fill",
                           label: model.player.isPlaying ? "Pause" : "Play", size: 11, color: room.ink) {
                model.togglePlayPause()
            }
            .disabled(!model.canTogglePlayPause)
            TransportGlyph(systemName: "forward.fill", label: "Next", size: 11, color: room.ink) { model.next() }
                .disabled(!model.canSkipNext)
            sizeButton
        }
        .padding(.leading, 14).padding(.trailing, 6)
        .overlay(alignment: .bottom) { progressLine }
    }

    /// Progress as a hairline along the bottom edge — the only chrome at rest.
    private var progressLine: some View {
        GeometryReader { geo in
            Rectangle().fill(room.accent)
                .frame(width: geo.size.width * model.player.progressFraction, height: 2)
        }
        .frame(height: 2)
        .accessibilityHidden(true)
    }

    private var sizeButton: some View {
        Button { sizeRaw = size.next.rawValue } label: {
            Image(systemName: "arrow.up.left.and.arrow.down.right")
                .font(.system(size: 10, weight: .bold))
                .foregroundStyle(size == .full ? .white : room.inkMuted)
                .frame(width: 24, height: 24)
                .background(Circle().fill((size == .full ? Color.white : room.ink).opacity(0.14)))
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .help("Resize mini player")
        .accessibilityLabel("Resize mini player")
    }

    private var openMainButton: some View {
        Button { openWindow(id: "player") } label: {
            Image(systemName: "macwindow")
                .font(.system(size: 10, weight: .bold))
                .foregroundStyle(.white)
                .frame(width: 24, height: 24)
                .background(Circle().fill(.white.opacity(0.14)))
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .help("Open main window")
        .accessibilityLabel("Open main window")
    }
}
