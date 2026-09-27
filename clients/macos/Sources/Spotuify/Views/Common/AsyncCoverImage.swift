import SwiftUI

/// Loads album artwork from a Spotify CDN URL via `CoverArtCache`, with a
/// graceful placeholder while loading or when missing.
struct AsyncCoverImage: View {
    let url: String?
    var cornerRadius: CGFloat = Theme.artCornerRadius
    /// Hide the note glyph when the image is decorative (e.g. a blurred wash).
    var showsPlaceholderGlyph = true

    @State private var image: NSImage?
    @State private var loadedURL: String?

    var body: some View {
        // The image fills an overlay on a layout-neutral clear box: sized by
        // the parent, never by the image. A non-square cover laid out directly
        // grew taller than its square tile and pushed the caption below out of line.
        Color.clear
            .overlay {
                if let image {
                    Image(nsImage: image)
                        .resizable()
                        .interpolation(.high)
                        .aspectRatio(contentMode: .fill)
                } else {
                    placeholder
                }
            }
            .clipped()
            .clipShape(RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
            .animation(.easeInOut(duration: 0.25), value: image)
            .task(id: url) {
                guard loadedURL != url else { return }
                image = nil
                loadedURL = url
                image = await CoverArtCache.shared.image(for: url)
            }
    }

    /// A soft sleeve rather than a flat grey box, with the glyph scaled to the
    /// tile so it reads the same on a 40pt row and a 500pt stage.
    private var placeholder: some View {
        ZStack {
            LinearGradient(
                colors: [Color.primary.opacity(0.10), Color.primary.opacity(0.04)],
                startPoint: .topLeading, endPoint: .bottomTrailing)
            if showsPlaceholderGlyph {
                GeometryReader { geo in
                    Image(systemName: "music.note")
                        .font(.system(size: min(geo.size.width, geo.size.height) * 0.32, weight: .light))
                        .foregroundStyle(.tertiary)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
        }
    }
}
