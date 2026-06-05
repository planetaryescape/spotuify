import SwiftUI
import SpotuifyKit

/// Subscribed podcasts grid → show detail (episodes, unplayed filter, sort).
struct PodcastsView: View {
    @Environment(AppModel.self) private var model

    private let columns = [GridItem(.adaptive(minimum: 150, maximum: 200), spacing: 16)]

    var body: some View {
        NavigationStack {
            Group {
                if model.library.loadingShows && model.library.savedShows.isEmpty {
                    ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
                } else if model.library.savedShows.isEmpty {
                    ContentUnavailableView("No podcasts", systemImage: "mic",
                        description: Text("Shows you follow on Spotify appear here."))
                } else {
                    ScrollView {
                        LazyVGrid(columns: columns, spacing: 16) {
                            ForEach(model.library.savedShows) { show in
                                NavigationLink(value: show) { ArtworkTile(item: show) }
                                    .buttonStyle(.plain)
                            }
                        }
                        .padding(16)
                    }
                }
            }
            .navigationTitle("Podcasts")
            .mediaDetailDestinations()
        }
        .background(.background)
        .task { await model.library.loadShows() }
    }
}
