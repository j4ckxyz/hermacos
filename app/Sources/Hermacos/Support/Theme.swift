import AppKit
import HermesCore
import SwiftUI

/// Type sizes and colours shared by the transcript.
enum Theme {
    /// Reading width of the conversation column.
    static let columnWidth: CGFloat = 720
    static let bodySize: CGFloat = 14
    static let codeSize: CGFloat = 12.5
    static let lineSpacing: CGFloat = 3.5

    static func headingSize(_ level: UInt8) -> CGFloat {
        switch level {
        case 1: 22
        case 2: 18
        case 3: 16
        default: 14.5
        }
    }

    /// Syntax colours that follow the system light/dark appearance.
    static func tokenColor(_ kind: MdTokenKind) -> Color {
        switch kind {
        case .plain: .primary
        case .keyword: Color(nsColor: .systemPink)
        case .string: Color(nsColor: .systemRed)
        case .comment: .secondary
        case .number: Color(nsColor: .systemOrange)
        case .type: Color(nsColor: .systemTeal)
        case .function: Color(nsColor: .systemBlue)
        case .property: Color(nsColor: .systemPurple)
        }
    }

    static func toolSymbol(_ name: String) -> String {
        let name = name.lowercased()
        let table: [(String, String)] = [
            ("terminal", "terminal"), ("shell", "terminal"), ("exec", "terminal"), ("process", "terminal"),
            ("search", "magnifyingglass"), ("extract", "doc.text.magnifyingglass"), ("browser", "globe"),
            ("web", "globe"), ("read", "doc.text"), ("write", "square.and.pencil"), ("patch", "square.and.pencil"),
            ("edit", "square.and.pencil"), ("memory", "brain"), ("delegate", "person.2"), ("subagent", "person.2"),
            ("image", "photo"), ("vision", "eye"), ("cron", "clock"), ("schedule", "clock"), ("todo", "checklist"),
            ("skill", "sparkles"), ("clarify", "questionmark.bubble"), ("send", "paperplane"), ("tts", "speaker.wave.2"),
            ("code", "chevron.left.forwardslash.chevron.right"),
        ]
        return table.first { name.contains($0.0) }?.1 ?? "wrench.and.screwdriver"
    }
}

extension [MdRun] {
    /// Styled runs -> one attributed string a `Text` can draw and select.
    func attributed(size: CGFloat, weight: Font.Weight = .regular) -> AttributedString {
        var out = AttributedString()
        for run in self {
            if run.citation, let link = run.link, let url = URL(string: link) {
                // A small raised chip; clicking it opens the cited source.
                var chip = AttributedString("\u{2009}\(run.text)\u{2009}")
                chip.font = .system(size: Swift.max(size - 3, 10.5), weight: .semibold).monospacedDigit()
                chip.foregroundColor = .accentColor
                chip.backgroundColor = Color.accentColor.opacity(0.16)
                chip.baselineOffset = size * 0.2
                chip.link = url
                out.append(chip)
                out.append(AttributedString("\u{200A}"))
                continue
            }
            var piece = AttributedString(run.text)
            var font: Font = run.code
                ? .system(size: size - 1, weight: run.bold ? .semibold : weight, design: .monospaced)
                : .system(size: size, weight: run.bold ? .semibold : weight)
            if run.italic { font = font.italic() }
            piece.font = font
            if run.code { piece.backgroundColor = Color.primary.opacity(0.07) }
            if run.strike { piece.strikethroughStyle = .single }
            if let link = run.link, let url = URL(string: link) {
                piece.link = url
                piece.foregroundColor = .accentColor
            }
            out.append(piece)
        }
        return out
    }

    var plainText: String { map(\.text).joined() }
}

func copyToPasteboard(_ text: String) {
    NSPasteboard.general.clearContents()
    NSPasteboard.general.setString(text, forType: .string)
}

/// How a message's time is written under it.
enum MessageTime {
    /// A bare time today; the day is added as the message gets older.
    static func label(for date: Date) -> String {
        let calendar = Calendar.current
        let time = date.formatted(date: .omitted, time: .shortened)
        if calendar.isDateInToday(date) { return time }
        if calendar.isDateInYesterday(date) { return "Yesterday, \(time)" }
        if calendar.isDate(date, equalTo: Date(), toGranularity: .year) {
            return "\(date.formatted(.dateTime.day().month(.abbreviated))), \(time)"
        }
        return "\(date.formatted(.dateTime.day().month(.abbreviated).year())), \(time)"
    }
}
