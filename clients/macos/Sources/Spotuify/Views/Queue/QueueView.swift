import SwiftUI
import SpotuifyKit

struct QueueView: View {
    @Environment(AppModel.self) private var model
    @State private var viewSort: TrackSort = .original

    private var upcoming: [MediaItem] {
        let items = model.player.queue?.items ?? []
        switch viewSort {
        case .original: return items
        case .title: return items.sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
        case .artist: return items.sorted { $0.subtitle.localizedCaseInsensitiveCompare($1.subtitle) == .orderedAscending }
        case .album: return items.sorted { ($0.albumLabel ?? "").localizedCaseInsensitiveCompare($1.albumLabel ?? "") == .orderedAscending }
        case .duration: return items.sorted { $0.durationMs < $1.durationMs }
        case .dateAdded: return items
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            EditorialPageHeader(title: "Queue") {
                Picker("View order", selection: $viewSort) {
                    ForEach([TrackSort.original, .title, .artist, .album, .duration]) {
                        Text($0 == .original ? "Play order" : $0.rawValue).tag($0)
                    }
                }
                .pickerStyle(.menu).fixedSize().labelsHidden()
            }
            if viewSort != .original {
                Text("Sorted for viewing — Spotify plays in the original order.")
                    .font(.caption2).foregroundStyle(.secondary)
                    .padding(.horizontal, 16).padding(.bottom, 6)
            }
            Divider()
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 2) {
                    if let current = model.player.currentItem {
                        sectionHeader("Now Playing")
                        MediaRow(item: current)
                    }
                    if !upcoming.isEmpty {
                        sectionHeader("Next Up")
                        ForEach(Array(upcoming.enumerated()), id: \.offset) { _, item in
                            MediaRow(item: item)
                        }
                    } else if model.player.currentItem == nil {
                        ContentUnavailableView("Queue is empty", systemImage: "list.bullet",
                            description: Text("Songs you queue will show up here."))
                            .padding(.top, 60)
                    }
                }
                .padding(10)
            }
        }
        .background(.background)
    }

    private func sectionHeader(_ title: String) -> some View {
        Text(title)
            .editorialSectionHeader()
            .foregroundStyle(.secondary)
            .padding(.horizontal, 8)
            .padding(.top, 10)
            .padding(.bottom, 2)
    }
}
