import SwiftUI

/// Loads album artwork from a Spotify CDN URL via `CoverArtCache`, with a
/// graceful placeholder while loading or when missing.
struct AsyncCoverImage: View {
    let url: String?
    var cornerRadius: CGFloat = Theme.artCornerRadius

    @State private var image: NSImage?
    @State private var loadedURL: String?

    var body: some View {
        ZStack {
            if let image {
                Image(nsImage: image)
                    .resizable()
                    .interpolation(.high)
                    .aspectRatio(contentMode: .fill)
            } else {
                ZStack {
                    Rectangle().fill(.quaternary)
                    Image(systemName: "music.note")
                        .font(.system(size: 28))
                        .foregroundStyle(.secondary)
                }
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
        .animation(.easeInOut(duration: 0.25), value: image)
        .task(id: url) {
            guard loadedURL != url else { return }
            image = nil
            loadedURL = url
            image = await CoverArtCache.shared.image(for: url)
        }
    }
}
