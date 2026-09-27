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
        NavigationStack {
            queueContent.mediaDetailDestinations()
        }
    }

    private var queueContent: some View {
        VStack(alignment: .leading, spacing: 0) {
            EditorialPageHeader(title: "Queue", eyebrow: "Listen") {
                RoomMenuPicker(
                    label: "View",
                    options: [TrackSort.original, .title, .artist, .album, .duration].map {
                        (value: $0, title: $0 == .original ? "Play order" : $0.rawValue)
                    },
                    selection: $viewSort)
            }
            if viewSort != .original {
                Text("Sorted for viewing — Spotify plays in the original order.")
                    .font(.caption).foregroundStyle(.secondary)
                    .padding(.horizontal, 32).padding(.bottom, 6)
            }
            if !model.canReadQueue {
                EmptyState(
                    "Queue unavailable", systemImage: "list.bullet.rectangle",
                    description: Text("The current provider does not expose its playback queue."))
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 2) {
                        if let current = model.player.currentItem {
                            sectionHeader("Now Playing")
                            MediaRow(item: current)
                        }
                        if !upcoming.isEmpty {
                            sectionHeader("Next Up")
                            ForEach(Array(upcoming.enumerated()), id: \.offset) { index, item in
                                MediaRow(item: item, index: index + 1)
                            }
                        } else if model.player.currentItem == nil {
                            EmptyState("Queue is empty", systemImage: "list.bullet",
                                description: Text("Songs you queue will show up here."))
                                .padding(.top, 60)
                        }
                    }
                    .padding(.horizontal, 24)
                    .padding(.bottom, 24)
                }
            }
        }
    }

    private func sectionHeader(_ title: String) -> some View {
        RoomSectionLabel(title).padding(.horizontal, 8)
    }
}
