import SwiftUI

/// The room's light: the current cover blown up, blurred and saturated behind
/// the stage, with a veil that keeps text legible on any cover — white, black
/// or busy. Reduce Transparency drops the blur and keeps the palette flood.
struct NowPlayingBackdrop: View {
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    let imageURL: String?
    let palette: ArtworkPalette
    /// True under a fixed light theme: veil toward white instead of black.
    let isLight: Bool

    var body: some View {
        ZStack {
            palette.background
            if !reduceTransparency {
                AsyncCoverImage(url: imageURL, cornerRadius: 0, showsPlaceholderGlyph: false)
                    .scaleEffect(1.4)
                    .blur(radius: 90, opaque: true)
                    .saturation(1.3)
                    .opacity(0.9)
                    .id(imageURL)
                    .transition(.opacity)
            }
            veil
        }
        .animation(.easeInOut(duration: 0.9), value: imageURL)
        .clipped()
        .accessibilityHidden(true)
    }

    /// Darkens most where text sits (the lower half and edges) and least where
    /// the colour should glow through.
    private var veil: some View {
        let tone: Color = isLight ? .white : .black
        return ZStack {
            LinearGradient(
                stops: [
                    .init(color: tone.opacity(isLight ? 0.35 : 0.18), location: 0),
                    .init(color: tone.opacity(isLight ? 0.45 : 0.32), location: 0.55),
                    .init(color: tone.opacity(isLight ? 0.6 : 0.55), location: 1),
                ],
                startPoint: .top, endPoint: .bottom)
            RadialGradient(
                colors: [.clear, tone.opacity(isLight ? 0.2 : 0.35)],
                center: .center, startRadius: 200, endRadius: 900)
        }
    }
}
