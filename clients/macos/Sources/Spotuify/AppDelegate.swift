import AppKit

/// Keeps the app (and its menubar item) alive after the main window closes —
/// the player keeps running headless and can be reopened from the menubar.
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationWillFinishLaunching(_ notification: Notification) {
        EditorialFont.register()
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }
}
