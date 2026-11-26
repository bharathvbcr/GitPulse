package com.gitpulse.crypto

import android.app.AlertDialog
import android.content.Context
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
import expo.modules.kotlin.AppContext
import expo.modules.kotlin.viewevent.EventDispatcher
import expo.modules.kotlin.views.ExpoView
import java.security.Signature
import java.util.UUID

/**
 * Native Android Approval Confirmation Sheet (Opus M1 & M3).
 * Calls Rust `render_canonical_tool_input` + `hash_canonical` (board #18) so the exact
 * canonical JSON rendered natively matches the signed `input_sha256`.
 */
object WaylodeApprovalSheetPresenter {
    fun present(
        activity: FragmentActivity,
        request: Map<String, Any?>,
        deviceId: String,
        prepareSignature: () -> Signature,
        onOutcome: (Map<String, Any?>) -> Unit,
    ) {
        val hostId = request["host_id"] as? String ?: ""
        val ticketId = request["ticket_id"] as? String ?: ""
        val toolName = request["tool_name"] as? String ?: ""
        val toolInputJson = request["tool_input_json"] as? String ?: ""
        val cwd = request["cwd"] as? String ?: ""
        val expectedSha = request["expected_input_sha256"] as? String ?: ""
        val repoLabel = request["repo_label"] as? String ?: ""
        val worktreeLabel = request["worktree_label"] as? String
        val harnessLabel = request["harness_label"] as? String ?: ""
        val expiresAtSecs = (request["expires_at"] as? Number)?.toLong() ?: 0L

        if (ticketId.isEmpty() || toolName.isEmpty() || expectedSha.isEmpty()) {
            onOutcome(mapOf("status" to "cancelled", "error_message" to "Malformed approval request"))
            return
        }

        val canonicalRendered: String
        val computedSha: String
        try {
            canonicalRendered = WaylodeRustBridge.nativeRenderCanonicalToolInput(
                toolName,
                toolInputJson,
                cwd,
            )
            computedSha = WaylodeRustBridge.nativeHashCanonical(toolName, toolInputJson, cwd)
        } catch (e: Throwable) {
            onOutcome(
                mapOf(
                    "status" to "hash_mismatch",
                    "error_message" to "Rust canonical render/hash failed: ${e.message}",
                ),
            )
            return
        }

        if (!computedSha.equals(expectedSha, ignoreCase = true)) {
            onOutcome(
                mapOf(
                    "status" to "hash_mismatch",
                    "computed_input_sha256" to computedSha,
                    "error_message" to "Canonical payload hash mismatch; refusing to sign.",
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

        val wtSuffix = if (!worktreeLabel.isNullOrEmpty()) " ($worktreeLabel)" else ""
        val metaView = TextView(activity).apply {
            text = "Tool: $toolName\nRepo: $repoLabel$wtSuffix\nHarness: $harnessLabel\nCWD: $cwd\nVerified SHA-256: $computedSha"
            typeface = Typeface.MONOSPACE
            textSize = 12f
            setTextColor(Color.parseColor("#CBD5E1"))
        }

        val payloadScroll = ScrollView(activity).apply {
            setBackgroundColor(Color.parseColor("#020617"))
            setPadding(24, 24, 24, 24)
            addView(
                TextView(activity).apply {
                    text = canonicalRendered
                    typeface = Typeface.MONOSPACE
                    textSize = 12f
                    setTextColor(Color.parseColor("#E2E8F0"))
                },
            )
        }

        container.addView(metaView)
        container.addView(
            payloadScroll,
            LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT,
                520,
            ).apply { topMargin = 20; bottomMargin = 20 },
        )

        val dialog = AlertDialog.Builder(activity)
            .setTitle("Hardware-Signed Approval")
            .setView(container)
            .setOnCancelListener { finish(mapOf("status" to "cancelled")) }
            .create()

        fun triggerBiometricSign(decision: String) {
            if (decision != "allow_once" && decision != "deny" && decision != "deny_and_interrupt") {
                finish(mapOf("status" to "cancelled", "error_message" to "Forbidden decision"))
                dialog.dismiss()
                return
            }

            val issuedAt = System.currentTimeMillis() / 1000L
            if (issuedAt >= expiresAtSecs) {
                finish(mapOf("status" to "expired", "error_message" to "Approval ticket expired"))
                dialog.dismiss()
                return
            }

            val nonce = UUID.randomUUID().toString()
            val signingBytes = WaylodeRustBridge.nativeBuildApprovalSigningBytes(
                hostId = hostId,
                ticketId = ticketId,
                inputSha256 = computedSha,
                decision = decision,
                nonce = nonce,
                issuedAt = issuedAt,
                expiresAt = expiresAtSecs,
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
                                    "computed_input_sha256" to computedSha,
                                    "signed_decision" to mapOf(
                                        "host_id" to hostId,
                                        "ticket_id" to ticketId,
                                        "input_sha256" to computedSha,
                                        "decision" to decision,
                                        "nonce" to nonce,
                                        "issued_at" to issuedAt,
                                        "expires_at" to expiresAtSecs,
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
                .setTitle("Authorize $toolName")
                .setSubtitle("Sign $decision with StrongBox P-256 key")
                .setAllowedAuthenticators(BiometricManager.Authenticators.BIOMETRIC_STRONG)
                .setNegativeButtonText("Cancel")
                .build()

            prompt.authenticate(promptInfo, BiometricPrompt.CryptoObject(signature))
        }

        fun addActionButton(title: String, colorHex: String, onClick: () -> Unit) {
            val btn = Button(activity).apply {
                text = title
                setBackgroundColor(Color.parseColor(colorHex))
                setTextColor(Color.WHITE)
                setOnClickListener { onClick() }
            }
            container.addView(
                btn,
                LinearLayout.LayoutParams(
                    LinearLayout.LayoutParams.MATCH_PARENT,
                    LinearLayout.LayoutParams.WRAP_CONTENT,
                ).apply { topMargin = 12 },
            )
        }

        addActionButton("Allow Once (Biometric / StrongBox)", "#16A34A") {
            triggerBiometricSign("allow_once")
        }
        addActionButton("Deny", "#DC2626") {
            triggerBiometricSign("deny")
        }
        addActionButton("Deny & Interrupt Session", "#991B1B") {
            triggerBiometricSign("deny_and_interrupt")
        }
        addActionButton("Cancel", "#334155") {
            dialog.dismiss()
            finish(mapOf("status" to "cancelled"))
        }

        dialog.show()
    }
}

class WaylodeApprovalSheetView(context: Context, appContext: AppContext) : ExpoView(context, appContext) {
    val onOutcome by EventDispatcher()
    private val infoLabel = TextView(context).apply {
        typeface = Typeface.MONOSPACE
        textSize = 12f
        setTextColor(Color.WHITE)
        setPadding(24, 24, 24, 24)
    }

    init {
        addView(infoLabel)
    }

    fun configure(request: Map<String, Any?>) {
        val tool = request["tool_name"] as? String ?: "Unknown"
        val sha = request["expected_input_sha256"] as? String ?: ""
        infoLabel.text = "Native Approval Surface: $tool\nSHA-256: $sha"
    }
}
