package com.gitpulse.mobile.push

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.util.Base64
import androidx.core.app.NotificationCompat
import com.gitpulse.crypto.WaylodeRustBridge
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeoutOrNull
import org.json.JSONObject

/**
 * Waylode Android FCM Service (Opus M1, M2, M5, R8, board #18).
 *
 * Security & platform invariants:
 * 1. ALWAYS posts a visible notification for every approval wake so FCM never deprioritizes
 *    high-priority messages (Opus M5).
 * 2. Executes local preview decryption via Rust `libwaylode_protocol.so` (`open_preview`) within
 *    an 8s guard (inside Android's 10s FCM budget) and falls back to a generic banner on timeout/error.
 * 3. Enforces `preview_ct_b64` raw bytes <= 2048 B (`"GPPV"` v1); zero plaintext travels through FCM.
 * 4. Notification actions expose ONLY `Deny` and `Deny & Stop` — NEVER `Approve` (Opus M1).
 *    Tapping the notification opens the app to the native full-bytes approval confirmation sheet.
 * 5. NEVER calls `setFullScreenIntent` (Android 14 compliance, Opus M5).
 */
class WaylodeMessagingService : FirebaseMessagingService() {

    companion object {
        const val CHANNEL_ID = "gitpulse_approvals"
        const val CHANNEL_NAME = "Waylode Approval Requests"
        const val MAX_PREVIEW_CT_BYTES = 2048
        const val DECRYPT_BUDGET_MS = 8_000L

        const val ACTION_DENY = "com.gitpulse.mobile.action.DENY"
        const val ACTION_DENY_AND_INTERRUPT = "com.gitpulse.mobile.action.DENY_AND_INTERRUPT"
        const val EXTRA_TICKET_ID = "ticket_id"
        const val EXTRA_HOST_ID = "host_id"
        const val EXTRA_INPUT_SHA256 = "input_sha256"
    }

    override fun onMessageReceived(message: RemoteMessage) {
        ensureNotificationChannel()

        val data = message.data
        val ticketId = data["ticket_id"].orEmpty()
        val hostDevice = data["host_device"] ?: data["host_id"].orEmpty()
        val inputSha256 = data["input_sha256"].orEmpty()
        val previewCtB64 = data["preview_ct_b64"] ?: data["preview_ct"]

        val preview = runBlocking {
            withTimeoutOrNull(DECRYPT_BUDGET_MS) {
                tryDecryptPreview(previewCtB64, hostDevice)
            }
        }

        val resolvedTicketId = preview?.optString("ticket_id")?.takeIf { it.isNotEmpty() } ?: ticketId
        val title = preview?.optString("title")?.takeIf { it.isNotEmpty() }
            ?: "Waylode Approval Required"
        val body = preview?.optString("body")?.takeIf { it.isNotEmpty() }
            ?: "Open Waylode to inspect the full command and verify its hash before deciding."

        postVisibleApprovalNotification(
            ticketId = resolvedTicketId,
            hostId = hostDevice,
            inputSha256 = inputSha256,
            title = title,
            body = body,
        )
    }

    private fun tryDecryptPreview(previewCtB64: String?, hostDevice: String): JSONObject? {
        if (previewCtB64.isNullOrBlank() || hostDevice.isBlank()) return null
        val decoded = try {
            Base64.decode(previewCtB64, Base64.DEFAULT)
        } catch (_: IllegalArgumentException) {
            return null
        }
        if (decoded.isEmpty() || decoded.size > MAX_PREVIEW_CT_BYTES) {
            return null
        }
        val hostPubPrefs = applicationContext.getSharedPreferences(
            "gitpulse_pinned_hosts",
            Context.MODE_PRIVATE,
        )
        val senderPubB64 = hostPubPrefs.getString("ed25519_pub_$hostDevice", null) ?: return null
        val senderPub = try {
            Base64.decode(senderPubB64, Base64.NO_WRAP)
        } catch (_: IllegalArgumentException) {
            return null
        }

        return try {
            val plaintextJson = WaylodeRustBridge.openPreviewWithStoredIdentityKey(
                applicationContext,
                decoded,
                senderPub,
                hostDevice,
            )
            JSONObject(plaintextJson)
        } catch (_: Throwable) {
            null
        }
    }

    private fun postVisibleApprovalNotification(
        ticketId: String,
        hostId: String,
        inputSha256: String,
        title: String,
        body: String,
    ) {
        val notificationId = if (ticketId.isNotEmpty()) ticketId.hashCode() else System.currentTimeMillis().toInt()
        val deepLinkUri = Uri.parse(
            if (ticketId.isNotEmpty()) "gitpulse://approvals/$ticketId" else "gitpulse://",
        )
        val openAppIntent = Intent(Intent.ACTION_VIEW, deepLinkUri).apply {
            setPackage(packageName)
            flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP
        }
        val contentPendingIntent = PendingIntent.getActivity(
            this,
            notificationId,
            openAppIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )

        val denyPendingIntent = buildActionPendingIntent(
            action = ACTION_DENY,
            requestCode = notificationId xor 0x11,
            ticketId = ticketId,
            hostId = hostId,
            inputSha256 = inputSha256,
        )
        val denyStopPendingIntent = buildActionPendingIntent(
            action = ACTION_DENY_AND_INTERRUPT,
            requestCode = notificationId xor 0x22,
            ticketId = ticketId,
            hostId = hostId,
            inputSha256 = inputSha256,
        )

        // Opus M1: Expose ONLY Deny and Deny & Stop on notifications — NEVER Approve.
        // Opus M5: Always post a visible notification; never use full-screen intents.
        val notification = NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.ic_lock_lock)
            .setContentTitle(title)
            .setContentText(body)
            .setStyle(NotificationCompat.BigTextStyle().bigText(body))
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setCategory(NotificationCompat.CATEGORY_MESSAGE)
            .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
            .setAutoCancel(true)
            .setContentIntent(contentPendingIntent)
            .addAction(0, "Deny", denyPendingIntent)
            .addAction(0, "Deny & Stop", denyStopPendingIntent)
            .build()

        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        manager.notify(notificationId, notification)
    }

    private fun buildActionPendingIntent(
        action: String,
        requestCode: Int,
        ticketId: String,
        hostId: String,
        inputSha256: String,
    ): PendingIntent {
        val intent = Intent(this, WaylodeNotificationActionReceiver::class.java).apply {
            this.action = action
            putExtra(EXTRA_TICKET_ID, ticketId)
            putExtra(EXTRA_HOST_ID, hostId)
            putExtra(EXTRA_INPUT_SHA256, inputSha256)
        }
        return PendingIntent.getBroadcast(
            this,
            requestCode,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }

    private fun ensureNotificationChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            val channel = NotificationChannel(
                CHANNEL_ID,
                CHANNEL_NAME,
                NotificationManager.IMPORTANCE_HIGH,
            ).apply {
                description = "Encrypted wake notifications for agent approval requests (Deny-only quick actions)."
            }
            manager.createNotificationChannel(channel)
        }
    }
}
