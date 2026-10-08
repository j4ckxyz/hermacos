import CryptoKit
import Foundation
import HermesCore
import Security

/// Which dashboard the app is signed in to. Not secret.
struct Account: Codable, Equatable {
    var baseURL: String
    var host: String
    var userId: String
    var version: String
}

/// Saved sign-in: the account, and its tokens in a `CredentialVault`.
enum AccountStore {
    private static let accountKey = "account"

    /// Development runs set this so they never read or write real credentials.
    static let isEphemeral = ProcessInfo.processInfo.environment["HERMACOS_EPHEMERAL"] != nil
    /// Tests point the whole store at a scratch folder instead of the user's own.
    static let overrideDirectory = ProcessInfo.processInfo.environment["HERMACOS_STORE_DIR"].map {
        URL(fileURLWithPath: $0, isDirectory: true)
    }

    private struct StoredTokens: Codable {
        var legacy: Bool
        var accessToken: String
        var refreshToken: String?
        var expiresAt: Int64
        var provider: String
        var userId: String
    }

    private static var accountData: Data? {
        get {
            if let overrideDirectory { return try? Data(contentsOf: overrideDirectory.appendingPathComponent("account.json")) }
            return UserDefaults.standard.data(forKey: accountKey)
        }
        set {
            if let overrideDirectory {
                let file = overrideDirectory.appendingPathComponent("account.json")
                try? FileManager.default.createDirectory(at: overrideDirectory, withIntermediateDirectories: true)
                if let newValue { try? newValue.write(to: file, options: .atomic) } else { try? FileManager.default.removeItem(at: file) }
            } else if let newValue {
                UserDefaults.standard.set(newValue, forKey: accountKey)
            } else {
                UserDefaults.standard.removeObject(forKey: accountKey)
            }
        }
    }

    static func load() -> (Account, AuthTokens)? {
        guard !isEphemeral,
              let data = accountData,
              let account = try? JSONDecoder().decode(Account.self, from: data),
              let secret = CredentialVault.load(account: account.baseURL),
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
        accountData = data
        CredentialVault.save(secret, account: account.baseURL)
    }

    /// Forget the tokens but remember the address, so signing back in is one field.
    static func clearTokens(for account: Account) {
        guard !isEphemeral else { return }
        CredentialVault.delete(account: account.baseURL)
    }

    static func lastAddress() -> String? {
        guard !isEphemeral, let data = accountData else { return nil }
        return (try? JSONDecoder().decode(Account.self, from: data))?.baseURL
    }

    static func forget() {
        guard !isEphemeral else { return }
        if let address = lastAddress() { CredentialVault.delete(account: address) }
        accountData = nil
    }
}

/// Where the sign-in tokens are kept.
///
/// The login keychain ties every item to the app that made it. An app signed with an Apple
/// Developer ID is recognised across updates by its team; an app without one is identified by
/// the hash of that exact build, so every update would make macOS ask for the keychain password
/// again. So the keychain is used only when the app carries a team identifier.
///
/// Otherwise the tokens go in a file only this user can read, encrypted with a key that never
/// leaves this Mac's Secure Enclave. That keeps a copied or backed-up file useless elsewhere.
/// It does not stop other software running as the same user on this Mac, which the keychain
/// would. Macs without a Secure Enclave store the file unencrypted, still owner-only.
enum CredentialVault {
    private static let service = "app.hermacos.Hermacos"

    /// The Apple team that signed this app, when it was signed with a Developer ID.
    static let teamIdentifier: String? = {
        var code: SecCode?
        guard SecCodeCopySelf([], &code) == errSecSuccess, let code else { return nil }
        var staticCode: SecStaticCode?
        guard SecCodeCopyStaticCode(code, [], &staticCode) == errSecSuccess, let staticCode else { return nil }
        var info: CFDictionary?
        let flags = SecCSFlags(rawValue: kSecCSSigningInformation)
        guard SecCodeCopySigningInformation(staticCode, flags, &info) == errSecSuccess,
              let details = info as? [String: Any]
        else { return nil }
        return details[kSecCodeInfoTeamIdentifier as String] as? String
    }()

    private static var usesKeychain: Bool { teamIdentifier != nil && AccountStore.overrideDirectory == nil }

    static func load(account: String) -> Data? {
        if usesKeychain {
            if let secret = readKeychain(account: account) { return secret }
            // First launch of a signed build after an unsigned one: bring the file along.
            guard let secret = readFile(account: account) else { return nil }
            writeKeychain(account: account, data: secret)
            try? FileManager.default.removeItem(at: fileURL)
            return secret
        }
        if let secret = readFile(account: account) { return secret }
        // An earlier build kept the tokens in the keychain. Take them over only if macOS hands
        // them back silently; otherwise the user signs in once more rather than being asked
        // for a keychain password.
        guard AccountStore.overrideDirectory == nil, let secret = readKeychain(account: account, silently: true) else { return nil }
        writeFile(secret, account: account)
        deleteKeychain(account: account)
        return secret
    }

    static func save(_ secret: Data, account: String) {
        if usesKeychain { writeKeychain(account: account, data: secret) } else { writeFile(secret, account: account) }
    }

    static func delete(account: String) {
        if usesKeychain { deleteKeychain(account: account) }
        try? FileManager.default.removeItem(at: fileURL)
    }

    // MARK: File

    private struct Envelope: Codable {
        var version = 1
        var account: String
        /// Secure Enclave key, usable only by this Mac's enclave. Absent when stored plainly.
        var enclaveKey: Data?
        /// Public half of the one-off key the secret was sealed against.
        var peerKey: Data?
        /// AES-GCM box when `enclaveKey` is present, else the secret itself.
        var payload: Data
    }

    private static var fileURL: URL {
        let folder = AccountStore.overrideDirectory
            ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
                .appendingPathComponent("Hermacos", isDirectory: true)
        return folder.appendingPathComponent("credentials.json")
    }

    private static let keyInfo = Data("hermacos-credentials-v1".utf8)

    private static func sealingKey(enclave: SecureEnclave.P256.KeyAgreement.PrivateKey, peer: P256.KeyAgreement.PublicKey) throws -> SymmetricKey {
        try enclave.sharedSecretFromKeyAgreement(with: peer)
            .hkdfDerivedSymmetricKey(using: SHA256.self, salt: Data(), sharedInfo: keyInfo, outputByteCount: 32)
    }

    private static func readFile(account: String) -> Data? {
        guard let data = try? Data(contentsOf: fileURL),
              let envelope = try? JSONDecoder().decode(Envelope.self, from: data),
              envelope.account == account
        else { return nil }
        guard let blob = envelope.enclaveKey, let peerData = envelope.peerKey else { return envelope.payload }
        guard let enclave = try? SecureEnclave.P256.KeyAgreement.PrivateKey(dataRepresentation: blob),
              let peer = try? P256.KeyAgreement.PublicKey(rawRepresentation: peerData),
              let key = try? sealingKey(enclave: enclave, peer: peer),
              let box = try? AES.GCM.SealedBox(combined: envelope.payload)
        else { return nil }
        return try? AES.GCM.open(box, using: key)
    }

    private static func writeFile(_ secret: Data, account: String) {
        var envelope = Envelope(account: account, payload: secret)
        if SecureEnclave.isAvailable {
            // Reuse this Mac's enclave key if the file already has one; seal against a fresh
            // one-off key each time.
            let existing = (try? Data(contentsOf: fileURL))
                .flatMap { try? JSONDecoder().decode(Envelope.self, from: $0) }?.enclaveKey
                .flatMap { try? SecureEnclave.P256.KeyAgreement.PrivateKey(dataRepresentation: $0) }
            let peer = P256.KeyAgreement.PrivateKey().publicKey
            if let enclave = existing ?? (try? SecureEnclave.P256.KeyAgreement.PrivateKey()),
               let key = try? sealingKey(enclave: enclave, peer: peer),
               let sealed = try? AES.GCM.seal(secret, using: key).combined {
                envelope.enclaveKey = enclave.dataRepresentation
                envelope.peerKey = peer.rawRepresentation
                envelope.payload = sealed
            }
        }
        guard let data = try? JSONEncoder().encode(envelope) else { return }
        let folder = fileURL.deletingLastPathComponent()
        try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        // Created owner-only from the start, never briefly world-readable.
        FileManager.default.createFile(atPath: fileURL.path, contents: data, attributes: [.posixPermissions: 0o600])
        try? FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: fileURL.path)
    }

    // MARK: Keychain

    private static func query(_ account: String) -> [CFString: Any] {
        [kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecAttrAccount: account]
    }

    private static func readKeychain(account: String, silently: Bool = false) -> Data? {
        // With interaction off, an item this build may not read fails instead of prompting.
        if silently { SecKeychainSetUserInteractionAllowed(false) }
        defer { if silently { SecKeychainSetUserInteractionAllowed(true) } }
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
        SecKeychainSetUserInteractionAllowed(false)
        defer { SecKeychainSetUserInteractionAllowed(true) }
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
