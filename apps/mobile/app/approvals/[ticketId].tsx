import { useState } from "react";
import { Pressable, ScrollView, StyleSheet, Text, View } from "react-native";
import { useLocalSearchParams, useRouter } from "expo-router";
import type { NativeApprovalSheetOutcome } from "@waylode/crypto";
import { useControlPlane } from "@/state/ControlPlaneContext";
import { colors, radii, spacing } from "@/theme/tokens";

/**
 * Approval Queue Detail & Native Hardware-Signed Sheet Trigger (Opus M1, M2, M3, P0-4).
 *
 * Security invariants:
 * - Displays tool summary, repo/worktree context, and `input_sha256` prefix.
 * - Launches the native/Rust `WaylodeApprovalSheet` which renders the FULL canonical bytes
 *   directly in native UIKit/Android View, recomputes `input_sha256` in Rust, prompts biometrics,
 *   and signs strictly `allow_once | deny | deny_and_interrupt` with Secure Enclave / StrongBox P-256.
 * - NEVER allows `updatedInput` or `updatedPermissions`.
 */
export default function ApprovalTicketScreen() {
  const { ticketId } = useLocalSearchParams<{ ticketId: string }>();
  const router = useRouter();
  const { state, launchNativeApprovalSheet } = useControlPlane();
  const [outcome, setOutcome] = useState<NativeApprovalSheetOutcome | null>(null);
  const [busy, setBusy] = useState(false);

  const ticket = state.openTickets.find((t) => t.ticket_id === ticketId);

  if (!ticket) {
    return (
      <View style={styles.emptyWrap}>
        <Text style={styles.emptyTitle}>Ticket No Longer Pending</Text>
        <Text style={styles.emptyBody}>
          This approval request was already resolved locally on desktop, cancelled (user returned
          or pressed Esc/Ctrl-C), or expired.
        </Text>
        <Pressable style={styles.backBtn} onPress={() => router.back()}>
          <Text style={styles.btnText}>Return to Inbox</Text>
        </Pressable>
      </View>
    );
  }

  const handleOpenNativeSheet = async () => {
    setBusy(true);
    try {
      const result = await launchNativeApprovalSheet(ticket);
      setOutcome(result);
    } finally {
      setBusy(false);
    }
  };

  const expiresInSec = Math.max(0, ticket.expires_at - Math.floor(Date.now() / 1000));

  return (
    <ScrollView style={styles.screen} contentContainerStyle={styles.content}>
      <View style={styles.card}>
        <Text style={styles.kicker}>
          {ticket.harness.toUpperCase()} • {ticket.channel}
        </Text>
        <Text style={styles.toolName}>{ticket.tool_name}</Text>

        {ticket.summary && <Text style={styles.summary}>{ticket.summary}</Text>}

        <View style={styles.metaBlock}>
          <Text style={styles.metaRow}>Repo: {ticket.repo_label ?? ticket.cwd}</Text>
          <Text style={styles.metaRow}>CWD: {ticket.cwd}</Text>
          <Text style={styles.metaRow}>Dedup Key: {ticket.dedup_key}</Text>
          <Text style={styles.metaRow}>
            input_sha256: {ticket.input_sha256.slice(0, 20)}…
          </Text>
          <Text style={styles.metaRow}>Expires in: {expiresInSec}s</Text>
        </View>

        <View style={styles.securityNotice}>
          <Text style={styles.securityTitle}>Hardware-Signed Full-Bytes Verification</Text>
          <Text style={styles.securityText}>
            Tapping below opens the native OS confirmation sheet. Rust recomputes{" "}
            `input_sha256` from the exact canonical bytes rendered on screen before your Secure
            Enclave / StrongBox P-256 key signs `allow_once`, `deny`, or `deny_and_interrupt`.
            Parameter overrides (`updatedInput` / `updatedPermissions`) are disabled by protocol.
          </Text>
        </View>

        <Pressable
          style={[styles.launchBtn, busy && styles.disabled]}
          disabled={busy}
          onPress={handleOpenNativeSheet}
        >
          <Text style={styles.btnText}>
            {busy
              ? "Waiting for Biometric Signature..."
              : "Inspect Full Payload & Decide (Native Sheet)"}
          </Text>
        </Pressable>

        {outcome && (
          <View style={styles.outcomeBox}>
            <Text style={styles.outcomeTitle}>Outcome: {outcome.status.toUpperCase()}</Text>
            {outcome.signed_decision && (
              <Text style={styles.outcomeText}>
                Signed `{outcome.signed_decision.decision}` with hardware P-256 key and
                dispatched to host `{outcome.signed_decision.host_id}`.
              </Text>
            )}
            {outcome.status === "hash_mismatch" && (
              <Text style={styles.errorText}>
                Canonical payload verification failed (hash mismatch or non-canonical numeric
                value). Signing refused — approve at desktop only.
              </Text>
            )}
            {outcome.error_message && (
              <Text style={styles.errorText}>{outcome.error_message}</Text>
            )}
          </View>
        )}
      </View>
    </ScrollView>
  );
}

const styles = StyleSheet.create({
  screen: { flex: 1, backgroundColor: colors.bg },
  content: { padding: spacing.lg },
  emptyWrap: {
    flex: 1,
    backgroundColor: colors.bg,
    justifyContent: "center",
    padding: spacing.xl,
  },
  emptyTitle: { color: colors.text, fontSize: 18, fontWeight: "800", marginBottom: spacing.sm },
  emptyBody: { color: colors.textMuted, fontSize: 14, lineHeight: 20, marginBottom: spacing.lg },
  backBtn: {
    backgroundColor: colors.surfaceElevated,
    paddingVertical: spacing.md,
    borderRadius: radii.md,
    alignItems: "center",
  },
  card: {
    backgroundColor: colors.surface,
    borderWidth: 1,
    borderColor: colors.border,
    borderRadius: radii.lg,
    padding: spacing.lg,
  },
  kicker: { color: colors.accent, fontSize: 11, fontWeight: "800", letterSpacing: 0.4 },
  toolName: { color: colors.text, fontSize: 22, fontWeight: "800", marginTop: 4 },
  summary: { color: colors.text, fontSize: 14, marginTop: spacing.sm, lineHeight: 20 },
  metaBlock: {
    marginTop: spacing.md,
    backgroundColor: colors.bg,
    borderRadius: radii.sm,
    padding: spacing.md,
    gap: 4,
  },
  metaRow: { color: colors.textMuted, fontFamily: "Courier", fontSize: 12 },
  securityNotice: {
    marginTop: spacing.md,
    backgroundColor: colors.infoBg,
    borderWidth: 1,
    borderColor: colors.info,
    borderRadius: radii.md,
    padding: spacing.md,
  },
  securityTitle: { color: colors.info, fontWeight: "800", fontSize: 12, marginBottom: 4 },
  securityText: { color: colors.text, fontSize: 12, lineHeight: 18 },
  launchBtn: {
    marginTop: spacing.lg,
    backgroundColor: colors.success,
    paddingVertical: spacing.md + 2,
    borderRadius: radii.md,
    alignItems: "center",
  },
  disabled: { opacity: 0.5 },
  btnText: { color: colors.text, fontWeight: "800", fontSize: 15 },
  outcomeBox: {
    marginTop: spacing.md,
    backgroundColor: colors.surfaceElevated,
    borderRadius: radii.md,
    padding: spacing.md,
  },
  outcomeTitle: { color: colors.text, fontWeight: "800", fontSize: 13 },
  outcomeText: { color: colors.textMuted, fontSize: 12, marginTop: 4 },
  errorText: { color: colors.danger, fontSize: 12, marginTop: 4 },
});
