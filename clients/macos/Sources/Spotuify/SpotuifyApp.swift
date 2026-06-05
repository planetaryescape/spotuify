import SwiftUI
import SpotuifyKit

@main
struct SpotuifyApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @State private var model = AppModel()
    @State private var theme = ArtworkTheme()

    var body: some Scene {
        WindowGroup("Spotuify", id: "player") {
            AppShell()
                .environment(model)
                .environment(theme)
                .task {
                    model.start()
                    SystemMediaController.shared.configure(model: model)
                }
                .onChange(of: model.player.playback) { _, _ in
                    Task { await SystemMediaController.shared.updateNowPlaying(player: model.player) }
                }
        }
        .windowResizability(.contentSize)
        .defaultSize(width: 980, height: 720)
        .commands {
            CommandGroup(after: .windowArrangement) {
                MiniPlayerCommand()
            }
        }

        WindowGroup(id: "mini-player") {
            MiniPlayerView()
                .environment(model)
                .environment(theme)
                .task { model.start() }
        }
        .windowResizability(.contentSize)
        .defaultSize(width: 320, height: 380)

        MenuBarExtra("Spotuify", systemImage: "music.note") {
            MenuBarView()
                .environment(model)
                .environment(theme)
        }
        .menuBarExtraStyle(.window)

        Settings {
            SettingsView()
                .environment(model)
        }
    }
}

/// Menu command + ⌘⇧M shortcut to open the floating mini-player.
private struct MiniPlayerCommand: View {
    @Environment(\.openWindow) private var openWindow
    var body: some View {
        Button("Mini Player") { openWindow(id: "mini-player") }
            .keyboardShortcut("m", modifiers: [.command, .shift])
    }
}
