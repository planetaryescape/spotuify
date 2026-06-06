import AppKit

/// Opens Terminal pre-filled with a sequence of commands by writing a temporary
/// executable `.command` file and opening it. Using a `.command` file (rather
/// than AppleScript-driving Terminal) avoids the Automation permission prompt.
enum TerminalLauncher {
    static func run(_ commands: [String]) {
        let script = "#!/bin/bash\n"
            + "echo '— spotuify —'\n"
            + commands.joined(separator: "\n") + "\n"
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("spotuify-\(UUID().uuidString).command")
        do {
            try script.write(to: url, atomically: true, encoding: .utf8)
            try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: url.path)
            NSWorkspace.shared.open(url)
        } catch {
            // Fall back to the docs if we somehow can't stage the script.
            if let docs = URL(string: "https://spotuify.vercel.app") {
                NSWorkspace.shared.open(docs)
            }
        }
    }
}
