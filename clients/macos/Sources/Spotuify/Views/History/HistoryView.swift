import SwiftUI
import SpotuifyKit

/// Listening history: either a flat chronological list of recently-played
/// tracks, or grouped into sessions (a session opens to its track list). Fed by
/// `LibraryStore.historySessions` (local plays merged with Spotify
/// recently-played, split on gaps).
struct HistoryView: View {
    @Environment(AppModel.self) private var model
    /// false = chronological list, true = session albums.
    @AppStorage("historySessionMode") private var sessionMode = false

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 0) {
                EditorialPageHeader(title: "History", eyebrow: "You") {
                    RoomTabs(
                        options: [(value: false, title: "Recent"), (value: true, title: "Sessions")],
                        selection: $sessionMode)
                }
                content
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .navigationDestination(for: ListenSession.self) {
                // Its hero carries the "Back" link; hide the system chevron.
                SessionDetailView(session: $0).navigationBarBackButtonHidden(true)
            }
            .mediaDetailDestinations()
        }
        .task { await model.library.loadHistory() }
    }

    @ViewBuilder
    private var content: some View {
        let sessions = model.library.historySessions
        if model.library.loadingHistory && sessions.isEmpty {
            SkeletonRows()
        } else if sessions.isEmpty {
            EmptyState(
                "No listening history", systemImage: "clock.arrow.circlepath",
                description: Text("Tracks you play show up here, grouped into sessions."))
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if sessionMode {
            ScrollView {
                LazyVStack(spacing: 8) {
                    ForEach(sessions) { session in
                        NavigationLink(value: session) { SessionRow(session: session) }
                            .buttonStyle(.plain)
                    }
                }
                .padding(.horizontal, 24).padding(.bottom, 24)
            }
        } else {
            ScrollView {
                LazyVStack(spacing: 2) {
                    ForEach(Array(sessions.flatMap(\.tracks).enumerated()), id: \.offset) { index, track in
                        MediaRow(item: track, detailed: true, index: index + 1)
                    }
                }
                .padding(.horizontal, 24).padding(.bottom, 24)
            }
        }
    }
}

/// A session summary row: lead artwork, dominant context, track count + when.
struct SessionRow: View {
    @Environment(\.room) private var room
    let session: ListenSession
    @State private var hovering = false

    var body: some View {
        HStack(spacing: 16) {
            StackedCover(urls: session.tracks.prefix(3).map(\.imageURL), size: 56)
            VStack(alignment: .leading, spacing: 6) {
                Text(session.contextLabel ?? "Mixed session")
                    .font(.displayTitle(18)).foregroundStyle(room.ink).lineLimit(1)
                MonoCaps("\(session.trackCount) track\(session.trackCount == 1 ? "" : "s") · \(Self.when(session.startedAtMs))", size: 9.5)
            }
            Spacer()
            Image(systemName: "chevron.right").font(.system(size: 10, weight: .bold)).foregroundStyle(room.inkFaint)
        }
        .padding(12)
        .background(RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous)
            .fill(room.ink.opacity(hovering ? 0.07 : 0.04)))
        .overlay(RoundedRectangle(cornerRadius: Theme.rowRadius, style: .continuous).strokeBorder(room.hairline))
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
    }

    static func when(_ ms: Int64) -> String {
        let date = Date(timeIntervalSince1970: Double(ms) / 1000)
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .abbreviated
        return formatter.localizedString(for: date, relativeTo: Date())
    }
}

/// A single session's track list.
struct SessionDetailView: View {
    let session: ListenSession

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HeroHeader(eyebrow: "Session", title: session.contextLabel ?? "Mixed session", showsBack: true) {
                StackedCover(urls: session.tracks.prefix(3).map(\.imageURL), size: 140)
            } credits: {
                Text("\(session.trackCount) track\(session.trackCount == 1 ? "" : "s") · \(SessionRow.when(session.startedAtMs))")
                    .font(.mono(12))
            } actions: {
                EmptyView()
            }
            ScrollView {
                LazyVStack(spacing: 2) {
                    ForEach(Array(session.tracks.enumerated()), id: \.offset) { index, track in
                        MediaRow(item: track, detailed: true, index: index + 1)
                    }
                }
                .padding(.horizontal, 24).padding(.bottom, 24)
            }
        }
        .navigationTitle(session.contextLabel ?? "Session")
    }
}

/// Up to three covers fanned as a card stack — the first track sits on top, the
/// rest peek behind it to signal "a session is a stack of tracks".
struct StackedCover: View {
    let urls: [String?]
    var size: CGFloat = 56

    var body: some View {
        let covers = Array(urls.prefix(3))
        ZStack {
            ForEach(Array(covers.enumerated()), id: \.offset) { index, url in
                AsyncCoverImage(url: url, cornerRadius: 6)
                    .frame(width: size, height: size)
                    .scaleEffect(1 - CGFloat(index) * 0.08)
                    .offset(x: CGFloat(index) * 5, y: CGFloat(index) * 5)
                    .shadow(color: .black.opacity(0.35), radius: 3, y: 1)
                    .zIndex(Double(covers.count - index))
            }
        }
        // Reserve room for the fanned cards so the row's text doesn't shift.
        .frame(width: size + CGFloat(max(covers.count - 1, 0)) * 5,
               height: size + CGFloat(max(covers.count - 1, 0)) * 5,
               alignment: .topLeading)
    }
}
