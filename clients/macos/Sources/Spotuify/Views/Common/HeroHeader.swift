import SwiftUI

/// The top of every collection page (albums, artists, playlists, shows, Liked
/// Songs): the art, a mono eyebrow, a big Fraunces title, the credits, and an
/// action row led by one round accent play button. An accent wash rises from
/// behind it and fades into the room.
struct HeroHeader<Art: View, Credits: View, Actions: View>: View {
    @Environment(\.room) private var room
    let eyebrow: String
    let title: String
    /// Pushed pages get a back link; root pages don't.
    var showsBack = false
    @ViewBuilder var art: () -> Art
    @ViewBuilder var credits: () -> Credits
    @ViewBuilder var actions: () -> Actions

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if showsBack {
                RoomBackButton().padding(.bottom, 20)
            }
            ViewThatFits(in: .horizontal) {
                layout(artSize: 216, titleSize: 56)
                layout(artSize: 160, titleSize: 42)
                layout(artSize: 120, titleSize: 32)
            }
        }
        .padding(.horizontal, 32)
        .padding(.top, showsBack ? 20 : 44)
        .padding(.bottom, 24)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(alignment: .top) {
            LinearGradient(
                colors: [room.accent.opacity(room.isLight ? 0.16 : 0.24), room.accent.opacity(0)],
                startPoint: .top, endPoint: .bottom)
                .ignoresSafeArea()
                .allowsHitTesting(false)
        }
    }

    private func layout(artSize: CGFloat, titleSize: CGFloat) -> some View {
        HStack(alignment: .bottom, spacing: 28) {
            art()
                .frame(width: artSize, height: artSize)
                .shadow(color: .black.opacity(0.4), radius: 24, y: 14)
            VStack(alignment: .leading, spacing: 10) {
                MonoCaps(eyebrow, size: 10, color: room.accent)
                Text(title)
                    .font(.displayHero(titleSize))
                    .foregroundStyle(room.ink)
                    .lineLimit(2)
                    .minimumScaleFactor(0.5)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityAddTraits(.isHeader)
                credits()
                    .font(.system(size: 14, weight: .medium))
                    .foregroundStyle(room.inkMuted)
                HStack(spacing: 12) {
                    actions()
                }
                .padding(.top, 10)
            }
            .frame(minWidth: 260, alignment: .leading)
        }
    }
}

/// Collections without a cover (Liked Songs) get one: the accent, deepened,
/// with a glyph.
struct GlyphCoverArt: View {
    @Environment(\.room) private var room
    let systemName: String

    var body: some View {
        RoundedRectangle(cornerRadius: Theme.tileCornerRadius, style: .continuous)
            .fill(LinearGradient(
                colors: [room.accent, room.accent.mix(with: .black, by: 0.55)],
                startPoint: .topLeading, endPoint: .bottomTrailing))
            .overlay {
                GeometryReader { geo in
                    Image(systemName: systemName)
                        .font(.system(size: geo.size.width * 0.34, weight: .bold))
                        .foregroundStyle(.white.opacity(0.92))
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .overlay { Grain(intensity: 1.5).clipShape(RoundedRectangle(cornerRadius: Theme.tileCornerRadius, style: .continuous)) }
            .accessibilityHidden(true)
    }
}
