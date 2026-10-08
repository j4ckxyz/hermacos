import AppKit
import Foundation
import HermesCore
import Observation

/// App-wide state: the signed-in connection, the session list, and which chat is on screen.
@MainActor @Observable
final class AppModel {
    enum Phase {
        case launching
        case signedOut
        case ready
    }

    enum Selection: Hashable {
        case newChat
        case session(String)
        case status
    }

    struct SessionGroup: Identifiable {
        let id: String
        var sessions: [SessionSummary]
    }

    var phase: Phase = .launching
    var account: Account?
    var connection: ConnectionState = .disconnected
    var sessions: [SessionSummary] = []
    var hasLoadedSessions = false
    /// Why the chat list couldn't be loaded, shown in the sidebar.
    var sessionsError: String?
    /// The server has older chats beyond those loaded.
    var hasMoreSessions = false
    @ObservationIgnored private var isLoadingMoreSessions = false
    /// Chats fetched per request; the server serves no more than this at once.
    private static let sessionPage = 100
    var selection: Selection = .newChat {
        didSet { if selection != oldValue { selectionChanged() } }
    }
    /// The conversation shown in the detail pane.
    var chat: ChatModel!
    var searchText = "" {
        didSet { if searchText != oldValue { searchChanged() } }
    }
    var searchHits: [SearchHit] = []
    /// Why the user was signed out, shown on the sign-in screen.
    var signInNotice: String?
    var renaming: SessionSummary?
    var pendingDelete: SessionSummary?

    /// Bumped to move keyboard focus; views watch these.
    var composerFocusRequests = 0
    var searchFocusRequests = 0

    /// Slash commands the server offers, for the command menu.
    var commands: [SlashCommand] = []
    /// Token and cost totals for the sidebar's usage ring; nil until first loaded.
    var usage: UsageSummary?
    var showingUsage = false
    /// Show hover-only controls without a pointer (screenshots).
    var revealHoverControls = false
    /// Show every activity list expanded (screenshots).
    var expandActivity = false
    /// Bumped to scroll the transcript to its start (screenshots).
    var scrollToTopRequests = 0

    /// The chat window, so keystrokes can be told apart from those meant for sheets and
    /// other windows.
    @ObservationIgnored weak var mainWindow: NSWindow?
    /// Where ⌘V reads from. Tests substitute a private pasteboard.
    @ObservationIgnored var pasteSource: NSPasteboard = .general
    @ObservationIgnored private var keyMonitor: Any?
    /// Typing was just redirected to the message field and the field is still taking focus.
    @ObservationIgnored private var handingOffToComposer = false
    @ObservationIgnored private var usageTask: Task<Void, Never>?

    @ObservationIgnored private(set) var client: HermesClient?
    @ObservationIgnored private var bridge: ListenerBridge?
    @ObservationIgnored private var signalTask: Task<Void, Never>?
    @ObservationIgnored private var searchTask: Task<Void, Never>?
    @ObservationIgnored private var refreshTask: Task<Void, Never>?
    /// Chats with state worth keeping (a transcript, a turn in flight), by stored id.
    @ObservationIgnored private var chats: [String: ChatModel] = [:]
    @ObservationIgnored private var liveChats: [String: ChatModel] = [:]
    @ObservationIgnored private var draftChat: ChatModel!
    @ObservationIgnored private var hasConnectedOnce = false

    var reduceMotion: Bool { NSWorkspace.shared.accessibilityDisplayShouldReduceMotion }

    init() {
        Preferences.register()
        draftChat = ChatModel(app: self)
        chat = draftChat
    }

    // MARK: Launch and sign-in

    func bootstrap() {
        guard phase == .launching else { return }
        installKeyRouter()
        if let (account, tokens) = AccountStore.load() {
            start(account: account, tokens: tokens)
            if let script = ProcessInfo.processInfo.environment["HERMACOS_SCRIPT"] {
                Task { await Automation(model: self).run(script) }
            }
        } else {
            phase = .signedOut
            runLaunchAutomation()
        }
    }

    func signedIn(server: ServerInfo, tokens: AuthTokens) {
        let account = Account(baseURL: server.baseUrl, host: server.host, userId: tokens.userId, version: server.version)
        AccountStore.save(account: account, tokens: tokens)
        signInNotice = nil
        start(account: account, tokens: tokens)
    }

    private func start(account: Account, tokens: AuthTokens) {
        let bridge = ListenerBridge()
        do {
            client = try HermesClient(baseUrl: account.baseURL, tokens: tokens, listener: bridge)
        } catch {
            signInNotice = error.userMessage
            phase = .signedOut
            return
        }
        self.bridge = bridge
        self.account = account
        hasConnectedOnce = false
        signalTask = Task { [weak self] in
            for await signal in bridge.signals {
                self?.receive(signal)
            }
        }
        resetChats()
        phase = .ready
        Task {
            async let connected: Void? = try? client?.connect()
            await refreshSessions()
            _ = await connected
            await refreshUsage()
        }
        usageTask = Task { [weak self] in
            // Other surfaces (Discord, cron) spend too; keep the ring roughly current.
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(300))
                await self?.refreshUsage()
            }
        }
    }

    func signOut(forgetServer: Bool = false) {
        teardown()
        if forgetServer {
            AccountStore.forget()
        } else if let account {
            AccountStore.clearTokens(for: account)
        }
        account = nil
        phase = .signedOut
    }

    private func teardown() {
        client?.disconnect()
        client = nil
        bridge?.finish()
        bridge = nil
        signalTask?.cancel()
        searchTask?.cancel()
        refreshTask?.cancel()
        usageTask?.cancel()
        usage = nil
        commands = []
        showingUsage = false
        sessionsError = nil
        hasMoreSessions = false
        sessions = []
        hasLoadedSessions = false
        searchText = ""
        connection = .disconnected
        resetChats()
    }

    private func resetChats() {
        chats = [:]
        liveChats = [:]
        draftChat = ChatModel(app: self)
        chat = draftChat
        selection = .newChat
    }

    // MARK: Signals from the core

    private func receive(_ signal: CoreSignal) {
        switch signal {
        case .tokens(let tokens):
            if let account { AccountStore.save(account: account, tokens: tokens) }

        case .connection(let state):
            let wasConnected = connection == .connected
            connection = state
            switch state {
            case .connected:
                if hasConnectedOnce { scheduleRefresh() }
                hasConnectedOnce = true
                Task { await refreshCommands() }
            case .reconnecting, .disconnected:
                if wasConnected { dropLiveSessions() }
            case .unauthorized:
                signInNotice = "Your session expired. Sign in again."
                signOut()
            case .connecting:
                break
            }

        case .event(let event):
            route(event)
        }
    }

    private func dropLiveSessions() {
        let affected = Set(liveChats.values.map(ObjectIdentifier.init))
        for chat in chats.values where affected.contains(ObjectIdentifier(chat)) { chat.connectionLost() }
        if affected.contains(ObjectIdentifier(draftChat)) { draftChat.connectionLost() }
        liveChats = [:]
    }

    private func route(_ event: ChatEvent) {
        switch event {
        case .titleChanged(let storedId, let title):
            guard !title.isEmpty else { return }
            chats[storedId]?.title = title
            if let index = sessions.firstIndex(where: { $0.id == storedId }) { sessions[index].title = title }
        case .sessionsChanged:
            scheduleRefresh()
        case .turnStarted(let id), .textDelta(let id, _), .reasoningDelta(let id, _),
             .reasoningAvailable(let id, _), .segmentBreak(let id),
             .toolStarted(let id, _), .toolCompleted(let id, _), .status(let id, _, _),
             .turnCompleted(let id, _, _, _), .modelChanged(let id, _), .notice(let id, _), .failure(let id, _):
            liveChats[id]?.handle(event)
        case .approval(let request):
            liveChats[request.sessionId]?.handle(event)
        case .clarify(let request):
            liveChats[request.sessionId]?.handle(event)
        case .input(let request):
            (liveChats[request.sessionId] ?? chat)?.handle(event)
        case .requestCancelled:
            for chat in liveChats.values { chat.handle(event) }
        }
    }

    // MARK: Chats

    /// A chat obtained a live session id.
    func register(_ chat: ChatModel) {
        if let liveId = chat.liveId { liveChats[liveId] = chat }
        if let storedId = chat.storedId { chats[storedId] = chat }
    }

    /// A new chat sent its first prompt and now exists on the server.
    func chatWasCreated(_ chat: ChatModel) {
        guard let storedId = chat.storedId else { return }
        chats[storedId] = chat
        if chat === draftChat { draftChat = ChatModel(app: self) }
        let now = Date().timeIntervalSince1970
        if !sessions.contains(where: { $0.id == storedId }) {
            sessions.insert(
                SessionSummary(id: storedId, title: chat.title, preview: "", source: "desktop", startedAt: now,
                               lastActive: now, messageCount: 1, isActive: true, model: chat.modelName),
                at: 0
            )
        }
        if self.chat === chat { selection = .session(storedId) }
    }

    func turnFinished(_ chat: ChatModel) {
        scheduleRefresh()
    }

    func newChat() {
        guard phase == .ready else { return }
        if selection == .newChat, chat === draftChat {
            // Already on the new chat. If it has something in it (command output, a draft
            // that was never sent as a message), start it over.
            if !chat.items.isEmpty {
                draftChat = ChatModel(app: self)
                chat = draftChat
            }
            composerFocusRequests += 1
            return
        }
        selection = .newChat
    }

    private func selectionChanged() {
        switch selection {
        case .newChat:
            chat = draftChat
            composerFocusRequests += 1
        case .session(let id):
            if let existing = chats[id] {
                chat = existing
            } else {
                let title = sessions.first { $0.id == id }?.title ?? ""
                let opened = ChatModel(app: self, storedId: id, title: title)
                chats[id] = opened
                chat = opened
                trimChatCache(keeping: id)
            }
            let current = chat!
            Task { await current.loadHistory() }
        case .status:
            break
        }
    }

    /// Keep memory flat: forget transcripts of chats that are neither visible nor busy.
    private func trimChatCache(keeping id: String) {
        guard chats.count > 6 else { return }
        for (key, cached) in chats where key != id && !cached.isStreaming && !cached.hasPendingRequest {
            if let liveId = cached.liveId {
                liveChats[liveId] = nil
                let client = client
                Task { try? await client?.closeSession(sessionId: liveId) }
            }
            chats[key] = nil
            if chats.count <= 4 { break }
        }
    }

    func selectAdjacentSession(offset: Int) {
        let ordered = sessions.map(\.id)
        guard !ordered.isEmpty else { return }
        guard case .session(let current) = selection, let index = ordered.firstIndex(of: current) else {
            selection = .session(offset > 0 ? ordered[0] : ordered[ordered.count - 1])
            return
        }
        let next = index + offset
        if ordered.indices.contains(next) { selection = .session(ordered[next]) }
    }

    // MARK: Session list

    func refreshSessions() async {
        guard let client else { return }
        // Reload as many as are on screen, so a refresh never shortens a list the user scrolled.
        let pages = max(1, (sessions.count + Self.sessionPage - 1) / Self.sessionPage)
        let wanted = pages * Self.sessionPage
        do {
            let fetched = try await client.listSessions(limit: UInt32(wanted), offset: 0)
            sessions = fetched
            hasMoreSessions = fetched.count >= wanted
            sessionsError = nil
            hasLoadedSessions = true
        } catch let error as HermesError where error.isUnauthorized {
            // The connection handler signs the user out.
        } catch {
            sessionsError = error.userMessage
            hasLoadedSessions = true
        }
    }

    /// Fetch the next page of older chats (the sidebar asks as its end scrolls into view).
    func loadMoreSessions() async {
        guard let client, hasMoreSessions, !isLoadingMoreSessions else { return }
        isLoadingMoreSessions = true
        defer { isLoadingMoreSessions = false }
        do {
            let more = try await client.listSessions(limit: UInt32(Self.sessionPage), offset: UInt32(sessions.count))
            let known = Set(sessions.map(\.id))
            sessions.append(contentsOf: more.filter { !known.contains($0.id) })
            hasMoreSessions = more.count >= Self.sessionPage
        } catch {
            hasMoreSessions = false
            sessionsError = error.userMessage
        }
    }

    /// Coalesce bursts of "something changed" into one reload.
    func scheduleRefresh() {
        refreshTask?.cancel()
        refreshTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(400))
            guard !Task.isCancelled else { return }
            await self?.refreshSessions()
            await self?.refreshUsage()
        }
    }

    // MARK: Slash commands

    func refreshCommands() async {
        guard let client else { return }
        if let catalog = try? await client.commands(sessionId: nil), !catalog.isEmpty { commands = catalog }
    }

    // MARK: Usage

    func refreshUsage() async {
        guard let client else { return }
        if let summary = try? await client.usage(days: 30) { usage = summary }
    }

    // MARK: Typing and pasting anywhere in the window

    /// Watch key presses so that typing with nothing focused starts a message, and ⌘V puts
    /// files, images or text into the composer wherever the focus is.
    private func installKeyRouter() {
        guard keyMonitor == nil else { return }
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self else { return event }
            return MainActor.assumeIsolated { self.route(event) } ? nil : event
        }
    }

    /// Returns true when the key press was used here and must not travel further.
    private func route(_ event: NSEvent) -> Bool {
        guard phase == .ready, selection != .status,
              let window = mainWindow, event.window === window, window.attachedSheet == nil
        else { return false }
        let modifiers = event.modifierFlags.intersection([.command, .control, .option, .shift])
        let typingInField = window.firstResponder is NSText && !handingOffToComposer

        if modifiers == .command, event.charactersIgnoringModifiers?.lowercased() == "v" {
            return paste(from: pasteSource, typingInField: typingInField)
        }

        // Plain typing with no text field focused goes to the message field.
        guard !typingInField, modifiers.isDisjoint(with: [.command, .control]),
              let characters = event.characters, !characters.isEmpty,
              characters.unicodeScalars.allSatisfy(Self.isTypeable),
              // A leading space is more likely "scroll the page" than the start of a message.
              !(chat.draft.isEmpty && characters.allSatisfy(\.isWhitespace))
        else { return false }
        chat.draft.append(characters)
        handOffToComposer(in: window)
        return true
    }

    /// Focus the message field and keep routing keys here until its caret sits at the end.
    ///
    /// A text field selects all of its text when it takes focus, so the key after the one that
    /// triggered the focus would replace what was typed so far. Until the field is ready, the
    /// keys keep being appended to the draft here instead.
    private func handOffToComposer(in window: NSWindow) {
        composerFocusRequests += 1
        guard !handingOffToComposer else { return }
        handingOffToComposer = true
        Task {
            for _ in 0..<80 {
                if window.firstResponder is NSTextView { break }
                try? await Task.sleep(for: .milliseconds(10))
            }
            // Twice, a moment apart: the field is still receiving the redirected text.
            for _ in 0..<2 {
                try? await Task.sleep(for: .milliseconds(30))
                if let editor = window.firstResponder as? NSTextView {
                    editor.setSelectedRange(NSRange(location: editor.string.utf16.count, length: 0))
                }
            }
            handingOffToComposer = false
        }
    }

    /// Printable, as opposed to arrows, function keys, Return, Tab, Escape and Delete.
    private static func isTypeable(_ scalar: Unicode.Scalar) -> Bool {
        scalar.value >= 0x20 && scalar.value != 0x7F && !(0xF700...0xF8FF).contains(scalar.value)
    }

    /// Put the pasteboard's contents into the current chat's composer. Files and images
    /// become attachments from anywhere in the window; plain text is left to a focused text
    /// field and otherwise appended to the draft.
    @discardableResult
    func paste(from pasteboard: NSPasteboard, typingInField: Bool) -> Bool {
        guard phase == .ready, selection != .status else { return false }
        let files = pasteboard.fileURLs
        if !files.isEmpty {
            chat.addFiles(files)
        } else if let image = pasteboard.imagePayload {
            chat.addImage(data: image.data, name: image.name)
        } else if !typingInField, let text = pasteboard.string(forType: .string), !text.isEmpty {
            chat.draft.append(text)
        } else {
            return false
        }
        composerFocusRequests += 1
        return true
    }

    func rename(_ session: SessionSummary, to title: String) {
        let title = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !title.isEmpty, title != session.title, let client else { return }
        if let index = sessions.firstIndex(where: { $0.id == session.id }) { sessions[index].title = title }
        chats[session.id]?.title = title
        Task {
            try? await client.renameSession(sessionId: session.id, title: title)
            await refreshSessions()
        }
    }

    func delete(_ session: SessionSummary) {
        guard let client else { return }
        sessions.removeAll { $0.id == session.id }
        if let cached = chats.removeValue(forKey: session.id), let liveId = cached.liveId { liveChats[liveId] = nil }
        if selection == .session(session.id) { selection = .newChat }
        Task {
            try? await client.deleteSession(sessionId: session.id)
            await refreshSessions()
        }
    }

    var selectedSession: SessionSummary? {
        guard case .session(let id) = selection else { return nil }
        return sessions.first { $0.id == id }
    }

    /// Sessions bucketed the way people think about recency.
    var groupedSessions: [SessionGroup] {
        let calendar = Calendar.current
        let today = calendar.startOfDay(for: Date())
        let monthFormat = Date.FormatStyle().month(.wide)
        let monthYearFormat = Date.FormatStyle().month(.wide).year()
        var groups: [SessionGroup] = []
        let needle = searchText.trimmingCharacters(in: .whitespaces).lowercased()
        for session in sessions {
            if !needle.isEmpty, !session.displayTitle.lowercased().contains(needle) { continue }
            let date = Date(timeIntervalSince1970: session.lastActive)
            let days = calendar.dateComponents([.day], from: calendar.startOfDay(for: date), to: today).day ?? 0
            let label: String
            switch days {
            case ..<1: label = "Today"
            case 1: label = "Yesterday"
            case 2..<7: label = "Previous 7 Days"
            case 7..<30: label = "Previous 30 Days"
            default:
                let sameYear = calendar.isDate(date, equalTo: today, toGranularity: .year)
                label = date.formatted(sameYear ? monthFormat : monthYearFormat)
            }
            if groups.last?.id == label {
                groups[groups.count - 1].sessions.append(session)
            } else {
                groups.append(SessionGroup(id: label, sessions: [session]))
            }
        }
        return groups
    }

    // MARK: Search

    private func searchChanged() {
        searchTask?.cancel()
        let query = searchText.trimmingCharacters(in: .whitespaces)
        guard query.count >= 2, let client else {
            searchHits = []
            return
        }
        searchTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(280))
            guard !Task.isCancelled else { return }
            let hits = (try? await client.searchSessions(query: query)) ?? []
            guard !Task.isCancelled else { return }
            self?.searchHits = hits
        }
    }

    /// Full-text hits in conversations whose titles didn't already match.
    var contentHits: [SearchHit] {
        let titled = Set(groupedSessions.flatMap(\.sessions).map(\.id))
        var seen = Set<String>()
        return searchHits.filter { !titled.contains($0.sessionId) && seen.insert($0.sessionId).inserted }
    }

    // MARK: Development automation

    /// `HERMACOS_AUTOLOGIN="url|user|password"` signs in at launch; `HERMACOS_SCRIPT` then
    /// drives the app (see `Automation`).
    private func runLaunchAutomation() {
        let env = ProcessInfo.processInfo.environment
        guard let spec = env["HERMACOS_AUTOLOGIN"] else { return }
        let fields = spec.split(separator: "|", maxSplits: 2, omittingEmptySubsequences: false).map(String.init)
        guard fields.count == 3 else { return }
        Task {
            do {
                let server = try await probeServer(url: fields[0])
                let tokens = server.authRequired
                    ? try await loginPassword(baseUrl: server.baseUrl, provider: server.providers.first?.name ?? "basic",
                                              username: fields[1], password: fields[2])
                    : try await loginOpen(baseUrl: server.baseUrl)
                signedIn(server: server, tokens: tokens)
                if let script = env["HERMACOS_SCRIPT"] {
                    await Automation(model: self).run(script)
                }
            } catch {
                signInNotice = error.userMessage
            }
        }
    }
}

extension SessionSummary {
    var displayTitle: String {
        if !title.isEmpty { return title }
        let line = preview.split(whereSeparator: \.isNewline).first.map(String.init) ?? ""
        return line.isEmpty ? "Untitled" : line
    }

    /// SF Symbol for where the conversation happened; nil for this app's own chats.
    var sourceSymbol: String? {
        switch source.lowercased() {
        case "", "desktop", "tui", "dashboard", "web": nil
        case "cli", "terminal": "terminal"
        case "cron", "scheduler": "clock"
        case "discord", "telegram", "slack", "matrix", "whatsapp", "signal", "sms", "imessage": "bubble.left.and.bubble.right"
        case "email": "envelope"
        case "api", "api_server", "webhook": "network"
        default: "ellipsis.bubble"
        }
    }

    /// Compact "when" for a sidebar row: a time today, a weekday this week, else a date.
    var shortTime: String {
        guard lastActive > 0 else { return "" }
        let date = Date(timeIntervalSince1970: lastActive)
        let calendar = Calendar.current
        if calendar.isDateInToday(date) { return date.formatted(date: .omitted, time: .shortened) }
        let days = calendar.dateComponents([.day], from: calendar.startOfDay(for: date), to: calendar.startOfDay(for: Date())).day ?? 0
        if days < 7 { return date.formatted(.dateTime.weekday(.abbreviated)) }
        if calendar.isDate(date, equalTo: Date(), toGranularity: .year) { return date.formatted(.dateTime.day().month(.abbreviated)) }
        return date.formatted(.dateTime.month(.abbreviated).year())
    }

    var sourceLabel: String {
        source.isEmpty ? "Desktop" : source.prefix(1).uppercased() + source.dropFirst()
    }
}
