import Foundation
import Observation

/// Backs the Podcasts page: a Shows ⇄ Episodes mode toggle, a search box that
/// either filters the followed list (Library) or queries Spotify, and (in
/// Episodes mode) a cross-show, date-ordered feed with a sort control.
@MainActor
@Observable
public final class PodcastsStore {
    public enum Mode: String, CaseIterable, Sendable { case shows, episodes }

    /// Shows (followed podcasts) vs Episodes (cross-show date feed).
    public var mode: Mode = .shows
    /// Filter/search text.
    public var query: String = ""
    /// `.local` filters the followed list; `.spotify` searches the catalog.
    public var source: SearchSource = .local
    /// Episode feed ordering.
    public var episodeSort: EpisodeSort = .newest

    /// The merged cross-show episode feed (Library mode).
    public private(set) var episodeFeed: [MediaItem] = []
    /// Spotify search results (Spotify mode), already kind-filtered for display.
    public private(set) var spotifyResults: [MediaItem] = []
    public private(set) var loadingEpisodes = false
    public private(set) var searching = false

    private weak var model: AppModel?
    private var searchTask: Task<Void, Never>?

    public init() {}
    func connect(_ model: AppModel) { self.model = model }

    /// Followed shows for the current view: the library's saved shows, filtered
    /// by `query` in Library mode, or the Spotify show results in Spotify mode.
    public func shows(libraryShows: [MediaItem]) -> [MediaItem] {
        switch source {
        case .spotify:
            return spotifyResults.filter { $0.kind == .show }
        default:
            return filterByQuery(libraryShows)
        }
    }

    /// Episodes for the current view: the cross-show feed filtered by `query` in
    /// Library mode, or the Spotify episode results (date-sorted) in Spotify mode.
    public var episodes: [MediaItem] {
        switch source {
        case .spotify:
            return spotifyResults.filter { $0.kind == .episode }
        default:
            return filterByQuery(episodeFeed)
        }
    }

    private func filterByQuery(_ items: [MediaItem]) -> [MediaItem] {
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !q.isEmpty else { return items }
        return items.filter {
            $0.name.lowercased().contains(q) || $0.subtitle.lowercased().contains(q)
        }
    }

    /// Load (or refresh) the cross-show episode feed for the current sort.
    public func loadEpisodes(refresh: Bool = false) async {
        guard let model else { return }
        loadingEpisodes = true
        defer { loadingEpisodes = false }
        if case .mediaItems(let items)? = try? await model.request(
            .episodeFeed(limit: 200, sort: episodeSort, refresh: refresh), timeout: .seconds(25))
        {
            episodeFeed = items
        }
    }

    public func setMode(_ newMode: Mode) {
        mode = newMode
        if source == .spotify { runSearch() }
        else if newMode == .episodes && episodeFeed.isEmpty { Task { await loadEpisodes() } }
    }

    public func setSource(_ newSource: SearchSource) {
        source = newSource
        runSearch()
    }

    public func setEpisodeSort(_ newSort: EpisodeSort) {
        episodeSort = newSort
        // The daemon caches the merged set; a re-fetch just re-sorts cheaply.
        Task { await loadEpisodes() }
    }

    /// Debounced search. Only meaningful in Spotify mode; Library mode filters
    /// the already-loaded lists client-side via `shows`/`episodes`.
    public func scheduleSearch() {
        searchTask?.cancel()
        guard source == .spotify else {
            spotifyResults = []
            searching = false
            return
        }
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            spotifyResults = []
            searching = false
            return
        }
        searching = true
        searchTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(350))
            guard let self, !Task.isCancelled else { return }
            await self.perform(query: trimmed)
        }
    }

    public func runSearch() {
        searchTask?.cancel()
        guard source == .spotify else {
            spotifyResults = []
            searching = false
            return
        }
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            spotifyResults = []
            searching = false
            return
        }
        searching = true
        searchTask = Task { [weak self] in await self?.perform(query: trimmed) }
    }

    private func perform(query: String) async {
        guard let model else { return }
        let scope: SearchScope = mode == .shows ? .show : .episode
        let sort: SearchSort? = mode == .episodes ? .date : nil
        defer { searching = false }
        if case .searchResults(let items)? = try? await model.request(
            .search(query: query, scope: scope, source: .spotify, limit: 40, kinds: nil, sort: sort),
            timeout: .seconds(15))
        {
            guard !Task.isCancelled else { return }
            spotifyResults = items
        }
    }
}
