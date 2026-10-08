import AppKit
import HermesCore
import SwiftUI

@main
struct HermacosApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @State private var model = AppModel()

    var body: some Scene {
        Window("Hermacos", id: "main") {
            RootView()
                .environment(model)
                .frame(minWidth: 620, minHeight: 440)
                .task { model.bootstrap() }
        }
        .defaultSize(width: 1060, height: 740)
        .windowToolbarStyle(.unified)
        .commands { AppCommands(model: model) }

        Settings {
            SettingsView()
                .environment(model)
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        // Launched as a bare executable (swift run) the process is not a regular app yet.
        NSApp.setActivationPolicy(.regular)
        NSApp.activate()
        switch ProcessInfo.processInfo.environment["HERMACOS_APPEARANCE"] {
        case "light": NSApp.appearance = NSAppearance(named: .aqua)
        case "dark": NSApp.appearance = NSAppearance(named: .darkAqua)
        default: break
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows: Bool) -> Bool {
        if !hasVisibleWindows {
            sender.windows.first { $0.canBecomeMain }?.makeKeyAndOrderFront(nil)
        }
        return true
    }
}

/// Menu bar commands and their standard Mac shortcuts.
struct AppCommands: Commands {
    let model: AppModel
    @Environment(\.openWindow) private var openWindow

    private var ready: Bool { model.phase == .ready }

    var body: some Commands {
        CommandGroup(replacing: .newItem) {
            Button("New Chat") {
                openWindow(id: "main")
                model.newChat()
            }
            .keyboardShortcut("n")
            .disabled(!ready)
        }

        CommandGroup(after: .textEditing) {
            Button("Search Chats") {
                openWindow(id: "main")
                model.searchFocusRequests += 1
            }
            .keyboardShortcut("f")
            .disabled(!ready)
        }

        SidebarCommands()

        CommandMenu("Chat") {
            Button("Focus Message Field") { model.composerFocusRequests += 1 }
                .keyboardShortcut("l")
                .disabled(!ready)
            Button("Stop Responding") { model.chat.stop() }
                .keyboardShortcut(".")
                .disabled(!ready || !model.chat.isStreaming)
            Button("Copy Last Response") { copyLastResponse() }
                .keyboardShortcut("c", modifiers: [.command, .shift])
                .disabled(!ready || lastResponse == nil)

            Divider()

            Button("Previous Chat") { model.selectAdjacentSession(offset: -1) }
                .keyboardShortcut("[")
                .disabled(!ready)
            Button("Next Chat") { model.selectAdjacentSession(offset: 1) }
                .keyboardShortcut("]")
                .disabled(!ready)

            Divider()

            Button("Rename…") { model.renaming = model.selectedSession }
                .disabled(model.selectedSession == nil)
            // No shortcut: ⌘⌫ belongs to the message field (delete to start of line).
            Button("Delete Chat…") { model.pendingDelete = model.selectedSession }
                .disabled(model.selectedSession == nil)

            Divider()

            Button("Reload Chats") { Task { await model.refreshSessions() } }
                .keyboardShortcut("r")
                .disabled(!ready)
            Button("Server Status") { model.selection = .status }
                .keyboardShortcut("i")
                .disabled(!ready)
        }
    }

    private var lastResponse: String? {
        guard ready, let item = model.chat.items.last(where: { $0.role == .assistant }) else { return nil }
        let text = item.plainText
        return text.isEmpty ? nil : text
    }

    private func copyLastResponse() {
        guard let text = lastResponse else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }
}
