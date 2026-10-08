import Foundation
import HermesCore
import Observation

/// A run of assistant markdown. While it streams, a pacer reveals it smoothly and the document
/// is re-parsed for the visible prefix each frame.
@MainActor @Observable
final class TextSegment: Identifiable {
    let id: String
    var document: MdDocument
    /// Opacity of the newest glyphs that are still fading in, oldest first.
    var fade: [Float] = []
    var isStreaming: Bool
    @ObservationIgnored let pacer: StreamPacer?
    @ObservationIgnored private var settledSource: String

    /// A finished segment from history.
    init(id: String, text: String) {
        self.id = id
        self.document = parseMarkdown(text: text, streaming: false)
        self.isStreaming = false
        self.pacer = nil
        self.settledSource = text
    }

    /// A live segment that text will be appended to.
    init(id: String) {
        self.id = id
        self.document = MdDocument(blocks: [], links: [])
        self.isStreaming = true
        self.pacer = StreamPacer()
        self.settledSource = ""
    }

    /// The full markdown source, including text not yet revealed.
    var source: String { pacer?.text() ?? settledSource }
}

@MainActor @Observable
final class ReasoningSegment: Identifiable {
    let id: String
    var text: String
    var isStreaming: Bool

    init(id: String, text: String, isStreaming: Bool) {
        self.id = id
        self.text = text
        self.isStreaming = isStreaming
    }
}

enum ItemPart: Identifiable {
    case text(TextSegment)
    case reasoning(ReasoningSegment)
    case tool(ToolCall)
    case notice(id: String, text: String, isError: Bool)
    /// What a slash command printed: preformatted, shown as it came.
    case output(id: String, text: String)

    var id: String {
        switch self {
        case .text(let segment): segment.id
        case .reasoning(let segment): segment.id
        case .tool(let call): "tool-\(call.id)"
        case .notice(let id, _, _): id
        case .output(let id, _): id
        }
    }
}

/// One message on screen: the user's prompt, or a whole assistant turn.
@MainActor @Observable
final class ChatItem: Identifiable {
    let id: String
    let role: Role
    var parts: [ItemPart]
    var isStreaming: Bool
    /// When the message was sent (user) or finished (assistant).
    var timestamp: Date?
    /// Media the user attached.
    var attachments: [Attachment]
    /// Stored id of a user message: its address when it is rewritten.
    @ObservationIgnored var rowId: Int64?
    /// Exists only on this screen (a slash command and its output), not in the conversation
    /// the server keeps.
    @ObservationIgnored var isLocalOnly = false
    /// False for messages that were commands; editing their text would not rerun them.
    var canRewrite = true

    init(id: String, role: Role, parts: [ItemPart], isStreaming: Bool = false, timestamp: Date? = nil,
         attachments: [Attachment] = [], rowId: Int64? = nil) {
        self.id = id
        self.role = role
        self.parts = parts
        self.isStreaming = isStreaming
        self.timestamp = timestamp
        self.attachments = attachments
        self.rowId = rowId
    }

    convenience init(_ message: ChatMessage) {
        let parts: [ItemPart] = message.parts.enumerated().map { index, part in
            let partID = "\(message.id)-\(index)"
            switch part {
            case .text(let text): return .text(TextSegment(id: partID, text: text))
            case .reasoning(let text): return .reasoning(ReasoningSegment(id: partID, text: text, isStreaming: false))
            case .tool(let call):
                var call = call
                if call.id.isEmpty { call.id = partID }
                return .tool(call)
            case .notice(let text): return .notice(id: partID, text: text, isError: false)
            }
        }
        self.init(
            id: message.id, role: message.role, parts: parts,
            timestamp: message.timestamp > 0 ? Date(timeIntervalSince1970: message.timestamp) : nil,
            attachments: message.attachments.map(Attachment.init(stored:)),
            rowId: message.rowId
        )
    }

    /// Plain text of the message, for copying.
    var plainText: String {
        parts.compactMap { part -> String? in
            switch part {
            case .text(let segment): segment.source
            case .output(_, let text): text
            case .reasoning, .tool, .notice: nil
            }
        }
        .joined(separator: "\n\n")
        .trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

/// One conversation: its transcript, the turn in flight, and anything the agent is waiting on.
@MainActor @Observable
final class ChatModel: Identifiable {
    let id = UUID()
    /// Durable id; nil until the first prompt of a new chat creates the session.
    var storedId: String?
    /// Runtime id on the current gateway connection.
    @ObservationIgnored var liveId: String?
    var title: String
    var items: [ChatItem] = []
    var isLoadingHistory = false
    var loadError: String?
    var isStreaming = false
    /// What the agent is doing right now ("Thinking…", a tool name).
    var status: String?
    var draft = "" {
        // A new draft means a new list of suggestions: start from its first row again.
        didSet { if draft != oldValue { commandMenuSelection = 0 } }
    }
    /// Files and images waiting in the composer for the next message.
    var attachments: [Attachment] = []
    /// Why the last attachment couldn't be added.
    var attachmentError: String?
    /// The user message being rewritten in place, if any.
    var editingItemID: String?
    /// Highlighted row of the slash-command menu.
    var commandMenuSelection = 0
    /// The draft the menu was dismissed for; it stays closed until the draft changes.
    var commandMenuDismissedFor: String?
    /// A session this app created that the sidebar has not been told about yet.
    @ObservationIgnored private var needsAnnouncement = false
    var modelName: String?
    var approval: ApprovalRequest?
    var clarify: ClarifyRequest?
    var input: InputRequest?

    @ObservationIgnored weak var app: AppModel?
    @ObservationIgnored private var animating: [TextSegment] = []
    @ObservationIgnored private var counter = 0
    @ObservationIgnored private lazy var clock = FrameClock { [weak self] delta in self?.advance(by: delta) }

    init(app: AppModel, storedId: String? = nil, title: String = "") {
        self.app = app
        self.storedId = storedId
        self.title = title
    }

    var isEmpty: Bool { items.isEmpty && !isLoadingHistory }
    var hasPendingRequest: Bool { approval != nil || clarify != nil || input != nil }

    private func nextID(_ prefix: String) -> String {
        counter += 1
        return "\(prefix)-\(id.uuidString.prefix(8))-\(counter)"
    }

    // MARK: History

    func loadHistory() async {
        guard let storedId, let client = app?.client, items.isEmpty, !isLoadingHistory else { return }
        isLoadingHistory = true
        loadError = nil
        defer { isLoadingHistory = false }
        do {
            let messages = try await client.loadMessages(sessionId: storedId)
            // Don't clobber a turn that started while the transcript was loading.
            if items.isEmpty { items = messages.map(ChatItem.init) }
        } catch {
            loadError = error.userMessage
        }
    }

    // MARK: Attachments

    func addFiles(_ urls: [URL]) {
        attachmentError = nil
        for url in urls {
            guard !attachments.contains(where: { $0.source == .file(url) }) else { continue }
            add { try Attachment(fileURL: url) }
        }
    }

    func addImage(data: Data, name: String) {
        attachmentError = nil
        add { try Attachment(imageData: data, name: name) }
    }

    private func add(_ make: () throws -> Attachment) {
        guard attachments.count < Self.maxAttachments else {
            attachmentError = "A message can carry up to \(Self.maxAttachments) attachments."
            return
        }
        do {
            attachments.append(try make())
        } catch {
            attachmentError = error.localizedDescription
        }
    }

    func removeAttachment(_ id: UUID) {
        attachments.removeAll { $0.id == id }
        attachmentError = nil
    }

    private static let maxAttachments = 10

    // MARK: Sending

    var canSend: Bool {
        !isStreaming && (!draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !attachments.isEmpty)
    }

    func send() {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        let pending = attachments
        guard canSend, let app, let client = app.client else { return }
        if pending.isEmpty, CommandSuggester.isCommand(text, commands: app.commands) {
            runCommand(text)
            return
        }
        draft = ""
        attachments = []
        attachmentError = nil
        let message = userItem(text: text, attachments: pending)
        items.append(message)
        beginTurn(status: pending.isEmpty ? "Thinking…" : "Uploading…")
        Task { await deliver(message, text: text, rewind: nil, client: client, app: app) }
    }

    private func userItem(text: String, attachments: [Attachment]) -> ChatItem {
        ChatItem(
            id: nextID("user"), role: .user,
            parts: text.isEmpty ? [] : [.text(TextSegment(id: nextID("text"), text: text))],
            timestamp: Date(), attachments: attachments
        )
    }

    private func beginTurn(status: String) {
        items.append(ChatItem(id: nextID("assistant"), role: .assistant, parts: [], isStreaming: true))
        isStreaming = true
        self.status = status
    }

    /// Where a rewritten message sat: by stored id when known, else by position among the
    /// user's messages.
    private struct Rewind {
        var rowId: Int64?
        var ordinal: UInt32
    }

    /// Upload the message's attachments and submit it, as a new message or in place of an
    /// earlier one.
    private func deliver(_ message: ChatItem, text: String, rewind: Rewind?, client: HermesClient, app: AppModel) async {
        var retried = false
        while true {
            do {
                let title = text.isEmpty ? (message.attachments.first?.name ?? "Attachment") : text
                try await bind(client: client, app: app)
                announce(to: app, firstPrompt: title)
                let liveId = liveId ?? ""
                try await stage(message, rewind: rewind, liveId: liveId, client: client)
                if isStreaming { status = "Thinking…" }
                let prompt = Self.compose(text: text, attachments: message.attachments)
                if let rewind {
                    message.rowId = try await client.rewritePrompt(
                        sessionId: liveId, text: prompt, rowId: rewind.rowId, userOrdinal: rewind.ordinal)
                } else {
                    message.rowId = try await client.sendPrompt(sessionId: liveId, text: prompt)
                }
                return
            } catch let error as HermesError where !retried && storedId != nil && error.isStaleSession {
                // The runtime id went stale (server restarted, session reaped): rebind once.
                // Attachments are staged again on the new runtime session.
                retried = true
                liveId = nil
            } catch {
                if rewind != nil {
                    await restoreAfterFailedRewrite(error.userMessage)
                } else {
                    fail(error.userMessage)
                }
                return
            }
        }
    }

    /// Put every attachment of `message` in front of the agent for the next prompt.
    private func stage(_ message: ChatItem, rewind: Rewind?, liveId: String, client: HermesClient) async throws {
        var imagesOnlyInHistory = false
        for index in message.attachments.indices {
            var attachment = message.attachments[index]
            switch attachment.source {
            case .file, .data:
                let payload = try attachment.payload()
                let staged = try await client.attach(sessionId: liveId, name: payload.name, mime: payload.mime, data: payload.data)
                attachment.refText = staged.refText
                attachment.serverPath = staged.serverPath
                // The server has it now; let go of the bytes.
                attachment.source = .remote
            case .remote:
                guard attachment.isImage else { break }  // staged files travel as `refText`
                if let path = attachment.serverPath {
                    _ = try await client.attachServerImage(sessionId: liveId, path: path)
                } else {
                    imagesOnlyInHistory = true
                }
            }
            message.attachments[index] = attachment
        }
        // Images of a message loaded from history are fetched back from the transcript.
        if imagesOnlyInHistory, let storedId, let rowId = rewind?.rowId {
            let count = try await client.reattachImages(sessionId: liveId, storedId: storedId, rowId: rowId)
            if count == 0 { appendNotice("The images on that message couldn't be carried over.", isError: true) }
        }
    }

    /// File references first, then what the user typed, as the gateway expects.
    static func compose(text: String, attachments: [Attachment]) -> String {
        let references = attachments.compactMap(\.refText).joined(separator: "\n")
        let prompt = [references, text].filter { !$0.isEmpty }.joined(separator: "\n\n")
        if prompt.isEmpty, attachments.contains(where: \.isImage) { return "What do you see in this image?" }
        return prompt
    }

    /// Make sure this chat has a live session on the current connection.
    private func bind(client: HermesClient, app: AppModel) async throws {
        guard liveId == nil else { return }
        if let storedId {
            let live = try await client.resumeSession(storedId: storedId)
            adopt(live)
        } else {
            let live = try await client.createSession()
            storedId = live.storedId
            adopt(live)
            needsAnnouncement = true
        }
    }

    /// Tell the sidebar about a conversation this app started, once it has a real message.
    /// (A slash command alone leaves nothing on the server worth listing.)
    private func announce(to app: AppModel, firstPrompt: String) {
        guard needsAnnouncement else { return }
        needsAnnouncement = false
        if title.isEmpty { title = String(firstPrompt.prefix(60)) }
        app.chatWasCreated(self)
    }

    private func adopt(_ live: LiveSession) {
        liveId = live.sessionId
        if let model = live.model { modelName = model }
        app?.register(self)
    }

    // MARK: Slash commands

    /// Suggestions for the command being typed, empty when the menu should be closed.
    var commandSuggestions: [CommandSuggestion] {
        guard let app, draft != commandMenuDismissedFor, attachments.isEmpty else { return [] }
        return CommandSuggester.suggestions(for: draft, commands: app.commands)
    }

    var highlightedSuggestion: CommandSuggestion? {
        let suggestions = commandSuggestions
        guard !suggestions.isEmpty else { return nil }
        return suggestions[min(max(commandMenuSelection, 0), suggestions.count - 1)]
    }

    func moveCommandSelection(by offset: Int) {
        let count = commandSuggestions.count
        guard count > 0 else { return }
        commandMenuSelection = (min(max(commandMenuSelection, 0), count - 1) + offset + count) % count
    }

    /// Put a suggestion in the message field, ready for arguments or Return.
    func accept(_ suggestion: CommandSuggestion) {
        draft = suggestion.completion
        commandMenuSelection = 0
        app?.composerFocusRequests += 1
    }

    /// Run `/command arguments`: here for the few that are about this app, on the server
    /// for the rest.
    func runCommand(_ line: String) {
        guard let app, let client = app.client else { return }
        let name = line.split(separator: " ").first.map { $0.lowercased() } ?? ""
        draft = ""
        commandMenuSelection = 0
        switch name {
        case "/new", "/clear", "/reset":
            app.newChat()
            return
        default:
            break
        }
        let echo = userItem(text: line, attachments: [])
        echo.isLocalOnly = true
        echo.canRewrite = false
        items.append(echo)
        let result = ChatItem(id: nextID("command"), role: .assistant, parts: [], isStreaming: true)
        result.isLocalOnly = true
        items.append(result)
        if name == "/help" {
            finish(result, with: [.output(id: nextID("output"), text: CommandSuggester.helpText(app.commands))])
            return
        }
        isStreaming = true
        status = "Running \(name)…"
        Task {
            do {
                try await bind(client: client, app: app)
                let outcome = try await client.runSlash(sessionId: liveId ?? "", command: line)
                switch outcome {
                case .output(let text):
                    finish(result, with: [.output(id: nextID("output"), text: text)])
                case .prefill(let message, let notice):
                    draft = message
                    app.composerFocusRequests += 1
                    finish(result, with: notice.map { [.notice(id: nextID("notice"), text: $0, isError: false)] } ?? [])
                case .send(let message, let display, let notice):
                    // The command stands for a prompt: its echo becomes the real message.
                    items.removeAll { $0 === result }
                    if let display, !display.isEmpty {
                        echo.parts = [.text(TextSegment(id: nextID("text"), text: display))]
                    }
                    echo.isLocalOnly = false
                    if let notice, !notice.isEmpty { appendNotice(notice, isError: false) }
                    beginTurn(status: "Thinking…")
                    await deliver(echo, text: message, rewind: nil, client: client, app: app)
                }
            } catch {
                finish(result, with: [.notice(id: nextID("notice"), text: error.userMessage, isError: true)])
            }
        }
    }

    private func finish(_ result: ChatItem, with parts: [ItemPart]) {
        result.parts = parts
        result.isStreaming = false
        result.timestamp = Date()
        if parts.isEmpty { items.removeAll { $0 === result } }
        isStreaming = false
        status = nil
    }

    // MARK: Rewriting an earlier message

    /// Replace one of the user's messages: stop any reply in flight, cut the conversation back
    /// to just before that message, and send `newText` (with the same attachments) instead.
    func rewrite(_ message: ChatItem, text newText: String) {
        let text = newText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard message.role == .user, items.contains(where: { $0 === message }),
              !text.isEmpty || !message.attachments.isEmpty,
              let app, let client = app.client
        else { return }
        editingItemID = nil
        Task {
            if isStreaming {
                stop()
                // The gateway refuses a rewrite until the interrupted turn has settled.
                let deadline = ContinuousClock.now + .seconds(15)
                while isStreaming, ContinuousClock.now < deadline {
                    try? await Task.sleep(for: .milliseconds(40))
                }
            }
            guard let index = items.firstIndex(where: { $0 === message }) else { return }
            let rewind = Rewind(
                rowId: message.rowId,
                ordinal: UInt32(items[..<index].count { $0.role == .user && !$0.isLocalOnly }))
            items.removeSubrange(index...)
            let replacement = userItem(text: text, attachments: message.attachments)
            items.append(replacement)
            beginTurn(status: "Thinking…")
            await deliver(replacement, text: text, rewind: rewind, client: client, app: app)
        }
    }

    /// A rewrite the server refused left the screen ahead of the stored conversation; show
    /// what is really there and say why.
    private func restoreAfterFailedRewrite(_ reason: String) async {
        isStreaming = false
        status = nil
        items = []
        await loadHistory()
        appendNotice("That message couldn't be rewritten. \(reason)", isError: true)
    }

    func stop() {
        guard isStreaming, let liveId, let client = app?.client else { return }
        status = "Stopping…"
        Task { try? await client.interrupt(sessionId: liveId) }
    }

    // MARK: Events

    private var streamingItem: ChatItem {
        if let last = items.last, last.role == .assistant, last.isStreaming { return last }
        let item = ChatItem(id: nextID("assistant"), role: .assistant, parts: [], isStreaming: true)
        items.append(item)
        isStreaming = true
        return item
    }

    private var openTextSegment: TextSegment? {
        guard let last = items.last, last.isStreaming, case .text(let segment)? = last.parts.last,
              segment.pacer != nil, !sealed.contains(segment.id)
        else { return nil }
        return segment
    }

    @ObservationIgnored private var sealed: Set<String> = []

    /// End the current text segment so the next delta starts a new one.
    private func sealText() {
        guard let segment = openTextSegment else { return }
        sealed.insert(segment.id)
        segment.pacer?.finish()
        animate(segment)
    }

    private func sealReasoning() {
        guard let last = items.last, case .reasoning(let segment)? = last.parts.last else { return }
        segment.isStreaming = false
    }

    func handle(_ event: ChatEvent) {
        switch event {
        case .turnStarted:
            _ = streamingItem
            if status == nil { status = "Thinking…" }

        case .textDelta(_, let text):
            let item = streamingItem
            sealReasoning()
            let segment: TextSegment
            if let open = openTextSegment {
                segment = open
            } else {
                // Separate segments already read as separate paragraphs; drop the leading gap.
                let trimmed = text.drop(while: \.isNewline)
                guard !trimmed.isEmpty else { return }
                segment = TextSegment(id: nextID("text"))
                item.parts.append(.text(segment))
                segment.pacer?.append(delta: String(trimmed))
                status = nil
                animate(segment)
                return
            }
            segment.pacer?.append(delta: text)
            status = nil
            animate(segment)

        case .reasoningDelta(_, let text):
            let item = streamingItem
            if case .reasoning(let segment)? = item.parts.last, segment.isStreaming {
                segment.text += text
            } else {
                sealText()
                item.parts.append(.reasoning(ReasoningSegment(id: nextID("reasoning"), text: text, isStreaming: true)))
            }
            status = "Thinking…"

        case .reasoningAvailable(_, let text):
            // A whole response handed over at once, for providers that don't stream. When the
            // response already streamed (as the reply or as reasoning) this is a duplicate.
            guard let item = items.last, item.role == .assistant, item.isStreaming else { return }
            let alreadyShown = item.parts.contains { part in
                switch part {
                case .text(let segment): !segment.source.isEmpty
                case .reasoning: true
                case .tool, .notice, .output: false
                }
            }
            guard !alreadyShown else { return }
            item.parts.append(.reasoning(ReasoningSegment(id: nextID("reasoning"), text: text, isStreaming: false)))

        case .segmentBreak:
            sealText()

        case .toolStarted(_, let call):
            let item = streamingItem
            sealText()
            sealReasoning()
            var call = call
            if call.id.isEmpty { call.id = nextID("call") }
            item.parts.append(.tool(call))
            status = call.title

        case .toolCompleted(_, let call):
            guard let item = items.last(where: { $0.role == .assistant }) else { return }
            let index = item.parts.lastIndex { part in
                if case .tool(let existing) = part {
                    return existing.id == call.id || (call.id.isEmpty && existing.status == .running && existing.name == call.name)
                }
                return false
            }
            guard let index, case .tool(var merged) = item.parts[index] else { return }
            merged.status = call.status
            merged.duration = call.duration ?? merged.duration
            merged.summary = call.summary ?? merged.summary
            merged.detail = merged.detail ?? call.detail
            item.parts[index] = .tool(merged)
            status = "Thinking…"

        case .status(_, _, let text):
            if isStreaming, !text.isEmpty { status = text }

        case .turnCompleted(_, let text, let turnStatus, let error):
            finishTurn(finalText: text, status: turnStatus, error: error)

        case .failure(_, let message):
            if isStreaming { fail(message) } else { appendNotice(message, isError: true) }

        case .notice(_, let message):
            if !message.isEmpty { appendNotice(message, isError: false) }

        case .modelChanged(_, let model):
            modelName = model

        case .approval(let request):
            approval = request
        case .clarify(let request):
            clarify = request
        case .input(let request):
            input = request
        case .requestCancelled(let requestId):
            if approval?.requestId == requestId { approval = nil }
            if clarify?.requestId == requestId { clarify = nil }
            if input?.requestId == requestId { input = nil }

        case .titleChanged, .sessionsChanged:
            break
        }
    }

    private func finishTurn(finalText: String, status turnStatus: TurnStatus, error: String?) {
        guard let item = items.last(where: { $0.role == .assistant && $0.isStreaming }) else {
            isStreaming = false
            status = nil
            return
        }
        let streamedAnswer = item.parts.contains { part in
            if case .text(let segment) = part { return !segment.source.isEmpty }
            return false
        }
        if !streamedAnswer, !finalText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            // A non-streaming provider's reply may have been previewed as reasoning; it is the
            // answer, so show it once, as the answer.
            let answer = finalText.trimmingCharacters(in: .whitespacesAndNewlines)
            item.parts.removeAll { part in
                guard case .reasoning(let segment) = part else { return false }
                let thought = segment.text.trimmingCharacters(in: .whitespacesAndNewlines)
                return !thought.isEmpty && answer.hasPrefix(thought)
            }
            // Providers that don't stream deliver the whole answer here; still reveal it smoothly.
            let segment = TextSegment(id: nextID("text"))
            item.parts.append(.text(segment))
            segment.pacer?.append(delta: finalText)
        }
        for part in item.parts {
            switch part {
            case .text(let segment) where segment.pacer != nil:
                sealed.insert(segment.id)
                segment.pacer?.finish()
                animate(segment)
            case .reasoning(let segment):
                segment.isStreaming = false
            default:
                break
            }
        }
        for (index, part) in item.parts.enumerated() {
            if case .tool(var call) = part, call.status == .running {
                call.status = turnStatus == .complete ? .done : .failed
                item.parts[index] = .tool(call)
            }
        }
        switch turnStatus {
        case .failed:
            item.parts.append(.notice(id: nextID("notice"), text: error ?? "Hermes couldn't finish this reply.", isError: true))
        case .interrupted:
            item.parts.append(.notice(id: nextID("notice"), text: "Stopped", isError: false))
        case .complete:
            if item.parts.isEmpty {
                item.parts.append(.notice(id: nextID("notice"), text: "Hermes finished without a reply.", isError: false))
            }
        }
        item.isStreaming = false
        item.timestamp = Date()
        isStreaming = false
        status = nil
        approval = nil
        clarify = nil
        input = nil
        app?.turnFinished(self)
    }

    private func fail(_ message: String) {
        finishTurn(finalText: "", status: .failed, error: message)
    }

    private func appendNotice(_ text: String, isError: Bool) {
        let part = ItemPart.notice(id: nextID("notice"), text: text, isError: isError)
        if let last = items.last, last.role == .assistant {
            last.parts.append(part)
        } else {
            items.append(ChatItem(id: nextID("assistant"), role: .assistant, parts: [part]))
        }
    }

    /// The socket dropped: runtime ids are gone, and a turn in flight can't be followed.
    func connectionLost() {
        liveId = nil
        if isStreaming {
            finishTurn(finalText: "", status: .failed, error: "The connection dropped while Hermes was replying. The reply may have finished on the server; reopen this chat to check.")
        }
    }

    // MARK: Reveal animation

    private func animate(_ segment: TextSegment) {
        if !UserDefaults.standard.bool(forKey: Preferences.animateStreaming) || app?.reduceMotion == true {
            segment.pacer?.revealAll()
        }
        if !animating.contains(where: { $0 === segment }) { animating.append(segment) }
        clock.start()
    }

    private func advance(by delta: Double) {
        animating.removeAll { segment in
            guard let pacer = segment.pacer else { return true }
            let frame = pacer.tick(dt: delta)
            if let document = frame.document, document != segment.document { segment.document = document }
            if frame.fade != segment.fade { segment.fade = frame.fade }
            if frame.settled {
                segment.isStreaming = false
                return true
            }
            return false
        }
        if animating.isEmpty { clock.stop() }
    }

    // MARK: Requests from the agent

    func answerApproval(_ choice: String) {
        guard let approval, let client = app?.client else { return }
        try? client.respondApproval(requestId: approval.requestId, choice: choice)
        self.approval = nil
    }

    func answerClarify(_ answers: [String: String]) {
        guard let clarify, let client = app?.client else { return }
        try? client.respondClarify(requestId: clarify.requestId, answers: answers)
        self.clarify = nil
    }

    func answerInput(_ value: String) {
        guard let input, let client = app?.client else { return }
        try? client.respondInput(requestId: input.requestId, value: value)
        self.input = nil
    }
}
