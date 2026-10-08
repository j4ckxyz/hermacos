import Foundation
import HermesCore
import Security

/// Which dashboard the app is signed in to. Not secret; lives in user defaults.
struct Account: Codable, Equatable {
    var baseURL: String
    var host: String
    var userId: String
    var version: String
}

/// Saved sign-in: the account in user defaults, its tokens in the login keychain.
enum AccountStore {
    private static let accountKey = "account"
    private static let service = "app.hermacos.Hermacos"

    /// Development runs set this so they never read or write real credentials.
    static let isEphemeral = ProcessInfo.processInfo.environment["HERMACOS_EPHEMERAL"] != nil

    private struct StoredTokens: Codable {
        var legacy: Bool
        var accessToken: String
        var refreshToken: String?
        var expiresAt: Int64
        var provider: String
        var userId: String
    }

    static func load() -> (Account, AuthTokens)? {
        guard !isEphemeral,
              let data = UserDefaults.standard.data(forKey: accountKey),
              let account = try? JSONDecoder().decode(Account.self, from: data),
              let secret = readKeychain(account: account.baseURL),
              let stored = try? JSONDecoder().decode(StoredTokens.self, from: secret)
        else { return nil }
        let tokens = AuthTokens(
            kind: stored.legacy ? .legacy : .bearer,
            accessToken: stored.accessToken,
            refreshToken: stored.refreshToken,
            expiresAt: stored.expiresAt,
            provider: stored.provider,
            userId: stored.userId
        )
        return (account, tokens)
    }

    static func save(account: Account, tokens: AuthTokens) {
        guard !isEphemeral else { return }
        let stored = StoredTokens(
            legacy: tokens.kind == .legacy,
            accessToken: tokens.accessToken,
            refreshToken: tokens.refreshToken,
            expiresAt: tokens.expiresAt,
            provider: tokens.provider,
            userId: tokens.userId
        )
        guard let secret = try? JSONEncoder().encode(stored),
              let data = try? JSONEncoder().encode(account)
        else { return }
        UserDefaults.standard.set(data, forKey: accountKey)
        writeKeychain(account: account.baseURL, data: secret)
    }

    /// Forget the tokens but remember the address, so signing back in is one field.
    static func clearTokens(for account: Account) {
        guard !isEphemeral else { return }
        deleteKeychain(account: account.baseURL)
    }

    static func lastAddress() -> String? {
        guard !isEphemeral, let data = UserDefaults.standard.data(forKey: accountKey) else { return nil }
        return (try? JSONDecoder().decode(Account.self, from: data))?.baseURL
    }

    static func forget() {
        guard !isEphemeral else { return }
        if let address = lastAddress() { deleteKeychain(account: address) }
        UserDefaults.standard.removeObject(forKey: accountKey)
    }

    private static func query(_ account: String) -> [CFString: Any] {
        [kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecAttrAccount: account]
    }

    private static func readKeychain(account: String) -> Data? {
        var request = query(account)
        request[kSecReturnData] = true
        request[kSecMatchLimit] = kSecMatchLimitOne
        var result: CFTypeRef?
        guard SecItemCopyMatching(request as CFDictionary, &result) == errSecSuccess else { return nil }
        return result as? Data
    }

    private static func writeKeychain(account: String, data: Data) {
        let update = SecItemUpdate(query(account) as CFDictionary, [kSecValueData: data] as CFDictionary)
        if update == errSecItemNotFound {
            var item = query(account)
            item[kSecValueData] = data
            item[kSecAttrLabel] = "Hermacos sign-in"
            SecItemAdd(item as CFDictionary, nil)
        }
    }

    private static func deleteKeychain(account: String) {
        SecItemDelete(query(account) as CFDictionary)
    }
}

/// User preferences.
enum Preferences {
    static let linkPreviews = "linkPreviews"
    static let animateStreaming = "animateStreaming"
    /// `none`, `cost` or `tokens`: what the usage ring measures today against.
    static let dailyLimitKind = "dailyLimitKind"
    static let dailyLimitCost = "dailyLimitCost"
    static let dailyLimitTokens = "dailyLimitTokens"

    static func register() {
        UserDefaults.standard.register(defaults: [
            linkPreviews: true, animateStreaming: true,
            dailyLimitKind: "none", dailyLimitCost: 5.0, dailyLimitTokens: 1_000_000.0,
        ])
    }
}
