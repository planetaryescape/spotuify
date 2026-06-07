import SwiftUI
import SpotuifyKit

extension SearchSort {
    var displayName: String {
        switch self {
        case .relevance: "Relevance"
        case .name: "Name"
        case .duration: "Duration"
        case .artist: "Artist"
        case .date: "Date"
        }
    }
}

/// A pill toggle for the search type filter.
struct SearchFilterChip: View {
    let label: String
    let selected: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Text(label)
                .font(.caption.weight(.medium))
                .padding(.horizontal, 12)
                .padding(.vertical, 5)
                .background(
                    Capsule().fill(selected ? AnyShapeStyle(.tint) : AnyShapeStyle(.primary.opacity(0.08))))
                .foregroundStyle(selected ? AnyShapeStyle(.white) : AnyShapeStyle(.primary))
        }
        .buttonStyle(.plain)
    }
}

struct SearchView: View {
    @Environment(AppModel.self) private var model
    /// Focus the field as soon as the page appears so the user can just type.
    @FocusState private var searchFocused: Bool

    var body: some View {
        NavigationStack {
            searchBody
                .mediaDetailDestinations()
        }
        // `.task` runs after the view is in the window, when @FocusState will
        // actually take (setting it in init/onAppear too early is dropped).
        .task { searchFocused = true }
    }

    private var searchBody: some View {
        @Bindable var search = model.search
        return VStack(spacing: 0) {
            HStack(spacing: 12) {
                HStack(spacing: 8) {
                    Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                    TextField("Search songs, artists, albums, playlists…", text: $search.query)
                        .textFieldStyle(.plain)
                        .font(.system(size: 15))
                        .focused($searchFocused)
                        .onSubmit { model.search.runSearch() }
                        .onChange(of: search.query) { _, _ in model.search.scheduleSearch() }
                    if !search.query.isEmpty {
                        Button {
                            search.query = ""
                            model.search.runSearch()
                        } label: {
                            Image(systemName: "xmark.circle.fill")
                        }
                        .buttonStyle(.plain)
                        .foregroundStyle(.secondary)
                    }
                }
                .glassField()
                // All of Spotify vs the user's cached library.
                Picker("Source", selection: Binding(
                    get: { search.source },
                    set: { model.search.setSource($0) })
                ) {
                    Text("Spotify").tag(SearchSource.spotify)
                    Text("Library").tag(SearchSource.local)
                }
                .pickerStyle(.segmented).fixedSize().labelsHidden()
                .help("Search all of Spotify or just your library")
            }
            .padding(.horizontal, 16)
            .padding(.top, 16)

            if !search.query.isEmpty {
                filterBar
            }

            Divider()
            content
        }
        // Fill top-to-bottom and pin to the top so the search field stays put in
        // every state — without this the empty state lets the parent center the
        // stack, and the field jumps up once results force the list to fill.
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(.background)
    }

    private var filterBar: some View {
        @Bindable var search = model.search
        return HStack(spacing: 10) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 6) {
                    SearchFilterChip(label: "All", selected: search.typeFilter.isEmpty) {
                        search.typeFilter = []
                        model.search.runSearch()
                    }
                    ForEach(SearchStore.filterableKinds, id: \.self) { kind in
                        SearchFilterChip(
                            label: kind.sectionTitle,
                            selected: search.typeFilter.contains(kind)
                        ) {
                            model.search.toggleFilter(kind)
                        }
                    }
                }
            }
            Menu {
                ForEach(SearchSort.allCases, id: \.self) { option in
                    Button {
                        model.search.setSort(option)
                    } label: {
                        if search.sort == option {
                            Label(option.displayName, systemImage: "checkmark")
                        } else {
                            Text(option.displayName)
                        }
                    }
                }
            } label: {
                Label(search.sort.displayName, systemImage: "arrow.up.arrow.down")
            }
            .menuStyle(.borderlessButton).menuIndicator(.hidden).fixedSize()
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
    }

    @ViewBuilder
    private var content: some View {
        let store = model.search
        if store.isSearching && store.results.isEmpty {
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if let error = store.errorMessage {
            ContentUnavailableView("Search failed", systemImage: "exclamationmark.triangle", description: Text(error))
        } else if store.results.isEmpty {
            ContentUnavailableView(
                "Search",
                systemImage: "magnifyingglass",
                description: Text("Find tracks, artists, albums, and playlists."))
        } else {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 2, pinnedViews: [.sectionHeaders]) {
                    ForEach(store.grouped, id: \.kind) { group in
                        Section {
                            ForEach(group.items) { item in
                                if item.kind == .track || item.kind == .episode {
                                    MediaRow(item: item)
                                } else {
                                    NavigationLink(value: item) {
                                        MediaRow(item: item)
                                    }
                                    .buttonStyle(.plain)
                                }
                            }
                        } header: {
                            Text(group.kind.sectionTitle)
                                .editorialSectionHeader()
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(.horizontal, 8)
                                .padding(.vertical, 6)
                                .background(.background.opacity(0.96))
                        }
                    }
                }
                .padding(.horizontal, 10)
                .padding(.bottom, 12)
            }
        }
    }
}
