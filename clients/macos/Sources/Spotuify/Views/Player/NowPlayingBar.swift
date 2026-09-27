import SwiftUI
import SpotuifyKit

/// The deck: a solid raised strip along the bottom of every page except the
/// stage. Left, what's playing; centre, the transport over the seek line;
/// right, the one-click companions (lyrics, queue, device, volume) and an
/// overflow for the rarely used (EQ, speed, bookmark, mini player).
struct NowPlayingBar: View {
    @Environment(AppModel.self) private var model
    @Environment(ArtworkTheme.self) private var theme
    @Environment(Navigator.self) private var navigator
    @Environment(\.openWindow) private var openWindow
    @Environment(\.room) private var room
    @AppStorage("globalSidePanel") private var globalPanelRaw = GlobalPanel.none.rawValue

    private var item: MediaItem? { model.player.currentItem }
    private var globalPanel: GlobalPanel { GlobalPanel(rawValue: globalPanelRaw) ?? .none }

    var body: some View {
        HStack(spacing: 14) {
            trackCell
                .frame(minWidth: 130, maxWidth: 300, alignment: .leading)
                .layoutPriority(1)
            Spacer(minLength: 0)
            VStack(spacing: 2) {
                TransportCluster(
                    scale: .compact, color: room.ink, onColor: room.accent,
                    playFill: AnyShapeStyle(room.ink), playGlyph: room.base)
                SeekRow(barHeight: 3, fill: AnyShapeStyle(room.accent), textColor: room.inkFaint)
            }
            .frame(minWidth: 250, maxWidth: 460)
            .layoutPriority(3)
            Spacer(minLength: 0)
            ViewThatFits(in: .horizontal) {
                trailing(showsVolume: true)
                trailing(showsVolume: false)
            }
            .layoutPriority(2)
        }
        .padding(.leading, 14)
        .padding(.trailing, 18)
        .frame(height: 76)
        .background {
            ZStack {
                room.raised.opacity(0.92)
                Grain(intensity: 0.8)
            }
            .ignoresSafeArea()
        }
        .overlay(alignment: .top) {
            Rectangle().fill(room.hairline).frame(height: 1)
        }
        .overlay(alignment: .top) {
            // Progress, drawn along the deck's top edge: readable from across
            // the room even when the seek line is too thin to notice.
            GeometryReader { geo in
                Rectangle().fill(room.accent.opacity(0.7))
                    .frame(width: geo.size.width * model.player.progressFraction, height: 1)
            }
            .frame(height: 1)
            .allowsHitTesting(false)
        }
    }

    /// Art + title + artist. Clicking the art or title opens the stage.
    private var trackCell: some View {
        HStack(spacing: 10) {
            Button { navigator.selection = .nowPlaying } label: {
                DockArtwork(url: item?.imageURL)
            }
            .buttonStyle(PressableButtonStyle())
            .help("Open Now Playing")
            .accessibilityLabel("Open Now Playing")
            VStack(alignment: .leading, spacing: 2) {
                Text(item?.name ?? "Nothing playing")
                    .font(.displayTitle(15))
                    .foregroundStyle(room.ink)
                    .lineLimit(1)
                Text(item?.subtitle.isEmpty == false ? item!.subtitle : "Pick something to play")
                    .font(.system(size: 11.5, weight: .medium))
                    .foregroundStyle(room.inkMuted)
                    .lineLimit(1)
            }
            if let item {
                NowPlayingLikeButton(item: item, accent: room.accent, unlikedTint: room.inkMuted, diameter: 26) {
                    model.likeCurrent()
                }
            }
        }
    }

    private func trailing(showsVolume: Bool) -> some View {
        HStack(spacing: 2) {
            PlaybackSpeedButton()
                .padding(.trailing, 6)
            panelToggle(.lyrics, icon: "quote.bubble", label: "Lyrics")
            panelToggle(.queue, icon: "list.bullet", label: "Up Next")
                .disabled(!model.canReadQueue)
            DeviceMenu(showsActiveName: false)
            if showsVolume {
                VolumeControl(fill: AnyShapeStyle(room.ink), iconColor: room.inkMuted)
                    .frame(width: 92)
                    .padding(.leading, 4)
                    .disabled(!model.canSetVolume)
            }
            overflowMenu
        }
        .fixedSize()
    }

    private func panelToggle(_ target: GlobalPanel, icon: String, label: String) -> some View {
        TransportGlyph(
            systemName: icon, label: label, size: 12.5,
            isOn: globalPanel == target, color: room.ink, onColor: room.accent
        ) {
            globalPanelRaw = (globalPanel == target ? GlobalPanel.none : target).rawValue
        }
    }

    private var overflowMenu: some View {
        Menu {
            // A custom curve has no preset; the picker then shows nothing checked.
            Picker(selection: Binding(get: { model.eq.preset }, set: { if let preset = $0 { model.setEqPreset(preset) } })) {
                ForEach(EqSettings.presets, id: \.self) { Text($0).tag(Optional($0)) }
            } label: {
                Label("Equalizer", systemImage: "slider.horizontal.3")
            }
            Button { model.addBookmark() } label: {
                Label("Bookmark This Moment", systemImage: "bookmark")
            }
            .disabled(item == nil)
            Divider()
            Button { openWindow(id: "mini-player") } label: {
                Label("Mini Player", systemImage: "pip")
            }
        } label: {
            Image(systemName: "ellipsis")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(model.eq.isFlat ? room.ink : room.accent)
                .frame(width: 28, height: 28)
                .contentShape(Circle())
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .fixedSize()
        .help(model.eq.isFlat ? "More" : "More — Equalizer: \(model.eq.label)")
    }
}

/// The dock's cover: reveals an "open the stage" glyph on hover.
private struct DockArtwork: View {
    let url: String?
    @State private var hovering = false

    var body: some View {
        AsyncCoverImage(url: url, cornerRadius: 8)
            .frame(width: 50, height: 50)
            .overlay {
                if hovering {
                    RoundedRectangle(cornerRadius: 8, style: .continuous).fill(.black.opacity(0.4))
                    Image(systemName: "arrow.up.left.and.arrow.down.right")
                        .font(.system(size: 13, weight: .bold))
                        .foregroundStyle(.white)
                }
            }
            .shadow(color: .black.opacity(0.2), radius: 4, y: 2)
            .onHover { hovering = $0 }
            .animation(.easeOut(duration: 0.15), value: hovering)
    }
}
