import HermesCore
import SwiftUI

/// Sign-in: paste the dashboard address, then (only if the server asks) username and password.
struct OnboardingView: View {
    @Environment(AppModel.self) private var model

    private enum Field: Hashable {
        case address
        case username
        case password
    }

    @State private var address = ""
    @State private var server: ServerInfo?
    @State private var username = ""
    @State private var password = ""
    @State private var busy = false
    @State private var error: String?
    @FocusState private var focus: Field?

    private var passwordProvider: AuthProvider? {
        server?.providers.first { $0.supportsPassword }
    }

    private var browserProviders: [AuthProvider] {
        server?.providers.filter { !$0.supportsPassword } ?? []
    }

    var body: some View {
        VStack(spacing: 22) {
            VStack(spacing: 8) {
                Image(systemName: "bolt.horizontal.circle.fill")
                    .font(.system(size: 52))
                    .symbolRenderingMode(.hierarchical)
                    .foregroundStyle(.tint)
                    .accessibilityHidden(true)
                Text("Connect to Hermes")
                    .font(.system(size: 24, weight: .semibold))
                Text(server == nil
                     ? "Paste the address of your Hermes dashboard. It can be on this Mac, your network, or a tailnet."
                     : "Sign in to \(server?.host ?? "your server").")
                    .font(.system(size: 13.5))
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
            }

            VStack(spacing: 10) {
                if let server {
                    serverSummary(server)
                    credentialFields
                } else {
                    TextField("https://hermes.example.ts.net", text: $address)
                        .textFieldStyle(.roundedBorder)
                        .controlSize(.large)
                        .textContentType(.URL)
                        .autocorrectionDisabled()
                        .focused($focus, equals: .address)
                        .onSubmit(connect)
                        .accessibilityLabel("Dashboard address")
                }

                if let message = error ?? model.signInNotice {
                    Label(message, systemImage: "exclamationmark.triangle")
                        .font(.system(size: 12.5))
                        .foregroundStyle(.orange)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .fixedSize(horizontal: false, vertical: true)
                        .transition(.opacity)
                }
            }

            primaryButton
        }
        .padding(34)
        .frame(width: 400)
        .glassEffect(.regular, in: .rect(cornerRadius: 28))
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(backdrop)
        .animation(.snappy(duration: 0.3), value: server)
        .animation(.easeOut(duration: 0.2), value: error)
        .onAppear {
            if address.isEmpty { address = AccountStore.lastAddress() ?? "" }
            focus = .address
            // Coming back after an expired session: skip straight to the credentials.
            if !address.isEmpty, model.signInNotice != nil { connect() }
        }
    }

    private var backdrop: some View {
        ZStack {
            Color(nsColor: .windowBackgroundColor)
            RadialGradient(colors: [Color.accentColor.opacity(0.22), .clear], center: .init(x: 0.25, y: 0.2),
                           startRadius: 0, endRadius: 520)
            RadialGradient(colors: [Color.orange.opacity(0.16), .clear], center: .init(x: 0.8, y: 0.85),
                           startRadius: 0, endRadius: 480)
        }
        .ignoresSafeArea()
    }

    private func serverSummary(_ server: ServerInfo) -> some View {
        HStack(spacing: 8) {
            Image(systemName: "checkmark.circle.fill")
                .foregroundStyle(.green)
            VStack(alignment: .leading, spacing: 0) {
                Text(server.host)
                    .font(.system(size: 13, weight: .medium))
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text("Hermes \(server.version)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button("Change") {
                self.server = nil
                error = nil
                focus = .address
            }
            .buttonStyle(.link)
            .font(.system(size: 12.5))
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 9)
        .background(.fill.quaternary, in: .rect(cornerRadius: 10))
    }

    @ViewBuilder private var credentialFields: some View {
        if passwordProvider != nil {
            TextField("Username", text: $username)
                .textFieldStyle(.roundedBorder)
                .controlSize(.large)
                .textContentType(.username)
                .autocorrectionDisabled()
                .focused($focus, equals: .username)
                .onSubmit { focus = .password }
            SecureField("Password", text: $password)
                .textFieldStyle(.roundedBorder)
                .controlSize(.large)
                .textContentType(.password)
                .focused($focus, equals: .password)
                .onSubmit(signIn)
        }
        ForEach(browserProviders, id: \.name) { provider in
            Button {
                signInWithBrowser(provider)
            } label: {
                Label("Continue with \(provider.displayName)", systemImage: "safari")
                    .frame(maxWidth: .infinity)
            }
            .controlSize(.large)
            .disabled(busy)
        }
    }

    @ViewBuilder private var primaryButton: some View {
        if server == nil || passwordProvider != nil {
            Button(action: server == nil ? connect : signIn) {
                HStack(spacing: 8) {
                    if busy { ProgressView().controlSize(.small) }
                    Text(server == nil ? "Continue" : "Sign In")
                        .fontWeight(.medium)
                }
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(.glassProminent)
            .controlSize(.large)
            .keyboardShortcut(.defaultAction)
            .disabled(busy || !canSubmit)
        }
    }

    private var canSubmit: Bool {
        if server == nil { return !address.trimmingCharacters(in: .whitespaces).isEmpty }
        return !username.isEmpty && !password.isEmpty
    }

    private func run(_ work: @escaping () async throws -> Void) {
        guard !busy else { return }
        busy = true
        error = nil
        model.signInNotice = nil
        Task {
            do {
                try await work()
            } catch {
                self.error = error.userMessage
            }
            busy = false
        }
    }

    private func connect() {
        run {
            let info = try await probeServer(url: address)
            if !info.authRequired {
                // No accounts on this dashboard: nothing more to ask.
                let tokens = try await loginOpen(baseUrl: info.baseUrl)
                model.signedIn(server: info, tokens: tokens)
                return
            }
            guard info.supportsNativeFlow else {
                throw HermesError.Unsupported(message: "Hermes \(info.version) is too old for app sign-in. Update Hermes on the server and try again.")
            }
            server = info
            focus = .username
        }
    }

    private func signIn() {
        guard let server, let provider = passwordProvider, canSubmit else { return }
        run {
            let tokens = try await loginPassword(baseUrl: server.baseUrl, provider: provider.name,
                                                 username: username, password: password)
            password = ""
            model.signedIn(server: server, tokens: tokens)
        }
    }

    private func signInWithBrowser(_ provider: AuthProvider) {
        guard let server else { return }
        run {
            let tokens = try await loginBrowser(baseUrl: server.baseUrl, provider: provider.name, opener: BrowserOpener())
            model.signedIn(server: server, tokens: tokens)
        }
    }
}
