import Foundation
import Observation

/// Holds saved bookmarks, refreshing on `BookmarksChanged`. The daemon owns
/// the truth; this store renders it.
@MainActor
@Observable
public final class BookmarksStore {
    public private(set) var bookmarks: [Bookmark] = []
    public private(set) var loading = false

    private weak var model: AppModel?

    public init() {}

    func connect(_ model: AppModel) {
        self.model = model
        model.addEventObserver { [weak self] event in
            guard let self else { return }
            if case .bookmarksChanged = event {
                Task { await self.load(force: true) }
            }
        }
    }

    public func load(force: Bool = false) async {
        guard let model else { return }
        if !force && !bookmarks.isEmpty { return }
        loading = true
        defer { loading = false }
        if case .bookmarks(let result) = try? await model.request(
            .bookmarksList(uri: nil), timeout: .seconds(20)) {
            bookmarks = result
        }
    }

    /// Bookmarks on one item, in position order (chapters-style).
    public func bookmarks(for mediaURI: String) -> [Bookmark] {
        bookmarks.filter { $0.mediaURI == mediaURI }.sorted { $0.positionMs < $1.positionMs }
    }
}
