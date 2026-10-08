import SwiftUI
import UniformTypeIdentifiers

/// The message field. Return sends, Shift-Return (or Option-Return) adds a line. Files and
/// images wait above the text as chips until the message is sent.
struct ComposerView: View {
    @Environment(AppModel.self) private var model
    let chat: ChatModel
    @FocusState private var focused: Bool
    @State private var selection: TextSelection?
    @State private var choosingFiles = false

    var body: some View {
        @Bindable var chat = chat
        VStack(alignment: .leading, spacing: 0) {
            if !chat.attachments.isEmpty {
                ScrollView(.horizontal) {
                    HStack(spacing: 8) {
                        ForEach(chat.attachments) { attachment in
                            AttachmentChip(attachment: attachment) { chat.removeAttachment(attachment.id) }
                                .transition(.scale(scale: 0.85).combined(with: .opacity))
                        }
                    }
                    .padding(.horizontal, 12)
                    .padding(.top, 10)
                }
                .scrollIndicators(.never)
                // Fixed: a horizontal scroll view takes whatever height it is offered.
                .frame(height: AttachmentChip.height + 10)
            }
            if let problem = chat.attachmentError {
                Label(problem, systemImage: "exclamationmark.triangle")
                    .font(.system(size: 12))
                    .foregroundStyle(.orange)
                    .lineLimit(2)
                    .padding(.horizontal, 14)
                    .padding(.top, 8)
            }

            HStack(alignment: .bottom, spacing: 2) {
                Button {
                    choosingFiles = true
                } label: {
                    Image(systemName: "plus")
                        .font(.system(size: 14, weight: .medium))
                        .frame(width: 28, height: 28)
                        .contentShape(.circle)
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .padding(.leading, 8)
                .padding(.bottom, 8)
                .help("Attach files (or paste, or drop them here)")
                .accessibilityLabel("Attach files")

                TextField("Message Hermes", text: $chat.draft, selection: $selection, axis: .vertical)
                    .textFieldStyle(.plain)
                    .font(.system(size: Theme.bodySize))
                    .lineLimit(1...10)
                    .focused($focused)
                    .onSubmit(send)
                    .onKeyPress(.return, phases: .down) { press in
                        guard press.modifiers.contains(.shift) else { return .ignored }
                        insertNewline()
                        return .handled
                    }
                    // While the command menu is open the arrows move through it, Tab takes
                    // the highlighted command and Escape closes it.
                    .onKeyPress(.downArrow) { moveMenu(1) }
                    .onKeyPress(.upArrow) { moveMenu(-1) }
                    .onKeyPress(.tab) {
                        guard let suggestion = chat.highlightedSuggestion else { return .ignored }
                        chat.accept(suggestion)
                        return .handled
                    }
                    .onKeyPress(.escape) {
                        guard !chat.commandSuggestions.isEmpty else { return .ignored }
                        chat.commandMenuDismissedFor = chat.draft
                        return .handled
                    }
                    .padding(.leading, 6)
                    .padding(.vertical, 12)
                    .accessibilityLabel("Message")

                Group {
                    if chat.isStreaming {
                        Button(action: chat.stop) {
                            Image(systemName: "stop.fill")
                                .font(.system(size: 11, weight: .bold))
                                .frame(width: 28, height: 28)
                                .background(.primary, in: .circle)
                                .foregroundStyle(.background)
                        }
                        .help("Stop (⌘.)")
                        .accessibilityLabel("Stop responding")
                    } else {
                        Button(action: send) {
                            Image(systemName: "arrow.up")
                                .font(.system(size: 13, weight: .bold))
                                .frame(width: 28, height: 28)
                                .background(chat.canSend ? AnyShapeStyle(.tint) : AnyShapeStyle(.quaternary), in: .circle)
                                .foregroundStyle(chat.canSend ? AnyShapeStyle(.white) : AnyShapeStyle(.secondary))
                        }
                        .disabled(!chat.canSend)
                        .help("Send (Return)")
                        .accessibilityLabel("Send message")
                    }
                }
                .buttonStyle(.plain)
                .padding(.trailing, 8)
                .padding(.bottom, 8)
            }
        }
        .glassEffect(.regular, in: .rect(cornerRadius: 22))
        .contentShape(.rect(cornerRadius: 22))
        .onTapGesture { focused = true }
        .animation(.snappy(duration: 0.22), value: chat.attachments.map(\.id))
        .fileImporter(isPresented: $choosingFiles, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
            if case .success(let urls) = result { chat.addFiles(urls) }
            focused = true
        }
        .onAppear { focusAtEnd() }
        .onChange(of: model.composerFocusRequests) { focusAtEnd() }
    }

    /// Focus the field with the caret after the existing text, so typing continues it rather
    /// than replacing a selection.
    private func focusAtEnd() {
        focused = true
        selection = TextSelection(insertionPoint: chat.draft.endIndex)
    }

    private func moveMenu(_ offset: Int) -> KeyPress.Result {
        guard !chat.commandSuggestions.isEmpty else { return .ignored }
        chat.moveCommandSelection(by: offset)
        return .handled
    }

    private func send() {
        // Return on a highlighted command that isn't fully typed yet completes it first.
        if let suggestion = chat.highlightedSuggestion,
           suggestion.title.lowercased() != chat.draft.trimmingCharacters(in: .whitespaces).lowercased() {
            chat.accept(suggestion)
            return
        }
        guard chat.canSend else { return }
        chat.send()
        focused = true
    }

    private func insertNewline() {
        if let selection, case .selection(let range) = selection.indices {
            chat.draft.replaceSubrange(range, with: "\n")
            let caret = chat.draft.index(after: range.lowerBound)
            self.selection = TextSelection(insertionPoint: caret)
        } else {
            chat.draft.append("\n")
        }
    }
}

/// One attachment: a thumbnail for images, an icon and name for files.
struct AttachmentChip: View {
    static let height: CGFloat = 52

    let attachment: Attachment
    /// Present while the attachment can still be taken off the message.
    var onRemove: (() -> Void)?
    @State private var hovering = false

    var body: some View {
        Group {
            if attachment.isImage, let thumbnail = attachment.thumbnail {
                Image(nsImage: thumbnail)
                    .resizable()
                    .scaledToFill()
                    .frame(width: Self.height, height: Self.height)
                    .clipShape(.rect(cornerRadius: 10))
            } else {
                HStack(spacing: 8) {
                    Image(systemName: attachment.symbol)
                        .font(.system(size: 15))
                        .foregroundStyle(.secondary)
                        .frame(width: 30, height: 30)
                        .background(.fill.tertiary, in: .rect(cornerRadius: 7))
                    VStack(alignment: .leading, spacing: 1) {
                        Text(attachment.name)
                            .font(.system(size: 12.5, weight: .medium))
                            .lineLimit(1)
                            .truncationMode(.middle)
                        if let detail = attachment.sizeLabel ?? (attachment.isImage ? "Image" : nil) {
                            Text(detail)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                .padding(.horizontal, 9)
                .frame(height: Self.height)
                // Hug the label up to a cap, rather than stretching to fill the row.
                .frame(maxWidth: 210, alignment: .leading)
                .fixedSize()
                .background(.fill.quaternary, in: .rect(cornerRadius: 10))
            }
        }
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(.separator.opacity(0.7), lineWidth: 1))
        .overlay(alignment: .topTrailing) {
            if let onRemove, hovering {
                Button(action: onRemove) {
                    Image(systemName: "xmark.circle.fill")
                        .font(.system(size: 15))
                        .symbolRenderingMode(.palette)
                        .foregroundStyle(.white, .black.opacity(0.65))
                }
                .buttonStyle(.plain)
                .offset(x: 5, y: -5)
                .help("Remove")
                .accessibilityLabel("Remove \(attachment.name)")
            }
        }
        .onHover { hovering = $0 }
        .help(attachment.name)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(attachment.isImage ? "Image \(attachment.name)" : "File \(attachment.name)")
    }
}

/// The slash-command menu that opens above the message field.
struct SlashCommandMenu: View {
    let chat: ChatModel
    private static let rowHeight: CGFloat = 30
    private static let visibleRows = 7

    var body: some View {
        let suggestions = chat.commandSuggestions
        let selected = chat.highlightedSuggestion?.id
        ScrollViewReader { proxy in
            ScrollView {
                VStack(spacing: 0) {
                    ForEach(suggestions) { suggestion in
                        Button {
                            chat.accept(suggestion)
                        } label: {
                            HStack(spacing: 10) {
                                Text(suggestion.title)
                                    .font(.system(size: 12.5, weight: .medium, design: .monospaced))
                                    .lineLimit(1)
                                    .layoutPriority(1)
                                Text(suggestion.detail)
                                    .font(.system(size: 12.5))
                                    .foregroundStyle(.secondary)
                                    .lineLimit(1)
                                Spacer(minLength: 0)
                            }
                            .padding(.horizontal, 10)
                            .frame(height: Self.rowHeight)
                            .background(suggestion.id == selected ? AnyShapeStyle(.tint.opacity(0.22)) : AnyShapeStyle(.clear),
                                        in: .rect(cornerRadius: 8))
                            .contentShape(.rect)
                        }
                        .buttonStyle(.plain)
                        .id(suggestion.id)
                        .accessibilityLabel("\(suggestion.title), \(suggestion.detail)")
                        .accessibilityAddTraits(suggestion.id == selected ? .isSelected : [])
                    }
                }
                .padding(5)
            }
            // A fixed height: this sits outside the transcript's scroll view.
            .frame(height: CGFloat(min(suggestions.count, Self.visibleRows)) * Self.rowHeight + 10)
            .onChange(of: selected) { _, id in
                if let id { proxy.scrollTo(id) }
            }
        }
        .glassEffect(.regular, in: .rect(cornerRadius: 14))
        .accessibilityLabel("Commands")
    }
}
