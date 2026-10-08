import AppKit
import HermesCore
import SwiftUI

/// Renders a parsed markdown document as native views, one per block.
struct MarkdownView: View {
    let document: MdDocument
    /// Opacity of the newest glyphs while the text streams in; empty when settled.
    var fade: [Float] = []
    var isStreaming = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(document.blocks.enumerated()), id: \.element.id) { index, block in
                let isLast = index == document.blocks.count - 1
                BlockView(block: block, fade: isLast ? fade : [], isStreaming: isStreaming && isLast)
                    .equatable()
                    .padding(.top, index == 0 ? 0 : spacing(above: block, below: document.blocks[index - 1]))
                    .transition(.opacity)
            }
        }
        .animation(.easeOut(duration: 0.22), value: document.blocks.count)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func spacing(above block: MdBlock, below previous: MdBlock) -> CGFloat {
        if case .heading(let level, _) = block.kind { return level <= 2 ? 22 : 16 }
        if case .heading = previous.kind { return 8 }
        // Items of the same list sit closer than separate paragraphs.
        if block.indent > 0, previous.indent > 0 { return block.marker == nil ? 6 : 7 }
        switch block.kind {
        case .code, .table, .image, .rule: return 14
        default: break
        }
        switch previous.kind {
        case .code, .table, .image, .rule: return 14
        default: return 11
        }
    }
}

private struct BlockView: View, Equatable {
    let block: MdBlock
    let fade: [Float]
    let isStreaming: Bool

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 0) {
            if block.quote > 0 {
                ForEach(0..<Int(block.quote), id: \.self) { _ in
                    RoundedRectangle(cornerRadius: 1.5)
                        .fill(.tertiary)
                        .frame(width: 3)
                        .padding(.trailing, 11)
                        .alignmentGuide(.firstTextBaseline) { $0[.top] + Theme.bodySize }
                }
            }
            if block.indent > 0 {
                Color.clear.frame(width: CGFloat(block.indent - 1) * 22, height: 1)
                marker
                    .frame(width: 24, alignment: .trailing)
                    .padding(.trailing, 7)
            }
            content
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    @ViewBuilder private var marker: some View {
        switch block.marker {
        case "[ ]":
            Image(systemName: "square")
                .foregroundStyle(.secondary)
                .accessibilityLabel("Not done")
        case "[x]":
            Image(systemName: "checkmark.square.fill")
                .foregroundStyle(.tint)
                .accessibilityLabel("Done")
        case .some(let text):
            Text(text)
                .font(.system(size: Theme.bodySize).monospacedDigit())
                .foregroundStyle(.secondary)
        case .none:
            Color.clear.frame(height: 1)
        }
    }

    @ViewBuilder private var content: some View {
        switch block.kind {
        case .paragraph(let runs):
            prose(runs.attributed(size: Theme.bodySize))
                .foregroundStyle(block.quote > 0 ? .secondary : .primary)
        case .heading(let level, let runs):
            prose(runs.attributed(size: Theme.headingSize(level), weight: .semibold))
                .accessibilityAddTraits(.isHeader)
        case .code(let language, let code, let spans, let closed):
            CodeBlockView(language: language, code: code, spans: spans, isWriting: !closed && isStreaming)
        case .table(let alignments, let header, let rows):
            TableBlockView(alignments: alignments, header: header, rows: rows)
        case .image(let url, let alt):
            RemoteImageView(url: url, alt: alt)
        case .rule:
            Divider().padding(.vertical, 4)
        }
    }

    @ViewBuilder private func prose(_ text: AttributedString) -> some View {
        if fade.isEmpty {
            Text(text)
                .lineSpacing(Theme.lineSpacing)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
        } else {
            Text(text)
                .lineSpacing(Theme.lineSpacing)
                .textRenderer(TailFade(fade: fade))
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

// MARK: Code

struct CodeBlockView: View {
    let language: String?
    let code: String
    let spans: [MdCodeSpan]
    let isWriting: Bool
    @State private var copied = false

    private var highlighted: AttributedString {
        var out = AttributedString()
        for span in spans {
            var piece = AttributedString(span.text)
            piece.foregroundColor = Theme.tokenColor(span.kind)
            if span.kind == .comment { piece.font = .system(size: Theme.codeSize, design: .monospaced).italic() }
            out.append(piece)
        }
        return out
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Text(language ?? "code")
                    .font(.system(size: 11.5, weight: .medium, design: .monospaced))
                    .foregroundStyle(.secondary)
                if isWriting {
                    ProgressView().controlSize(.mini)
                }
                Spacer()
                Button {
                    copyToPasteboard(code)
                    copied = true
                    Task {
                        try? await Task.sleep(for: .seconds(1.6))
                        copied = false
                    }
                } label: {
                    Label(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "square.on.square")
                        .font(.system(size: 11.5))
                        .contentTransition(.symbolEffect(.replace))
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .help("Copy code")
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 7)
            .background(Color.primary.opacity(0.04))

            ScrollView(.horizontal) {
                Text(highlighted)
                    .font(.system(size: Theme.codeSize, design: .monospaced))
                    .lineSpacing(3)
                    .textSelection(.enabled)
                    .fixedSize(horizontal: true, vertical: true)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 10)
            }
            .scrollIndicators(.automatic)
        }
        .background(Color(nsColor: .textBackgroundColor).opacity(0.6))
        .clipShape(.rect(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(.separator, lineWidth: 1))
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(language ?? "Code") block")
    }
}

// MARK: Table

struct TableBlockView: View {
    let alignments: [MdAlign]
    let header: [MdCell]
    let rows: [[MdCell]]

    private func alignment(_ column: Int) -> Alignment {
        guard alignments.indices.contains(column) else { return .leading }
        switch alignments[column] {
        case .leading: return .leading
        case .center: return .center
        case .trailing: return .trailing
        }
    }

    private func cell(_ cell: MdCell, column: Int, isHeader: Bool) -> some View {
        Text(cell.runs.attributed(size: 13, weight: isHeader ? .semibold : .regular))
            .lineSpacing(2)
            .textSelection(.enabled)
            .multilineTextAlignment(alignment(column) == .trailing ? .trailing : .leading)
            .frame(maxWidth: .infinity, alignment: alignment(column))
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
    }

    var body: some View {
        ScrollView(.horizontal) {
            Grid(alignment: .leading, horizontalSpacing: 0, verticalSpacing: 0) {
                GridRow {
                    ForEach(Array(header.enumerated()), id: \.offset) { column, value in
                        cell(value, column: column, isHeader: true)
                    }
                }
                .background(Color.primary.opacity(0.045))
                ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                    Divider()
                    GridRow {
                        ForEach(0..<max(header.count, row.count), id: \.self) { column in
                            cell(column < row.count ? row[column] : MdCell(runs: []), column: column, isHeader: false)
                        }
                    }
                }
            }
            .frame(minWidth: 240)
            // The frame hugs the table, so a narrow table doesn't sit in a wide empty box.
            .clipShape(.rect(cornerRadius: 10))
            .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(.separator, lineWidth: 1))
        }
        .scrollBounceBehavior(.basedOnSize, axes: .horizontal)
        .fixedSize(horizontal: false, vertical: true)
    }
}

// MARK: Image

struct RemoteImageView: View {
    let url: String
    let alt: String

    var body: some View {
        AsyncImage(url: URL(string: url), transaction: Transaction(animation: .easeOut(duration: 0.25))) { phase in
            switch phase {
            case .success(let image):
                image
                    .resizable()
                    .scaledToFit()
                    .frame(maxWidth: 520, maxHeight: 380, alignment: .leading)
                    .clipShape(.rect(cornerRadius: 12))
                    .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(.separator, lineWidth: 1))
            case .failure:
                Label(alt.isEmpty ? "Image couldn't be loaded" : alt, systemImage: "photo")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .padding(12)
                    .background(.quinary, in: .rect(cornerRadius: 10))
            default:
                RoundedRectangle(cornerRadius: 12)
                    .fill(.quinary)
                    .frame(width: 320, height: 180)
                    .overlay(ProgressView().controlSize(.small))
            }
        }
        .accessibilityLabel(alt.isEmpty ? "Image" : alt)
        .contextMenu {
            if let link = URL(string: url) {
                Button("Open Image", systemImage: "arrow.up.right.square") { NSWorkspace.shared.open(link) }
                Button("Copy Image Address", systemImage: "link") { copyToPasteboard(url) }
            }
        }
    }
}
