import HermesCore
import SwiftUI

struct SettingsView: View {
    @Environment(AppModel.self) private var model
    @AppStorage(Preferences.linkPreviews) private var linkPreviews = true
    @AppStorage(Preferences.animateStreaming) private var animateStreaming = true
    @AppStorage(Preferences.dailyLimitKind) private var limitKind = "none"
    @AppStorage(Preferences.dailyLimitCost) private var limitCost = 5.0
    @AppStorage(Preferences.dailyLimitTokens) private var limitTokens = 1_000_000.0

    var body: some View {
        Form {
            Section("Chat") {
                Toggle("Animate replies as they arrive", isOn: $animateStreaming)
                Toggle("Show link previews", isOn: $linkPreviews)
                Text("Previews load a small part of each linked page from this Mac.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Section("Usage") {
                Picker("Daily limit", selection: $limitKind) {
                    Text("None").tag("none")
                    Text("Dollars").tag("cost")
                    Text("Tokens").tag("tokens")
                }
                if limitKind == "cost" {
                    TextField("Dollars per day", value: $limitCost, format: .number.precision(.fractionLength(0...2)))
                } else if limitKind == "tokens" {
                    TextField("Tokens per day", value: $limitTokens, format: .number.grouping(.automatic))
                }
                Text("The ring in the sidebar fills as today's usage approaches the limit. Hermes keeps working past it.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            Section("Server") {
                if let account = model.account {
                    LabeledContent("Address", value: account.baseURL)
                    LabeledContent("Signed in as", value: account.userId)
                    LabeledContent("Hermes version", value: account.version)
                    HStack {
                        Spacer()
                        Button("Sign Out") { model.signOut() }
                        Button("Sign Out and Forget Server", role: .destructive) { model.signOut(forgetServer: true) }
                    }
                } else {
                    Text("Not signed in.")
                        .foregroundStyle(.secondary)
                }
            }
        }
        .formStyle(.grouped)
        .frame(width: 480)
        .fixedSize(horizontal: false, vertical: true)
    }
}

/// Health of the Hermes host: gateway, connected platforms, memory and disk.
struct StatusView: View {
    @Environment(AppModel.self) private var model
    @State private var status: ServerStatus?
    @State private var error: String?
    @State private var loading = false

    var body: some View {
        Form {
            if let status {
                Section("Hermes") {
                    LabeledContent("Server", value: model.account?.host ?? "")
                    LabeledContent("Version", value: "\(status.version)  (\(status.releaseDate))")
                    LabeledContent("Overall") { StateBadge(state: status.overall) }
                    LabeledContent("Gateway") { StateBadge(state: status.gatewayState) }
                    LabeledContent("Active sessions", value: "\(status.activeSessions)")
                    LabeledContent("Agents running", value: "\(status.activeAgents)")
                }
                if !status.platforms.isEmpty {
                    Section("Platforms") {
                        ForEach(status.platforms, id: \.name) { platform in
                            LabeledContent(platform.name.capitalized) { StateBadge(state: platform.state) }
                        }
                    }
                }
                Section("Host") {
                    if status.memoryTotalMb > 0 {
                        let used = Double(status.memoryTotalMb - min(status.memoryAvailableMb, status.memoryTotalMb))
                        Gauge(value: used, in: 0...Double(status.memoryTotalMb)) {
                            Text("Memory")
                        } currentValueLabel: {
                            Text("\(megabytes(UInt32(used))) of \(megabytes(status.memoryTotalMb))")
                        }
                    }
                    if status.diskTotalMb > 0 {
                        let used = Double(status.diskTotalMb - min(status.diskFreeMb, status.diskTotalMb))
                        Gauge(value: used, in: 0...Double(status.diskTotalMb)) {
                            Text("Disk")
                        } currentValueLabel: {
                            Text("\(megabytes(UInt32(used))) of \(megabytes(status.diskTotalMb))")
                        }
                    }
                }
            } else if let error {
                ContentUnavailableView {
                    Label("Status Unavailable", systemImage: "wifi.exclamationmark")
                } description: {
                    Text(error)
                } actions: {
                    Button("Try Again") { Task { await load() } }
                }
            } else {
                ProgressView().frame(maxWidth: .infinity)
            }
        }
        .formStyle(.grouped)
        .frame(maxWidth: 620)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .navigationTitle("Server Status")
        .navigationSubtitle(model.account?.host ?? "")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button("Refresh", systemImage: "arrow.clockwise") { Task { await load() } }
                    .disabled(loading)
            }
        }
        .task { await load() }
    }

    private func megabytes(_ value: UInt32) -> String {
        Measurement(value: Double(value), unit: UnitInformationStorage.mebibytes)
            .formatted(.byteCount(style: .memory))
    }

    private func load() async {
        guard let client = model.client else { return }
        loading = true
        defer { loading = false }
        do {
            status = try await client.status()
            error = nil
        } catch {
            if status == nil { self.error = error.userMessage }
        }
    }
}

private struct StateBadge: View {
    let state: String

    private var color: Color {
        switch state.lowercased() {
        case "ok", "running", "connected", "healthy": .green
        case "starting", "connecting", "degraded", "retrying", "draining": .orange
        default: .red
        }
    }

    var body: some View {
        HStack(spacing: 6) {
            Circle().fill(color).frame(width: 7, height: 7)
            Text(state.isEmpty ? "unknown" : state.capitalized)
        }
        .accessibilityElement(children: .combine)
    }
}
