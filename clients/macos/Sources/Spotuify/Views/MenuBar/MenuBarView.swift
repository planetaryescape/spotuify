import SwiftUI
import SpotuifyKit

/// The menu bar companion: the record on a blurred wash of itself, the status
/// eyebrow, a Fraunces title, the transport, and a mono footer. Shares the
/// same AppModel + ArtworkTheme as the main window so they stay in sync.
struct MenuBarView: View {
    @Environment(AppModel.self) private var model
    @Environment(ArtworkTheme.self) private var theme
    @Environment(\.openWindow) private var openWindow
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var item: MediaItem? { model.player.currentItem }
    private var room: Room { theme.room }
    private var text: Color { theme.immersiveText }

    var body: some View {
        VStack(spacing: 0) {
            stage
            footer
        }
        .frame(width: 320)
        .environment(\.room, room)
        .environment(\.colorScheme, room.isLight ? .light : .dark)
        .tint(room.accent)
        .task(id: "\(theme.adaptiveEnabled)#\(item?.imageURL ?? "")") {
            await theme.update(for: item?.imageURL, reduceMotion: reduceMotion)
        }
    }

    private var stage: some View {
        VStack(spacing: 14) {
            RecordArtwork(item: item, size: 168, isPlaying: model.player.isPlaying)
                .padding(.top, 8)
            RecordInfo(alignment: .center, titleSize: 22, showsLike: false, linksCredits: false)
            SeekRow(barHeight: 3, fill: AnyShapeStyle(text), textColor: text.opacity(0.7), layout: .stacked)
            TransportCluster(
                scale: .regular, color: text, onColor: theme.palette.accent,
                playFill: AnyShapeStyle(text), playGlyph: theme.immersivePillGlyph)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 20)
        .background {
            NowPlayingBackdrop(imageURL: item?.imageURL, palette: theme.palette, isLight: theme.immersiveIsLight)
        }
        .environment(\.colorScheme, theme.immersiveIsLight ? .light : .dark)
    }

    private var footer: some View {
        HStack(spacing: 14) {
            DeviceMenu(showsActiveName: false)
            Spacer()
            footerLink("Player") { openWindow(id: "player") }
            footerLink("Mini") { openWindow(id: "mini-player") }
            footerLink("Quit") { NSApplication.shared.terminate(nil) }
        }
        .padding(.horizontal, 14)
        .frame(height: 44)
        .background { ZStack { room.raised; Grain() } }
        .overlay(alignment: .top) { Rectangle().fill(room.hairline).frame(height: 1) }
    }

    private func footerLink(_ title: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            MonoCaps(title, size: 9.5, color: room.inkMuted)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}
