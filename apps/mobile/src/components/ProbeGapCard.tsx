import { StyleSheet, Text, View } from "react-native";
import type { ProbeGap } from "../protocol/types";
import { colors, radii, spacing } from "../theme/tokens";

export function ProbeGapCard({ gap }: { gap: ProbeGap }) {
  const isBlocking = gap.kind === "failed" || gap.kind === "skipped";
  return (
    <View
      style={[
        styles.card,
        {
          borderColor: isBlocking ? colors.danger : colors.warning,
          backgroundColor: isBlocking ? colors.dangerBg : colors.warningBg,
        },
      ]}
    >
      <View style={styles.header}>
        <Text style={[styles.badge, { color: isBlocking ? colors.danger : colors.warning }]}>
          PROBE GAP • {gap.kind.toUpperCase()}
        </Text>
        <Text style={styles.repo}>{gap.label}</Text>
      </View>
      <Text style={styles.reason}>{gap.reason}</Text>
      <Text style={styles.footnote}>
        An unread or partial probe is not a quiet workspace.
      </Text>
    </View>
  );
}

const styles = StyleSheet.create({
  card: {
    borderWidth: 1,
    borderRadius: radii.md,
    padding: spacing.md,
    marginBottom: spacing.sm,
  },
  header: {
    flexDirection: "row",
    justifyContent: "space-between",
    alignItems: "center",
    marginBottom: spacing.xs,
  },
  badge: {
    fontSize: 11,
    fontWeight: "800",
    letterSpacing: 0.4,
  },
  repo: {
    fontSize: 12,
    fontWeight: "600",
    color: colors.text,
  },
  reason: {
    fontSize: 13,
    color: colors.text,
    lineHeight: 18,
  },
  footnote: {
    marginTop: spacing.xs,
    fontSize: 11,
    color: colors.textMuted,
  },
});
