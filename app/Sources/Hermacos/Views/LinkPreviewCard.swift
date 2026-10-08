import AppKit
import HermesCore
import Observation
import SwiftUI

/// Fetches and remembers link previews for the session.
@MainActor @Observable
final class PreviewStore {
    static let shared = PreviewStore()

    enum State {
        case loading
        case loaded(LinkPreview)
        case unavailable
    }

    private(set) var states: [String: State] = [:]

    func state(for url: String) -> State? { states[url] }

    func load(_ url: String) {
        guard states[url] == nil else { return }
        states[url] = .loading
        Task {
            do {
                states[url] = .loaded(try await fetchLinkPreview(url: url))
            } catch {
                states[url] = .unavailable
            }
        }
    }
}

/// Cards for the links in a finished message.
struct LinkPreviewList: View {
    let links: [String]
    @AppStorage(Preferences.linkPreviews) private var enabled = true
    private let store = PreviewStore.shared
    private static let limit = 4

    /// Loaded previews in link order, minus cards that would read the same as an earlier one
    /// (several links into one site often share a title and description).
    private var distinctPreviews: [LinkPreview] {
        var seen = Set<String>()
        return links.prefix(Self.limit).compactMap { link in
            guard case .loaded(let preview)? = store.state(for: link) else { return nil }
            return seen.insert("\(preview.siteName)|\(preview.title)").inserted ? preview : nil
        }
    }

    var body: some View {
        if enabled, !links.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(distinctPreviews, id: \.url) { preview in
                    LinkPreviewCard(preview: preview)
                        .transition(.opacity.combined(with: .offset(y: 4)))
                }
            }
            .animation(.smooth(duration: 0.3), value: store.states.count)
            .task(id: links) {
                for link in links.prefix(Self.limit) { store.load(link) }
            }
        }
    }
}

struct LinkPreviewCard: View {
    let preview: LinkPreview
    @State private var hovering = false

    private var destination: URL? { URL(string: preview.url) }

    var body: some View {
        Button {
            if let destination { NSWorkspace.shared.open(destination) }
        } label: {
            HStack(alignment: .center, spacing: 0) {
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 6) {
                        favicon
                        Text(preview.siteName)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                    Text(preview.title)
                        .font(.system(size: 13.5, weight: .semibold))
                        .lineLimit(2)
                        .multilineTextAlignment(.leading)
                    if let description = preview.description, !description.isEmpty {
                        Text(description)
                            .font(.system(size: 12.5))
                            .foregroundStyle(.secondary)
                            .lineLimit(2)
                            .multilineTextAlignment(.leading)
                    }
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 12)
                .frame(maxWidth: .infinity, alignment: .leading)

                if let image = preview.imageUrl.flatMap(URL.init(string:)) {
                    AsyncImage(url: image) { phase in
                        if case .success(let picture) = phase {
                            picture.resizable().scaledToFill()
                        } else {
                            Rectangle().fill(.quinary)
                        }
                    }
                    .frame(width: preview.kind == .image ? 150 : 132, height: 92)
                    .clipped()
                    .accessibilityHidden(true)
                }
            }
            .frame(maxWidth: 520, minHeight: 92, alignment: .leading)
            .background(.background.secondary)
            .clipShape(.rect(cornerRadius: 12))
            .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(.separator.opacity(hovering ? 1 : 0.7), lineWidth: 1))
            .shadow(color: .black.opacity(hovering ? 0.1 : 0.04), radius: hovering ? 8 : 3, y: hovering ? 3 : 1)
            .contentShape(.rect(cornerRadius: 12))
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .animation(.easeOut(duration: 0.15), value: hovering)
        .help(preview.url)
        .contextMenu {
            Button("Open Link", systemImage: "safari") {
                if let destination { NSWorkspace.shared.open(destination) }
            }
            Button("Copy Link", systemImage: "link") { copyToPasteboard(preview.url) }
        }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isLink)
    }

    @ViewBuilder private var favicon: some View {
        if let icon = preview.iconUrl.flatMap(URL.init(string:)) {
            AsyncImage(url: icon) { phase in
                if case .success(let image) = phase {
                    image.resizable().scaledToFit()
                } else {
                    Image(systemName: "globe").resizable().scaledToFit().foregroundStyle(.tertiary)
                }
            }
            .frame(width: 14, height: 14)
            .clipShape(.rect(cornerRadius: 3))
        } else {
            Image(systemName: "globe")
                .font(.caption)
                .foregroundStyle(.tertiary)
        }
    }
}
