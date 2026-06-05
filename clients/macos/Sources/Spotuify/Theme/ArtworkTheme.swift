import SwiftUI

/// Tracks an accent color derived from the current artwork, animated on change.
/// Applied as the app's `.tint`, so the whole UI subtly adopts the album's hue.
@MainActor
@Observable
final class ArtworkTheme {
    var accent: Color = .accentColor
    /// Darkened, hue-matched tint used for the full-window background gradient.
    var background: Color = Color(white: 0.13)
    private var lastURL: String?

    func update(for urlString: String?) async {
        guard let urlString, urlString != lastURL else { return }
        lastURL = urlString
        guard let image = await CoverArtCache.shared.image(for: urlString),
              let palette = DominantColor.palette(from: image) else { return }
        withAnimation(.easeInOut(duration: 0.7)) {
            accent = palette.accent
            background = palette.background
        }
    }
}
