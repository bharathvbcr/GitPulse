import Foundation

#if canImport(WaylodeProtocol)
import WaylodeProtocol
#endif

/// Thin Swift wrapper over the `src-tauri/crates/waylode-protocol` UniFFI XCFramework (Opus M4).
/// Matches the frozen `uniffi_api.rs` contract (board #18) 1:1.
final class WaylodeRustBridge {
    static let shared = WaylodeRustBridge()

    enum BridgeError: Error, LocalizedError {
        case frameworkUnavailable(String)
        case tunnelSessionNotFound(String)

        var errorDescription: String? {
            switch self {
            case .frameworkUnavailable(let fn):
                return "WaylodeProtocol XCFramework not linked for \(fn)"
            case .tunnelSessionNotFound(let id):
                return "Noise IK tunnel session not found: \(id)"
            }
        }
    }

    private let lock = NSLock()
    private var tunnelHandles: [String: AnyObject] = [:]

    func hashCanonical(toolName: String, toolInputJson: String, cwd: String) throws -> String {
        #if canImport(WaylodeProtocol)
        return try WaylodeProtocol.hashCanonical(
            toolName: toolName,
            toolInputJson: toolInputJson,
            cwd: cwd
        )
        #else
        throw BridgeError.frameworkUnavailable("hash_canonical")
        #endif
    }

    func renderCanonicalToolInput(toolName: String, toolInputJson: String, cwd: String) throws -> String {
        #if canImport(WaylodeProtocol)
        return try WaylodeProtocol.renderCanonicalToolInput(
            toolName: toolName,
            toolInputJson: toolInputJson,
            cwd: cwd
        )
        #else
        throw BridgeError.frameworkUnavailable("render_canonical_tool_input")
        #endif
    }

    func buildApprovalSigningBytes(
        hostId: String,
        ticketId: String,
        inputSha256: String,
        decision: String,
        nonce: String,
        issuedAt: Int64,
        expiresAt: Int64
    ) throws -> Data {
        #if canImport(WaylodeProtocol)
        return try WaylodeProtocol.buildApprovalSigningBytes(
            hostId: hostId,
            ticketId: ticketId,
            inputSha256: inputSha256,
            decision: decision,
            nonce: nonce,
            issuedAt: issuedAt,
            expiresAt: expiresAt
        )
        #else
        throw BridgeError.frameworkUnavailable("build_approval_signing_bytes")
        #endif
    }

    func hashSteeringText(text: String) throws -> String {
        #if canImport(WaylodeProtocol)
        return WaylodeProtocol.hashSteeringText(text: text)
        #else
        throw BridgeError.frameworkUnavailable("hash_steering_text")
        #endif
    }

    func buildSteeringSigningBytes(
        hostId: String,
        sessionKey: String,
        requestId: String,
        expectRoute: String,
        textSha256: String,
        nonce: String,
        issuedAt: Int64,
        expiresAt: Int64
    ) throws -> Data {
        #if canImport(WaylodeProtocol)
        return try WaylodeProtocol.buildSteeringSigningBytes(
            hostId: hostId,
            sessionKey: sessionKey,
            requestId: requestId,
            expectRoute: expectRoute,
            textSha256: textSha256,
            nonce: nonce,
            issuedAt: issuedAt,
            expiresAt: expiresAt
        )
        #else
        throw BridgeError.frameworkUnavailable("build_steering_signing_bytes")
        #endif
    }

    func verifyApprovalDecision(
        decisionJson: String,
        certJson: String,
        identityPubEd25519: Data,
        expectedHostId: String,
        expectedTicketId: String,
        expectedInputSha256: String,
        revocationsJson: String,
        minRevocationVersion: UInt64,
        nowSec: Int64
    ) throws -> String {
        #if canImport(WaylodeProtocol)
        let kind = try WaylodeProtocol.verifyApprovalDecision(
            decisionJson: decisionJson,
            certJson: certJson,
            identityPubEd25519: identityPubEd25519,
            expectedHostId: expectedHostId,
            expectedTicketId: expectedTicketId,
            expectedInputSha256: expectedInputSha256,
            revocationsJson: revocationsJson,
            minRevocationVersion: minRevocationVersion,
            nowSec: nowSec
        )
        return kind.rawValue
        #else
        throw BridgeError.frameworkUnavailable("verify_approval_decision")
        #endif
    }

    func openPreview(
        sealedBytes: Data,
        recipientX25519Secret: Data,
        senderEd25519Pub: Data,
        expectedSenderDeviceId: String
    ) throws -> Data {
        #if canImport(WaylodeProtocol)
        return try WaylodeProtocol.openPreview(
            sealedBytes: sealedBytes,
            recipientX25519Secret: recipientX25519Secret,
            senderEd25519Pub: senderEd25519Pub,
            expectedSenderDeviceId: expectedSenderDeviceId
        )
        #else
        throw BridgeError.frameworkUnavailable("open_preview")
        #endif
    }

    func signRelayRequest(
        deviceId: String,
        ed25519Seed: Data,
        method: String,
        requestUri: String,
        timestampMs: Int64,
        body: Data
    ) throws -> [String: String] {
        #if canImport(WaylodeProtocol)
        let headers = try WaylodeProtocol.signRelayRequest(
            deviceId: deviceId,
            ed25519Seed: ed25519Seed,
            method: method,
            requestUri: requestUri,
            timestampMs: timestampMs,
            body: body
        )
        return [
            "X-GitPulse-Device": headers.device,
            "X-GitPulse-Timestamp": headers.timestamp,
            "X-GitPulse-Signature": headers.signature,
        ]
        #else
        throw BridgeError.frameworkUnavailable("sign_relay_request")
        #endif
    }

    func tunnelInitiatorNew(
        localX25519Secret: Data,
        remoteCertJson: String,
        identityPubEd25519: Data,
        revocationsJson: String,
        minRevocationVersion: UInt64,
        nowSec: Int64,
        streamId: String,
        hostEpoch: UInt64
    ) throws -> (sessionId: String, handshakeInitB64: String) {
        #if canImport(WaylodeProtocol)
        let handshake = try WaylodeProtocol.tunnelInitiatorNew(
            localX25519Secret: localX25519Secret,
            remoteCertJson: remoteCertJson,
            identityPubEd25519: identityPubEd25519,
            revocationsJson: revocationsJson,
            minRevocationVersion: minRevocationVersion,
            nowSec: nowSec,
            streamId: streamId,
            hostEpoch: hostEpoch
        )
        let sessionId = UUID().uuidString.lowercased()
        lock.lock()
        tunnelHandles[sessionId] = handshake.session as AnyObject
        lock.unlock()
        return (sessionId, handshake.firstMessage.base64EncodedString())
        #else
        throw BridgeError.frameworkUnavailable("tunnel_initiator_new")
        #endif
    }

    func tunnelEncrypt(sessionId: String, plaintext: Data) throws -> Data {
        #if canImport(WaylodeProtocol)
        lock.lock()
        let handle = tunnelHandles[sessionId]
        lock.unlock()
        guard let session = handle as? TunnelSession else {
            throw BridgeError.tunnelSessionNotFound(sessionId)
        }
        return try WaylodeProtocol.tunnelEncrypt(session: session, plaintext: plaintext)
        #else
        throw BridgeError.frameworkUnavailable("tunnel_encrypt")
        #endif
    }

    func tunnelDecrypt(sessionId: String, ciphertext: Data) throws -> Data {
        #if canImport(WaylodeProtocol)
        lock.lock()
        let handle = tunnelHandles[sessionId]
        lock.unlock()
        guard let session = handle as? TunnelSession else {
            throw BridgeError.tunnelSessionNotFound(sessionId)
        }
        return try WaylodeProtocol.tunnelDecrypt(session: session, ciphertext: ciphertext)
        #else
        throw BridgeError.frameworkUnavailable("tunnel_decrypt")
        #endif
    }

    func tunnelClose(sessionId: String) {
        lock.lock()
        tunnelHandles.removeValue(forKey: sessionId)
        lock.unlock()
    }
}
