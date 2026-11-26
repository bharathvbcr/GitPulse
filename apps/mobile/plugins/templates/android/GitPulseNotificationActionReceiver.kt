package com.gitpulse.mobile.push

import android.app.NotificationManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/**
 * Handles lock-screen notification quick actions on Android.
 *
 * Opus M1: ONLY accepts `DENY` and `DENY_AND_INTERRUPT`. Any unknown or approve-shaped
 * intent action is dropped without executing.
 */
class GitPulseNotificationActionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val decision = when (intent.action) {
            GitPulseMessagingService.ACTION_DENY -> "deny"
            GitPulseMessagingService.ACTION_DENY_AND_INTERRUPT -> "deny_and_interrupt"
            else -> return // Fail closed: never execute approve or unknown actions from notifications.
        }

        val ticketId = intent.getStringExtra(GitPulseMessagingService.EXTRA_TICKET_ID) ?: return
        val hostId = intent.getStringExtra(GitPulseMessagingService.EXTRA_HOST_ID).orEmpty()
        val inputSha256 = intent.getStringExtra(GitPulseMessagingService.EXTRA_INPUT_SHA256).orEmpty()

        // Queue pending deny intent for the control-plane relay sender / background worker
        val prefs = context.getSharedPreferences("gitpulse_pending_denials", Context.MODE_PRIVATE)
        prefs.edit()
            .putString(ticketId, "$decision|$hostId|$inputSha256|${System.currentTimeMillis()}")
            .apply()

        val manager = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        manager.cancel(ticketId.hashCode())
    }
}
