import SwiftUI
import SpotuifyKit

/// The Bookmarks page: saved positions grouped by item, newest item first.
/// Play jumps straight to the saved position; the note is edited inline.
struct BookmarksView: View {
    @Environment(AppModel.self) private var model

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
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 8, pinnedViews: [.sectionHeaders]) {
                if groups.isEmpty {
                    Label("No bookmarks yet — press the bookmark button while listening", systemImage: "bookmark")
                        .foregroundStyle(.secondary).font(.callout)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.vertical, 12)
                }
                ForEach(groups, id: \.uri) { group in
                    Section {
                        ForEach(group.items) { BookmarkRow(bookmark: $0) }
                    } header: {
                        HStack(spacing: 10) {
                            AsyncCoverImage(url: group.imageURL, cornerRadius: 6)
                                .frame(width: 36, height: 36)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(group.title).font(.headline).lineLimit(1)
                                Text(group.subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                            }
                            Spacer()
                        }
                        .padding(.vertical, 6)
                        .background(.background)
                    }
                }
            }
            .padding(16)
        }
        .background(.background)
        .navigationTitle("Bookmarks")
        .task { await model.bookmarks.load() }
    }
}

/// One saved position: play, edit the note in place, delete.
struct BookmarkRow: View {
    @Environment(AppModel.self) private var model
    let bookmark: Bookmark
    @State private var draft = ""
    @State private var editing = false

    var body: some View {
        HStack(spacing: 10) {
            Button { model.playBookmark(id: bookmark.id) } label: {
                Image(systemName: "play.circle.fill").font(.title3)
            }.buttonStyle(.plain).help("Play from \(bookmark.positionLabel)")
            Text(bookmark.positionLabel)
                .font(.system(size: 13, weight: .medium).monospacedDigit())
                .frame(width: 64, alignment: .leading)
            if editing {
                TextField("Note", text: $draft)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit(commit)
                    .onExitCommand { editing = false }
            } else {
                Text(bookmark.note ?? "Add a note…")
                    .font(.callout)
                    .foregroundStyle(bookmark.note == nil ? .tertiary : .primary)
                    .lineLimit(2)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
                    .onTapGesture { draft = bookmark.note ?? ""; editing = true }
            }
            Spacer(minLength: 8)
            Text(bookmark.createdDate, style: .date)
                .font(.caption2).foregroundStyle(.tertiary)
            Button { model.deleteBookmark(id: bookmark.id) } label: {
                Image(systemName: "trash")
            }.buttonStyle(.plain).foregroundStyle(.secondary).help("Delete bookmark")
        }
        .padding(.vertical, 4).padding(.horizontal, 8)
        .background(RoundedRectangle(cornerRadius: Theme.rowRadius).fill(.primary.opacity(0.04)))
    }

    private func commit() {
        editing = false
        model.updateBookmarkNote(id: bookmark.id, note: draft)
    }
}
