import CryptoKit
import ExpoModulesCore
import Foundation
import LocalAuthentication
import Security
import UIKit

/// GitPulse Native Crypto & Hardware Signing Expo Module for iOS (Opus M1–M4).
/// Aligned 100% with `src-tauri/crates/gitpulse-protocol` (board #18).
///
/// Two-Key Hardware Model (Opus M2):
/// 1. Approval Key: Non-exportable hardware P-256 key in Secure Enclave
///    (`kSecAttrAccessibleWhenUnlockedThisDeviceOnly`, `.biometryCurrentSet`, `.privateKeyUsage`).
///    Used EXCLUSIVELY for signing `approval.decide` payloads.
/// 2. Identity Keys (Ed25519 + X25519): Stored in shared App Group Keychain
///    (`group.com.gitpulse.mobile`, `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`) so the
///    iOS Notification Service Extension can decrypt `<=2KiB` previews without Hermes.
/// 3. Pinned Account IdentityKey Pub: Stored in Keychain (Opus #15 trust boundary).
///
/// Invariant: Raw private key bytes NEVER cross the JS bridge.
public final class GitPulseCryptoModule: Module {
    static let appGroup = "group.com.gitpulse.mobile"
    static let approvalP256Tag = "com.gitpulse.mobile.approval.p256.v1"
    static let ed25519KeyAccount = "gitpulse.identity.ed25519.v1"
    static let x25519KeyAccount = "gitpulse.identity.x25519.v1"
    static let pinnedAccountIdentityPubAccount = "gitpulse.account.identity_pub_ed25519.v1"
    static let deviceIdAccount = "gitpulse.device.id.v1"

    public func definition() -> ModuleDefinition {
        Name("GitPulseCrypto")

        AsyncFunction("ensureDeviceKeys") { () throws -> [String: Any] in
            return try self.ensureOrCreateDeviceKeys()
        }

        AsyncFunction("hashCanonical") { (toolName: String, toolInputJson: String, cwd: String) throws -> String in
            return try GitPulseRustBridge.shared.hashCanonical(
                toolName: toolName,
                toolInputJson: toolInputJson,
                cwd: cwd
            )
        }

        AsyncFunction("renderCanonicalToolInput") { (toolName: String, toolInputJson: String, cwd: String) throws -> String in
            return try GitPulseRustBridge.shared.renderCanonicalToolInput(
                toolName: toolName,
                toolInputJson: toolInputJson,
                cwd: cwd
            )
        }

        AsyncFunction("signRelayRequest") { (method: String, requestUri: String, timestampMs: Int64, bodyBytesB64: String) throws -> [String: String] in
            let bodyData = Data(base64Encoded: bodyBytesB64) ?? Data()
            var edSecret = try self.loadSharedKeychainSecret(account: Self.ed25519KeyAccount)
            defer { edSecret.resetBytes(in: 0..<edSecret.count) }

            let deviceId = try self.ensureDeviceId()
            return try GitPulseRustBridge.shared.signRelayRequest(
                deviceId: deviceId,
                ed25519Seed: edSecret,
                method: method,
                requestUri: requestUri,
                timestampMs: timestampMs,
                body: bodyData
            )
        }

        AsyncFunction("openPreview") { (previewCtB64: String, senderEd25519PubB64: String, expectedSenderDeviceId: String) throws -> [String: Any] in
            guard
                let sealedBytes = Data(base64Encoded: previewCtB64),
                let senderPub = Data(base64Encoded: senderEd25519PubB64)
            else {
                throw NSError(domain: "GitPulseCrypto", code: 1001, userInfo: [
                    NSLocalizedDescriptionKey: "Invalid base64 preview ciphertext or sender Ed25519 pubkey",
                ])
            }
            var xSecret = try self.loadSharedKeychainSecret(account: Self.x25519KeyAccount)
            defer { xSecret.resetBytes(in: 0..<xSecret.count) }

            let plaintextBytes = try GitPulseRustBridge.shared.openPreview(
                sealedBytes: sealedBytes,
                recipientX25519Secret: xSecret,
                senderEd25519Pub: senderPub,
                expectedSenderDeviceId: expectedSenderDeviceId
            )
            guard let parsed = try JSONSerialization.jsonObject(with: plaintextBytes) as? [String: Any] else {
                throw NSError(domain: "GitPulseCrypto", code: 1002, userInfo: [
                    NSLocalizedDescriptionKey: "Decrypted preview was not a valid JSON object",
                ])
            }
            return parsed
        }

        AsyncFunction("tunnelInitiatorNew") { (params: [String: Any]) throws -> [String: String] in
            guard
                let remoteCertJson = params["remote_cert_json"] as? String,
                let revocationsJson = params["revocations_json"] as? String,
                let minRevVersion = params["min_revocation_version"] as? Int,
                let nowSec = params["now_sec"] as? Int64,
                let streamId = params["stream_id"] as? String,
                let hostEpoch = params["host_epoch"] as? Int
            else {
                throw NSError(domain: "GitPulseCrypto", code: 1004, userInfo: [
                    NSLocalizedDescriptionKey: "Malformed tunnelInitiatorNew params",
                ])
            }

            var xSecret = try self.loadSharedKeychainSecret(account: Self.x25519KeyAccount)
            defer { xSecret.resetBytes(in: 0..<xSecret.count) }
            let identityPub = try self.loadSharedKeychainSecret(account: Self.pinnedAccountIdentityPubAccount)

            let result = try GitPulseRustBridge.shared.tunnelInitiatorNew(
                localX25519Secret: xSecret,
                remoteCertJson: remoteCertJson,
                identityPubEd25519: identityPub,
                revocationsJson: revocationsJson,
                minRevocationVersion: UInt64(max(0, minRevVersion)),
                nowSec: nowSec,
                streamId: streamId,
                hostEpoch: UInt64(max(0, hostEpoch))
            )
            return [
                "session_id": result.sessionId,
                "handshake_init_b64": result.handshakeInitB64,
            ]
        }

        AsyncFunction("tunnelEncrypt") { (sessionId: String, plaintextB64: String) throws -> String in
            guard let plaintext = Data(base64Encoded: plaintextB64) else {
                throw NSError(domain: "GitPulseCrypto", code: 1005)
            }
            let ct = try GitPulseRustBridge.shared.tunnelEncrypt(sessionId: sessionId, plaintext: plaintext)
            return ct.base64EncodedString()
        }

        AsyncFunction("tunnelDecrypt") { (sessionId: String, ciphertextB64: String) throws -> String in
            guard let ct = Data(base64Encoded: ciphertextB64) else {
                throw NSError(domain: "GitPulseCrypto", code: 1006)
            }
            let plaintext = try GitPulseRustBridge.shared.tunnelDecrypt(sessionId: sessionId, ciphertext: ct)
            return plaintext.base64EncodedString()
        }

        AsyncFunction("tunnelClose") { (sessionId: String) in
            GitPulseRustBridge.shared.tunnelClose(sessionId: sessionId)
        }

        AsyncFunction("presentApprovalSheet") { (request: [String: Any], promise: Promise) in
            DispatchQueue.main.async {
                GitPulseApprovalSheetPresenter.shared.present(
                    request: request,
                    signer: { signingBytes in
                        try self.signWithSecureEnclaveP256(signingBytes: signingBytes)
                    },
                    deviceIdProvider: {
                        try self.ensureDeviceId()
                    },
                    completion: { outcome in
                        promise.resolve(outcome)
                    }
                )
            }
        }

        // Opus #23 P1: Steering confirmation MUST be rendered and P-256 biometric-signed natively
        // so compromised JS/OTA cannot forge HumanConfirmed mailbox envelopes.
        AsyncFunction("presentSteeringConfirmSheet") { (request: [String: Any], promise: Promise) in
            DispatchQueue.main.async {
                GitPulseSteeringSheetPresenter.shared.present(
                    request: request,
                    signer: { signingBytes in
                        try self.signWithSecureEnclaveP256(signingBytes: signingBytes)
                    },
                    deviceIdProvider: {
                        try self.ensureDeviceId()
                    },
                    completion: { outcome in
                        promise.resolve(outcome)
                    }
                )
            }
        }

        // Opus M1: Notification quick-action signing ONLY supports deny and deny_and_interrupt.
        AsyncFunction("signDenyDecision") { (request: [String: Any]) throws -> [String: Any] in
            guard
                let decision = request["decision"] as? String,
                decision == "deny" || decision == "deny_and_interrupt",
                let hostId = request["host_id"] as? String,
                let ticketId = request["ticket_id"] as? String,
                let inputSha256 = request["input_sha256"] as? String,
                let expiresAt = request["expires_at"] as? Int64
            else {
                throw NSError(domain: "GitPulseCrypto", code: 1003, userInfo: [
                    NSLocalizedDescriptionKey: "Opus M1: signDenyDecision accepts ONLY deny or deny_and_interrupt.",
                ])
            }

            let deviceId = try self.ensureDeviceId()
            let nonce = UUID().uuidString.lowercased()
            let issuedAt = Int64(Date().timeIntervalSince1970)
            let signingBytes = try GitPulseRustBridge.shared.buildApprovalSigningBytes(
                hostId: hostId,
                ticketId: ticketId,
                inputSha256: inputSha256,
                decision: decision,
                nonce: nonce,
                issuedAt: issuedAt,
                expiresAt: expiresAt
            )
            let sigDer = try self.signWithSecureEnclaveP256(signingBytes: signingBytes)
            return [
                "host_id": hostId,
                "ticket_id": ticketId,
                "input_sha256": inputSha256,
                "decision": decision,
                "nonce": nonce,
                "issued_at": issuedAt,
                "expires_at": expiresAt,
                "device_id": deviceId,
                "sig_p256_der_b64": sigDer.base64EncodedString(),
            ]
        }

        AsyncFunction("verifyApprovalDecision") { (params: [String: Any]) throws -> String in
            guard
                let decisionJson = params["decision_json"] as? String,
                let certJson = params["cert_json"] as? String,
                let expectedHostId = params["expected_host_id"] as? String,
                let expectedTicketId = params["expected_ticket_id"] as? String,
                let expectedInputSha256 = params["expected_input_sha256"] as? String,
                let revocationsJson = params["revocations_json"] as? String,
                let minRevVersion = params["min_revocation_version"] as? Int,
                let nowSec = params["now_sec"] as? Int64
            else {
                throw NSError(domain: "GitPulseCrypto", code: 1007)
            }
            let identityPub = try self.loadSharedKeychainSecret(account: Self.pinnedAccountIdentityPubAccount)
            return try GitPulseRustBridge.shared.verifyApprovalDecision(
                decisionJson: decisionJson,
                certJson: certJson,
                identityPubEd25519: identityPub,
                expectedHostId: expectedHostId,
                expectedTicketId: expectedTicketId,
                expectedInputSha256: expectedInputSha256,
                revocationsJson: revocationsJson,
                minRevocationVersion: UInt64(max(0, minRevVersion)),
                nowSec: nowSec
            )
        }

        AsyncFunction("authenticateBiometricStepUp") { (reason: String, promise: Promise) in
            let context = LAContext()
            var authError: NSError?
            guard context.canEvaluatePolicy(.deviceOwnerAuthenticationWithBiometrics, error: &authError) else {
                promise.resolve([
                    "ok": false,
                    "error_message": authError?.localizedDescription ?? "Biometric authentication unavailable",
                ])
                return
            }
            context.evaluatePolicy(.deviceOwnerAuthenticationWithBiometrics, localizedReason: reason) { success, err in
                if success {
                    promise.resolve([
                        "ok": true,
                        "attested_at_ms": Int(Date().timeIntervalSince1970 * 1000),
                    ])
                } else {
                    promise.resolve([
                        "ok": false,
                        "error_message": err?.localizedDescription ?? "Biometric authentication failed",
                    ])
                }
            }
        }

        View(GitPulseApprovalSheetView.self) {
            Prop("request") { (view: GitPulseApprovalSheetView, request: [String: Any]) in
                view.configure(request: request)
            }
            Events("onOutcome")
        }
    }

    // MARK: - Two-Key Hardware Storage (Opus M2)

    private func ensureOrCreateDeviceKeys() throws -> [String: Any] {
        let deviceId = try ensureDeviceId()
        let p256Key = try ensureSecureEnclaveP256Key()
        guard let p256Pub = SecKeyCopyPublicKey(p256Key) else {
            throw NSError(domain: "GitPulseCrypto", code: 2001)
        }
        var exportErr: Unmanaged<CFError>?
        // Raw uncompressed 65-byte SEC1 P-256 point (0x04 || X || Y) matching DeviceCertificate v2.
        guard let sec1P256Pub = SecKeyCopyExternalRepresentation(p256Pub, &exportErr) as Data? else {
            throw exportErr?.takeRetainedValue() ?? NSError(domain: "GitPulseCrypto", code: 2002)
        }

        let edPub = try ensureIdentityKeyPair(account: Self.ed25519KeyAccount, kind: .ed25519)
        let xPub = try ensureIdentityKeyPair(account: Self.x25519KeyAccount, kind: .x25519)

        // Return ONLY public keys — raw private keys NEVER leave native Swift.
        return [
            "device_id": deviceId,
            "ed25519_pub_b64": edPub.base64EncodedString(),
            "x25519_pub_b64": xPub.base64EncodedString(),
            "approval_pub_p256_b64": sec1P256Pub.base64EncodedString(),
            "hardware_backed": true,
            "key_storage_backend": "ios_secure_enclave",
        ]
    }

    private func ensureSecureEnclaveP256Key() throws -> SecKey {
        let tagData = Self.approvalP256Tag.data(using: .utf8)!
        let query: [String: Any] = [
            kSecClass as String: kSecClassKey,
            kSecAttrApplicationTag as String: tagData,
            kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
            kSecReturnRef as String: true,
        ]
        var item: CFTypeRef?
        if SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess, let key = item {
            return (key as! SecKey)
        }

        var accessErr: Unmanaged<CFError>?
        guard let accessControl = SecAccessControlCreateWithFlags(
            kCFAllocatorDefault,
            kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
            [.privateKeyUsage, .biometryCurrentSet],
            &accessErr
        ) else {
            throw accessErr?.takeRetainedValue() ?? NSError(domain: "GitPulseCrypto", code: 2003)
        }

        let attributes: [String: Any] = [
            kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
            kSecAttrKeySizeInBits as String: 256,
            kSecAttrTokenID as String: kSecAttrTokenIDSecureEnclave,
            kSecPrivateKeyAttrs as String: [
                kSecAttrIsPermanent as String: true,
                kSecAttrApplicationTag as String: tagData,
                kSecAttrAccessControl as String: accessControl,
            ],
        ]

        var createErr: Unmanaged<CFError>?
        guard let privateKey = SecKeyCreateRandomKey(attributes as CFDictionary, &createErr) else {
            throw createErr?.takeRetainedValue() ?? NSError(domain: "GitPulseCrypto", code: 2004)
        }
        return privateKey
    }

    private func signWithSecureEnclaveP256(signingBytes: Data) throws -> Data {
        let key = try ensureSecureEnclaveP256Key()
        var signErr: Unmanaged<CFError>?
        guard let signature = SecKeyCreateSignature(
            key,
            .ecdsaSignatureMessageX962SHA256,
            signingBytes as CFData,
            &signErr
        ) as Data? else {
            throw signErr?.takeRetainedValue() ?? NSError(domain: "GitPulseCrypto", code: 2005)
        }
        return signature
    }

    private enum IdentityKeyKind {
        case ed25519
        case x25519
    }

    private func ensureIdentityKeyPair(account: String, kind: IdentityKeyKind) throws -> Data {
        if var existing = try? loadSharedKeychainSecret(account: account) {
            defer { existing.resetBytes(in: 0..<existing.count) }
            switch kind {
            case .ed25519:
                return try Curve25519.Signing.PrivateKey(rawRepresentation: existing).publicKey.rawRepresentation
            case .x25519:
                return try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: existing).publicKey.rawRepresentation
            }
        }

        var rawPrivate: Data
        let rawPublic: Data
        switch kind {
        case .ed25519:
            let priv = Curve25519.Signing.PrivateKey()
            rawPrivate = priv.rawRepresentation
            rawPublic = priv.publicKey.rawRepresentation
        case .x25519:
            let priv = Curve25519.KeyAgreement.PrivateKey()
            rawPrivate = priv.rawRepresentation
            rawPublic = priv.publicKey.rawRepresentation
        }
        defer { rawPrivate.resetBytes(in: 0..<rawPrivate.count) }

        let addQuery: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: Self.appGroup,
            kSecAttrAccount as String: account,
            kSecAttrAccessGroup as String: Self.appGroup,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData as String: rawPrivate,
        ]
        SecItemDelete(addQuery as CFDictionary)
        let status = SecItemAdd(addQuery as CFDictionary, nil)
        guard status == errSecSuccess else {
            throw NSError(domain: "GitPulseCrypto", code: Int(status))
        }
        return rawPublic
    }

    private func loadSharedKeychainSecret(account: String) throws -> Data {
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
        guard status == errSecSuccess, let data = item as? Data else {
            throw NSError(domain: "GitPulseCrypto", code: Int(status), userInfo: [
                NSLocalizedDescriptionKey: "Shared Keychain item missing for \(account)",
            ])
        }
        return data
    }

    private func ensureDeviceId() throws -> String {
        if let existing = try? loadSharedKeychainSecret(account: Self.deviceIdAccount),
           let str = String(data: existing, encoding: .utf8),
           !str.isEmpty {
            return str
        }
        let generated = "ios-\(UUID().uuidString.lowercased())"
        let addQuery: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: Self.appGroup,
            kSecAttrAccount as String: Self.deviceIdAccount,
            kSecAttrAccessGroup as String: Self.appGroup,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData as String: Data(generated.utf8),
        ]
        SecItemDelete(addQuery as CFDictionary)
        let status = SecItemAdd(addQuery as CFDictionary, nil)
        guard status == errSecSuccess else {
            throw NSError(domain: "GitPulseCrypto", code: Int(status))
        }
        return generated
    }
}
