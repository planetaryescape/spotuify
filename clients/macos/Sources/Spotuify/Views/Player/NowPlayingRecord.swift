import SwiftUI
import SpotuifyKit

/// The cover on the stage: shown whole and square, lifted by a soft shadow.
/// While paused it eases back to 88%, so play state reads from across the room.
struct RecordArtwork: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let item: MediaItem?
    let size: CGFloat
    let isPlaying: Bool

    private var radius: CGFloat { min(14, max(8, size * 0.025)) }

    var body: some View {
        AsyncCoverImage(url: item?.imageURL, cornerRadius: radius)
            .frame(width: size, height: size)
            .overlay {
                // Hairline edge so black covers don't dissolve into a dark room.
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .strokeBorder(.white.opacity(0.1), lineWidth: 1)
            }
            .id(item?.uri)
            .transition(.opacity)
            .shadow(color: .black.opacity(isPlaying ? 0.45 : 0.28), radius: isPlaying ? 36 : 18, y: isPlaying ? 22 : 10)
            .scaleEffect(isPlaying ? 1 : 0.88)
            .animation(reduceMotion ? nil : .spring(response: 0.6, dampingFraction: 0.82), value: isPlaying)
            .animation(reduceMotion ? nil : .easeInOut(duration: 0.6), value: item?.uri)
            .accessibilityLabel(item.map { "Artwork for \($0.name)" } ?? "No artwork")
    }
}

/// Where and whether the music is playing, title, and the artist + album line.
/// Title uses the display serif; everything else stays on the system font.
struct RecordInfo: View {
    @Environment(AppModel.self) private var model
    @Environment(ArtworkTheme.self) private var theme
    var alignment: HorizontalAlignment = .leading
    var titleSize: CGFloat = 44
    var showsLike = true
    /// Artist/album link to their pages. Off where there's no navigation
    /// stack to push onto (the menu bar).
    var linksCredits = true

    private var item: MediaItem? { model.player.currentItem }
    private var text: Color { theme.immersiveText }
    private var textAlignment: TextAlignment { alignment == .center ? .center : .leading }

    var body: some View {
        VStack(alignment: alignment, spacing: 10) {
            statusEyebrow
            Text(item?.name ?? "Nothing playing")
                .font(.displayHero(titleSize))
                .foregroundStyle(text)
                .multilineTextAlignment(textAlignment)
                .lineLimit(2)
                .minimumScaleFactor(0.55)
                .fixedSize(horizontal: false, vertical: true)
            creditLine
            if showsLike, let item {
                NowPlayingLikeButton(item: item, accent: theme.palette.accent, unlikedTint: text.opacity(0.85)) {
                    model.likeCurrent()
                }
                .padding(.top, 2)
            }
        }
        .frame(maxWidth: .infinity, alignment: Alignment(horizontal: alignment, vertical: .center))
    }

    /// "Playing on Kitchen" — answers "is it playing, and where" in one line.
    private var statusEyebrow: some View {
        let playing = model.player.isPlaying
        let device = model.player.activeDevice?.name
        let label: String = {
            guard item != nil else { return "Ready when you are" }
            let verb = playing ? "Playing" : "Paused"
            return device.map { "\(verb) on \($0)" } ?? verb
        }()
        return HStack(spacing: 6) {
            if item != nil {
                LevelMeter(isPlaying: playing, size: 10)
            }
            Text(label.uppercased())
                .font(.system(size: 10.5, weight: .semibold))
                .tracking(1.4)
                .lineLimit(1)
        }
        .foregroundStyle(text.opacity(0.7))
    }

    /// Artist links, then the album in the light italic display cut.
    @ViewBuilder
    private var creditLine: some View {
        let artists = item?.artistNavItems ?? []
        VStack(alignment: alignment, spacing: 4) {
            if !artists.isEmpty && linksCredits {
                HStack(spacing: 4) {
                    ForEach(Array(artists.enumerated()), id: \.element.id) { index, artist in
                        if index > 0 {
                            Text(",").font(.title3.weight(.medium)).foregroundStyle(text.opacity(0.8))
                        }
                        NavigationLink(value: artist) {
                            NowPlayingLink(text: artist.name, font: .title3.weight(.medium), color: text.opacity(0.88))
                        }
                        .buttonStyle(.plain)
                    }
                }
            } else if let subtitle = item?.subtitle, !subtitle.isEmpty {
                Text(subtitle)
                    .font(.title3.weight(.medium))
                    .foregroundStyle(text.opacity(0.88))
                    .lineLimit(1)
            }
            if let album = item?.albumLabel, !album.isEmpty {
                if linksCredits, let albumNav = item?.albumNavItem {
                    NavigationLink(value: albumNav) {
                        NowPlayingLink(text: album, font: .displayAccent(16), color: text.opacity(0.62))
                    }
                    .buttonStyle(.plain)
                } else {
                    Text(album).font(.displayAccent(16)).foregroundStyle(text.opacity(0.62)).lineLimit(1)
                }
            }
        }
    }
}

/// Heart toggle for the now-playing track. Fills + accent-tints the instant it's
/// tapped (optimistic local state) and bounces, so the user feels the action land
/// without waiting for the daemon round-trip. The optimistic override is dropped
/// once the authoritative `inLibrary` catches up or the track changes.
struct NowPlayingLikeButton: View {
    let item: MediaItem
    let accent: Color
    /// Tint for the unliked heart — theme-aware so it reads over a fixed light scrim.
    var unlikedTint: Color = .white.opacity(0.85)
    var diameter: CGFloat = 36
    let action: () -> Void
    @State private var bounce = 0
    @State private var optimistic: Bool?

    private var liked: Bool { optimistic ?? (item.inLibrary == true) }

    var body: some View {
        Button {
            optimistic = !liked
            bounce += 1
            action()
        } label: {
            Image(systemName: liked ? "heart.fill" : "heart")
                .font(.system(size: diameter * 0.44, weight: .semibold))
                .foregroundStyle(liked ? AnyShapeStyle(accent) : AnyShapeStyle(unlikedTint))
                .frame(width: diameter, height: diameter)
                .background(unlikedTint.opacity(0.12), in: Circle())
                .contentShape(Circle())
                .contentTransition(.symbolEffect(.replace))
                .symbolEffect(.bounce, value: bounce)
        }
        .buttonStyle(PressableButtonStyle())
        .help(liked ? "Remove from Liked Songs" : "Add to Liked Songs")
        .accessibilityLabel(liked ? "Remove from Liked Songs" : "Add to Liked Songs")
        .onChange(of: item.inLibrary) { optimistic = nil }
        .onChange(of: item.uri) { optimistic = nil }
    }
}

/// A tappable album/artist label floated over the stage. Underlines on hover
/// so it reads as clickable against the backdrop.
struct NowPlayingLink: View {
    let text: String
    let font: Font
    let color: Color
    @State private var hovering = false

    var body: some View {
        Text(text)
            .font(font)
            .underline(hovering, color: color)
            .foregroundStyle(color)
            .lineLimit(1)
            .contentShape(Rectangle())
            .onHover { hovering = $0 }
            .pointerStyle(.link)
    }
}
