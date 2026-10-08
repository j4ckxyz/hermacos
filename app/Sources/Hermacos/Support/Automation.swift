import AppKit
import Foundation
import HermesCore

/// Drives the running app from a script, for smoke tests and screenshots.
///
/// Set `HERMACOS_SCRIPT` to newline-separated steps (with `HERMACOS_AUTOLOGIN`). Key presses
/// and pastes are posted as real events to the app's own queue, so they travel the same path
/// as the keyboard without needing Accessibility permission.
///
///     open:first | open:status | open:<session id>
///     wait:<seconds>          wait-idle          wait-streaming
///     keys:<text>             type with no text field focused
///     type:<text>             type into whatever has focus
///     paste-files:<a>,<b>     ⌘V with files on the pasteboard
///     paste-image:<path>      ⌘V with image bytes on the pasteboard
///     paste-text:<text>       ⌘V with text on the pasteboard
///     key:down|up|tab|return|escape   press a named key
///     focus:search|none       put the caret in the sidebar search, or nowhere
///     reload                  re-read the open chat from the server
///     attach:<a>,<b>          what the + button does once files are chosen
///     prompt:<text>           fill the message field
///     send                    stop
///     edit:<n>                open the editor on the n-th user message
///     rewrite:<n>:<text>      rewrite the n-th user message
///     hover:on|off            show hover-only controls
///     activity:open|closed    expand every activity list
///     scroll:top              scroll the transcript to its start
///     usage:open|close        search:<text>
///     more | refresh          load the next page of chats | reload the chat list
///     limit:none | limit:cost:<usd> | limit:tokens:<count>
///     dump:<path>             write the app's state as JSON
///     snap:<path>             write a PNG of the window, rendered by the app itself
@MainActor
struct Automation {
    let model: AppModel
    /// A private pasteboard, so tests never touch what the user has copied.
    private let pasteboard = NSPasteboard(name: NSPasteboard.Name("app.hermacos.automation"))

    func run(_ script: String) async {
        try? await Task.sleep(for: .milliseconds(900))
        for line in script.split(whereSeparator: \.isNewline) {
            let step = line.trimmingCharacters(in: .whitespaces)
            let (command, argument) = step.split(separator: ":", maxSplits: 1).map(String.init).pair
            await perform(command, argument)
        }
    }

    private var chat: ChatModel { model.chat }

    private func userMessage(_ index: Int) -> ChatItem? {
        let mine = chat.items.filter { $0.role == .user }
        return mine.indices.contains(index) ? mine[index] : nil
    }

    private func perform(_ command: String, _ argument: String) async {
        switch command {
        case "wait":
            try? await Task.sleep(for: .seconds(Double(argument) ?? 1))
        case "wait-idle":
            await waitUntil(timeout: 90) { !chat.isStreaming }
            try? await Task.sleep(for: .milliseconds(700))
        case "wait-streaming":
            await waitUntil(timeout: 30) { chat.items.last?.parts.isEmpty == false }
        case "open":
            switch argument {
            case "status": model.selection = .status
            case "first": if let id = model.sessions.first?.id { model.selection = .session(id) }
            case "new": model.newChat()
            default: model.selection = .session(argument)
            }
            try? await Task.sleep(for: .milliseconds(600))
        case "keys", "type":
            bringToFront()
            if command == "keys" { model.mainWindow?.makeFirstResponder(nil) }
            for character in argument {
                post(String(character))
                try? await Task.sleep(for: .milliseconds(25))
            }
            try? await Task.sleep(for: .milliseconds(300))
        case "key":
            // A named key, delivered like a press on the keyboard.
            let keys: [String: (String, UInt16)] = [
                "down": ("\u{F701}", 125), "up": ("\u{F700}", 126), "tab": ("\t", 48),
                "return": ("\r", 36), "escape": ("\u{1B}", 53),
            ]
            if let (characters, code) = keys[argument] {
                bringToFront()
                post(characters, keyCode: code)
                try? await Task.sleep(for: .milliseconds(250))
            }
        case "focus":
            bringToFront()
            if argument == "search" { model.searchFocusRequests += 1 }
            if argument == "none" { model.mainWindow?.makeFirstResponder(nil) }
            try? await Task.sleep(for: .milliseconds(400))
        case "reload":
            // Forget the on-screen transcript and read it back from the server.
            chat.items = []
            await chat.loadHistory()
        case "paste-files":
            pasteboard.clearContents()
            pasteboard.writeObjects(paths(argument) as [NSURL])
            await pressPaste()
        case "paste-image":
            pasteboard.clearContents()
            if let data = try? Data(contentsOf: URL(fileURLWithPath: argument)) { pasteboard.setData(data, forType: .png) }
            await pressPaste()
        case "paste-text":
            pasteboard.clearContents()
            pasteboard.setString(argument, forType: .string)
            await pressPaste()
        case "attach":
            chat.addFiles(paths(argument).map { $0 as URL })
        case "prompt":
            chat.draft = argument
        case "send":
            chat.send()
        case "stop":
            chat.stop()
        case "edit":
            chat.editingItemID = userMessage(Int(argument) ?? 0)?.id
        case "rewrite":
            let (index, text) = argument.split(separator: ":", maxSplits: 1).map(String.init).pair
            if let message = userMessage(Int(index) ?? 0) { chat.rewrite(message, text: text) }
            try? await Task.sleep(for: .milliseconds(200))
        case "hover":
            model.revealHoverControls = argument == "on"
        case "scroll":
            if argument == "top" { model.scrollToTopRequests += 1 }
            try? await Task.sleep(for: .milliseconds(400))
        case "activity":
            model.expandActivity = argument == "open"
        case "usage":
            if argument == "open" { await model.refreshUsage() }
            model.showingUsage = argument == "open"
        case "limit":
            let (kind, value) = argument.split(separator: ":", maxSplits: 1).map(String.init).pair
            UserDefaults.standard.set(kind, forKey: Preferences.dailyLimitKind)
            if kind == "cost" { UserDefaults.standard.set(Double(value) ?? 0, forKey: Preferences.dailyLimitCost) }
            if kind == "tokens" { UserDefaults.standard.set(Double(value) ?? 0, forKey: Preferences.dailyLimitTokens) }
        case "search":
            model.searchText = argument
        case "more":
            await model.loadMoreSessions()
        case "refresh":
            await model.refreshSessions()
        case "dump":
            dump(to: argument)
        case "snap":
            snapshot(to: argument)
        default:
            break
        }
    }

    private func waitUntil(timeout: Double, _ condition: () -> Bool) async {
        let deadline = ContinuousClock.now + .seconds(timeout)
        while !condition(), ContinuousClock.now < deadline {
            try? await Task.sleep(for: .milliseconds(50))
        }
    }

    private func paths(_ list: String) -> [NSURL] {
        list.split(separator: ",").map { NSURL(fileURLWithPath: String($0)) }
    }

    private func bringToFront() {
        NSApp.activate()
        model.mainWindow?.makeKeyAndOrderFront(nil)
    }

    private func pressPaste() async {
        bringToFront()
        model.pasteSource = pasteboard
        post("v", modifiers: .command, keyCode: 9)
        try? await Task.sleep(for: .milliseconds(500))
        model.pasteSource = .general
    }

    /// Queue a key press exactly as the window server would deliver it.
    private func post(_ characters: String, modifiers: NSEvent.ModifierFlags = [], keyCode: UInt16 = 0) {
        guard let window = model.mainWindow,
              let event = NSEvent.keyEvent(
                  with: .keyDown, location: .zero, modifierFlags: modifiers,
                  timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber, context: nil,
                  characters: characters, charactersIgnoringModifiers: characters, isARepeat: false, keyCode: keyCode)
        else { return }
        NSApp.postEvent(event, atStart: false)
    }

    /// Render the window's own view tree to a PNG. Unlike a screen capture this works with
    /// the screen locked and without Screen Recording permission, at the cost of system
    /// materials (glass, vibrancy) not being composited.
    private func snapshot(to path: String) {
        guard let window = model.mainWindow, let view = window.contentView?.superview ?? window.contentView,
              let bitmap = view.bitmapImageRepForCachingDisplay(in: view.bounds)
        else { return }
        view.cacheDisplay(in: view.bounds, to: bitmap)
        try? bitmap.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: path))
    }

    private func dump(to path: String) {
        let usage = UsageSnapshot(summary: model.usage)
        let items: [[String: Any]] = chat.items.map { item in
            [
                "role": item.role == .user ? "user" : "assistant",
                "text": item.plainText,
                "streaming": item.isStreaming,
                "time": item.timestamp.map { MessageTime.label(for: $0) } ?? "",
                "rowId": item.rowId.map { Int($0) } ?? -1,
                "attachments": item.attachments.map { ["name": $0.name, "image": $0.isImage, "staged": $0.source == .remote] },
                "local": item.isLocalOnly,
                "canRewrite": item.canRewrite,
                "tools": item.parts.compactMap { part -> String? in
                    if case .tool(let call) = part { return call.name }
                    return nil
                },
                "citations": item.parts.flatMap { part -> [String] in
                    guard case .text(let segment) = part else { return [] }
                    return segment.document.blocks.flatMap { block -> [String] in
                        switch block.kind {
                        case .paragraph(let runs), .heading(_, let runs):
                            return runs.filter(\.citation).map { "\($0.text)=\($0.link ?? "")" }
                        default:
                            return []
                        }
                    }
                },
                "reasoning": item.parts.compactMap { part -> String? in
                    if case .reasoning(let segment) = part { return segment.text }
                    return nil
                },
                "steps": item.parts.map { part -> String in
                    switch part {
                    case .text: "text"
                    case .reasoning: "reasoning"
                    case .tool(let call): "tool:\(call.name)"
                    case .notice: "notice"
                    case .output: "output"
                    }
                },
                "notices": item.parts.compactMap { part -> String? in
                    if case .notice(_, let text, _) = part { return text }
                    return nil
                },
            ]
        }
        let state: [String: Any] = [
            "draft": chat.draft,
            "focusInTextField": model.mainWindow?.firstResponder is NSText,
            "windowIsKey": model.mainWindow?.isKeyWindow ?? false,
            "pendingAttachments": chat.attachments.map {
                ["name": $0.name, "image": $0.isImage, "bytes": $0.byteCount, "hasThumbnail": $0.thumbnail != nil]
            },
            "attachmentError": chat.attachmentError ?? "",
            "isStreaming": chat.isStreaming,
            "editing": chat.editingItemID != nil,
            "items": items,
            "searchText": model.searchText,
            "commandCount": model.commands.count,
            "commandMenu": chat.commandSuggestions.map(\.title),
            "commandHighlighted": chat.highlightedSuggestion?.title ?? "",
            "signedIn": model.phase == .ready,
            "connection": String(describing: model.connection),
            "sessionTimes": model.sessions.prefix(8).map(\.shortTime),
            "sessionCount": model.sessions.count,
            "sessionSources": Array(Set(model.sessions.map(\.source))).sorted(),
            "hasMoreSessions": model.hasMoreSessions,
            "sessionsError": model.sessionsError ?? "",
            "status": chat.status ?? "",
            "usage": [
                "loaded": model.usage != nil,
                "todayCost": usage.cost,
                "todayTokens": Int(usage.tokens),
                "label": usage.shortLabel,
                "fraction": usage.fraction ?? -1,
                "limit": usage.limitDescription ?? "",
                "days": model.usage?.days.count ?? 0,
                "plan": model.usage?.plan?.planName ?? "",
            ],
        ]
        if let data = try? JSONSerialization.data(withJSONObject: state, options: [.prettyPrinted, .sortedKeys]) {
            try? data.write(to: URL(fileURLWithPath: path))
        }
    }
}

private extension Array where Element == String {
    /// The two halves of a `command:argument` split; the argument may be absent.
    var pair: (String, String) { (first ?? "", count > 1 ? self[1] : "") }
}
