import SwiftUI
import SpotuifyKit

/// Root layout: sidebar + destination content, with the always-visible
/// NowPlayingBar pinned to the bottom across the full width.
struct AppShell: View {
    @Environment(AppModel.self) private var model
    @Environment(ArtworkTheme.self) private var theme
    @State private var selection: Destination = .nowPlaying

    var body: some View {
        VStack(spacing: 0) {
            NavigationSplitView {
                Sidebar(selection: $selection)
                    .navigationSplitViewColumnWidth(min: 200, ideal: Theme.sidebarWidth, max: 260)
            } detail: {
                destinationView
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            .navigationSplitViewStyle(.balanced)
            Divider()
            NowPlayingBar()
        }
        .frame(minWidth: 880, minHeight: 620)
        .overlay(alignment: .top) { bannerView }
        .tint(theme.accent)
        .environment(theme)
        .task(id: model.player.currentItem?.imageURL) {
            await theme.update(for: model.player.currentItem?.imageURL)
        }
        .sheet(
            isPresented: Binding(
                get: { model.presentDueInbox },
                set: { model.presentDueInbox = $0 })
        ) {
            DueRemindersSheet { selection = .notifications }
        }
    }

    @ViewBuilder
    private var destinationView: some View {
        switch selection {
        case .nowPlaying: NowPlayingView()
        case .search: SearchView()
        case .likedSongs: LikedSongsView()
        case .albums: AlbumsView()
        case .artists: ArtistsView()
        case .podcasts: PodcastsView()
        case .playlists: PlaylistsView()
        case .queue: QueueView()
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
