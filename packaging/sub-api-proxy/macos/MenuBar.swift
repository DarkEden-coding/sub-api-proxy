import AppKit

@main
final class MenuBar: NSObject, NSApplicationDelegate {
    private var item: NSStatusItem!
    private let panel = URL(string: "http://127.0.0.1:8317/sap/")!

    static func main() {
        let app = NSApplication.shared
        let delegate = MenuBar()
        app.delegate = delegate
        app.setActivationPolicy(.accessory)
        withExtendedLifetime(delegate) { app.run() }
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        if let button = item.button {
            let image = NSImage(systemSymbolName: "arrow.triangle.branch", accessibilityDescription: "Sub-Api-Proxy")
            image?.isTemplate = true
            button.image = image
            button.toolTip = "Sub-Api-Proxy • Click to open admin panel"
            button.target = self
            button.action = #selector(clicked)
            button.sendAction(on: [.leftMouseUp, .rightMouseUp])
        }
    }

    @objc private func clicked() {
        if NSApp.currentEvent?.type == .rightMouseUp {
            let menu = NSMenu()
            menu.addItem(withTitle: "Open admin panel", action: #selector(openPanel), keyEquivalent: "")
            menu.addItem(withTitle: "Copy admin key", action: #selector(copyKey), keyEquivalent: "")
            menu.addItem(.separator())
            menu.addItem(withTitle: "Quit menu bar app", action: #selector(quit), keyEquivalent: "q")
            for entry in menu.items { entry.target = self }
            item.menu = menu
            item.button?.performClick(nil)
            item.menu = nil
        } else {
            openPanel()
        }
    }

    @objc private func openPanel() {
        NSWorkspace.shared.open(panel)
    }

    @objc private func copyKey() {
        let file = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".config/sub-api-proxy/management-key")
        guard let contents = try? String(contentsOf: file, encoding: .utf8),
              !contents.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            let alert = NSAlert()
            alert.messageText = "Admin key unavailable"
            alert.informativeText = "The management key file could not be read. Check your local Sub-Api-Proxy installation."
            NSApp.activate(ignoringOtherApps: true)
            alert.runModal()
            return
        }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(contents.trimmingCharacters(in: .whitespacesAndNewlines), forType: .string)
        let revision = pasteboard.changeCount
        // Clear our key after one minute, unless the user copied something else.
        DispatchQueue.main.asyncAfter(deadline: .now() + 60) {
            if pasteboard.changeCount == revision { pasteboard.clearContents() }
        }
    }

    @objc private func quit() {
        NSApp.terminate(nil)
    }
}
