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
    var fallbackImageURL: String?
    /// Optional lazy-pagination hook. Fired when the user scrolls near the end
    /// of the list; only the Liked Songs call site supplies it. `nil` (every
    /// other call site) keeps the shared view unchanged — no pagination.
    var onReachEnd: (() -> Void)?
    /// Collection context the rows play inside of (album/playlist URI, or
    /// ``AppModel/likedContext``). `nil` keeps single-track play behaviour.
    var contextURI: String?
    /// The collection's full size when only some pages are loaded (Liked
    /// Songs), so the count reads "50 of 693" instead of contradicting the header.
    var totalCount: Int?
    let header: () -> Header

    @State private var filter = ""
    @State private var sort: TrackSort = .original
    @State private var columns: TrackTableColumns = .wide
    @CollectionLayoutStorage private var layout: CollectionLayout

    init(
        tracks: [MediaItem],
        detailed: Bool = true,
        sortOptions: [TrackSort] = TrackSort.allCases,
        storageKey: String = "trackListLayout",
        fallbackImageURL: String? = nil,
        onReachEnd: (() -> Void)? = nil,
        contextURI: String? = nil,
        totalCount: Int? = nil,
        @ViewBuilder header: @escaping () -> Header
    ) {
        self.tracks = tracks
        self.detailed = detailed
        self.sortOptions = sortOptions
        self.fallbackImageURL = fallbackImageURL
        self.onReachEnd = onReachEnd
        self.contextURI = contextURI
        self.totalCount = totalCount
        self.header = header
        // Tracks default to a list; the grid (cards) is opt-in per surface.
        _layout = CollectionLayoutStorage(storageKey, default: .list)
    }

    // NOTE: filter/sort run in memory over the currently-loaded pages. With
    // lazy pagination that means an active sort/filter only reorders what's
    // loaded so far; `onReachEnd` still appends server-order pages, which then
    // fold into the in-memory sort. Acceptable for v1 — Liked Songs' default
    // order (date-added desc) already matches the server order.
    private func maybeLoadMore(at index: Int) {
        guard let onReachEnd, index >= visible.count - 10 else { return }
        onReachEnd()
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

    private var countLabel: String {
        if filter.isEmpty, let totalCount, totalCount > visible.count {
            return "\(visible.count) of \(totalCount) loaded"
        }
        return "\(visible.count) \(visible.count == 1 ? "item" : "items")"
    }

    var body: some View {
        VStack(spacing: 0) {
            header()
            HStack(spacing: 14) {
                HStack(spacing: 8) {
                    Image(systemName: "magnifyingglass")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(.secondary)
                    TextField("Filter", text: $filter)
                        .frame(maxWidth: 240)
                }
                .glassField()
                Spacer()
                MonoCaps(countLabel, size: 9.5)
                RoomMenuPicker(label: "Sort", options: sortOptions.map { (value: $0, title: $0.rawValue) }, selection: $sort)
                LayoutToggle(layout: $layout)
            }
            .padding(.horizontal, 32).padding(.vertical, 10)
            content
        }
    }

    @ViewBuilder
    private var content: some View {
        if visible.isEmpty {
            EmptyState("Nothing here", systemImage: "music.note",
                description: Text(filter.isEmpty ? "No items." : "No matches for \u{201c}\(filter)\u{201d}."))
        } else if layout == .grid {
            ScrollView {
                LazyVGrid(columns: gridColumns, spacing: 16) {
                    ForEach(Array(visible.enumerated()), id: \.offset) { index, item in
                        TrackCard(item: item, fallbackImageURL: fallbackImageURL, contextURI: contextURI)
                            .onAppear { maybeLoadMore(at: index) }
                    }
                }
                .padding(16)
            }
        } else {
            ScrollView {
                LazyVStack(spacing: 1, pinnedViews: .sectionHeaders) {
                    Section {
                        ForEach(Array(visible.enumerated()), id: \.offset) { index, item in
                            MediaRow(
                                item: item, detailed: detailed, fallbackImageURL: fallbackImageURL,
                                contextURI: contextURI, index: index + 1)
                                .onAppear { maybeLoadMore(at: index) }
                        }
                    } header: {
                        if detailed {
                            TrackTableHeader().roomPinnedBackground()
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 24)
                .padding(.bottom, 24)
            }
            .environment(\.trackColumns, columns)
            .onGeometryChange(for: TrackTableColumns.self) { proxy in
                TrackTableColumns.fitting(proxy.size.width - 48 - 26 - 12)
            } action: { columns = $0 }
        }
    }
}

/// Column header row matching `MediaRow`'s detailed layout. Both read the
/// same `TrackTableColumns` from the environment, so header labels and row
/// values share one width source and cannot drift.
struct TrackTableHeader: View {
    @Environment(\.trackColumns) private var columns
    @Environment(\.room) private var room

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: TrackColumnLayout.spacing) {
                Text("#").frame(width: 26)
                Color.clear.frame(width: Theme.TrackColumn.artwork, height: 1)
                Text("Title")
                    .frame(maxWidth: .infinity, alignment: .leading)
                if columns.showsAlbum {
                    Text("Album")
                        .frame(width: columns.albumWidth, alignment: .leading)
                }
                if columns.showsDateAdded {
                    Text("Added")
                        .frame(width: Theme.TrackColumn.dateAdded, alignment: .leading)
                }
                Color.clear.frame(width: Theme.TrackColumn.actions, height: 1)
                Image(systemName: "clock")
                    .frame(width: Theme.TrackColumn.duration, alignment: .leading)
                    .accessibilityLabel("Duration")
            }
            .font(.mono(9.5, weight: .semibold))
            .textCase(.uppercase)
            .tracking(1.2)
            .foregroundStyle(room.inkFaint)
            .padding(.horizontal, TrackColumnLayout.horizontalPadding)
            .padding(.vertical, 6)
            Rectangle().fill(room.hairline).frame(height: 1)
        }
    }
}

/// Single source of truth for the track-table column geometry. Both
/// `TrackTableHeader` and `MediaRow` read these so columns can never drift.
enum TrackColumnLayout {
    static let spacing: CGFloat = 12
    static let horizontalPadding: CGFloat = 8
}

/// Which optional columns a track table can afford at its current width. The
/// title always gets first claim on space; album and date-added drop out as
/// the table narrows instead of squeezing the title to "Never Too…".
struct TrackTableColumns: Equatable {
    var showsAlbum: Bool
    var showsDateAdded: Bool
    var albumWidth: CGFloat

    static func fitting(_ width: CGFloat) -> TrackTableColumns {
        TrackTableColumns(
            showsAlbum: width >= 560,
            showsDateAdded: width >= 860,
            albumWidth: min(300, max(150, width * 0.26)))
    }

    static let wide = fitting(1000)
}

extension EnvironmentValues {
    @Entry var trackColumns: TrackTableColumns = .wide
}

/// Convenience initialisers for a header-less `TrackListView`.
extension TrackListView where Header == EmptyView {
    init(
        tracks: [MediaItem],
        detailed: Bool = true,
        sortOptions: [TrackSort] = TrackSort.allCases,
        storageKey: String = "trackListLayout",
        fallbackImageURL: String? = nil,
        contextURI: String? = nil
    ) {
        self.init(
            tracks: tracks, detailed: detailed, sortOptions: sortOptions,
            storageKey: storageKey, fallbackImageURL: fallbackImageURL,
            contextURI: contextURI, header: { EmptyView() })
    }
}

/// Big-art card for a track/episode in grid mode: tap to play, hover lifts the
/// cover and reveals a play badge, right-click for the shared action menu. The
/// track-list counterpart to `ArtworkTile` (which navigates).
struct TrackCard: View {
    @Environment(AppModel.self) private var model
    let item: MediaItem
    var fallbackImageURL: String?
    var contextURI: String?
    @State private var hovering = false
    @State private var showReminderPicker = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ZStack(alignment: .bottomTrailing) {
                AsyncCoverImage(url: item.imageURL ?? fallbackImageURL, cornerRadius: Theme.tileCornerRadius)
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
        .onTapGesture { model.play(uri: item.uri, contextURI: contextURI) }
        .onHover { hovering = $0 }
        .animation(.spring(response: 0.3, dampingFraction: 0.7), value: hovering)
        .contextMenu {
            MediaItemMenu(item: item, contextURI: contextURI, onRemind: { showReminderPicker = true })
        }
        .sheet(isPresented: $showReminderPicker) {
            ReminderPickerView(item: item)
        }
    }
}
