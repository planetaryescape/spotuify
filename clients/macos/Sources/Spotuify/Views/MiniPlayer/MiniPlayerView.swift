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

/// A compact, always-on-top now-playing HUD with three graduated sizes.
struct MiniPlayerView: View {
    @Environment(AppModel.self) private var model
    @Environment(ArtworkTheme.self) private var theme
    @Environment(\.openWindow) private var openWindow
    @AppStorage("miniSize") private var sizeRaw = MiniSize.full.rawValue

    private var size: MiniSize { MiniSize(rawValue: sizeRaw) ?? .full }
    private var item: MediaItem? { model.player.currentItem }

    var body: some View {
        ZStack {
            LinearGradient(
                colors: [theme.background.opacity(0.95), theme.palette.accent.opacity(0.22), .black.opacity(0.9)],
                startPoint: .top, endPoint: .bottom)
            content
                .padding(size == .tiny ? 8 : 14)
        }
        .frame(width: width, height: height)
        .tint(theme.accent)
        .background(FloatingWindowAccessor())
        .background(.ultraThinMaterial)
        .task(id: item?.imageURL) { await theme.update(for: item?.imageURL) }
    }

    private var width: CGFloat { size == .tiny ? 360 : 320 }
    private var height: CGFloat? {
        switch size {
        case .full: 380
        case .compact: 132
        case .tiny: 64
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

    private var fullContent: some View {
        VStack(spacing: 12) {
            HStack {
                sizeButton
                Spacer()
                Button { openWindow(id: "player") } label: { Image(systemName: "macwindow") }
                    .buttonStyle(.plain).help("Open main window")
            }
            AsyncCoverImage(url: item?.imageURL)
                .frame(width: 200, height: 200)
                .shadow(radius: 10, y: 5)
            VStack(spacing: 3) {
                Text(item?.name ?? "Nothing playing")
                    .font(.displayHero(20))
                    .foregroundStyle(theme.palette.primary)
                    .lineLimit(1).minimumScaleFactor(0.6)
                Text(item?.subtitle ?? "")
                    .font(.caption).foregroundStyle(theme.palette.secondary).lineLimit(1)
            }
            SeekBar(progress: model.player.progressFraction) { model.seek(toFraction: $0) }
            transport(size: 16)
        }
    }

    private var compactContent: some View {
        HStack(spacing: 12) {
            AsyncCoverImage(url: item?.imageURL, cornerRadius: 6)
                .frame(width: 56, height: 56)
            VStack(alignment: .leading, spacing: 2) {
                Text(item?.name ?? "Nothing playing").font(.system(size: 13, weight: .semibold)).lineLimit(1)
                Text(item?.subtitle ?? "").font(.caption2).foregroundStyle(.secondary).lineLimit(1)
                transport(size: 12)
            }
            Spacer(minLength: 0)
            sizeButton
        }
    }

    private var tinyContent: some View {
        HStack(spacing: 10) {
            Text(item?.name ?? "—").font(.system(size: 12, weight: .medium)).lineLimit(1)
            Spacer(minLength: 4)
            Button { model.togglePlayPause() } label: {
                Image(systemName: model.player.isPlaying ? "pause.fill" : "play.fill")
            }.buttonStyle(.plain)
            Button { model.next() } label: { Image(systemName: "forward.fill") }.buttonStyle(.plain)
            sizeButton
        }
    }

    private func transport(size iconSize: CGFloat) -> some View {
        HStack(spacing: 16) {
            Button { model.previous() } label: { Image(systemName: "backward.fill") }.buttonStyle(.plain)
            Button { model.togglePlayPause() } label: {
                Image(systemName: model.player.isPlaying ? "pause.fill" : "play.fill")
                    .font(.system(size: iconSize + 4))
            }.buttonStyle(.plain)
            Button { model.next() } label: { Image(systemName: "forward.fill") }.buttonStyle(.plain)
        }
        .font(.system(size: iconSize))
    }

    private var sizeButton: some View {
        Button { sizeRaw = size.next.rawValue } label: {
            Image(systemName: "arrow.up.left.and.arrow.down.right")
        }
        .buttonStyle(.plain)
        .help("Resize HUD")
    }
}
