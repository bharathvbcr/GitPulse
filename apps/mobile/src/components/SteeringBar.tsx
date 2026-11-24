import { useState } from "react";
import { Pressable, StyleSheet, Text, TextInput, View } from "react-native";
import type {
  SessionSnapshot,
  SteeringConfirmResp,
  SteeringPreviewResp,
} from "../protocol/types";
import { useControlPlane } from "../state/ControlPlaneContext";
import { colors, radii, spacing } from "../theme/tokens";

function makeRequestId(): string {
  return `steer-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
}

/**
 * Two-step `preview` -> `confirm` steering bar (Opus Section 4).
 * Carries `expect_route` and idempotent `request_id`, and displays honest
 * `Queued (next turn)` + 2h TTL alerts for idle Antigravity sessions.
 */
export function SteeringBar({ session }: { session: SessionSnapshot }) {
  const { previewSteering, confirmSteering } = useControlPlane();
  const [draft, setDraft] = useState("");
  const [preview, setPreview] = useState<SteeringPreviewResp | null>(null);
  const [receipt, setReceipt] = useState<SteeringConfirmResp | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const handlePreview = async () => {
    const trimmed = draft.trim();
    if (!trimmed) return;
    setBusy(true);
    setError(null);
    setReceipt(null);
    try {
      const resp = await previewSteering({
        request_id: makeRequestId(),
        session_key: session.session_key,
        text: trimmed,
      });
      setPreview(resp);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleConfirm = async () => {
    if (!preview) return;
    setBusy(true);
    setError(null);
    try {
      const resp = await confirmSteering({
        request_id: preview.request_id,
        session_key: preview.session_key,
        expect_route: preview.expect_route,
        text: draft.trim(),
        confirm_token: preview.confirm_token,
      });
      setReceipt(resp);
      if (resp.status === "delivered" || resp.status === "queued") {
        setDraft("");
        setPreview(null);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <View style={styles.container}>
      <Text style={styles.label}>2-Step Confirmed Steering</Text>
      <TextInput
        style={styles.input}
        value={draft}
        onChangeText={(next: string) => {
          setDraft(next);
          if (preview) setPreview(null);
        }}
        placeholder={`Steer ${session.harness} session...`}
        placeholderTextColor={colors.textDim}
        multiline
      />

      {!preview ? (
        <Pressable
          style={[styles.primaryBtn, (!draft.trim() || busy) && styles.disabled]}
          onPress={handlePreview}
          disabled={!draft.trim() || busy}
        >
          <Text style={styles.btnText}>
            {busy ? "Checking route..." : "Step 1: Preview Delivery Route"}
          </Text>
        </Pressable>
      ) : (
        <View style={styles.previewCard}>
          <Text style={styles.previewTitle}>
            Route: {preview.expect_route} ({preview.delivery_mode})
          </Text>
          {preview.delivery_mode === "queued_next_turn" && (
            <Text style={styles.warning}>
              Queued (next turn) — no native idle wake. TTL:{" "}
              {Math.round((preview.queued_ttl_sec ?? 7200) / 60)} min. You will be alerted if
              it expires.
            </Text>
          )}
          {preview.warning && <Text style={styles.warning}>{preview.warning}</Text>}
          <Text style={styles.reqId}>Idempotent ID: {preview.request_id}</Text>

          <View style={styles.confirmRow}>
            <Pressable style={styles.cancelBtn} onPress={() => setPreview(null)} disabled={busy}>
              <Text style={styles.btnText}>Edit</Text>
            </Pressable>
            <Pressable style={styles.confirmBtn} onPress={handleConfirm} disabled={busy}>
              <Text style={styles.btnText}>
                {busy ? "Sending..." : "Step 2: Confirm & Send"}
              </Text>
            </Pressable>
          </View>
        </View>
      )}

      {receipt && (
        <View style={styles.receipt}>
          <Text style={styles.receiptText}>
            [{receipt.status.toUpperCase()}] {receipt.reason ?? `Route: ${receipt.route_used ?? "n/a"}`}
          </Text>
        </View>
      )}
      {error && <Text style={styles.error}>{error}</Text>}
    </View>
  );
}

const styles = StyleSheet.create({
  container: {
    backgroundColor: colors.surface,
    borderWidth: 1,
    borderColor: colors.border,
    borderRadius: radii.md,
    padding: spacing.md,
    marginTop: spacing.md,
  },
  label: {
    fontSize: 12,
    fontWeight: "800",
    color: colors.textMuted,
    textTransform: "uppercase",
    marginBottom: spacing.sm,
  },
  input: {
    backgroundColor: colors.bg,
    borderWidth: 1,
    borderColor: colors.border,
    borderRadius: radii.sm,
    color: colors.text,
    padding: spacing.sm,
    minHeight: 68,
    fontSize: 14,
  },
  primaryBtn: {
    marginTop: spacing.sm,
    backgroundColor: colors.accent,
    paddingVertical: spacing.md,
    borderRadius: radii.sm,
    alignItems: "center",
  },
  disabled: {
    opacity: 0.45,
  },
  btnText: {
    color: colors.text,
    fontWeight: "700",
    fontSize: 14,
  },
  previewCard: {
    marginTop: spacing.sm,
    backgroundColor: colors.surfaceElevated,
    borderRadius: radii.sm,
    padding: spacing.sm,
    borderWidth: 1,
    borderColor: colors.accent,
  },
  previewTitle: {
    fontSize: 13,
    fontWeight: "700",
    color: colors.text,
  },
  warning: {
    marginTop: spacing.xs,
    fontSize: 12,
    color: colors.warning,
  },
  reqId: {
    marginTop: spacing.xs,
    fontFamily: "Courier",
    fontSize: 11,
    color: colors.textDim,
  },
  confirmRow: {
    flexDirection: "row",
    gap: spacing.sm,
    marginTop: spacing.sm,
  },
  cancelBtn: {
    flex: 1,
    backgroundColor: colors.border,
    paddingVertical: spacing.sm,
    borderRadius: radii.sm,
    alignItems: "center",
  },
  confirmBtn: {
    flex: 2,
    backgroundColor: colors.success,
    paddingVertical: spacing.sm,
    borderRadius: radii.sm,
    alignItems: "center",
  },
  receipt: {
    marginTop: spacing.sm,
    padding: spacing.sm,
    borderRadius: radii.sm,
    backgroundColor: colors.infoBg,
  },
  receiptText: {
    color: colors.info,
    fontSize: 12,
    fontWeight: "600",
  },
  error: {
    marginTop: spacing.xs,
    color: colors.danger,
    fontSize: 12,
  },
});
