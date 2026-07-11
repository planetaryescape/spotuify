import AppKit
import SwiftUI
import SpotuifyKit

/// Root layout: sidebar + destination content, with the always-visible
/// NowPlayingBar pinned to the bottom across the full width.
struct AppShell: View {
    @Environment(AppModel.self) private var model
    @Environment(ArtworkTheme.self) private var theme
    @Environment(Navigator.self) private var navigator
    /// Shared with NowPlayingView: when it minimises its controls for full art,
    /// the footer transport reappears so playback stays controllable.
    @AppStorage("nowPlayingMinimized") private var nowPlayingMinimized = false
    /// The global right-hand panel (queue / lyrics), toggled from the footer bar
    /// and available on every page (Now Playing has its own panels instead).
    @AppStorage("globalSidePanel") private var globalPanelRaw = GlobalPanel.none.rawValue
    private var globalPanel: GlobalPanel { GlobalPanel(rawValue: globalPanelRaw) ?? .none }
    /// Whether to surface the "newer release available" banner. Mirrors the
    /// Settings toggle; the daemon's check itself is opt-out via env/config.
    @AppStorage("autoCheckUpdates") private var autoCheckUpdates = true

    var body: some View {
        @Bindable var nav = navigator
        return VStack(spacing: 0) {
            HStack(spacing: 0) {
                NavigationSplitView {
                    Sidebar(selection: $nav.selection)
                        .navigationSplitViewColumnWidth(min: 200, ideal: Theme.sidebarWidth, max: 260)
                } detail: {
                    destinationView
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
                .navigationSplitViewStyle(.balanced)
                // Global queue/lyrics rail — shown on every page except Now
                // Playing (which has its own in-stage panels).
                if globalPanel != .none && navigator.selection != .nowPlaying {
                    Divider()
                    GlobalSidePanel(panel: globalPanel) { globalPanelRaw = GlobalPanel.none.rawValue }
                        .frame(width: 340)
                        .transition(.move(edge: .trailing).combined(with: .opacity))
                }
            }
            // The immersive Now Playing page has its own full transport, so hide
            // the bottom bar there — unless its controls are minimised for full
            // art, in which case the footer is where the transport lives.
            if navigator.selection != .nowPlaying || nowPlayingMinimized {
                Divider()
                NowPlayingBar()
            }
        }
        .animation(.easeInOut(duration: 0.25), value: globalPanel)
        .frame(minWidth: 880, minHeight: 620)
        .overlay(alignment: .top) { bannerView }
        .overlay(alignment: .top) { updateBannerView }
        .overlay(alignment: .bottom) { toastView }
        .animation(.spring(response: 0.35, dampingFraction: 0.82), value: model.toast)
        .animation(.spring(response: 0.35, dampingFraction: 0.82), value: model.availableUpdate)
        .tint(theme.accent)
        .environment(theme)
        // Re-key on `adaptiveEnabled` so switching back to Adaptive re-extracts
        // the current cover; under a fixed theme `update` no-ops (the fixed
        // palette is applied at the app root via `.desktopTheme`).
        .task(id: "\(theme.adaptiveEnabled)#\(model.player.currentItem?.imageURL ?? "")") {
            await theme.update(for: model.player.currentItem?.imageURL)
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
            .background(.thinMaterial, in: Capsule())
            .foregroundStyle(.primary)
            .padding(.top, 10)
            .shadow(radius: 6, y: 2)
            .transition(.move(edge: .top).combined(with: .opacity))
        }
    }

    /// "A newer release is available" banner with an upgrade action. Shown only
    /// when auto-check is on and no error banner is competing for the top slot.
    @ViewBuilder
    private var updateBannerView: some View {
        if autoCheckUpdates, model.banner == nil, let update = model.availableUpdate {
            HStack(spacing: 10) {
                Image(systemName: "arrow.up.circle.fill").foregroundStyle(.tint)
                Text(updateBannerTitle(for: update))
                    .font(.callout.weight(.medium))
                    .lineLimit(2)
                Spacer(minLength: 8)
                switch model.updater.phase {
                case .downloading, .verifying, .installing:
                    ProgressView().controlSize(.small)
                case .installed(let url):
                    Button("Relaunch") { AppRelaunch.relaunch(from: url) }
                        .buttonStyle(.borderedProminent).controlSize(.small)
                case .failed:
                    if let urlString = update.url, let url = URL(string: urlString) {
                        Button("Open releases page") { NSWorkspace.shared.open(url) }
                            .buttonStyle(.bordered).controlSize(.small)
                    }
                    Button("Retry") {
                        model.updater.reset()
                        model.installAvailableUpdate()
                    }
                    .buttonStyle(.borderedProminent).controlSize(.small)
                case .idle:
                    Button("Update Now") { model.installAvailableUpdate() }
                        .buttonStyle(.borderedProminent).controlSize(.small)
                }
                Button { model.dismissUpdate() } label: { Image(systemName: "xmark") }
                    .buttonStyle(.plain).foregroundStyle(.secondary)
                    // Dismissing mid-install orphaned a completed swap
                    // with no Relaunch button anywhere.
                    .disabled(model.updater.phase.isBusy)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .background(.thinMaterial, in: Capsule())
            .overlay(Capsule().strokeBorder(.white.opacity(0.08)))
            .shadow(color: .black.opacity(0.3), radius: 8, y: 2)
            .padding(.top, 10)
            .frame(maxWidth: 520)
            .transition(.move(edge: .top).combined(with: .opacity))
        }
    }

    private func updateBannerTitle(for update: AvailableUpdate) -> String {
        switch model.updater.phase {
        case .downloading: return "Downloading spotuify \(update.latestVersion)…"
        case .verifying: return "Verifying download…"
        case .installing: return "Installing spotuify \(update.latestVersion)…"
        case .installed: return "spotuify \(update.latestVersion) installed — relaunch to finish"
        case .failed(let message): return message
        case .idle: return "spotuify \(update.latestVersion) is available"
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
            .background(.thinMaterial, in: Capsule())
            .overlay(Capsule().strokeBorder(.white.opacity(0.08)))
            .shadow(color: .black.opacity(0.3), radius: 10, y: 3)
            .padding(.bottom, 112)
            .transition(.move(edge: .bottom).combined(with: .opacity))
        }
    }
}

/// The global right-hand rail: up-next queue or synced lyrics, openable from
/// any page via the footer bar (Apple-Music-style).
enum GlobalPanel: String { case none, queue, lyrics }

struct GlobalSidePanel: View {
    @Environment(ArtworkTheme.self) private var theme
    let panel: GlobalPanel
    let onClose: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text(panel == .queue ? "Up Next" : "Lyrics")
                    .font(.headline)
                Spacer()
                Button(action: onClose) {
                    Image(systemName: "xmark").font(.system(size: 12, weight: .bold))
                }
                .buttonStyle(.plain).foregroundStyle(.secondary)
                .help("Close")
            }
            .padding(.horizontal, 14).padding(.vertical, 10)
            Divider()
            content
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .padding(.horizontal, 10)
        }
        // The header/content stay in the safe area (below the titlebar), but the
        // material fills up behind the (translucent) titlebar so the rail reads as
        // one continuous full-height panel. Without this, the toolbar region over
        // this sibling-of-the-split-view showed as an empty dark strip above the
        // header.
        .background {
            Rectangle()
                .fill(.regularMaterial)
                .ignoresSafeArea(.container, edges: .top)
        }
    }

    @ViewBuilder
    private var content: some View {
        switch panel {
        case .queue: NowPlayingQueue(accent: theme.accent)
        case .lyrics: LyricsView()
        case .none: EmptyView()
        }
    }
}

/// Placeholder for destinations filled in by later phases.
struct ComingSoonView: View {
    let destination: Destination

    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: destination.icon)
                .font(.system(size: 44))
                .foregroundStyle(.tertiary)
            Text(destination.title)
                .font(.title2.bold())
            Text("Coming soon")
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(.background)
    }
}
