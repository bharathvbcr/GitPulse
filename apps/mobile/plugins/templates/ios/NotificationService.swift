import Foundation
import Security
import UserNotifications

// Linked from src-tauri/crates/waylode-protocol XCFramework.
// NEVER link React Native or Hermes into the Notification Service Extension (24 MB memory budget).
#if canImport(WaylodeProtocol)
import WaylodeProtocol
#endif

/// Waylode iOS Notification Service Extension (Opus M1, M2, R8, board #18).
///
/// Security invariants:
/// 1. Runs pure Swift + Rust XCFramework (zero Hermes / JS runtime).
/// 2. Reads X25519 private key and pinned host Ed25519 pubkey from shared App Group Keychain
///    (`kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`) and zeroizes secret immediately.
/// 3. Enforces `preview_ct_b64` raw bytes <= 2048 B (`"GPPV"` v1); never sends plaintext through APNs.
/// 4. Assigns category `WAYLODE_APPROVAL` which ONLY exposes `Deny` and `Deny & Stop` actions.
///    Lock-screen Approve is strictly forbidden (Opus M1): tapping opens the app to the native full-bytes sheet.
final class NotificationService: UNNotificationServiceExtension {
    private static let appGroup = "__APP_GROUP__"
    private static let x25519KeyAccount = "gitpulse.identity.x25519.v1"
    private static let approvalCategoryIdentifier = "WAYLODE_APPROVAL"
    private static let maxPreviewCiphertextBytes = 2048

    private var contentHandler: ((UNNotificationContent) -> Void)?
    private var bestAttemptContent: UNMutableNotificationContent?

    override func didReceive(
        _ request: UNNotificationRequest,
        withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void
    ) {
        self.contentHandler = contentHandler
        let mutableContent = (request.content.mutableCopy() as? UNMutableNotificationContent)
            ?? UNMutableNotificationContent()
        self.bestAttemptContent = mutableContent

        // Register category identifier with Deny / Deny & Stop ONLY (never Approve).
        mutableContent.categoryIdentifier = Self.approvalCategoryIdentifier

        guard
            let previewCtB64 = (mutableContent.userInfo["preview_ct_b64"] as? String)
                ?? (mutableContent.userInfo["preview_ct"] as? String),
            let hostDevice = mutableContent.userInfo["host_device"] as? String,
            let rawCiphertext = Data(base64Encoded: previewCtB64),
            rawCiphertext.count > 0,
            rawCiphertext.count <= Self.maxPreviewCiphertextBytes
        else {
            applyGenericFallback(to: mutableContent)
            contentHandler(mutableContent)
            return
        }

        do {
            let preview = try decryptPreviewLocally(
                sealedBytes: rawCiphertext,
                expectedSenderDeviceId: hostDevice
            )
            mutableContent.title = preview.title
            mutableContent.subtitle = preview.subtitle
            mutableContent.body = preview.body
            mutableContent.sound = .default
            mutableContent.interruptionLevel = .timeSensitive
            if let ticketId = preview.ticketId {
                mutableContent.userInfo["ticket_id"] = ticketId
                mutableContent.userInfo["deep_link"] = "gitpulse://approvals/\(ticketId)"
            }
        } catch {
            applyGenericFallback(to: mutableContent)
        }

        contentHandler(mutableContent)
    }

    override func serviceExtensionTimeWillExpire() {
        if let contentHandler = contentHandler, let content = bestAttemptContent {
            applyGenericFallback(to: content)
            contentHandler(content)
        }
    }

    private func applyGenericFallback(to content: UNMutableNotificationContent) {
        content.title = "Waylode Approval Required"
        content.subtitle = ""
        content.body = "Open Waylode to inspect the full command and verify its hash before deciding."
        content.categoryIdentifier = Self.approvalCategoryIdentifier
        content.sound = .default
    }

    private struct DecryptedPreview: Decodable {
        let ticketId: String?
        let title: String
        let subtitle: String
        let body: String

        enum CodingKeys: String, CodingKey {
            case ticketId = "ticket_id"
            case title
            case subtitle
            case body
        }
    }

    private enum NseError: Error {
        case missingIdentityKey
        case missingSenderPubKey
        case rustBridgeUnavailable
    }

    private func decryptPreviewLocally(
        sealedBytes: Data,
        expectedSenderDeviceId: String
    ) throws -> DecryptedPreview {
        var x25519Secret = try loadSharedKeychainItem(account: Self.x25519KeyAccount)
        defer {
            x25519Secret.resetBytes(in: 0..<x25519Secret.count)
        }
        let senderEd25519Pub = try loadSharedKeychainItem(
            account: "gitpulse.host.\(expectedSenderDeviceId).ed25519_pub"
        )

        #if canImport(WaylodeProtocol)
        let plaintextBytes = try WaylodeProtocol.openPreview(
            sealedBytes: sealedBytes,
            recipientX25519Secret: x25519Secret,
            senderEd25519Pub: senderEd25519Pub,
            expectedSenderDeviceId: expectedSenderDeviceId
        )
        return try JSONDecoder().decode(DecryptedPreview.self, from: plaintextBytes)
        #else
        throw NseError.rustBridgeUnavailable
        #endif
    }

    private func loadSharedKeychainItem(account: String) throws -> Data {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: Self.appGroup,
            kSecAttrAccount as String: account,
            kSecAttrAccessGroup as String: Self.appGroup,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        guard status == errSecSuccess, let data = item as? Data, data.count == 32 else {
            throw NseError.missingIdentityKey
        }
        return data
    }
}
