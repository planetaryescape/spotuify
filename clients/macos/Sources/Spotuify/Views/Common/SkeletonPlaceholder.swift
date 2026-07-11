import SwiftUI

/// Skeleton placeholders shown while a surface's first fetch is in flight.
/// They sketch the real layout (rows / tiles) so loading reads as "content
/// is coming" — distinct from the genuinely-empty `ContentUnavailableView`.

/// Pulsing dim shapes shared by both skeleton layouts.
private struct SkeletonPulse: ViewModifier {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var dimmed = false

    func body(content: Content) -> some View {
        content
            .foregroundStyle(.quaternary)
            .opacity(dimmed ? 0.45 : 1)
            .animation(
                reduceMotion ? nil : .easeInOut(duration: 0.9).repeatForever(autoreverses: true),
                value: dimmed
            )
            .onAppear { dimmed = !reduceMotion }
            .onChange(of: reduceMotion) { _, shouldReduce in dimmed = !shouldReduce }
            .accessibilityLabel("Loading")
    }
}

/// List-shaped skeleton: artwork square + two text bars per row, widths
/// varied deterministically so the column doesn't read as a barcode.
struct SkeletonRows: View {
    var rows: Int = 9

    private static let titleWidths: [CGFloat] = [180, 220, 150, 200, 170, 240, 160, 210, 190]
    private static let subtitleWidths: [CGFloat] = [120, 90, 140, 100, 130, 110, 95, 125, 105]

    var body: some View {
        VStack(spacing: 0) {
            ForEach(0..<rows, id: \.self) { index in
                HStack(spacing: 12) {
                    RoundedRectangle(cornerRadius: 6)
                        .frame(width: 36, height: 36)
                    VStack(alignment: .leading, spacing: 6) {
                        RoundedRectangle(cornerRadius: 3)
                            .frame(width: Self.titleWidths[index % Self.titleWidths.count], height: 10)
                        RoundedRectangle(cornerRadius: 3)
                            .frame(width: Self.subtitleWidths[index % Self.subtitleWidths.count], height: 8)
                    }
                    Spacer()
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 8)
            }
            Spacer(minLength: 0)
        }
        .modifier(SkeletonPulse())
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
    }
}

/// Grid-shaped skeleton for album/artist/show card grids.
struct SkeletonTiles: View {
    var tiles: Int = 8
    var minTile: CGFloat = 160

    var body: some View {
        ScrollView {
            LazyVGrid(
                columns: [GridItem(.adaptive(minimum: minTile), spacing: 16)],
                spacing: 16
            ) {
                ForEach(0..<tiles, id: \.self) { _ in
                    VStack(alignment: .leading, spacing: 8) {
                        RoundedRectangle(cornerRadius: Theme.tileCornerRadius)
                            .aspectRatio(1, contentMode: .fit)
                        RoundedRectangle(cornerRadius: 3)
                            .frame(width: 110, height: 10)
                        RoundedRectangle(cornerRadius: 3)
                            .frame(width: 70, height: 8)
                    }
                    .padding(6)
                }
            }
            .padding(16)
        }
        .modifier(SkeletonPulse())
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
    }
}
