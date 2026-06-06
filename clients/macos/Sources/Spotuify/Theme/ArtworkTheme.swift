import SwiftUI

/// Tracks the role-based `ArtworkPalette` derived from the current artwork,
/// animated on change. `accent` is applied as the app's `.tint`, so the whole
/// UI subtly adopts the album's hue; the editorial surfaces read the full
/// palette (background flood, text roles) directly.
@MainActor
@Observable
final class ArtworkTheme {
    private(set) var palette: ArtworkPalette = .fallback
    private var lastURL: String?

    /// Vivid accent for controls/tint.
    var accent: Color { palette.accent }
    /// Darkened, hue-matched tint used for full-window background gradients.
    var background: Color { palette.background }

    func update(for urlString: String?) async {
        guard let urlString, urlString != lastURL else { return }
        lastURL = urlString
        guard let image = await CoverArtCache.shared.image(for: urlString),
              let next = ArtworkPalette.extract(from: image) else { return }
        withAnimation(.easeInOut(duration: 0.7)) {
            palette = next
        }
    }
}
