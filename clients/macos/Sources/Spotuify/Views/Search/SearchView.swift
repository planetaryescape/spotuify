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

/// A mono pill toggle for the search type filter.
struct SearchFilterChip: View {
    @Environment(\.room) private var room
    let label: String
    let selected: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            MonoCaps(label, size: 9.5, color: selected ? room.base : room.inkMuted)
                .padding(.horizontal, 12)
                .frame(height: 26)
                .background(Capsule().fill(selected ? room.ink : room.ink.opacity(0.05)))
                .overlay(Capsule().strokeBorder(room.ink.opacity(selected ? 0 : 0.1)))
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(selected ? [.isSelected] : [])
    }
}

struct SearchView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.room) private var room
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
        return VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 14) {
                HStack(alignment: .firstTextBaseline) {
                    MonoCaps("Search", size: 10)
                    Spacer()
                    // Provider catalogs exposed by the daemon plus local cache.
                    RoomTabs(
                        options: search.sourceOptions.map { (value: $0.source, title: $0.label) },
                        selection: Binding(get: { search.selectedSource }, set: { model.search.setSource($0) }))
                        .help("Search a provider catalog or just your local library")
                }
                // The query is the page's headline: set in the display serif on
                // a hairline, like writing on the sleeve.
                HStack(alignment: .center, spacing: 12) {
                    TextField("What do you want to hear?", text: $search.query)
                        .textFieldStyle(.plain)
                        .font(.displayTitle(34))
                        .foregroundStyle(room.ink)
                        .focused($searchFocused)
                        .onSubmit { model.search.runSearch() }
                        .onChange(of: search.query) { _, _ in model.search.scheduleSearch() }
                    if model.search.isSearching {
                        ProgressView().controlSize(.small)
                    }
                    if !search.query.isEmpty {
                        Button {
                            search.query = ""
                            model.search.runSearch()
                        } label: {
                            Image(systemName: "xmark")
                                .font(.system(size: 11, weight: .bold))
                                .foregroundStyle(room.inkMuted)
                                .frame(width: 26, height: 26)
                                .background(Circle().fill(room.ink.opacity(0.07)))
                        }
                        .buttonStyle(.plain)
                        .help("Clear")
                    }
                }
                .padding(.bottom, 8)
                .overlay(alignment: .bottom) {
                    Rectangle()
                        .fill(searchFocused ? room.accent : room.ink.opacity(0.18))
                        .frame(height: searchFocused ? 2 : 1)
                        .animation(.easeOut(duration: 0.2), value: searchFocused)
                }
            }
            .padding(.horizontal, 32)
            .padding(.top, 44)

            if !search.query.isEmpty {
                filterBar
            }
            content
        }
        // Fill top-to-bottom and pin to the top so the search field stays put in
        // every state — without this the empty state lets the parent center the
        // stack, and the field jumps up once results force the list to fill.
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
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
                    ForEach(search.filterableKinds, id: \.self) { kind in
                        SearchFilterChip(
                            label: kind.sectionTitle,
                            selected: search.typeFilter.contains(kind)
                        ) {
                            model.search.toggleFilter(kind)
                        }
                    }
                }
            }
            RoomMenuPicker(
                label: "Sort",
                options: SearchSort.allCases.map { (value: $0, title: $0.displayName) },
                selection: Binding(get: { search.sort }, set: { model.search.setSort($0) }))
        }
        .padding(.horizontal, 32)
        .padding(.top, 16)
        .padding(.bottom, 4)
    }

    @ViewBuilder
    private var content: some View {
        let store = model.search
        if store.isSearching && store.results.isEmpty {
            SkeletonRows()
        } else if let error = store.errorMessage {
            EmptyState("Search failed", systemImage: "exclamationmark.triangle", description: Text(error))
        } else if store.results.isEmpty {
            EmptyState(
                store.query.isEmpty ? "Start typing" : "Nothing found",
                systemImage: "magnifyingglass",
                description: Text(store.query.isEmpty
                    ? "Songs, artists, albums, playlists and podcasts — from your library and the catalogue."
                    : "Try fewer words, or search a different source."))
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
                            RoomSectionLabel(group.kind.sectionTitle)
                                .padding(.horizontal, 8)
                                .roomPinnedBackground()
                        }
                    }
                }
                .padding(.horizontal, 24)
                .padding(.bottom, 24)
            }
        }
    }
}
