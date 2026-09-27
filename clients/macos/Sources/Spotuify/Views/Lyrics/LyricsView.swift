import SwiftUI
import SpotuifyKit

/// Synced lyrics that keep the active line centered as playback advances.
/// Top/bottom spacers let the first and last lines reach the vertical center.
struct LyricsView: View {
    @Environment(AppModel.self) private var model
    /// Line colour. White on the stage; the side rail passes `.primary`.
    var textColor: Color = .white
    @State private var activeIndex: Int?

    private var currentURI: String? { model.player.currentItem?.uri }

    var body: some View {
        Group {
            if model.lyrics.loading {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let lyrics = model.lyrics.lyrics, !lyrics.lines.isEmpty {
                lyricsScroll(lyrics)
            } else {
                EmptyState(
                    "No lyrics",
                    systemImage: "quote.bubble",
                    description: Text(currentURI == nil
                        ? "Play a track to see its lyrics."
                        : "Lyrics aren't available for this track."))
            }
        }
        .task(id: currentURI) {
            await model.lyrics.load(uri: currentURI)
            activeIndex = model.lyrics.activeIndex(positionMs: model.player.displayProgressMs)
        }
        .onChange(of: model.player.displayProgressMs) { _, ms in
            let index = model.lyrics.activeIndex(positionMs: ms)
            if index != activeIndex { activeIndex = index }
        }
        .onChange(of: model.lyrics.lyrics) { _, _ in
            activeIndex = model.lyrics.activeIndex(positionMs: model.player.displayProgressMs)
        }
    }

    private func lyricsScroll(_ lyrics: SyncedLyrics) -> some View {
        ScrollViewReader { proxy in
            ScrollView(showsIndicators: false) {
                // Apple-Music feel: big bold active line, surrounding lines
                // dimmed; left-aligned over the dark now-playing backdrop.
                VStack(alignment: .leading, spacing: 16) {
                    // Top spacer lets the first line scroll to center.
                    Color.clear.frame(height: 200).id("lyrics-top")
                    ForEach(Array(lyrics.lines.enumerated()), id: \.offset) { index, line in
                        let isActive = index == activeIndex
                        Text(line.text.isEmpty ? "\u{266A}" : line.text)
                            // One size for every line: growing the active line reflowed
                            // the column and made the scroll jump.
                            .font(.system(size: 28, weight: .bold))
                            .scaleEffect(isActive ? 1 : 0.94, anchor: line.isRtl ? .trailing : .leading)
                            .foregroundStyle(isActive ? textColor : textColor.opacity(0.38))
                            .multilineTextAlignment(.leading)
                            .fixedSize(horizontal: false, vertical: true)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .environment(\.layoutDirection, line.isRtl ? .rightToLeft : .leftToRight)
                            .id(index)
                            .contentShape(Rectangle())
                            .onTapGesture { model.seek(toMs: line.startMs) }
                            .animation(.easeInOut(duration: 0.25), value: activeIndex)
                    }
                    Color.clear.frame(height: 200).id("lyrics-bottom")
                }
                .padding(.horizontal, 8)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            // Soft fade at top/bottom edges, like Apple Music.
            .mask(
                LinearGradient(
                    stops: [
                        .init(color: .clear, location: 0),
                        .init(color: .black, location: 0.12),
                        .init(color: .black, location: 0.88),
                        .init(color: .clear, location: 1),
                    ],
                    startPoint: .top, endPoint: .bottom))
            .onChange(of: activeIndex) { _, index in
                guard let index else { return }
                withAnimation(.easeInOut(duration: 0.35)) {
                    proxy.scrollTo(index, anchor: .center)
                }
            }
            .onAppear {
                if let index = activeIndex {
                    proxy.scrollTo(index, anchor: .center)
                }
            }
        }
    }
}
