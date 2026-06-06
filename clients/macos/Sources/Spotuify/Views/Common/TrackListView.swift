import SwiftUI
import SpotuifyKit

enum TrackSort: String, CaseIterable, Identifiable {
    case original = "Default"
    case title = "Title"
    case artist = "Artist"
    case album = "Album"
    case duration = "Duration"
    case dateAdded = "Date Added"
    var id: String { rawValue }
}

/// A reusable filterable + sortable track/episode list. Used by Liked Songs,
/// album/playlist detail, and podcast episodes. The header (Play/Shuffle/Queue
/// actions) is supplied by the caller.
struct TrackListView<Header: View>: View {
    let tracks: [MediaItem]
    var detailed = true
    var sortOptions: [TrackSort] = TrackSort.allCases
    @ViewBuilder var header: () -> Header

    @State private var filter = ""
    @State private var sort: TrackSort = .original

    private var visible: [MediaItem] {
        var result = tracks
        let needle = filter.trimmingCharacters(in: .whitespaces).lowercased()
        if !needle.isEmpty {
            result = result.filter {
                $0.name.lowercased().contains(needle)
                    || $0.subtitle.lowercased().contains(needle)
                    || ($0.albumLabel?.lowercased().contains(needle) ?? false)
            }
        }
        switch sort {
        case .original: break
        case .title: result.sort { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
        case .artist: result.sort { $0.subtitle.localizedCaseInsensitiveCompare($1.subtitle) == .orderedAscending }
        case .album: result.sort { ($0.albumLabel ?? "").localizedCaseInsensitiveCompare($1.albumLabel ?? "") == .orderedAscending }
        case .duration: result.sort { $0.durationMs < $1.durationMs }
        case .dateAdded: result.sort { ($0.addedAtMs ?? 0) > ($1.addedAtMs ?? 0) }
        }
        return result
    }

    var body: some View {
        VStack(spacing: 0) {
            header()
            HStack(spacing: 10) {
                HStack(spacing: 6) {
                    Image(systemName: "line.3.horizontal.decrease.circle").foregroundStyle(.secondary)
                    TextField("Filter", text: $filter)
                        .textFieldStyle(.plain)
                        .frame(maxWidth: 260)
                }
                .glassField()
                Spacer()
                Picker("Sort", selection: $sort) {
                    ForEach(sortOptions) { Text($0.rawValue).tag($0) }
                }
                .pickerStyle(.menu)
                .fixedSize()
                .labelsHidden()
                Text("\(visible.count)")
                    .font(.caption).foregroundStyle(.secondary)
            }
            .padding(.horizontal, 16).padding(.vertical, 8)
            Divider()
            if visible.isEmpty {
                ContentUnavailableView("Nothing here", systemImage: "music.note",
                    description: Text(filter.isEmpty ? "No items." : "No matches for \u{201c}\(filter)\u{201d}."))
            } else {
                ScrollView {
                    LazyVStack(spacing: 2) {
                        ForEach(Array(visible.enumerated()), id: \.offset) { _, item in
                            MediaRow(item: item, detailed: detailed)
                        }
                    }
                    .padding(10)
                }
            }
        }
    }
}

extension TrackListView where Header == EmptyView {
    init(tracks: [MediaItem], detailed: Bool = true, sortOptions: [TrackSort] = TrackSort.allCases) {
        self.init(tracks: tracks, detailed: detailed, sortOptions: sortOptions, header: { EmptyView() })
    }
}
