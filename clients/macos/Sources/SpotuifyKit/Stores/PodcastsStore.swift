import Foundation
import Observation

/// Backs the Podcasts page: a Shows ⇄ Episodes mode toggle, a search box that
/// either filters the followed list (Library) or queries the selected provider,
/// and (in Episodes mode) a cross-show, date-ordered feed with a sort control.
@MainActor
@Observable
public final class PodcastsStore {
    public enum Mode: String, CaseIterable, Sendable { case shows, episodes }

    public struct CatalogSourceOption: Identifiable, Sendable, Equatable {
        public let source: SearchSource
        public let label: String
        public var id: SearchSource { source }
    }

    /// Shows (followed podcasts) vs Episodes (cross-show date feed).
    public var mode: Mode = .shows
    /// Filter/search text.
    public var query: String = ""
    /// `.local` filters the followed list; `.remote` searches that provider's
    /// catalog.
    public var source: SearchSource = .local
    /// Episode feed ordering.
    public var episodeSort: EpisodeSort = .newest

    /// The merged cross-show episode feed (Library mode).
    public private(set) var episodeFeed: [MediaItem] = []
    /// Remote catalog results, already kind-filtered for display.
    public private(set) var catalogResults: [MediaItem] = []
    @available(*, deprecated, renamed: "catalogResults")
    public var spotifyResults: [MediaItem] { catalogResults }
    public private(set) var loadingEpisodes = false
    public private(set) var searching = false

    private weak var model: AppModel?
    private var searchTask: Task<Void, Never>?

    public init() {}
    func connect(_ model: AppModel) { self.model = model }

    /// Provider-backed sources the production picker can actually select.
    /// A nil catalog means a released daemon, so retain its Spotify option.
    /// A present catalog without a valid default is authoritative and exposes
    /// no remote option until the daemon configuration is repaired.
    public var catalogSourceOptions: [CatalogSourceOption] {
        guard let model else { return [] }
        guard let catalog = model.providerCatalog else {
            return [CatalogSourceOption(source: .spotify, label: "Spotify")]
        }
        guard catalog.defaultDescriptor != nil else { return [] }
        let kind: MediaKind = mode == .shows ? .show : .episode
        return catalog.providers.compactMap { provider in
            guard provider.capabilities.search.remote,
                  provider.capabilities.search.kinds.contains(kind)
            else { return nil }
            return CatalogSourceOption(
                source: .remote(provider.id), label: provider.displayName)
        }
    }

    /// Current explicit provider when eligible, otherwise the eligible default.
    public var selectedCatalogSource: SearchSource? {
        if case .remote = source,
           catalogSourceOptions.contains(where: { $0.source == source })
        {
            return source
        }
        if let provider = model?.providerCatalog?.defaultProvider {
            let defaultSource = SearchSource.remote(provider)
            if catalogSourceOptions.contains(where: { $0.source == defaultSource }) {
                return defaultSource
            }
            return nil
        }
        return model?.providerCatalog == nil ? catalogSourceOptions.first?.source : nil
    }

    public var selectedCatalogLabel: String {
        guard let selectedCatalogSource else { return "Catalog" }
        return catalogSourceOptions
            .first(where: { $0.source == selectedCatalogSource })?.label ?? "Catalog"
    }

    public var isCatalogSource: Bool {
        switch source {
        case .local:
            return false
        case .hybrid:
            return true
        case .remote:
            return catalogSourceOptions.contains(where: { $0.source == source })
        }
    }

    /// Followed shows for the current view: the library's saved shows, filtered
    /// by `query` in Library mode, or remote show results in catalog mode.
    public func shows(libraryShows: [MediaItem]) -> [MediaItem] {
        if isCatalogSource {
            return catalogResults.filter { $0.kind == .show }
        }
        return filterByQuery(libraryShows)
    }

    /// Episodes for the current view: the cross-show feed filtered by `query` in
    /// Library mode, or remote episode results (date-sorted) in catalog mode.
    public var episodes: [MediaItem] {
        if isCatalogSource {
            return catalogResults.filter { $0.kind == .episode }
        }
        return filterByQuery(episodeFeed)
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
        if case .remote = source, !isCatalogSource { source = .local }
        if isCatalogSource { runSearch() }
        else if newMode == .episodes && episodeFeed.isEmpty { Task { await loadEpisodes() } }
    }

    public func setSource(_ newSource: SearchSource) {
        if case .remote = newSource,
           !catalogSourceOptions.contains(where: { $0.source == newSource })
        {
            source = .local
        } else {
            source = newSource
        }
        runSearch()
    }

    public func setEpisodeSort(_ newSort: EpisodeSort) {
        episodeSort = newSort
        // The daemon caches the merged set; a re-fetch just re-sorts cheaply.
        Task { await loadEpisodes() }
    }

    /// Debounced search. Only meaningful in catalog mode; Library mode filters
    /// the already-loaded lists client-side via `shows`/`episodes`.
    public func scheduleSearch() {
        searchTask?.cancel()
        guard isCatalogSource else {
            catalogResults = []
            searching = false
            return
        }
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            catalogResults = []
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
        guard isCatalogSource else {
            catalogResults = []
            searching = false
            return
        }
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            catalogResults = []
            searching = false
            return
        }
        searching = true
        searchTask = Task { [weak self] in await self?.perform(query: trimmed) }
    }

    private func perform(query: String) async {
        defer { searching = false }
        guard let request = catalogSearchRequest(query: query), let model else { return }
        if case .searchResults(let items)? = try? await model.request(
            request, timeout: .seconds(15))
        {
            guard !Task.isCancelled else { return }
            catalogResults = items
        }
    }

    /// Kept separate from transport so tests can lock provider routing without
    /// opening a daemon socket.
    func catalogSearchRequest(query: String) -> DaemonRequest? {
        guard isCatalogSource, let model else { return nil }
        let scope: SearchScope = mode == .shows ? .show : .episode
        let kind: MediaKind = mode == .shows ? .show : .episode
        let sort: SearchSort? = mode == .episodes ? .date : nil
        guard model.canSearch(source: source, kinds: [kind]) else {
            catalogResults = []
            return nil
        }
        let provider: ProviderID?
        if case .remote(let selectedProvider) = source {
            provider = selectedProvider
        } else {
            provider = nil
        }
        return .search(
            query: query, scope: scope, source: source, limit: 40,
            provider: provider, kinds: nil, sort: sort)
    }
}
