import AppKit
import SwiftUI
import SpotuifyKit

/// Root layout: the typographic sidebar, the page, and the optional queue /
/// lyrics rail, all standing in one room, with the deck along the bottom.
/// Custom rather than `NavigationSplitView` so the app has its own face.
struct AppShell: View {
    @Environment(AppModel.self) private var model
    @Environment(ArtworkTheme.self) private var theme
    @Environment(Navigator.self) private var navigator
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    /// Shared with NowPlayingView: when it minimises its controls for full art,
    /// the deck reappears so playback stays controllable.
    @AppStorage("nowPlayingMinimized") private var nowPlayingMinimized = false
    /// The global right-hand panel (queue / lyrics), toggled from the deck
    /// and available on every page (Now Playing has its own companions).
    @AppStorage("globalSidePanel") private var globalPanelRaw = GlobalPanel.none.rawValue
    @AppStorage("sidebarVisible") private var sidebarVisible = true
    private var globalPanel: GlobalPanel { GlobalPanel(rawValue: globalPanelRaw) ?? .none }

    /// The deck sits under every page except the stage, which has its own
    /// transport — unless the stage is in cinema mode, where the deck returns.
    private var showsDeck: Bool { navigator.selection != .nowPlaying || nowPlayingMinimized }

    var body: some View {
        @Bindable var nav = navigator
        let room = theme.room
        return VStack(spacing: 0) {
            HStack(spacing: 0) {
                if sidebarVisible {
                    Sidebar(selection: $nav.selection)
                        .frame(width: Theme.sidebarWidth)
                        .transition(.move(edge: .leading).combined(with: .opacity))
                    Rectangle().fill(room.hairline).frame(width: 1).ignoresSafeArea()
                }
                destinationView
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                // Global queue/lyrics rail — on every page except Now Playing
                // (which has its own companions).
                if globalPanel != .none && navigator.selection != .nowPlaying {
                    GlobalSidePanel(panel: globalPanel) { globalPanelRaw = GlobalPanel.none.rawValue }
                        .frame(width: 340)
                        .transition(.move(edge: .trailing).combined(with: .opacity))
                }
            }
            if showsDeck {
                NowPlayingBar()
                    .transition(.move(edge: .bottom).combined(with: .opacity))
            }
        }
        .background { RoomFloor(imageURL: model.player.currentItem?.imageURL) }
        .animation(.easeInOut(duration: 0.25), value: globalPanel)
        .animation(.spring(response: 0.4, dampingFraction: 0.86), value: showsDeck)
        .animation(.spring(response: 0.4, dampingFraction: 0.86), value: sidebarVisible)
        .frame(minWidth: 880, minHeight: 620)
        .overlay(alignment: .top) { bannerView }
        .overlay(alignment: .bottom) { toastView }
        .animation(.spring(response: 0.35, dampingFraction: 0.82), value: model.toast)
        .tint(room.accent)
        .foregroundStyle(room.ink)
        .environment(\.room, room)
        // System pieces we keep (menus, text fields, sheets) match the room.
        .environment(\.colorScheme, room.isLight ? .light : .dark)
        .environment(theme)
        // Re-key on `adaptiveEnabled` so switching back to Adaptive re-extracts
        // the current cover; under a fixed theme `update` no-ops (the fixed
        // palette is applied at the app root via `.desktopTheme`).
        .task(id: "\(theme.adaptiveEnabled)#\(model.player.currentItem?.imageURL ?? "")") {
            await theme.update(for: model.player.currentItem?.imageURL, reduceMotion: reduceMotion)
        }
        .sheet(
            isPresented: Binding(
                get: { model.presentDueInbox },
                set: { model.presentDueInbox = $0 })
        ) {
            DueRemindersSheet { navigator.selection = .notifications }
        }
    }

    @ViewBuilder
    private var destinationView: some View {
        switch navigator.selection {
        case .nowPlaying: NowPlayingView()
        case .search: SearchView()
        case .likedSongs: LikedSongsView()
        case .albums: AlbumsView()
        case .artists: ArtistsView()
        case .podcasts: PodcastsView()
        case .playlists: PlaylistsView()
        case .queue: QueueView()
        case .history: HistoryView()
        case .notifications: RemindersView()
        case .bookmarks: BookmarksView()
        case .devices: DevicesView()
        }
    }

    @ViewBuilder
    private var bannerView: some View {
        if let banner = model.banner {
            HStack(spacing: 8) {
                Image(systemName: "exclamationmark.triangle.fill")
                Text(banner).font(.callout)
                Spacer()
                Button {
                    model.clearBanner()
                } label: { Image(systemName: "xmark") }
                    .buttonStyle(.plain)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .background(theme.room.raised, in: Capsule())
            .overlay(Capsule().strokeBorder(theme.room.hairline))
            .foregroundStyle(theme.room.ink)
            .frame(maxWidth: 560)
            .padding(.top, 14)
            .shadow(radius: 6, y: 2)
            .transition(.move(edge: .top).combined(with: .opacity))
        }
    }

    /// Transient confirmation toast (e.g. "Added to queue"), floated above the
    /// player bar so fire-and-forget actions get instant, visible feedback.
    @ViewBuilder
    private var toastView: some View {
        if let toast = model.toast {
            HStack(spacing: 8) {
                Image(systemName: "checkmark.circle.fill").foregroundStyle(.tint)
                Text(toast).font(.callout.weight(.medium))
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 10)
            .background(theme.room.raised, in: Capsule())
            .overlay(Capsule().strokeBorder(theme.room.hairline))
            .shadow(color: .black.opacity(0.3), radius: 10, y: 3)
            .padding(.bottom, 96)
            .transition(.move(edge: .bottom).combined(with: .opacity))
        }
    }
}

/// The global right-hand rail: up-next queue or synced lyrics, openable from
/// any page via the footer bar (Apple-Music-style).
enum GlobalPanel: String { case none, queue, lyrics }

struct GlobalSidePanel: View {
    @Environment(ArtworkTheme.self) private var theme
    @Environment(\.room) private var room
    let panel: GlobalPanel
    let onClose: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                MonoCaps(panel == .queue ? "Up next" : "Lyrics", size: 10.5, color: room.inkMuted)
                Spacer()
                Button(action: onClose) {
                    Image(systemName: "xmark").font(.system(size: 10, weight: .bold))
                        .foregroundStyle(room.inkMuted)
                        .frame(width: 24, height: 24)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help("Close")
            }
            .padding(.horizontal, 18).padding(.top, 18).padding(.bottom, 6)
            content
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .padding(.horizontal, 8)
        }
        .background(room.raised.opacity(0.55).ignoresSafeArea())
        .overlay(alignment: .leading) {
            Rectangle().fill(room.hairline).frame(width: 1).ignoresSafeArea()
        }
    }

    @ViewBuilder
    private var content: some View {
        switch panel {
        case .queue: NowPlayingQueue(accent: theme.accent, textColor: room.ink)
        case .lyrics: LyricsView(textColor: room.ink)
        case .none: EmptyView()
        }
    }
}
