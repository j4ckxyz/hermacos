import HermesCore
import SwiftUI

/// The conversation pane: transcript, anything the agent is waiting on, and the composer.
struct ChatView: View {
    @Environment(AppModel.self) private var model
    let chat: ChatModel

    @State private var position = ScrollPosition(edge: .bottom)
    /// The reader is at the end, so new text should keep it in view.
    @State private var pinned = true

    private var title: String {
        chat.title.isEmpty ? "New Chat" : chat.title
    }

    var body: some View {
        Group {
            if chat.isEmpty, chat.loadError == nil {
                EmptyChatView(host: model.account?.host)
            } else {
                transcript
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .safeAreaBar(edge: .bottom, spacing: 0) {
            VStack(spacing: 8) {
                if chat.hasPendingRequest {
                    RequestCard(chat: chat)
                        .transition(.move(edge: .bottom).combined(with: .opacity))
                }
                ConnectionNotice(state: model.connection)
                ComposerView(chat: chat)
            }
            .frame(maxWidth: Theme.columnWidth + 24)
            .padding(.horizontal, 16)
            .padding(.bottom, 14)
            .padding(.top, 6)
            .frame(maxWidth: .infinity)
            .animation(.snappy(duration: 0.28), value: chat.hasPendingRequest)
        }
        .dropDestination(for: URL.self) { urls, _ in
            let files = urls.filter(\.isFileURL)
            chat.addFiles(files)
            if !files.isEmpty { model.composerFocusRequests += 1 }
            return !files.isEmpty
        }
        .navigationTitle(title)
        .navigationSubtitle(chat.modelName ?? "")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button("New Chat", systemImage: "square.and.pencil") { model.newChat() }
                    .help("New Chat (⌘N)")
            }
        }
    }

    /// The "working" line under the transcript. Hidden while the reply ends in an activity
    /// line, which already says what is happening.
    private var showsTailIndicator: Bool {
        guard chat.isStreaming, chat.status != nil else { return false }
        switch chat.items.last?.parts.last {
        case .tool?, .reasoning?: return false
        default: return true
        }
    }

    private var transcript: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 22) {
                if chat.isLoadingHistory {
                    ProgressView()
                        .controlSize(.small)
                        .frame(maxWidth: .infinity)
                        .padding(.top, 40)
                }
                if let error = chat.loadError {
                    ContentUnavailableView {
                        Label("Couldn't Load This Chat", systemImage: "exclamationmark.bubble")
                    } description: {
                        Text(error)
                    } actions: {
                        Button("Try Again") { Task { await chat.loadHistory() } }
                    }
                }
                ForEach(chat.items) { item in
                    MessageView(item: item, chat: chat)
                        .id(item.id)
                }
                if showsTailIndicator {
                    ActivityIndicator(text: chat.status)
                        .transition(.opacity)
                }
            }
            .frame(maxWidth: Theme.columnWidth)
            .padding(.horizontal, 28)
            .padding(.top, 18)
            .padding(.bottom, 20)
            .frame(maxWidth: .infinity)
        }
        .scrollPosition($position)
        .defaultScrollAnchor(.bottom, for: .initialOffset)
        .scrollEdgeEffectStyle(.soft, for: [.top, .bottom])
        .onScrollGeometryChange(for: Bool.self) { geometry in
            geometry.contentSize.height - geometry.visibleRect.maxY < 60
        } action: { _, atEnd in
            pinned = atEnd
        }
        .onScrollGeometryChange(for: CGFloat.self) { geometry in
            geometry.contentSize.height
        } action: { _, _ in
            // Text streaming in grows the content; follow it only if the reader was at the end.
            if pinned { position.scrollTo(edge: .bottom) }
        }
        .onChange(of: model.scrollToTopRequests) {
            pinned = false
            position.scrollTo(edge: .top)
        }
        .onChange(of: chat.items.count) {
            pinned = true
            withAnimation(.smooth(duration: 0.3)) { position.scrollTo(edge: .bottom) }
        }
        .overlay(alignment: .bottom) {
            if !pinned {
                Button {
                    pinned = true
                    withAnimation(.smooth(duration: 0.35)) { position.scrollTo(edge: .bottom) }
                } label: {
                    Image(systemName: "arrow.down")
                        .font(.system(size: 12, weight: .semibold))
                        .frame(width: 30, height: 30)
                }
                .buttonStyle(.plain)
                .glassEffect(.regular.interactive(), in: .circle)
                .padding(.bottom, 10)
                .help("Scroll to latest")
                .accessibilityLabel("Scroll to latest")
                .transition(.scale(scale: 0.8).combined(with: .opacity))
            }
        }
        .animation(.snappy(duration: 0.2), value: pinned)
    }
}

private struct EmptyChatView: View {
    let host: String?

    private var greeting: String {
        switch Calendar.current.component(.hour, from: Date()) {
        case 5..<12: "Good morning"
        case 12..<18: "Good afternoon"
        default: "Good evening"
        }
    }

    var body: some View {
        VStack(spacing: 10) {
            Image(systemName: "bolt.horizontal.circle.fill")
                .font(.system(size: 40, weight: .regular))
                .symbolRenderingMode(.hierarchical)
                .foregroundStyle(.tint)
                .accessibilityHidden(true)
            Text(greeting)
                .font(.system(size: 26, weight: .semibold))
            Text("Ask Hermes anything, or pick up an earlier chat from the sidebar.")
                .font(.system(size: 14))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
        }
        .padding(40)
        .frame(maxWidth: 460)
    }
}

/// A quiet line above the composer while the live connection is down.
private struct ConnectionNotice: View {
    let state: ConnectionState

    private var message: String? {
        switch state {
        case .connected, .unauthorized: nil
        case .connecting: "Connecting to Hermes…"
        case .reconnecting(_, _, let reason): reason.isEmpty ? "Reconnecting…" : "Reconnecting. \(reason)"
        case .disconnected: "Not connected."
        }
    }

    var body: some View {
        if let message {
            HStack(spacing: 7) {
                ProgressView().controlSize(.mini)
                Text(message)
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            .padding(.horizontal, 11)
            .padding(.vertical, 5)
            .glassEffect(.regular, in: .capsule)
            .transition(.opacity)
        }
    }
}
