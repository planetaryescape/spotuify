import Foundation
import Observation

/// Catalog search backed by the daemon's one-shot `search` request. Results
/// arrive as a flat list and are grouped by kind for display. A debounce keeps
/// keystroke-driven searches from flooding the daemon.
@MainActor
@Observable
public final class SearchStore {
    public var query: String = ""
    public private(set) var results: [MediaItem] = []
    public private(set) var isSearching = false
    public private(set) var errorMessage: String?

    private weak var model: AppModel?
    private var searchTask: Task<Void, Never>?

    public init() {}

    func connect(_ model: AppModel) { self.model = model }

    /// Group results by kind in a sensible display order.
    public var grouped: [(kind: MediaKind, items: [MediaItem])] {
        let order: [MediaKind] = [.track, .artist, .album, .playlist, .show, .episode]
        return order.compactMap { kind in
            let items = results.filter { $0.kind == kind }
            return items.isEmpty ? nil : (kind, items)
        }
    }

    /// Run a search now (e.g. on submit).
    public func runSearch() {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        searchTask?.cancel()
        guard !trimmed.isEmpty else {
            results = []; isSearching = false; errorMessage = nil
            return
        }
        isSearching = true
        errorMessage = nil
        searchTask = Task { [weak self] in
            await self?.perform(query: trimmed)
        }
    }

    /// Debounced search for live-as-you-type.
    public func scheduleSearch() {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        searchTask?.cancel()
        guard !trimmed.isEmpty else {
            results = []; isSearching = false; errorMessage = nil
            return
        }
        isSearching = true
        searchTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(350))
            guard !Task.isCancelled else { return }
            await self?.perform(query: trimmed)
        }
    }

    private func perform(query: String) async {
        guard let model else { return }
        do {
            let data = try await model.request(
                .search(query: query, scope: .all, source: .spotify, limit: 40),
                timeout: .seconds(15))
            guard !Task.isCancelled else { return }
            if case .searchResults(let items) = data {
                results = items
            } else {
                results = []
            }
            errorMessage = nil
        } catch {
            if !Task.isCancelled {
                errorMessage = "Search failed"
                results = []
            }
        }
        isSearching = false
    }
}
