import Foundation
import Observation

/// Catalog search backed by the daemon's one-shot `search` request. Results
/// arrive as a flat list and are grouped by kind for display. A debounce keeps
/// keystroke-driven searches from flooding the daemon.
@MainActor
@Observable
public final class SearchStore {
    public struct SourceOption: Identifiable, Sendable, Equatable {
        public let source: SearchSource
        public let label: String
        public var id: SearchSource { source }
    }

    public var query: String = ""
    public private(set) var results: [MediaItem] = []
    public private(set) var isSearching = false
    public private(set) var errorMessage: String?
    /// Active type filter. Empty = all kinds. The daemon restricts the fetch to
    /// these kinds; an empty set falls back to scope `.all`.
    public var typeFilter: Set<MediaKind> = []
    /// Result ordering. `.relevance` keeps the provider's order.
    public var sort: SearchSort = .relevance
    /// Requested search route. Until the user picks one, `selectedSource`
    /// follows the capable catalog default.
    public var source: SearchSource = .local

    private weak var model: AppModel?
    private var searchTask: Task<Void, Never>?
    private var sourceWasSelected = false

    public init() {}

    /// Search routes the connected daemon can actually serve. A missing
    /// catalog exposes only provider-neutral local search; a present catalog
    /// is authoritative for remote routes.
    public var sourceOptions: [SourceOption] {
        guard let catalog = model?.providerCatalog else {
            return [SourceOption(source: .local, label: "Library")]
        }
        let remote = catalog.providers.compactMap { provider -> SourceOption? in
            guard provider.capabilities.search.remote,
                  !provider.capabilities.search.kinds.isEmpty
            else { return nil }
            return SourceOption(
                source: .remote(provider.id), label: provider.displayName)
        }
        return remote + [SourceOption(source: .local, label: "Library")]
    }

    /// Effective picker/request route. Preserve an explicit valid selection;
    /// otherwise follow the catalog default, falling back to local search.
    public var selectedSource: SearchSource {
        if sourceWasSelected,
           sourceOptions.contains(where: { $0.source == source })
        {
            return source
        }
        if let defaultProvider = model?.providerCatalog?.defaultProvider {
            let defaultSource = SearchSource.remote(defaultProvider)
            if sourceOptions.contains(where: { $0.source == defaultSource }) {
                return defaultSource
            }
        }
        return .local
    }

    private static let kindOrder: [MediaKind] = [
        .track, .artist, .album, .playlist, .show, .episode,
    ]

    /// Filter chips supported by the effective route. Local search keeps the
    /// full set; remote routes expose only the selected provider's kinds.
    public var filterableKinds: [MediaKind] {
        guard case .remote(let provider) = selectedSource else {
            return Self.kindOrder
        }
        guard let supported = model?.providerCatalog?.providers
            .first(where: { $0.id == provider })?.capabilities.search.kinds
        else { return [] }
        let supportedSet = Set(supported)
        return Self.kindOrder.filter(supportedSet.contains)
    }

    /// Toggle a kind in the filter and re-run. Empty filter = all.
    public func toggleFilter(_ kind: MediaKind) {
        if typeFilter.contains(kind) {
            typeFilter.remove(kind)
        } else {
            typeFilter.insert(kind)
        }
        runSearch()
    }

    /// Change the sort and re-run.
    public func setSort(_ newSort: SearchSort) {
        sort = newSort
        runSearch()
    }

    /// Switch between a provider catalog and the local library, and re-run.
    public func setSource(_ newSource: SearchSource) {
        source = newSource
        sourceWasSelected = true
        reconcileProviderCapabilities()
        runSearch()
    }

    /// Drop filters the newly selected/reseeded provider cannot serve.
    func reconcileProviderCapabilities() {
        typeFilter.formIntersection(Set(filterableKinds))
    }

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
        guard let model, let request = searchRequest(query: query) else {
            errorMessage = "Search is unavailable for this provider"
            results = []
            isSearching = false
            return
        }
        do {
            let data = try await model.request(request, timeout: .seconds(15))
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

    /// Kept separate from transport so tests can lock catalog-derived routing.
    func searchRequest(query: String) -> DaemonRequest? {
        guard let model else { return nil }
        let sortParam: SearchSort? = sort == .relevance ? nil : sort
        let source = selectedSource
        let supportedKinds = filterableKinds
        let kinds: [MediaKind]?
        if typeFilter.isEmpty {
            if case .remote = source {
                kinds = supportedKinds
            } else {
                kinds = nil
            }
        } else {
            kinds = supportedKinds.filter(typeFilter.contains)
        }
        guard model.canSearch(source: source, kinds: kinds) else { return nil }
        let provider: ProviderID?
        if case .remote(let remoteProvider) = source {
            provider = remoteProvider
        } else {
            provider = nil
        }
        return .search(
            query: query, scope: .all, source: source, limit: 40,
            provider: provider, kinds: kinds, sort: sortParam)
    }
}
