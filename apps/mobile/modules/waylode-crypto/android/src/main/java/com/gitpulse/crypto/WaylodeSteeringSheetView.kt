package com.gitpulse.crypto

import android.app.AlertDialog
import android.graphics.Color
import android.graphics.Typeface
import android.util.Base64
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity
import java.security.Signature
import java.util.UUID

/**
 * Native Android Steering Confirmation Sheet (Opus #23 P1 steering authenticity).
 *
 * Security model:
 * - Renders the exact UTF-8 steering text natively in Android View (outside JS/OTA).
 * - Computes `text_sha256` directly over the rendered UTF-8 bytes.
 * - Builds `gitpulse-steering/v1\n` + canonical JSON signing bytes via Rust
 *   (`WaylodeRustBridge.nativeBuildSteeringSigningBytes`).
 * - Prompts `BiometricPrompt` with StrongBox P-256 `CryptoObject(Signature)` before returning
 *   `SignedSteeringConfirm`.
 */
object WaylodeSteeringSheetPresenter {
    fun present(
        activity: FragmentActivity,
        request: Map<String, Any?>,
        deviceId: String,
        prepareSignature: () -> Signature,
        onOutcome: (Map<String, Any?>) -> Unit,
    ) {
        val hostId = request["host_id"] as? String ?: ""
        val sessionKey = request["session_key"] as? String ?: ""
        val requestId = request["request_id"] as? String ?: ""
        val expectRoute = request["expect_route"] as? String ?: ""
        val deliveryMode = request["delivery_mode"] as? String ?: ""
        val exactText = request["text"] as? String ?: ""
        val repoLabel = request["repo_label"] as? String ?: ""
        val harnessLabel = request["harness_label"] as? String ?: ""
        val warning = request["warning"] as? String
        val ttlSec = ((request["ttl_sec"] as? Number)?.toLong() ?: 300L).coerceIn(30L, 900L)

        if (hostId.isEmpty() || sessionKey.isEmpty() || requestId.isEmpty() || exactText.isEmpty()) {
            onOutcome(
                mapOf(
                    "status" to "error",
                    "error_message" to "Malformed steering confirmation sheet request",
                ),
            )
            return
        }

        val textSha256 = try {
            WaylodeRustBridge.nativeHashSteeringText(exactText)
        } catch (e: Throwable) {
            onOutcome(
                mapOf(
                    "status" to "error",
                    "error_message" to "Rust hash_steering_text failed: ${e.message}",
                ),
            )
            return
        }

        var settled = false
        fun finish(outcome: Map<String, Any?>) {
            if (!settled) {
                settled = true
                onOutcome(outcome)
            }
        }

        val container = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(40, 32, 40, 32)
            setBackgroundColor(Color.parseColor("#0F172A"))
        }

        val metaBuilder = StringBuilder()
            .append("Repo: ").append(repoLabel).append('\n')
            .append("Harness: ").append(harnessLabel).append('\n')
            .append("Session: ").append(sessionKey).append('\n')
            .append("Route: ").append(expectRoute).append(" (").append(deliveryMode).append(")\n")
            .append("Request ID: ").append(requestId).append('\n')
            .append("Text SHA-256: ").append(textSha256)
        if (!warning.isNullOrEmpty()) {
            metaBuilder.append("\nWarning: ").append(warning)
        }

        val metaView = TextView(activity).apply {
            text = metaBuilder.toString()
            typeface = Typeface.MONOSPACE
            textSize = 12f
            setTextColor(Color.parseColor("#CBD5E1"))
        }

        // Exact UTF-8 steering text rendered natively (Opus #23 P1).
        val textScroll = ScrollView(activity).apply {
            setBackgroundColor(Color.parseColor("#020617"))
            setPadding(24, 24, 24, 24)
            addView(
                TextView(activity).apply {
                    text = exactText
                    typeface = Typeface.MONOSPACE
                    textSize = 13f
                    setTextColor(Color.parseColor("#E2E8F0"))
                },
            )
        }

        container.addView(metaView)
        container.addView(
            textScroll,
            LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT,
                520,
            ).apply { topMargin = 20; bottomMargin = 20 },
        )

        val dialog = AlertDialog.Builder(activity)
            .setTitle("Hardware-Signed Steering Confirm")
            .setView(container)
            .setOnCancelListener { finish(mapOf("status" to "cancelled")) }
            .create()

        fun triggerBiometricSign() {
            val issuedAt = System.currentTimeMillis() / 1000L
            val expiresAt = issuedAt + ttlSec
            val nonce = UUID.randomUUID().toString()

            val signingBytes = WaylodeRustBridge.nativeBuildSteeringSigningBytes(
                hostId = hostId,
                sessionKey = sessionKey,
                requestId = requestId,
                expectRoute = expectRoute,
                textSha256 = textSha256,
                nonce = nonce,
                issuedAt = issuedAt,
                expiresAt = expiresAt,
            )

            val signature = prepareSignature()
            val executor = ContextCompat.getMainExecutor(activity)
            val prompt = BiometricPrompt(
                activity,
                executor,
                object : BiometricPrompt.AuthenticationCallback() {
                    override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                        try {
                            val unlocked = requireNotNull(result.cryptoObject?.signature)
                            unlocked.update(signingBytes)
                            val sigDer = unlocked.sign()
                            dialog.dismiss()
                            finish(
                                mapOf(
                                    "status" to "signed",
                                    "computed_text_sha256" to textSha256,
                                    "signed_confirm" to mapOf(
                                        "host_id" to hostId,
                                        "session_key" to sessionKey,
                                        "request_id" to requestId,
                                        "expect_route" to expectRoute,
                                        "text_sha256" to textSha256,
                                        "issued_at" to issuedAt,
                                        "expires_at" to expiresAt,
                                        "nonce" to nonce,
                                        "device_id" to deviceId,
                                        "sig_p256_der_b64" to Base64.encodeToString(sigDer, Base64.NO_WRAP),
                                    ),
                                ),
                            )
                        } catch (e: Throwable) {
                            dialog.dismiss()
                            finish(mapOf("status" to "biometric_failed", "error_message" to e.message))
                        }
                    }

                    override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                        dialog.dismiss()
                        finish(mapOf("status" to "biometric_failed", "error_message" to errString.toString()))
                    }
                },
            )

            val promptInfo = BiometricPrompt.PromptInfo.Builder()
                .setTitle("Confirm Steering Message")
                .setSubtitle("Sign HumanConfirmed steering with StrongBox P-256 key")
                .setAllowedAuthenticators(BiometricManager.Authenticators.BIOMETRIC_STRONG)
                .setNegativeButtonText("Cancel")
                .build()

            prompt.authenticate(promptInfo, BiometricPrompt.CryptoObject(signature))
        }

        val signBtn = Button(activity).apply {
            text = "Sign & Send Steering (Biometric / StrongBox)"
            setBackgroundColor(Color.parseColor("#16A34A"))
            setTextColor(Color.WHITE)
            setOnClickListener { triggerBiometricSign() }
        }
        val cancelBtn = Button(activity).apply {
            text = "Cancel"
            setBackgroundColor(Color.parseColor("#334155"))
            setTextColor(Color.WHITE)
            setOnClickListener {
                dialog.dismiss()
                finish(mapOf("status" to "cancelled"))
            }
        }

        container.addView(
            signBtn,
            LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT,
                LinearLayout.LayoutParams.WRAP_CONTENT,
            ).apply { topMargin = 12 },
        )
        container.addView(
            cancelBtn,
            LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT,
                LinearLayout.LayoutParams.WRAP_CONTENT,
            ).apply { topMargin = 12 },
        )

        dialog.show()
    }
}
