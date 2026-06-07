import SwiftUI
import SpotuifyKit

/// Podcasts home: toggle between followed Shows and a flat, date-ordered
/// Episodes feed across every show you follow. A search box filters the
/// followed list (Library) or queries Spotify (all of the catalog), and the
/// Episodes feed can be sorted by date / duration / title / show.
struct PodcastsView: View {
    @Environment(AppModel.self) private var model

    private var store: PodcastsStore { model.podcasts }

    private var sortLabels: [(EpisodeSort, String)] {
        [(.newest, "Newest"), (.oldest, "Oldest"), (.duration, "Duration"),
         (.title, "Title"), (.show, "Show")]
    }

    var body: some View {
        @Bindable var store = model.podcasts
        return NavigationStack {
            VStack(alignment: .leading, spacing: 0) {
                EditorialPageHeader("Podcasts")
                controlBar($store)
                Divider()
                content
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .mediaDetailDestinations()
        }
        .background(.background)
        .task {
            await model.library.loadShows()
            if store.mode == .episodes { await store.loadEpisodes() }
        }
    }

    @ViewBuilder
    private func controlBar(_ store: Bindable<PodcastsStore>) -> some View {
        HStack(spacing: 12) {
            Picker("View", selection: Binding(
                get: { store.wrappedValue.mode },
                set: { store.wrappedValue.setMode($0) })
            ) {
                Text("Shows").tag(PodcastsStore.Mode.shows)
                Text("Episodes").tag(PodcastsStore.Mode.episodes)
            }
            .pickerStyle(.segmented).fixedSize().labelsHidden()

            HStack(spacing: 6) {
                Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                TextField(
                    store.wrappedValue.source == .spotify ? "Search Spotify…" : "Filter…",
                    text: store.query)
                    .textFieldStyle(.plain)
                    .onChange(of: store.wrappedValue.query) { store.wrappedValue.scheduleSearch() }
                    .onSubmit { store.wrappedValue.runSearch() }
            }
            .padding(.horizontal, 8).padding(.vertical, 5)
            .background(.quaternary, in: RoundedRectangle(cornerRadius: 7))
            .frame(maxWidth: 320)

            Picker("Source", selection: Binding(
                get: { store.wrappedValue.source },
                set: { store.wrappedValue.setSource($0) })
            ) {
                Text("Library").tag(SearchSource.local)
                Text("Spotify").tag(SearchSource.spotify)
            }
            .pickerStyle(.segmented).fixedSize().labelsHidden()

            Spacer()

            if store.wrappedValue.mode == .episodes {
                Menu {
                    ForEach(sortLabels, id: \.0) { value, label in
                        Button {
                            store.wrappedValue.setEpisodeSort(value)
                        } label: {
                            if store.wrappedValue.episodeSort == value {
                                Label(label, systemImage: "checkmark")
                            } else {
                                Text(label)
                            }
                        }
                    }
                } label: {
                    Label(
                        sortLabels.first { $0.0 == store.wrappedValue.episodeSort }?.1 ?? "Sort",
                        systemImage: "arrow.up.arrow.down")
                }
                .menuStyle(.borderlessButton).fixedSize()
            }
        }
        .padding(.horizontal, 16).padding(.vertical, 8)
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
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if shows.isEmpty {
            ContentUnavailableView(
                store.source == .spotify ? "No results" : "No podcasts",
                systemImage: "mic",
                description: Text(store.source == .spotify
                    ? "Try a different search."
                    : "Shows you follow on Spotify appear here."))
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            CollectionView(items: shows, storageKey: "podcastsLayout")
        }
    }

    @ViewBuilder
    private var episodesContent: some View {
        let episodes = store.episodes
        if store.loadingEpisodes && episodes.isEmpty {
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if episodes.isEmpty {
            ContentUnavailableView(
                "No episodes", systemImage: "waveform",
                description: Text(store.source == .spotify
                    ? "Search Spotify for episodes."
                    : "Episodes from the shows you follow appear here, newest first."))
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            ScrollView {
                LazyVStack(spacing: 2) {
                    ForEach(episodes) { episode in
                        MediaRow(item: episode, detailed: true)
                    }
                }
                .padding(10)
            }
        }
    }
}
