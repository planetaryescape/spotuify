import SwiftUI
import SpotuifyKit

struct SearchView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        NavigationStack {
            searchBody
                .mediaDetailDestinations()
        }
    }

    private var searchBody: some View {
        @Bindable var search = model.search
        return VStack(spacing: 0) {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                TextField("Search songs, artists, albums, playlists…", text: $search.query)
                    .textFieldStyle(.plain)
                    .font(.system(size: 15))
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
            .padding(.horizontal, 12)
            .padding(.vertical, 9)
            .background(.quaternary, in: Capsule())
            .padding(16)

            Divider()
            content
        }
        .background(.background)
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
                                .font(.headline)
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
