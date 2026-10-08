import AppKit
import Foundation
import HermesCore

/// Everything the Rust core pushes at the app, funnelled into one ordered stream.
enum CoreSignal {
    case connection(ConnectionState)
    case event(ChatEvent)
    case tokens(AuthTokens)
}

/// The core calls its listener from background threads. Yielding into an `AsyncStream` keeps
/// the events in order; one main-actor task drains it.
final class ListenerBridge: HermesListener, @unchecked Sendable {
    let signals: AsyncStream<CoreSignal>
    private let continuation: AsyncStream<CoreSignal>.Continuation

    init() {
        (signals, continuation) = AsyncStream.makeStream(of: CoreSignal.self, bufferingPolicy: .unbounded)
    }

    func onConnection(state: ConnectionState) { continuation.yield(.connection(state)) }
    func onEvent(event: ChatEvent) { continuation.yield(.event(event)) }
    func onTokens(tokens: AuthTokens) { continuation.yield(.tokens(tokens)) }

    func finish() { continuation.finish() }
}

/// Opens the sign-in page in the default browser for OAuth providers.
final class BrowserOpener: UrlOpener, @unchecked Sendable {
    func openUrl(url: String) {
        guard let url = URL(string: url) else { return }
        DispatchQueue.main.async { NSWorkspace.shared.open(url) }
    }
}

extension HermesError {
    /// The sentence written for people, without the enum wrapper.
    var message: String {
        switch self {
        case .Network(let message), .Unauthorized(let message), .InvalidCredentials(let message),
             .RateLimited(let message), .Protocol(let message), .NotConnected(let message),
             .Unsupported(let message):
            return message
        case .Server(_, let message), .Rpc(_, let message):
            return message
        }
    }

    var isUnauthorized: Bool {
        if case .Unauthorized = self { return true }
        return false
    }

    /// The gateway no longer knows the runtime session id this call used.
    var isStaleSession: Bool {
        guard case .Rpc(let code, let message) = self else { return false }
        let text = message.lowercased()
        return code == 4001 || code == 4007 || text.contains("unknown session") || text.contains("session not found")
    }
}

extension Error {
    var userMessage: String {
        (self as? HermesError)?.message ?? localizedDescription
    }
}
