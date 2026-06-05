import SwiftUI
import SpotuifyKit

/// Synced lyrics that keep the active line centered as playback advances.
/// Top/bottom spacers let the first and last lines reach the vertical center.
struct LyricsView: View {
    @Environment(AppModel.self) private var model
    @State private var activeIndex: Int?

    private var currentURI: String? { model.player.currentItem?.uri }

    var body: some View {
        Group {
            if model.lyrics.loading {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let lyrics = model.lyrics.lyrics, !lyrics.lines.isEmpty {
                lyricsScroll(lyrics)
            } else {
                ContentUnavailableView(
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
            ScrollView {
                VStack(spacing: 18) {
                    // Top spacer lets the first line scroll to center.
                    Color.clear.frame(height: 220).id("lyrics-top")
                    ForEach(Array(lyrics.lines.enumerated()), id: \.offset) { index, line in
                        Text(line.text.isEmpty ? "\u{266A}" : line.text)
                            .font(.system(size: index == activeIndex ? 26 : 19,
                                          weight: index == activeIndex ? .bold : .medium))
                            .foregroundStyle(index == activeIndex
                                ? AnyShapeStyle(.primary)
                                : AnyShapeStyle(.secondary.opacity(0.5)))
                            .multilineTextAlignment(.center)
                            .frame(maxWidth: .infinity)
                            .environment(\.layoutDirection, line.isRtl ? .rightToLeft : .leftToRight)
                            .id(index)
                            .contentShape(Rectangle())
                            .onTapGesture { model.seek(toMs: line.startMs) }
                            .animation(.easeInOut(duration: 0.2), value: activeIndex)
                    }
                    Color.clear.frame(height: 220).id("lyrics-bottom")
                }
                .padding(.horizontal, 40)
                .frame(maxWidth: .infinity)
            }
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
