import AppKit
import HermesCore
import SwiftUI

struct SidebarView: View {
    @Environment(AppModel.self) private var model
    @FocusState private var searchFocused: Bool

    /// The event being handled is a click or a list-navigation key, not typing.
    private static var userIsSelecting: Bool {
        guard let event = NSApp.currentEvent else { return false }
        switch event.type {
        case .leftMouseDown, .leftMouseUp:
            return true
        case .keyDown:
            // Arrow keys, Return, Enter, Home, End, Page Up, Page Down.
            return [123, 124, 125, 126, 36, 76, 115, 119, 116, 121].contains(event.keyCode)
        default:
            return false
        }
    }

    var body: some View {
        @Bindable var model = model
        let selection = Binding<AppModel.Selection?>(
            get: { model.selection },
            set: { value in
                guard let value else { return }
                // When filtering hides the selected row, List selects a neighbour on its own.
                // Follow only selections the user made, so typing a search doesn't switch chats.
                if !model.searchText.isEmpty, !Self.userIsSelecting { return }
                model.selection = value
            }
        )
        List(selection: selection) {
            if model.searchText.isEmpty {
                Section {
                    Label("New Chat", systemImage: "square.and.pencil")
                        .tag(AppModel.Selection.newChat)
                    Label("Server Status", systemImage: "waveform.path.ecg")
                        .tag(AppModel.Selection.status)
                }
            }

            if let problem = model.sessionsError {
                Section {
                    VStack(alignment: .leading, spacing: 6) {
                        Label("Chats couldn't be loaded", systemImage: "exclamationmark.triangle")
                            .foregroundStyle(.orange)
                        Text(problem)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(4)
                        Button("Try Again") { Task { await model.refreshSessions() } }
                            .controlSize(.small)
                    }
                    .padding(.vertical, 4)
                    .selectionDisabled()
                }
            }

            ForEach(model.groupedSessions) { group in
                Section(group.id) {
                    ForEach(group.sessions, id: \.id) { session in
                        SessionRow(session: session)
                            .tag(AppModel.Selection.session(session.id))
                    }
                }
            }

            if model.hasMoreSessions, model.searchText.isEmpty {
                // Reaching the end of the list fetches the next page of older chats.
                HStack {
                    Spacer()
                    ProgressView().controlSize(.small)
                    Spacer()
                }
                .selectionDisabled()
                .onAppear { Task { await model.loadMoreSessions() } }
                .accessibilityLabel("Loading older chats")
            }

            if !model.contentHits.isEmpty {
                Section("In Conversations") {
                    ForEach(model.contentHits, id: \.sessionId) { hit in
                        SearchHitRow(hit: hit)
                            .tag(AppModel.Selection.session(hit.sessionId))
                    }
                }
            }
        }
        .overlay {
            if !model.searchText.isEmpty, model.groupedSessions.isEmpty, model.contentHits.isEmpty {
                ContentUnavailableView.search(text: model.searchText)
            } else if model.hasLoadedSessions, model.sessions.isEmpty {
                ContentUnavailableView("No Chats Yet", systemImage: "bubble.left.and.text.bubble.right",
                                       description: Text("Conversations from every Hermes surface appear here."))
            }
        }
        .searchable(text: $model.searchText, placement: .sidebar, prompt: "Search")
        .searchFocused($searchFocused)
        .onChange(of: model.searchFocusRequests) { searchFocused = true }
        .safeAreaBar(edge: .bottom, spacing: 0) {
            HStack(spacing: 2) {
                AccountChip()
                UsageButton()
            }
            .padding(.leading, 12)
            .padding(.trailing, 14)
            .padding(.vertical, 8)
        }
        .navigationTitle("Hermacos")
    }
}

private struct SessionRow: View {
    @Environment(AppModel.self) private var model
    let session: SessionSummary

    var body: some View {
        HStack(spacing: 6) {
            Text(session.displayTitle)
                .lineLimit(1)
                .truncationMode(.tail)
            Spacer(minLength: 0)
            if session.isActive {
                Circle()
                    .fill(.green)
                    .frame(width: 6, height: 6)
                    .accessibilityLabel("Active")
            }
            if let symbol = session.sourceSymbol {
                Image(systemName: symbol)
                    .font(.caption)
                    .foregroundStyle(.tertiary)
                    .help("From \(session.sourceLabel)")
                    .accessibilityLabel("From \(session.sourceLabel)")
            }
            Text(session.shortTime)
                .font(.caption.monospacedDigit())
                .foregroundStyle(.tertiary)
                .layoutPriority(1)
                .help(Date(timeIntervalSince1970: session.lastActive).formatted(date: .complete, time: .shortened))
                .accessibilityLabel("Last message \(session.shortTime)")
        }
        .contextMenu {
            Button("Rename…", systemImage: "pencil") { model.renaming = session }
            Button("Copy Session ID", systemImage: "doc.on.doc") {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(session.id, forType: .string)
            }
            Divider()
            Button("Delete…", systemImage: "trash", role: .destructive) { model.pendingDelete = session }
        }
    }
}

private struct SearchHitRow: View {
    let hit: SearchHit

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(hit.title.isEmpty ? "Untitled" : hit.title)
                .lineLimit(1)
            Text(hit.snippet.replacingOccurrences(of: "\n", with: " "))
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(2)
        }
        .padding(.vertical, 2)
    }
}

/// Which server this is and whether the live connection is up.
private struct AccountChip: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openSettings) private var openSettings

    private var statusColor: Color {
        switch model.connection {
        case .connected: .green
        case .connecting, .reconnecting: .orange
        case .unauthorized, .disconnected: .red
        }
    }

    private var statusText: String {
        switch model.connection {
        case .connected: "Connected"
        case .connecting: "Connecting…"
        case .reconnecting: "Reconnecting…"
        case .unauthorized: "Signed out"
        case .disconnected: "Offline"
        }
    }

    var body: some View {
        Menu {
            Section(model.account?.baseURL ?? "") {
                Button("Server Status", systemImage: "waveform.path.ecg") { model.selection = .status }
                Button("Settings…", systemImage: "gearshape") { openSettings() }
            }
            Divider()
            Button("Sign Out", systemImage: "rectangle.portrait.and.arrow.right") { model.signOut() }
        } label: {
            HStack(spacing: 9) {
                ZStack(alignment: .bottomTrailing) {
                    Image(systemName: "server.rack")
                        .font(.system(size: 13, weight: .medium))
                        .frame(width: 28, height: 28)
                        .background(.quaternary, in: .circle)
                    Circle()
                        .fill(statusColor)
                        .frame(width: 8, height: 8)
                        .overlay(Circle().stroke(.background, lineWidth: 1.5))
                }
                VStack(alignment: .leading, spacing: 0) {
                    Text(model.account?.host ?? "Hermes")
                        .font(.callout.weight(.medium))
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Text(statusText)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 0)
            }
            .contentShape(.rect)
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .accessibilityLabel("\(model.account?.host ?? "Hermes"), \(statusText)")
    }
}
