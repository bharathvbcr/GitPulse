package com.gitpulse.crypto

import android.content.Context
import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.security.keystore.StrongBoxUnavailableException
import android.util.Base64
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity
import expo.modules.kotlin.Promise
import expo.modules.kotlin.modules.Module
import expo.modules.kotlin.modules.ModuleDefinition
import org.json.JSONObject
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.PrivateKey
import java.security.Signature
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec
import java.util.Arrays
import java.util.UUID

/**
 * Waylode Native Crypto & Hardware Signing Expo Module for Android (Opus M1–M4).
 * Aligned 100% with `src-tauri/crates/waylode-protocol` (board #18).
 */
class WaylodeCryptoModule : Module() {
    companion object {
        private const val ANDROID_KEYSTORE = "AndroidKeyStore"
        private const val APPROVAL_P256_ALIAS = "com.gitpulse.mobile.approval.p256.v1"
        private const val META_PREFS = "gitpulse_device_meta"
        private const val KEY_DEVICE_ID = "device_id"
        private const val KEY_STRONGBOX_BACKED = "strongbox_backed"
    }

    private val reactContext: Context
        get() = requireNotNull(appContext.reactContext) { "Android ReactContext unavailable" }

    override fun definition() = ModuleDefinition {
        Name("WaylodeCrypto")

        AsyncFunction("ensureDeviceKeys") {
            ensureOrCreateDeviceKeys()
        }

        AsyncFunction("hashCanonical") { toolName: String, toolInputJson: String, cwd: String ->
            WaylodeRustBridge.nativeHashCanonical(toolName, toolInputJson, cwd)
        }

        AsyncFunction("renderCanonicalToolInput") { toolName: String, toolInputJson: String, cwd: String ->
            WaylodeRustBridge.nativeRenderCanonicalToolInput(toolName, toolInputJson, cwd)
        }

        AsyncFunction("signRelayRequest") { method: String, requestUri: String, timestampMs: Long, bodyBytesB64: String ->
            val bodyBytes = Base64.decode(bodyBytesB64, Base64.DEFAULT)
            val deviceId = ensureDeviceId()
            WaylodeRustBridge.signRelayRequestWithStoredKey(
                reactContext,
                deviceId,
                method,
                requestUri,
                timestampMs,
                bodyBytes,
            )
        }

        AsyncFunction("openPreview") { previewCtB64: String, senderEd25519PubB64: String, expectedSenderDeviceId: String ->
            val sealed = Base64.decode(previewCtB64, Base64.DEFAULT)
            val senderPub = Base64.decode(senderEd25519PubB64, Base64.DEFAULT)
            val plaintextJson = WaylodeRustBridge.openPreviewWithStoredIdentityKey(
                reactContext,
                sealed,
                senderPub,
                expectedSenderDeviceId,
            )
            val obj = JSONObject(plaintextJson)
            mapOf(
                "ticket_id" to obj.optString("ticket_id"),
                "title" to obj.optString("title"),
                "subtitle" to obj.optString("subtitle"),
                "body" to obj.optString("body"),
            )
        }

        AsyncFunction("tunnelInitiatorNew") { params: Map<String, Any?> ->
            val remoteCertJson = params["remote_cert_json"] as? String ?: ""
            val revocationsJson = params["revocations_json"] as? String ?: ""
            val minRevVersion = (params["min_revocation_version"] as? Number)?.toLong() ?: 0L
            val nowSec = (params["now_sec"] as? Number)?.toLong() ?: (System.currentTimeMillis() / 1000L)
            val streamId = params["stream_id"] as? String ?: ""
            val hostEpoch = (params["host_epoch"] as? Number)?.toLong() ?: 0L

            val (sessionId, initB64) = WaylodeRustBridge.tunnelInitiatorNew(
                reactContext,
                remoteCertJson,
                revocationsJson,
                minRevVersion,
                nowSec,
                streamId,
                hostEpoch,
            )
            mapOf(
                "session_id" to sessionId,
                "handshake_init_b64" to initB64,
            )
        }

        AsyncFunction("tunnelEncrypt") { sessionId: String, plaintextB64: String ->
            val plaintext = Base64.decode(plaintextB64, Base64.DEFAULT)
            WaylodeRustBridge.tunnelEncrypt(sessionId, plaintext)
        }

        AsyncFunction("tunnelDecrypt") { sessionId: String, ciphertextB64: String ->
            val ct = Base64.decode(ciphertextB64, Base64.DEFAULT)
            val plaintext = WaylodeRustBridge.tunnelDecrypt(sessionId, ct)
            Base64.encodeToString(plaintext, Base64.NO_WRAP)
        }

        AsyncFunction("tunnelClose") { sessionId: String ->
            WaylodeRustBridge.tunnelClose(sessionId)
        }

        AsyncFunction("presentApprovalSheet") { request: Map<String, Any?>, promise: Promise ->
            val activity = appContext.currentActivity as? FragmentActivity
            if (activity == null) {
                promise.resolve(
                    mapOf(
                        "status" to "cancelled",
                        "error_message" to "Active FragmentActivity unavailable for native approval sheet",
                    ),
                )
                return@AsyncFunction
            }
            activity.runOnUiThread {
                WaylodeApprovalSheetPresenter.present(
                    activity = activity,
                    request = request,
                    deviceId = ensureDeviceId(),
                    prepareSignature = { prepareP256Signature() },
                    onOutcome = { outcome -> promise.resolve(outcome) },
                )
            }
        }

        // Opus #23 P1: Steering confirmation MUST be rendered and P-256 biometric-signed natively
        // so compromised JS/OTA cannot forge HumanConfirmed mailbox envelopes.
        AsyncFunction("presentSteeringConfirmSheet") { request: Map<String, Any?>, promise: Promise ->
            val activity = appContext.currentActivity as? FragmentActivity
            if (activity == null) {
                promise.resolve(
                    mapOf(
                        "status" to "cancelled",
                        "error_message" to "Active FragmentActivity unavailable for native steering sheet",
                    ),
                )
                return@AsyncFunction
            }
            activity.runOnUiThread {
                WaylodeSteeringSheetPresenter.present(
                    activity = activity,
                    request = request,
                    deviceId = ensureDeviceId(),
                    prepareSignature = { prepareP256Signature() },
                    onOutcome = { outcome -> promise.resolve(outcome) },
                )
            }
        }

        // Opus M1: Notification quick-action signing ONLY permits deny and deny_and_interrupt.
        AsyncFunction("signDenyDecision") { request: Map<String, Any?>, promise: Promise ->
            val decision = request["decision"] as? String
            if (decision != "deny" && decision != "deny_and_interrupt") {
                promise.reject(
                    "ERR_FORBIDDEN_DECISION",
                    "Opus M1: signDenyDecision accepts ONLY deny or deny_and_interrupt.",
                    null,
                )
                return@AsyncFunction
            }

            val activity = appContext.currentActivity as? FragmentActivity
            if (activity == null) {
                promise.reject("ERR_NO_ACTIVITY", "FragmentActivity required for StrongBox biometric signing", null)
                return@AsyncFunction
            }

            val hostId = request["host_id"] as? String ?: ""
            val ticketId = request["ticket_id"] as? String ?: ""
            val inputSha256 = request["input_sha256"] as? String ?: ""
            val expiresAt = (request["expires_at"] as? Number)?.toLong() ?: 0L
            val nonce = UUID.randomUUID().toString()
            val issuedAt = System.currentTimeMillis() / 1000L
            val deviceId = ensureDeviceId()

            val signingBytes = WaylodeRustBridge.nativeBuildApprovalSigningBytes(
                hostId = hostId,
                ticketId = ticketId,
                inputSha256 = inputSha256,
                decision = decision,
                nonce = nonce,
                issuedAt = issuedAt,
                expiresAt = expiresAt,
            )

            activity.runOnUiThread {
                val signature = prepareP256Signature()
                val executor = ContextCompat.getMainExecutor(activity)
                val prompt = BiometricPrompt(
                    activity,
                    executor,
                    object : BiometricPrompt.AuthenticationCallback() {
                        override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                            try {
                                val unlockedSig = requireNotNull(result.cryptoObject?.signature)
                                unlockedSig.update(signingBytes)
                                val sigDer = unlockedSig.sign()
                                promise.resolve(
                                    mapOf(
                                        "host_id" to hostId,
                                        "ticket_id" to ticketId,
                                        "input_sha256" to inputSha256,
                                        "decision" to decision,
                                        "nonce" to nonce,
                                        "issued_at" to issuedAt,
                                        "expires_at" to expiresAt,
                                        "device_id" to deviceId,
                                        "sig_p256_der_b64" to Base64.encodeToString(sigDer, Base64.NO_WRAP),
                                    ),
                                )
                            } catch (e: Throwable) {
                                promise.reject("ERR_SIGN_FAILED", e.message, e)
                            }
                        }

                        override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                            promise.reject("ERR_BIOMETRIC", errString.toString(), null)
                        }
                    },
                )
                val info = BiometricPrompt.PromptInfo.Builder()
                    .setTitle("Confirm Deny Decision")
                    .setSubtitle("Sign $decision with hardware P-256 key")
                    .setAllowedAuthenticators(BiometricManager.Authenticators.BIOMETRIC_STRONG)
                    .setNegativeButtonText("Cancel")
                    .build()
                prompt.authenticate(info, BiometricPrompt.CryptoObject(signature))
            }
        }

        AsyncFunction("verifyApprovalDecision") { params: Map<String, Any?> ->
            val identityPub = WaylodeRustBridge.loadPinnedAccountIdentityPub(reactContext)
            WaylodeRustBridge.nativeVerifyApprovalDecision(
                decisionJson = params["decision_json"] as? String ?: "",
                certJson = params["cert_json"] as? String ?: "",
                identityPubEd25519 = identityPub,
                expectedHostId = params["expected_host_id"] as? String ?: "",
                expectedTicketId = params["expected_ticket_id"] as? String ?: "",
                expectedInputSha256 = params["expected_input_sha256"] as? String ?: "",
                revocationsJson = params["revocations_json"] as? String ?: "",
                minRevocationVersion = (params["min_revocation_version"] as? Number)?.toLong() ?: 0L,
                nowSec = (params["now_sec"] as? Number)?.toLong() ?: (System.currentTimeMillis() / 1000L),
            )
        }

        AsyncFunction("authenticateBiometricStepUp") { reason: String, promise: Promise ->
            val activity = appContext.currentActivity as? FragmentActivity
            if (activity == null) {
                promise.resolve(mapOf("ok" to false, "error_message" to "No active FragmentActivity"))
                return@AsyncFunction
            }
            activity.runOnUiThread {
                val executor = ContextCompat.getMainExecutor(activity)
                val prompt = BiometricPrompt(
                    activity,
                    executor,
                    object : BiometricPrompt.AuthenticationCallback() {
                        override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                            promise.resolve(
                                mapOf(
                                    "ok" to true,
                                    "attested_at_ms" to System.currentTimeMillis(),
                                ),
                            )
                        }

                        override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                            promise.resolve(mapOf("ok" to false, "error_message" to errString.toString()))
                        }
                    },
                )
                val info = BiometricPrompt.PromptInfo.Builder()
                    .setTitle("Waylode Biometric Step-Up")
                    .setSubtitle(reason)
                    .setAllowedAuthenticators(BiometricManager.Authenticators.BIOMETRIC_STRONG)
                    .setNegativeButtonText("Cancel")
                    .build()
                prompt.authenticate(info)
            }
        }

        View(WaylodeApprovalSheetView::class) {
            Prop("request") { view: WaylodeApprovalSheetView, request: Map<String, Any?> ->
                view.configure(request)
            }
            Events("onOutcome")
        }
    }

    private fun ensureOrCreateDeviceKeys(): Map<String, Any> {
        val deviceId = ensureDeviceId()
        val strongBoxBacked = ensureApprovalP256KeyPair()
        val ks = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        val ecPub = ks.getCertificate(APPROVAL_P256_ALIAS).publicKey as ECPublicKey
        val sec1P256 = encodeUncompressedSec1P256(ecPub)

        val edSecret = WaylodeRustBridge.ensureWrappedIdentitySecret(reactContext, "ed25519_v1")
        val edPub = try {
            WaylodeRustBridge.nativeDeriveEd25519Public(edSecret)
        } finally {
            Arrays.fill(edSecret, 0.toByte())
        }

        val xSecret = WaylodeRustBridge.ensureWrappedIdentitySecret(reactContext, "x25519_v1")
        val xPub = try {
            WaylodeRustBridge.nativeDeriveX25519Public(xSecret)
        } finally {
            Arrays.fill(xSecret, 0.toByte())
        }

        return mapOf(
            "device_id" to deviceId,
            "ed25519_pub_b64" to Base64.encodeToString(edPub, Base64.NO_WRAP),
            "x25519_pub_b64" to Base64.encodeToString(xPub, Base64.NO_WRAP),
            "approval_pub_p256_b64" to Base64.encodeToString(sec1P256, Base64.NO_WRAP),
            "hardware_backed" to strongBoxBacked,
            "key_storage_backend" to if (strongBoxBacked) "android_strongbox" else "android_tee",
        )
    }

    private fun encodeUncompressedSec1P256(pub: ECPublicKey): ByteArray {
        val x = toFixed32Bytes(pub.w.affineX.toByteArray())
        val y = toFixed32Bytes(pub.w.affineY.toByteArray())
        val sec1 = ByteArray(65)
        sec1[0] = 0x04
        System.arraycopy(x, 0, sec1, 1, 32)
        System.arraycopy(y, 0, sec1, 33, 32)
        return sec1
    }

    private fun toFixed32Bytes(src: ByteArray): ByteArray {
        val out = ByteArray(32)
        if (src.size >= 32) {
            System.arraycopy(src, src.size - 32, out, 0, 32)
        } else {
            System.arraycopy(src, 0, out, 32 - src.size, src.size)
        }
        return out
    }

    private fun ensureApprovalP256KeyPair(): Boolean {
        val ks = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        val prefs = reactContext.getSharedPreferences(META_PREFS, Context.MODE_PRIVATE)
        if (ks.containsAlias(APPROVAL_P256_ALIAS)) {
            return prefs.getBoolean(KEY_STRONGBOX_BACKED, false)
        }

        val kpg = KeyPairGenerator.getInstance(KeyProperties.KEY_ALGORITHM_EC, ANDROID_KEYSTORE)
        val strongBoxResult = try {
            kpg.initialize(buildP256Spec(useStrongBox = true))
            kpg.generateKeyPair()
            true
        } catch (_: StrongBoxUnavailableException) {
            kpg.initialize(buildP256Spec(useStrongBox = false))
            kpg.generateKeyPair()
            false
        }
        prefs.edit().putBoolean(KEY_STRONGBOX_BACKED, strongBoxResult).apply()
        return strongBoxResult
    }

    private fun buildP256Spec(useStrongBox: Boolean): KeyGenParameterSpec {
        val builder = KeyGenParameterSpec.Builder(
            APPROVAL_P256_ALIAS,
            KeyProperties.PURPOSE_SIGN,
        )
            .setAlgorithmParameterSpec(ECGenParameterSpec("secp256r1"))
            .setDigests(KeyProperties.DIGEST_SHA256)
            .setUserAuthenticationRequired(true)
            .setInvalidatedByBiometricEnrollment(true)

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            builder.setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG)
        }
        if (useStrongBox && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            builder.setIsStrongBoxBacked(true)
        }
        return builder.build()
    }

    private fun prepareP256Signature(): Signature {
        ensureApprovalP256KeyPair()
        val ks = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        val privateKey = ks.getKey(APPROVAL_P256_ALIAS, null) as PrivateKey
        return Signature.getInstance("SHA256withECDSA").apply {
            initSign(privateKey)
        }
    }

    private fun ensureDeviceId(): String {
        val prefs = reactContext.getSharedPreferences(META_PREFS, Context.MODE_PRIVATE)
        prefs.getString(KEY_DEVICE_ID, null)?.takeIf { it.isNotEmpty() }?.let { return it }
        val generated = "android-${UUID.randomUUID()}"
        prefs.edit().putString(KEY_DEVICE_ID, generated).apply()
        return generated
    }
}
