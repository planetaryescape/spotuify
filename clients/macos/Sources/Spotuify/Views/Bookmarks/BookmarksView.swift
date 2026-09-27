import SwiftUI
import SpotuifyKit

/// The Bookmarks page: saved positions grouped by item, newest item first.
/// Play jumps straight to the saved position; the note is edited inline.
struct BookmarksView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.room) private var room

    /// Groups preserve the store's newest-first order by first appearance.
    private var groups: [(uri: String, title: String, subtitle: String, imageURL: String?, items: [Bookmark])] {
        var order: [String] = []
        var byURI: [String: [Bookmark]] = [:]
        for bookmark in model.bookmarks.bookmarks {
            if byURI[bookmark.mediaURI] == nil { order.append(bookmark.mediaURI) }
            byURI[bookmark.mediaURI, default: []].append(bookmark)
        }
        return order.compactMap { uri in
            guard let items = byURI[uri], let first = items.first else { return nil }
            return (uri, first.name, first.subtitle, first.imageURL,
                    items.sorted { $0.positionMs < $1.positionMs })
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            EditorialPageHeader("Bookmarks", eyebrow: "You")
            if groups.isEmpty {
                EmptyState(
                    "No bookmarks yet", systemImage: "bookmark",
                    description: Text("Press the bookmark button while listening to save the moment."))
            } else {
                list
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .navigationTitle("Bookmarks")
        .task { await model.bookmarks.load() }
    }

    private var list: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 6, pinnedViews: [.sectionHeaders]) {
                ForEach(groups, id: \.uri) { group in
                    Section {
                        ForEach(group.items) { BookmarkRow(bookmark: $0) }
                    } header: {
                        HStack(spacing: 12) {
                            AsyncCoverImage(url: group.imageURL, cornerRadius: 6)
                                .frame(width: 40, height: 40)
                            VStack(alignment: .leading, spacing: 3) {
                                Text(group.title).font(.displayTitle(17)).foregroundStyle(room.ink).lineLimit(1)
                                Text(group.subtitle).font(.system(size: 12, weight: .medium))
                                    .foregroundStyle(room.inkMuted).lineLimit(1)
                            }
                            Spacer()
                            MonoCaps("\(group.items.count) \(group.items.count == 1 ? "moment" : "moments")", size: 9.5)
                        }
                        .padding(.top, 14).padding(.bottom, 6)
                        .roomPinnedBackground()
                    }
                }
            }
            .padding(.horizontal, 24).padding(.bottom, 24)
        }
    }
}

/// One saved position: play, edit the note in place, delete.
struct BookmarkRow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.room) private var room
    let bookmark: Bookmark
    @State private var draft = ""
    @State private var editing = false

    var body: some View {
        HStack(spacing: 10) {
            Button { model.playBookmark(id: bookmark.id) } label: {
                Image(systemName: "play.fill").font(.system(size: 10, weight: .bold))
                    .foregroundStyle(room.base)
                    .frame(width: 26, height: 26)
                    .background(Circle().fill(room.accent))
            }.buttonStyle(PressableButtonStyle()).help("Play from \(bookmark.positionLabel)")
            Text(bookmark.positionLabel)
                .font(.mono(12.5, weight: .semibold))
                .foregroundStyle(room.ink)
                .frame(width: 64, alignment: .leading)
            if editing {
                TextField("Note", text: $draft)
                    .glassField()
                    .onSubmit(commit)
                    .onExitCommand { editing = false }
            } else {
                Text(bookmark.note ?? "Add a note…")
                    .font(bookmark.note == nil ? .callout : .displayAccent(15))
                    .foregroundStyle(bookmark.note == nil ? room.inkFaint : room.ink)
                    .lineLimit(2)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
                    .onTapGesture { draft = bookmark.note ?? ""; editing = true }
            }
            Spacer(minLength: 8)
            Text(bookmark.createdDate, style: .date)
                .font(.mono(10)).foregroundStyle(room.inkFaint)
            Button { model.deleteBookmark(id: bookmark.id) } label: {
                Image(systemName: "trash")
            }.buttonStyle(.plain).foregroundStyle(room.inkMuted).help("Delete bookmark")
        }
        .padding(.vertical, 6).padding(.horizontal, 10)
        .background(RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous).fill(room.ink.opacity(0.04)))
    }

    private func commit() {
        editing = false
        model.updateBookmarkNote(id: bookmark.id, note: draft)
    }
}
