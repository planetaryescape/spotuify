import SwiftUI
import SpotuifyKit

/// Sort orders offered by `TrackListView`'s sort menu. The raw value is the
/// label shown in the picker.
enum TrackSort: String, CaseIterable, Identifiable {
    case original = "Default"
    case title = "Title"
    case artist = "Artist"
    case album = "Album"
    case duration = "Duration"
    case dateAdded = "Date Added"
    var id: String { rawValue }
}

/// A reusable filterable + sortable track/episode collection. Used by Liked
/// Songs, album/playlist detail, and podcast episodes. Renders as a list of
/// `MediaRow`s or a grid of `TrackCard`s via the shared `LayoutToggle` (the
/// same switch every collection page uses). The header (Play/Shuffle/Queue
/// actions) is supplied by the caller.
struct TrackListView<Header: View>: View {
    let tracks: [MediaItem]
    var detailed: Bool
    var sortOptions: [TrackSort]
    let header: () -> Header

    @State private var filter = ""
    @State private var sort: TrackSort = .original
    @CollectionLayoutStorage private var layout: CollectionLayout

    init(
        tracks: [MediaItem],
        detailed: Bool = true,
        sortOptions: [TrackSort] = TrackSort.allCases,
        storageKey: String = "trackListLayout",
        @ViewBuilder header: @escaping () -> Header
    ) {
        self.tracks = tracks
        self.detailed = detailed
        self.sortOptions = sortOptions
        self.header = header
        // Tracks default to a list; the grid (cards) is opt-in per surface.
        _layout = CollectionLayoutStorage(storageKey, default: .list)
    }

    private let gridColumns = [GridItem(.adaptive(minimum: 150, maximum: 200), spacing: 16)]

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
                LayoutToggle(layout: $layout)
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
            content
        }
    }

    @ViewBuilder
    private var content: some View {
        if visible.isEmpty {
            ContentUnavailableView("Nothing here", systemImage: "music.note",
                description: Text(filter.isEmpty ? "No items." : "No matches for \u{201c}\(filter)\u{201d}."))
        } else if layout == .grid {
            ScrollView {
                LazyVGrid(columns: gridColumns, spacing: 16) {
                    ForEach(Array(visible.enumerated()), id: \.offset) { _, item in
                        TrackCard(item: item)
                    }
                }
                .padding(16)
            }
        } else {
            ScrollView {
                LazyVStack(spacing: 0, pinnedViews: .sectionHeaders) {
                    Section {
                        ForEach(Array(visible.enumerated()), id: \.offset) { _, item in
                            MediaRow(item: item, detailed: detailed)
                        }
                    } header: {
                        if detailed {
                            TrackTableHeader().background(.bar)
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 10)
                .padding(.bottom, 10)
            }
        }
    }
}

/// Column header row matching `MediaRow`'s detailed layout. Uses the same
/// `trackColumns` cell layout as `MediaRow`, so header labels and row values
/// share one width source and cannot drift.
struct TrackTableHeader: View {
    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: TrackColumnLayout.spacing) {
                Color.clear.frame(width: Theme.TrackColumn.artwork, height: 1)
                Text("Title")
                    .frame(maxWidth: .infinity, alignment: .leading)
                Text("Album")
                    .frame(width: Theme.TrackColumn.album, alignment: .leading)
                Text("Date Added")
                    .frame(width: Theme.TrackColumn.dateAdded, alignment: .leading)
                Color.clear.frame(width: Theme.TrackColumn.actions, height: 1)
                Text("Duration")
                    .frame(width: Theme.TrackColumn.duration, alignment: .leading)
            }
            .font(.caption2)
            .foregroundStyle(.tertiary)
            .padding(.horizontal, TrackColumnLayout.horizontalPadding)
            .padding(.vertical, 4)
            Divider()
        }
    }
}

/// Single source of truth for the track-table column geometry. Both
/// `TrackTableHeader` and `MediaRow` read these so columns can never drift.
enum TrackColumnLayout {
    static let spacing: CGFloat = 10
    static let horizontalPadding: CGFloat = 8
}

/// Convenience initialisers for a header-less `TrackListView`.
extension TrackListView where Header == EmptyView {
    init(
        tracks: [MediaItem],
        detailed: Bool = true,
        sortOptions: [TrackSort] = TrackSort.allCases,
        storageKey: String = "trackListLayout"
    ) {
        self.init(
            tracks: tracks, detailed: detailed, sortOptions: sortOptions,
            storageKey: storageKey, header: { EmptyView() })
    }
}

/// Big-art card for a track/episode in grid mode: tap to play, hover lifts the
/// cover and reveals a play badge, right-click for the shared action menu. The
/// track-list counterpart to `ArtworkTile` (which navigates).
struct TrackCard: View {
    @Environment(AppModel.self) private var model
    let item: MediaItem
    @State private var hovering = false
    @State private var showReminderPicker = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ZStack(alignment: .bottomTrailing) {
                AsyncCoverImage(url: item.imageURL, cornerRadius: Theme.tileCornerRadius)
                    .aspectRatio(1, contentMode: .fit)
                    .shadow(color: .black.opacity(hovering ? 0.4 : 0.22),
                            radius: hovering ? 18 : 8, y: hovering ? 10 : 4)
                Image(systemName: "play.circle.fill")
                    .font(.largeTitle)
                    .foregroundStyle(.white, .tint)
                    .padding(8)
                    .shadow(radius: 4)
                    .opacity(hovering ? 1 : 0)
            }
            .scaleEffect(hovering ? 1.03 : 1)
            Text(item.name)
                .font(.system(size: 13, weight: .semibold))
                .lineLimit(1)
            if !item.subtitle.isEmpty {
                Text(item.subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1)
            }
        }
        .padding(6)
        .contentShape(Rectangle())
        .onTapGesture { model.play(uri: item.uri) }
        .onHover { hovering = $0 }
        .animation(.spring(response: 0.3, dampingFraction: 0.7), value: hovering)
        .contextMenu {
            MediaItemMenu(item: item, onRemind: { showReminderPicker = true })
        }
        .sheet(isPresented: $showReminderPicker) {
            ReminderPickerView(item: item)
        }
    }
}
