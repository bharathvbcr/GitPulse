package com.gitpulse.crypto

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.security.KeyStore
import java.util.Arrays
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Thin Kotlin bridge to `libgitpulse_protocol.so` (crates/gitpulse-protocol UniFFI / JNI, Opus M4).
 * Matches frozen `uniffi_api.rs` contract (board #18) 1:1.
 *
 * Security invariants (Opus M2 & #15):
 * - Ed25519 / X25519 identity keys are encrypted at rest with an Android Keystore AES-256-GCM key.
 * - Unwrapped secret bytes live strictly in native/JVM memory for the Rust call and are
 *   immediately zeroed via `Arrays.fill(secret, 0)`.
 * - Raw private key bytes NEVER cross the JS bridge.
 */
object GitPulseRustBridge {
    private const val ANDROID_KEYSTORE = "AndroidKeyStore"
    private const val WRAP_KEY_ALIAS = "com.gitpulse.mobile.identity.wrap.aes256.v1"
    private const val PREFS_NAME = "gitpulse_wrapped_identity_keys"
    private const val GCM_TAG_BITS = 128

    private val tunnelHandles = ConcurrentHashMap<String, Long>()

    init {
        try {
            System.loadLibrary("gitpulse_protocol")
        } catch (_: UnsatisfiedLinkError) {
            // Native .so is built via `cargo ndk` during Android prebuild/CI.
        }
    }

    external fun nativeHashCanonical(toolName: String, toolInputJson: String, cwd: String): String
    external fun nativeRenderCanonicalToolInput(toolName: String, toolInputJson: String, cwd: String): String

    external fun nativeBuildApprovalSigningBytes(
        hostId: String,
        ticketId: String,
        inputSha256: String,
        decision: String,
        nonce: String,
        issuedAt: Long,
        expiresAt: Long,
    ): ByteArray

    external fun nativeHashSteeringText(text: String): String

    external fun nativeBuildSteeringSigningBytes(
        hostId: String,
        sessionKey: String,
        requestId: String,
        expectRoute: String,
        textSha256: String,
        nonce: String,
        issuedAt: Long,
        expiresAt: Long,
    ): ByteArray

    external fun nativeVerifyApprovalDecision(
        decisionJson: String,
        certJson: String,
        identityPubEd25519: ByteArray,
        expectedHostId: String,
        expectedTicketId: String,
        expectedInputSha256: String,
        revocationsJson: String,
        minRevocationVersion: Long,
        nowSec: Long,
    ): String

    external fun nativeOpenPreview(
        sealedBytes: ByteArray,
        recipientX25519Secret: ByteArray,
        senderEd25519Pub: ByteArray,
        expectedSenderDeviceId: String,
    ): ByteArray

    external fun nativeSignRelayRequest(
        deviceId: String,
        ed25519Seed: ByteArray,
        method: String,
        requestUri: String,
        timestampMs: Long,
        body: ByteArray,
    ): Map<String, String>

    external fun nativeTunnelInitiatorNew(
        localX25519Secret: ByteArray,
        remoteCertJson: String,
        identityPubEd25519: ByteArray,
        revocationsJson: String,
        minRevocationVersion: Long,
        nowSec: Long,
        streamId: String,
        hostEpoch: Long,
    ): Pair<Long, ByteArray>

    external fun nativeTunnelEncrypt(handle: Long, plaintext: ByteArray): ByteArray
    external fun nativeTunnelDecrypt(handle: Long, ciphertext: ByteArray): ByteArray
    external fun nativeTunnelFree(handle: Long)
    external fun nativeDeriveEd25519Public(secret: ByteArray): ByteArray
    external fun nativeDeriveX25519Public(secret: ByteArray): ByteArray

    fun openPreviewWithStoredIdentityKey(
        context: Context,
        sealedBytes: ByteArray,
        senderEd25519Pub: ByteArray,
        expectedSenderDeviceId: String,
    ): String {
        val secret = unwrapIdentitySecret(context, "x25519_v1")
            ?: throw IllegalStateException("X25519 identity key not initialized")
        return try {
            val plaintext = nativeOpenPreview(
                sealedBytes,
                secret,
                senderEd25519Pub,
                expectedSenderDeviceId,
            )
            String(plaintext, Charsets.UTF_8)
        } finally {
            Arrays.fill(secret, 0.toByte())
        }
    }

    fun signRelayRequestWithStoredKey(
        context: Context,
        deviceId: String,
        method: String,
        requestUri: String,
        timestampMs: Long,
        bodyBytes: ByteArray,
    ): Map<String, String> {
        val secret = unwrapIdentitySecret(context, "ed25519_v1")
            ?: throw IllegalStateException("Ed25519 identity key not initialized")
        return try {
            nativeSignRelayRequest(deviceId, secret, method, requestUri, timestampMs, bodyBytes)
        } finally {
            Arrays.fill(secret, 0.toByte())
        }
    }

    fun tunnelInitiatorNew(
        context: Context,
        remoteCertJson: String,
        revocationsJson: String,
        minRevocationVersion: Long,
        nowSec: Long,
        streamId: String,
        hostEpoch: Long,
    ): Pair<String, String> {
        val secret = unwrapIdentitySecret(context, "x25519_v1")
            ?: throw IllegalStateException("X25519 identity key not initialized")
        val identityPub = unwrapIdentitySecret(context, "account_identity_pub_ed25519_v1")
            ?: throw IllegalStateException("Pinned account IdentityKey pubkey missing")
        return try {
            val (handle, firstMessage) = nativeTunnelInitiatorNew(
                secret,
                remoteCertJson,
                identityPub,
                revocationsJson,
                minRevocationVersion,
                nowSec,
                streamId,
                hostEpoch,
            )
            val sessionId = UUID.randomUUID().toString()
            tunnelHandles[sessionId] = handle
            sessionId to Base64.encodeToString(firstMessage, Base64.NO_WRAP)
        } finally {
            Arrays.fill(secret, 0.toByte())
        }
    }

    fun loadPinnedAccountIdentityPub(context: Context): ByteArray {
        return unwrapIdentitySecret(context, "account_identity_pub_ed25519_v1")
            ?: throw IllegalStateException("Pinned account IdentityKey pubkey missing")
    }

    fun tunnelEncrypt(sessionId: String, plaintext: ByteArray): String {
        val handle = tunnelHandles[sessionId]
            ?: throw IllegalStateException("Noise IK tunnel session not found: $sessionId")
        val ct = nativeTunnelEncrypt(handle, plaintext)
        return Base64.encodeToString(ct, Base64.NO_WRAP)
    }

    fun tunnelDecrypt(sessionId: String, ciphertext: ByteArray): ByteArray {
        val handle = tunnelHandles[sessionId]
            ?: throw IllegalStateException("Noise IK tunnel session not found: $sessionId")
        return nativeTunnelDecrypt(handle, ciphertext)
    }

    fun tunnelClose(sessionId: String) {
        val handle = tunnelHandles.remove(sessionId) ?: return
        nativeTunnelFree(handle)
    }

    fun ensureWrappedIdentitySecret(context: Context, keyName: String): ByteArray {
        unwrapIdentitySecret(context, keyName)?.let { return it }
        val secret = ByteArray(32)
        java.security.SecureRandom().nextBytes(secret)
        wrapIdentitySecret(context, keyName, secret)
        return secret
    }

    private fun getOrCreateWrapKey(): SecretKey {
        val ks = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        (ks.getKey(WRAP_KEY_ALIAS, null) as? SecretKey)?.let { return it }

        val keyGen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE)
        val spec = KeyGenParameterSpec.Builder(
            WRAP_KEY_ALIAS,
            KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
        )
            .setKeySize(256)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setRandomizedEncryptionRequired(true)
            .build()
        keyGen.init(spec)
        return keyGen.generateKey()
    }

    private fun wrapIdentitySecret(context: Context, keyName: String, plaintext: ByteArray) {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, getOrCreateWrapKey())
        val ciphertext = cipher.doFinal(plaintext)
        val iv = cipher.iv
        val combined = ByteArray(1 + iv.size + ciphertext.size)
        combined[0] = iv.size.toByte()
        System.arraycopy(iv, 0, combined, 1, iv.size)
        System.arraycopy(ciphertext, 0, combined, 1 + iv.size, ciphertext.size)

        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        prefs.edit().putString(keyName, Base64.encodeToString(combined, Base64.NO_WRAP)).apply()
    }

    private fun unwrapIdentitySecret(context: Context, keyName: String): ByteArray? {
        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        val encoded = prefs.getString(keyName, null) ?: return null
        val combined = Base64.decode(encoded, Base64.NO_WRAP)
        if (combined.size < 14) return null
        val ivLen = combined[0].toInt() and 0xFF
        if (combined.size <= 1 + ivLen) return null
        val iv = combined.copyOfRange(1, 1 + ivLen)
        val ciphertext = combined.copyOfRange(1 + ivLen, combined.size)

        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, getOrCreateWrapKey(), GCMParameterSpec(GCM_TAG_BITS, iv))
        return cipher.doFinal(ciphertext)
    }
}
