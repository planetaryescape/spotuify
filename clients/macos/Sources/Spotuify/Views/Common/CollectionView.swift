import SwiftUI
import SpotuifyKit

enum CollectionLayout: String { case grid, list }

/// The one and only list/grid switch. Every collection surface — navigable
/// grids (`CollectionView`) and track lists (`TrackListView`) — uses this, so
/// the toggle is defined once instead of re-hand-rolled per page.
struct LayoutToggle: View {
    @Binding var layout: CollectionLayout
    @Environment(\.room) private var room

    var body: some View {
        HStack(spacing: 2) {
            option(.list, icon: "list.bullet", label: "List")
            option(.grid, icon: "square.grid.2x2", label: "Grid")
        }
        .padding(2)
        .background(Capsule().fill(room.ink.opacity(0.05)))
        .overlay(Capsule().strokeBorder(room.ink.opacity(0.1)))
    }

    private func option(_ value: CollectionLayout, icon: String, label: String) -> some View {
        let active = layout == value
        return Button { layout = value } label: {
            Image(systemName: icon)
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(active ? room.base : room.inkMuted)
                .frame(width: 28, height: 22)
                .background(Capsule().fill(active ? room.ink : .clear))
                .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .help(label)
        .accessibilityLabel(label)
        .accessibilityAddTraits(active ? [.isSelected] : [])
    }
}

/// Backs a per-surface `CollectionLayout` in `@AppStorage` from a string key.
/// Use as `@CollectionLayoutStorage("albumsLayout") var layout` so callers get
/// a persisted list/grid choice without re-deriving the AppStorage plumbing.
@propertyWrapper
struct CollectionLayoutStorage: DynamicProperty {
    @AppStorage private var raw: String
    init(_ key: String, default defaultLayout: CollectionLayout = .grid) {
        _raw = AppStorage(wrappedValue: defaultLayout.rawValue, key)
    }
    var wrappedValue: CollectionLayout {
        get { CollectionLayout(rawValue: raw) ?? .grid }
        nonmutating set { raw = newValue.rawValue }
    }
    var projectedValue: Binding<CollectionLayout> {
        Binding(get: { wrappedValue }, set: { wrappedValue = $0 })
    }
}

/// A collection of navigable media (albums/artists/shows/playlists) rendered as
/// either a card grid or a compact list, with a per-surface toggle persisted in
/// `@AppStorage`. Either way the cover art stays the visual anchor. Each item is
/// a `NavigationLink` into its detail page, so the host must sit in a
/// `NavigationStack` with `.mediaDetailDestinations()`.
struct CollectionView: View {
    let items: [MediaItem]
    private var minTile: CGFloat
    private var maxTile: CGFloat
    @CollectionLayoutStorage private var layout: CollectionLayout

    init(
        items: [MediaItem],
        storageKey: String,
        defaultLayout: CollectionLayout = .grid,
        minTile: CGFloat = 168,
        maxTile: CGFloat = 220
    ) {
        self.items = items
        self.minTile = minTile
        self.maxTile = maxTile
        _layout = CollectionLayoutStorage(storageKey, default: defaultLayout)
    }

    private var columns: [GridItem] {
        [GridItem(.adaptive(minimum: minTile, maximum: maxTile), spacing: 24)]
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                MonoCaps("\(items.count) \(items.count == 1 ? "item" : "items")", size: 9.5)
                Spacer()
                LayoutToggle(layout: $layout)
            }
            .padding(.horizontal, 32).padding(.bottom, 6)
            ScrollView {
                if layout == .grid {
                    LazyVGrid(columns: columns, spacing: 24) {
                        ForEach(items) { item in
                            NavigationLink(value: item) { ArtworkTile(item: item) }
                                .buttonStyle(.plain)
                        }
                    }
                    .padding(.horizontal, 32).padding(.vertical, 12)
                } else {
                    LazyVStack(spacing: 2) {
                        ForEach(items) { item in
                            NavigationLink(value: item) { CollectionRow(item: item) }
                                .buttonStyle(.plain)
                        }
                    }
                    .padding(.horizontal, 24).padding(.vertical, 8)
                }
            }
        }
    }
}

/// Compact list row for a navigable collection item (album/artist/show). Unlike
/// `MediaRow` (built for tracks, plays on click) this one navigates on click and
/// carries the shared `⋯` action menu.
struct CollectionRow: View {
    let item: MediaItem
    @State private var hovering = false

    var body: some View {
        HStack(spacing: 12) {
            AsyncCoverImage(url: item.imageURL, cornerRadius: item.kind == .artist ? 0 : 6)
                .circularArtwork(item.kind == .artist)
                .frame(width: 48, height: 48)
            VStack(alignment: .leading, spacing: 2) {
                Text(item.name).font(.system(size: 14, weight: .medium)).lineLimit(1)
                if !item.subtitle.isEmpty {
                    Text(item.subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            Spacer(minLength: 12)
            // Trailing meta ("N tracks", "2021 · 12 songs") right-aligned in a
            // fixed column so counts line up vertically down the list. A single
            // leading Spacer anchors it to the right; a second (trailing) one
            // would let it float to a different x on each row (ragged columns).
            if let meta = item.metaLine {
                Text(meta)
                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    .frame(width: 120, alignment: .trailing)
            }
            Menu {
                MediaItemMenu(item: item)
            } label: {
                Image(systemName: "ellipsis")
            }
            .menuStyle(.borderlessButton).menuIndicator(.hidden).fixedSize()
            .opacity(hovering ? 1 : 0.35)
            Image(systemName: "chevron.right").font(.caption).foregroundStyle(.tertiary)
        }
        .padding(.vertical, 4).padding(.horizontal, 8)
        .background {
            RoundedRectangle(cornerRadius: Theme.rowRadius)
                .fill(hovering ? AnyShapeStyle(.primary.opacity(0.06)) : AnyShapeStyle(.clear))
        }
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
    }
}
