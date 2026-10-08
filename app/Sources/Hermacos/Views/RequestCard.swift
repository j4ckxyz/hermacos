import HermesCore
import SwiftUI

/// What the agent is blocked on: approve a command, answer a question, or supply a secret.
///
/// Text in these cards is capped with `lineLimit` rather than `fixedSize`: the card sits outside
/// any scroll view, and the split view measures the column's minimum size at zero width, where
/// vertically fixed text wraps per character and makes the whole window taller than the screen.
struct RequestCard: View {
    let chat: ChatModel

    var body: some View {
        Group {
            if let approval = chat.approval {
                ApprovalCard(request: approval, answer: chat.answerApproval)
            } else if let clarify = chat.clarify {
                ClarifyCard(request: clarify, answer: chat.answerClarify)
            } else if let input = chat.input {
                InputCard(request: input, answer: chat.answerInput)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .glassEffect(.regular, in: .rect(cornerRadius: 18))
    }
}

private struct ApprovalCard: View {
    let request: ApprovalRequest
    let answer: (String) -> Void

    private var allows: (String) -> Bool { { request.choices.contains($0) } }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label("Hermes wants to run a command", systemImage: "exclamationmark.shield")
                .font(.system(size: 13, weight: .semibold))
            if !request.command.isEmpty {
                Text(request.command)
                    .font(.system(size: 12.5, design: .monospaced))
                    .textSelection(.enabled)
                    .lineLimit(6)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(.fill.tertiary, in: .rect(cornerRadius: 8))
            }
            if !request.description.isEmpty {
                Text(request.description)
                    .font(.system(size: 12.5))
                    .foregroundStyle(.secondary)
                    .lineLimit(5)
            }
            HStack(spacing: 8) {
                Button("Deny") { answer("deny") }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                if allows("session") || allows("always") {
                    Menu("More") {
                        if allows("session") { Button("Allow for This Session") { answer("session") } }
                        if allows("always") { Button("Always Allow") { answer("always") } }
                    }
                    .fixedSize()
                }
                Button("Allow Once") { answer("once") }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.glassProminent)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Approval needed")
    }
}

private struct ClarifyCard: View {
    let request: ClarifyRequest
    let answer: ([String: String]) -> Void
    @State private var answers: [String: String] = [:]

    private var complete: Bool {
        request.questions.allSatisfy { !(answers[$0.qid] ?? "").trimmingCharacters(in: .whitespaces).isEmpty }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            ForEach(request.questions, id: \.qid) { question in
                VStack(alignment: .leading, spacing: 7) {
                    Label(question.question, systemImage: "questionmark.bubble")
                        .font(.system(size: 13, weight: .semibold))
                        .lineLimit(6)
                    if !question.choices.isEmpty {
                        FlowChoices(choices: question.choices, selected: answers[question.qid]) { choice in
                            answers[question.qid] = choice
                            if request.questions.count == 1 { answer(answers) }
                        }
                    }
                    TextField(question.choices.isEmpty ? "Your answer" : "Or type your own", text: binding(question.qid))
                        .textFieldStyle(.roundedBorder)
                        .onSubmit { if complete { answer(answers) } }
                }
            }
            HStack {
                Spacer()
                Button("Send Answer") { answer(answers) }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.glassProminent)
                    .disabled(!complete)
            }
        }
    }

    private func binding(_ qid: String) -> Binding<String> {
        Binding(get: { answers[qid] ?? "" }, set: { answers[qid] = $0 })
    }
}

private struct FlowChoices: View {
    let choices: [String]
    let selected: String?
    let pick: (String) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(choices, id: \.self) { choice in
                Button {
                    pick(choice)
                } label: {
                    Text(choice)
                        .font(.system(size: 12.5))
                        .multilineTextAlignment(.leading)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 6)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(selected == choice ? AnyShapeStyle(.tint.opacity(0.18)) : AnyShapeStyle(.fill.tertiary),
                                    in: .rect(cornerRadius: 8))
                        .contentShape(.rect(cornerRadius: 8))
                }
                .buttonStyle(.plain)
            }
        }
    }
}

private struct InputCard: View {
    let request: InputRequest
    let answer: (String) -> Void
    @State private var value = ""
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Label(request.title.isEmpty ? "Hermes needs a value" : request.title, systemImage: "key")
                .font(.system(size: 13, weight: .semibold))
            if !request.prompt.isEmpty {
                Text(request.prompt)
                    .font(.system(size: 12.5))
                    .foregroundStyle(.secondary)
                    .lineLimit(5)
            }
            Group {
                if request.masked {
                    SecureField("Value", text: $value)
                } else {
                    TextField("Value", text: $value)
                }
            }
            .textFieldStyle(.roundedBorder)
            .focused($focused)
            .onSubmit { if !value.isEmpty { answer(value) } }
            HStack {
                Button("Skip") { answer("") }
                    .keyboardShortcut(.cancelAction)
                Spacer()
                Button("Submit") { answer(value) }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.glassProminent)
                    .disabled(value.isEmpty)
            }
        }
        .onAppear { focused = true }
    }
}
