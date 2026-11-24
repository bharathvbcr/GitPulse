import { StyleSheet, Text, View } from "react-native";
import type { BlastRadiusSummary } from "../protocol/types";
import { colors, radii, spacing } from "../theme/tokens";

export function BlastRadiusCard({ summary }: { summary: BlastRadiusSummary }) {
  return (
    <View style={styles.card}>
      <Text style={styles.title}>DevMap Blast Radius</Text>
      <Text style={styles.symbol}>{summary.target_symbol}</Text>

      <View style={styles.statsRow}>
        <Text style={styles.stat}>
          Callers: <Text style={styles.statVal}>{summary.direct_callers}</Text>
        </Text>
        <Text style={styles.stat}>
          Files: <Text style={styles.statVal}>{summary.impacted_files.length}</Text>
        </Text>
        <Text style={styles.stat}>
          Tests: <Text style={styles.statVal}>{summary.affected_tests.length}</Text>
        </Text>
      </View>

      {(summary.unattributed_count > 0 || summary.walk_incomplete || summary.truncated) && (
        <View style={styles.warningBox}>
          <Text style={styles.warningText}>
            Lower-bound impact:{" "}
            {[
              summary.unattributed_count > 0
                ? `${summary.unattributed_count} unattributed lines`
                : null,
              summary.walk_incomplete ? "walk_incomplete" : null,
              summary.truncated ? "truncated" : null,
            ]
              .filter(Boolean)
              .join(" • ")}
          </Text>
        </View>
      )}

      {summary.impacted_files.slice(0, 6).map((file) => (
        <Text key={file} style={styles.fileRow} numberOfLines={1}>
          • {file}
        </Text>
      ))}
    </View>
  );
}

const styles = StyleSheet.create({
  card: {
    backgroundColor: colors.surface,
    borderWidth: 1,
    borderColor: colors.border,
    borderRadius: radii.md,
    padding: spacing.md,
    marginBottom: spacing.md,
  },
  title: {
    fontSize: 12,
    fontWeight: "800",
    color: colors.accent,
    textTransform: "uppercase",
    letterSpacing: 0.4,
  },
  symbol: {
    marginTop: 2,
    fontFamily: "Courier",
    fontSize: 14,
    fontWeight: "700",
    color: colors.text,
  },
  statsRow: {
    flexDirection: "row",
    gap: spacing.lg,
    marginTop: spacing.sm,
    marginBottom: spacing.sm,
  },
  stat: {
    fontSize: 12,
    color: colors.textMuted,
  },
  statVal: {
    color: colors.text,
    fontWeight: "700",
  },
  warningBox: {
    backgroundColor: colors.warningBg,
    borderColor: colors.warning,
    borderWidth: 1,
    borderRadius: radii.sm,
    padding: spacing.sm,
    marginBottom: spacing.sm,
  },
  warningText: {
    fontSize: 12,
    color: colors.warning,
    fontWeight: "600",
  },
  fileRow: {
    fontFamily: "Courier",
    fontSize: 11,
    color: colors.textMuted,
    marginTop: 2,
  },
});
