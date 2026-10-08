import HermesCore
import SwiftUI

/// One row of the transcript.
struct MessageView: View {
    let item: ChatItem
    let chat: ChatModel

    var body: some View {
        switch item.role {
        case .user: UserMessageView(item: item, chat: chat)
        case .assistant: AssistantMessageView(item: item, chat: chat)
        }
    }
}

/// The user's own message. Hovering it offers Copy and Rewrite; Rewrite edits it in place and
/// resends, replacing everything that came after.
private struct UserMessageView: View {
    @Environment(AppModel.self) private var model
    let item: ChatItem
    let chat: ChatModel
    @State private var hovering = false
    @State private var copied = false
    @State private var edited = ""
    @FocusState private var editorFocused: Bool

    private var isEditing: Bool { chat.editingItemID == item.id }
    private var showsActions: Bool { hovering || copied || model.revealHoverControls }

    var body: some View {
        HStack(alignment: .top, spacing: 0) {
            Spacer(minLength: 72)
            VStack(alignment: .trailing, spacing: 6) {
                if !item.attachments.isEmpty {
                    AttachmentRow(attachments: item.attachments)
                }
                if isEditing {
                    editor
                } else {
                    if !item.plainText.isEmpty {
                        Text(item.plainText)
                            .font(.system(size: Theme.bodySize))
                            .lineSpacing(Theme.lineSpacing)
                            .textSelection(.enabled)
                            .padding(.horizontal, 14)
                            .padding(.vertical, 9)
                            .background(.fill.tertiary, in: .rect(cornerRadius: 18))
                    }
                    footer
                }
            }
        }
        .contentShape(.rect)
        .onHover { hovering = $0 }
        .contextMenu {
            Button("Copy", systemImage: "square.on.square") { copyToPasteboard(item.plainText) }
            if item.canRewrite {
                Button("Rewrite…", systemImage: "pencil") { beginEditing() }
            }
        }
        .onChange(of: isEditing) { _, editing in
            if editing {
                edited = item.plainText
                editorFocused = true
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("You said: \(item.plainText)")
    }

    private var footer: some View {
        HStack(spacing: 2) {
            Group {
                MessageActionButton(symbol: copied ? "checkmark" : "square.on.square", label: "Copy") {
                    copyToPasteboard(item.plainText)
                    copied = true
                    Task {
                        try? await Task.sleep(for: .seconds(1.6))
                        copied = false
                    }
                }
                if item.canRewrite {
                    MessageActionButton(symbol: "pencil", label: "Rewrite: edit and send again", action: beginEditing)
                }
            }
            .opacity(showsActions ? 1 : 0)
            .animation(.easeOut(duration: 0.15), value: showsActions)
            if let timestamp = item.timestamp {
                MessageTimeLabel(date: timestamp)
                    .padding(.leading, 4)
            }
        }
        .padding(.trailing, 4)
    }

    private var editor: some View {
        VStack(alignment: .trailing, spacing: 8) {
            TextField("Message", text: $edited, axis: .vertical)
                .textFieldStyle(.plain)
                .font(.system(size: Theme.bodySize))
                .lineSpacing(Theme.lineSpacing)
                .lineLimit(1...14)
                .focused($editorFocused)
                .onSubmit(submit)
                .onExitCommand { chat.editingItemID = nil }
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .frame(minWidth: 280, maxWidth: 520, alignment: .leading)
                .background(.fill.tertiary, in: .rect(cornerRadius: 18))
                .overlay(RoundedRectangle(cornerRadius: 18).strokeBorder(.tint, lineWidth: 1.5))
                .accessibilityLabel("Rewrite message")
            HStack(spacing: 8) {
                Text(chat.isStreaming ? "Stops the current reply and replaces everything after this message." : "Replaces everything after this message.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                Button("Cancel") { chat.editingItemID = nil }
                    .keyboardShortcut(.cancelAction)
                Button("Send", action: submit)
                    .buttonStyle(.glassProminent)
                    .disabled(edited.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && item.attachments.isEmpty)
            }
            .controlSize(.small)
        }
        .onAppear {
            edited = item.plainText
            editorFocused = true
        }
    }

    private func beginEditing() {
        edited = item.plainText
        chat.editingItemID = item.id
    }

    private func submit() {
        chat.rewrite(item, text: edited)
    }
}

/// Attachments shown on a sent message, right-aligned under the bubble's edge.
private struct AttachmentRow: View {
    let attachments: [Attachment]

    var body: some View {
        // Wraps without measuring: a few chips per line is plenty for a message.
        let rows = stride(from: 0, to: attachments.count, by: 3).map { Array(attachments[$0..<min($0 + 3, attachments.count)]) }
        VStack(alignment: .trailing, spacing: 6) {
            ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                HStack(spacing: 6) {
                    ForEach(row) { AttachmentChip(attachment: $0) }
                }
            }
        }
    }
}

/// When a message was sent; the full date appears on hover.
struct MessageTimeLabel: View {
    let date: Date

    var body: some View {
        Text(MessageTime.label(for: date))
            .font(.system(size: 11).monospacedDigit())
            .foregroundStyle(.tertiary)
            .help(date.formatted(date: .complete, time: .standard))
            .accessibilityLabel("Sent \(MessageTime.label(for: date))")
    }
}

/// A small icon button in a message's footer.
struct MessageActionButton: View {
    let symbol: String
    let label: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 12))
                .contentTransition(.symbolEffect(.replace))
                .frame(width: 26, height: 22)
                .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .foregroundStyle(.secondary)
        .help(label)
        .accessibilityLabel(label)
    }
}

private struct AssistantMessageView: View {
    @Environment(AppModel.self) private var model
    let item: ChatItem
    let chat: ChatModel
    @State private var hovering = false
    @State private var copied = false

    /// A run of thinking and tool calls collapses into one activity line; text and notices
    /// stay as they are.
    private var groups: [PartGroup] {
        var out: [PartGroup] = []
        for part in item.parts {
            switch part {
            case .tool, .reasoning:
                if case .activity(let id, var steps)? = out.last {
                    steps.append(part)
                    out[out.count - 1] = .activity(id: id, steps: steps)
                } else {
                    out.append(.activity(id: part.id, steps: [part]))
                }
            case .text, .notice, .output:
                out.append(.single(part))
            }
        }
        return out
    }

    var body: some View {
        let groups = groups
        VStack(alignment: .leading, spacing: 12) {
            ForEach(groups) { group in
                switch group {
                case .single(.text(let segment)):
                    TextSegmentView(segment: segment)
                case .single(.notice(_, let text, let isError)):
                    NoticeView(text: text, isError: isError)
                case .single(.output(_, let text)):
                    CommandOutputView(text: text)
                case .single:
                    EmptyView()
                case .activity(_, let steps):
                    // Only the last group of a reply in progress is "what Hermes is doing now".
                    ActivityView(
                        steps: steps,
                        isActive: item.isStreaming && group.id == groups.last?.id,
                        status: chat.status
                    )
                }
            }
            if !item.isStreaming, !item.plainText.isEmpty || item.timestamp != nil {
                actions
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .onHover { hovering = $0 }
    }

    private var actions: some View {
        HStack(spacing: 2) {
            if let timestamp = item.timestamp {
                MessageTimeLabel(date: timestamp)
                    .padding(.trailing, 4)
            }
            if !item.plainText.isEmpty {
                MessageActionButton(symbol: copied ? "checkmark" : "square.on.square", label: "Copy response") {
                    copyToPasteboard(item.plainText)
                    copied = true
                    Task {
                        try? await Task.sleep(for: .seconds(1.6))
                        copied = false
                    }
                }
                .opacity(hovering || copied || model.revealHoverControls ? 1 : 0)
                .animation(.easeOut(duration: 0.15), value: hovering)
            }
        }
        .padding(.top, -4)
    }
}

private enum PartGroup: Identifiable {
    case single(ItemPart)
    case activity(id: String, steps: [ItemPart])

    var id: String {
        switch self {
        case .single(let part): part.id
        case .activity(let id, _): "activity-\(id)"
        }
    }
}

/// Everything the agent did between two pieces of text, folded into one line.
///
/// While the agent works the line shows the current step, animated. Clicking it unfolds every
/// step so far (thoughts, searches, commands); clicking again folds them away. Once the work
/// is over the line becomes a summary that unfolds the same way.
private struct ActivityView: View {
    @Environment(AppModel.self) private var model
    let steps: [ItemPart]
    let isActive: Bool
    let status: String?
    @State private var expanded = false

    private var isOpen: Bool { expanded || model.expandActivity }

    private var calls: [ToolCall] {
        steps.compactMap { step in
            if case .tool(let call) = step { return call }
            return nil
        }
    }

    /// The step in progress: a running tool, live thinking, or whatever the agent reports.
    private var current: (symbol: String, title: String, detail: String?) {
        if case .tool(let call)? = steps.last, call.status == .running {
            return (Theme.toolSymbol(call.name), call.title, call.detail)
        }
        if case .reasoning(let segment)? = steps.last, segment.isStreaming {
            return ("brain", "Thinking", nil)
        }
        let label = (status ?? "Working").trimmingCharacters(in: CharacterSet(charactersIn: "…. "))
        return ("sparkle", label.isEmpty ? "Working" : label, nil)
    }

    /// What was done, once it is over: `Web search ×4, Skill view · 6.7s`.
    private var summary: String {
        var order: [String] = []
        var counts: [String: Int] = [:]
        for call in calls {
            if counts[call.title] == nil { order.append(call.title) }
            counts[call.title, default: 0] += 1
        }
        guard !order.isEmpty else { return "Thought process" }
        let names = order.map { title in
            let count = counts[title] ?? 1
            return count > 1 ? "\(title) ×\(count)" : title
        }
        let seconds = calls.compactMap(\.duration).reduce(0, +)
        let took = seconds >= 0.1
            ? " · " + Duration.seconds(seconds).formatted(.units(allowed: [.minutes, .seconds], width: .narrow, fractionalPart: .show(length: seconds < 10 ? 1 : 0)))
            : ""
        return names.joined(separator: ", ") + took
    }

    private var failed: Bool { calls.contains { $0.status == .failed } }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button {
                withAnimation(.snappy(duration: 0.26)) { expanded.toggle() }
            } label: {
                header
            }
            .buttonStyle(.plain)
            .accessibilityLabel(isActive ? "Working: \(current.title)" : "Activity: \(summary)")
            .accessibilityValue(isOpen ? "expanded" : "collapsed")
            .accessibilityHint("Shows or hides the steps")

            if isOpen {
                stepList
                    .transition(.opacity.combined(with: .offset(y: -4)))
            }
        }
    }

    private var header: some View {
        HStack(spacing: 7) {
            if isActive {
                Image(systemName: current.symbol)
                    .font(.system(size: 12, weight: .medium))
                    .symbolEffect(.pulse, options: .repeating)
                    .frame(width: 16)
                HStack(spacing: 6) {
                    Text(current.title)
                        .fontWeight(.medium)
                    if let detail = current.detail, !detail.isEmpty {
                        Text(detail)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                }
                .font(.system(size: 13))
                .contentTransition(.opacity)
                .shimmering()
                .animation(.easeInOut(duration: 0.25), value: current.title + (current.detail ?? ""))
            } else {
                Image(systemName: failed ? "exclamationmark.triangle" : "checkmark.circle")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(failed ? AnyShapeStyle(.orange) : AnyShapeStyle(.secondary))
                    .frame(width: 16)
                Text(summary)
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }
            if steps.count > 1 || !isActive {
                Image(systemName: "chevron.right")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(.tertiary)
                    .rotationEffect(.degrees(isOpen ? 90 : 0))
            }
        }
        .padding(.vertical, 2)
        .frame(maxWidth: 560, alignment: .leading)
        .contentShape(.rect)
    }

    /// Every step in order; neighbouring tool calls share one bordered list.
    private var stepList: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Self.batches(steps), id: \.id) { batch in
                switch batch.content {
                case .thought(let segment):
                    // Thoughts alone need no second header; among tools each folds on its own.
                    if calls.isEmpty {
                        ThoughtText(text: segment.text)
                    } else {
                        ReasoningView(segment: segment)
                    }
                case .tools(let calls):
                    ToolGroupView(calls: calls)
                }
            }
        }
        .padding(.leading, 12)
        .overlay(alignment: .leading) {
            RoundedRectangle(cornerRadius: 1).fill(.quaternary).frame(width: 2).padding(.vertical, 2)
        }
    }

    private struct Batch {
        enum Content {
            case thought(ReasoningSegment)
            case tools([ToolCall])
        }

        let id: String
        var content: Content
    }

    private static func batches(_ steps: [ItemPart]) -> [Batch] {
        var out: [Batch] = []
        for step in steps {
            switch step {
            case .reasoning(let segment):
                out.append(Batch(id: step.id, content: .thought(segment)))
            case .tool(let call):
                if case .tools(var calls)? = out.last?.content {
                    calls.append(call)
                    out[out.count - 1].content = .tools(calls)
                } else {
                    out.append(Batch(id: step.id, content: .tools([call])))
                }
            case .text, .notice, .output:
                break
            }
        }
        return out
    }
}

/// A band of light that sweeps across text while something is in progress.
private struct Shimmer: ViewModifier {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var sweep = false

    func body(content: Content) -> some View {
        if reduceMotion {
            content.foregroundStyle(.secondary)
        } else {
            content
                .foregroundStyle(.tertiary)
                .overlay {
                    content
                        .foregroundStyle(.primary)
                        .mask {
                            LinearGradient(
                                colors: [.clear, .black, .clear],
                                startPoint: UnitPoint(x: sweep ? 1.0 : -0.6, y: 0.5),
                                endPoint: UnitPoint(x: sweep ? 1.6 : 0.0, y: 0.5)
                            )
                        }
                }
                .onAppear {
                    withAnimation(.linear(duration: 1.5).repeatForever(autoreverses: false)) { sweep = true }
                }
        }
    }
}

private extension View {
    func shimmering() -> some View { modifier(Shimmer()) }
}

/// Markdown text that may still be streaming in, with link previews once it has settled.
private struct TextSegmentView: View {
    let segment: TextSegment

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            MarkdownView(document: segment.document, fade: segment.fade, isStreaming: segment.isStreaming)
            if !segment.isStreaming {
                LinkPreviewList(links: segment.document.links)
            }
        }
    }
}

private struct ToolGroupView: View {
    let calls: [ToolCall]

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(calls.enumerated()), id: \.element.id) { index, call in
                if index > 0 { Divider().padding(.leading, 36) }
                ToolRow(call: call)
            }
        }
        .background(.fill.quinary, in: .rect(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(.separator.opacity(0.6), lineWidth: 1))
        .frame(maxWidth: 520, alignment: .leading)
    }
}

private struct ToolRow: View {
    let call: ToolCall

    var body: some View {
        HStack(spacing: 9) {
            Image(systemName: Theme.toolSymbol(call.name))
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(.secondary)
                .frame(width: 18)
            Text(call.title)
                .font(.system(size: 12.5, weight: .medium))
                .lineLimit(1)
                .layoutPriority(1)
            if let detail = call.detail, !detail.isEmpty {
                Text(detail)
                    .font(.system(size: 12, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            Spacer(minLength: 6)
            trailing
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
        .help([call.detail, call.summary].compactMap { $0 }.joined(separator: "\n"))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(call.title), \(call.detail ?? ""), \(statusLabel)")
    }

    private var statusLabel: String {
        switch call.status {
        case .running: "running"
        case .done: "done"
        case .failed: "failed"
        }
    }

    @ViewBuilder private var trailing: some View {
        switch call.status {
        case .running:
            ProgressView().controlSize(.small).scaleEffect(0.75)
        case .done:
            HStack(spacing: 5) {
                if let summary = call.summary, !summary.isEmpty {
                    Text(summary).lineLimit(1)
                } else if let duration = call.duration, duration >= 0.1 {
                    Text(Duration.seconds(duration).formatted(.units(allowed: [.minutes, .seconds], width: .narrow, fractionalPart: .show(length: duration < 10 ? 1 : 0))))
                }
                Image(systemName: "checkmark")
                    .font(.system(size: 10, weight: .semibold))
            }
            .font(.system(size: 11.5))
            .foregroundStyle(.tertiary)
        case .failed:
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 11))
                .foregroundStyle(.orange)
        }
    }
}

private struct ReasoningView: View {
    let segment: ReasoningSegment
    @State private var expanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button {
                withAnimation(.snappy(duration: 0.22)) { expanded.toggle() }
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "brain")
                    Text(segment.isStreaming ? "Thinking…" : "Thought process")
                    Image(systemName: "chevron.right")
                        .font(.system(size: 9, weight: .semibold))
                        .rotationEffect(.degrees(expanded ? 90 : 0))
                }
                .font(.system(size: 12.5))
                .foregroundStyle(.secondary)
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityValue(expanded ? "expanded" : "collapsed")

            if expanded {
                ThoughtText(text: segment.text)
                    .padding(.leading, 11)
                    .overlay(alignment: .leading) {
                        RoundedRectangle(cornerRadius: 1).fill(.quaternary).frame(width: 2)
                    }
                    .transition(.opacity)
            }
        }
    }
}

/// A model's reasoning, set quieter than the reply. Reasoning is markdown too (providers
/// title their summaries in bold), so it goes through the same parser.
private struct ThoughtText: View {
    let text: String

    var body: some View {
        let blocks = parseMarkdown(text: text, streaming: false).blocks
        VStack(alignment: .leading, spacing: 7) {
            ForEach(blocks, id: \.id) { block in
                switch block.kind {
                case .paragraph(let runs), .heading(_, let runs):
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        if let marker = block.marker { Text(marker) }
                        Text(runs.attributed(size: 12.5))
                    }
                    .padding(.leading, CGFloat(max(Int(block.indent) - 1, 0)) * 14)
                case .code(_, let code, _, _):
                    Text(code).font(.system(size: 11.5, design: .monospaced))
                default:
                    EmptyView()
                }
            }
        }
        .font(.system(size: 12.5))
        .lineSpacing(3)
        .foregroundStyle(.secondary)
        .textSelection(.enabled)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// What a slash command printed. Terminal-shaped text: fixed pitch, columns kept aligned,
/// scrolling sideways rather than wrapping.
private struct CommandOutputView: View {
    let text: String

    var body: some View {
        ScrollView(.horizontal) {
            Text(text)
                .font(.system(size: 12, design: .monospaced))
                .lineSpacing(2.5)
                .textSelection(.enabled)
                .fixedSize(horizontal: true, vertical: true)
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
        }
        .scrollBounceBehavior(.basedOnSize, axes: .horizontal)
        .background(.fill.quinary, in: .rect(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(.separator.opacity(0.6), lineWidth: 1))
        .frame(maxWidth: 620, alignment: .leading)
        .accessibilityLabel("Command output")
    }
}

private struct NoticeView: View {
    let text: String
    let isError: Bool

    var body: some View {
        Label(text, systemImage: isError ? "exclamationmark.triangle" : "info.circle")
            .font(.system(size: 12.5))
            .foregroundStyle(isError ? AnyShapeStyle(.orange) : AnyShapeStyle(.secondary))
            .textSelection(.enabled)
    }
}

/// Shown under the last message while the agent is working but not writing.
struct ActivityIndicator: View {
    let text: String?
    @State private var pulse = false

    var body: some View {
        HStack(spacing: 8) {
            Circle()
                .fill(.secondary)
                .frame(width: 9, height: 9)
                .scaleEffect(pulse ? 1 : 0.6)
                .opacity(pulse ? 0.9 : 0.4)
                .animation(.easeInOut(duration: 0.75).repeatForever(autoreverses: true), value: pulse)
            if let text, !text.isEmpty {
                Text(text)
                    .font(.system(size: 12.5))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .contentTransition(.opacity)
            }
        }
        .onAppear { pulse = true }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(text ?? "Hermes is working")
    }
}
