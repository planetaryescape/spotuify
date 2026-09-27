import SwiftUI
import SpotuifyKit

/// Podcasts home: toggle between followed Shows and a flat, date-ordered
/// Episodes feed across every show you follow. A search box filters the
/// followed list (Library) or queries the selected provider's catalog, and the
/// Episodes feed can be sorted by date / duration / title / show.
struct PodcastsView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.room) private var room

    private var store: PodcastsStore { model.podcasts }

    private var sortLabels: [(EpisodeSort, String)] {
        [(.newest, "Newest"), (.oldest, "Oldest"), (.duration, "Duration"),
         (.title, "Title"), (.show, "Show")]
    }

    var body: some View {
        @Bindable var store = model.podcasts
        return NavigationStack {
            VStack(alignment: .leading, spacing: 0) {
                EditorialPageHeader("Podcasts", eyebrow: "Library")
                controlBar($store)
                content
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .mediaDetailDestinations()
        }
        .task {
            await model.library.loadShows()
            if store.mode == .episodes { await store.loadEpisodes() }
        }
    }

    @ViewBuilder
    private func controlBar(_ store: Bindable<PodcastsStore>) -> some View {
        HStack(spacing: 18) {
            RoomTabs(
                options: [(value: PodcastsStore.Mode.shows, title: "Shows"),
                          (value: PodcastsStore.Mode.episodes, title: "Episodes")],
                selection: Binding(
                    get: { store.wrappedValue.mode },
                    set: { store.wrappedValue.setMode($0) }))

            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(room.inkMuted)
                TextField(
                    store.wrappedValue.isCatalogSource
                        ? "Search \(store.wrappedValue.selectedCatalogLabel)…" : "Filter…",
                    text: store.query)
                    .onChange(of: store.wrappedValue.query) { store.wrappedValue.scheduleSearch() }
                    .onSubmit { store.wrappedValue.runSearch() }
            }
            .glassField()
            .frame(maxWidth: 300)

            // Library vs provider catalogs, as tabs: the source is part of
            // what you're looking at, not a setting.
            RoomTabs(
                options: [(value: SearchSource.local, title: "Library")]
                    + store.wrappedValue.catalogSourceOptions.map { (value: $0.source, title: $0.label) },
                selection: Binding(
                    get: { store.wrappedValue.source },
                    set: { store.wrappedValue.setSource($0) }))

            Spacer()

            if store.wrappedValue.mode == .episodes {
                RoomMenuPicker(
                    label: "Sort",
                    options: sortLabels.map { (value: $0.0, title: $0.1) },
                    selection: Binding(
                        get: { store.wrappedValue.episodeSort },
                        set: { store.wrappedValue.setEpisodeSort($0) }))
            }
        }
        .padding(.horizontal, 32).padding(.bottom, 10)
    }

    @ViewBuilder
    private var content: some View {
        switch store.mode {
        case .shows: showsContent
        case .episodes: episodesContent
        }
    }

    @ViewBuilder
    private var showsContent: some View {
        let shows = store.shows(libraryShows: model.library.savedShows)
        if model.library.loadingShows && model.library.savedShows.isEmpty {
            SkeletonTiles()
        } else if shows.isEmpty {
            EmptyState(
                store.isCatalogSource ? "No results" : "No podcasts",
                systemImage: "mic",
                description: Text(store.isCatalogSource
                    ? "Try a different search."
                    : "Shows you follow appear here."))
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            CollectionView(items: shows, storageKey: "podcastsLayout")
        }
    }

    @ViewBuilder
    private var episodesContent: some View {
        let episodes = store.episodes
        if store.loadingEpisodes && episodes.isEmpty {
            SkeletonRows()
        } else if episodes.isEmpty {
            EmptyState(
                "No episodes", systemImage: "waveform",
                description: Text(store.isCatalogSource
                    ? "Search \(store.selectedCatalogLabel) for episodes."
                    : "Episodes from the shows you follow appear here, newest first."))
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            ScrollView {
                LazyVStack(spacing: 2) {
                    ForEach(Array(episodes.enumerated()), id: \.element.id) { index, episode in
                        MediaRow(item: episode, detailed: true, index: index + 1)
                    }
                }
                .padding(.horizontal, 24).padding(.bottom, 24)
            }
        }
    }
}
