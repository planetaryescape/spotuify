import SwiftUI

/// Tracks the role-based `ArtworkPalette` derived from the current artwork,
/// animated on change. `accent` is applied as the app's `.tint`, so the whole
/// UI subtly adopts the album's hue; the editorial surfaces read the full
/// palette (background flood, text roles) directly.
///
/// When a fixed desktop theme (Light / Dark / Follow System) is active,
/// `adaptiveEnabled` is false: artwork extraction is skipped and the palette
/// holds a fixed light/dark fallback applied via `applyFixed(_:)`.
@MainActor
@Observable
final class ArtworkTheme {
    private(set) var palette: ArtworkPalette = .darkFallback
    private var lastURL: String?

    /// Whether cover-art extraction drives the palette. Mirrors
    /// `AppTheme.isAdaptive`; kept in sync from the app root.
    var adaptiveEnabled: Bool = true

    /// Vivid accent for controls/tint. Under a fixed theme this is the app
    /// AccentColor asset rather than an artwork-derived hue.
    var accent: Color { adaptiveEnabled ? palette.accent : .accentColor }
    /// Darkened, hue-matched tint used for full-window background gradients.
    var background: Color { palette.background }

    /// True only under a fixed LIGHT theme. Adaptive always floods dark (the
    /// derived `background` is capped dark even for bright covers), so immersive
    /// surfaces keep white text there; a fixed light theme needs dark text.
    var immersiveIsLight: Bool { !adaptiveEnabled && palette.isLight }
    /// Base text color over the immersive flood/scrim. `.white` under adaptive /
    /// dark (pixel-identical to before) and near-black under a fixed light theme.
    /// Apply the surface's original `.opacity(_:)` on top to preserve tiers.
    var immersiveText: Color { immersiveIsLight ? palette.primary : .white }
    /// Glyph color for the active (white-filled) mode/viz pill button. Uses
    /// `palette.background` (dark under adaptive/dark) but pins dark under a fixed
    /// light theme, where `palette.background` would vanish on the white pill.
    var immersivePillGlyph: Color { immersiveIsLight ? Color(white: 0.12) : palette.background }

    func update(for urlString: String?) async {
        guard adaptiveEnabled else { return }
        guard let urlString, urlString != lastURL else { return }
        lastURL = urlString
        guard let image = await CoverArtCache.shared.image(for: urlString),
              let next = ArtworkPalette.extract(from: image) else { return }
        withAnimation(.easeInOut(duration: 0.7)) {
            palette = next
        }
    }

    /// Apply a fixed, polished palette for the resolved base scheme. Called when
    /// a non-adaptive theme is active (and when the system scheme changes under
    /// Follow System). Clears `lastURL` so re-enabling adaptive re-extracts.
    func applyFixed(_ scheme: ColorScheme) {
        lastURL = nil
        let next: ArtworkPalette = scheme == .dark ? .darkFallback : .lightFallback
        withAnimation(.easeInOut(duration: 0.4)) {
            palette = next
        }
    }
}
