import { StyleSheet, Text, View } from "react-native";
import type { SubagentSummary } from "../protocol/types";
import { colors, radii, spacing } from "../theme/tokens";

export function SubagentTree({ subagents }: { subagents: readonly SubagentSummary[] }) {
  if (subagents.length === 0) {
    return null;
  }

  return (
    <View style={styles.container}>
      <Text style={styles.heading}>
        Active Swarm / Sub-Agents ({subagents.length})
      </Text>
      {subagents.map((sub) => (
        <View key={sub.agent_id} style={styles.row}>
          <View style={styles.branchLine} />
          <View style={styles.body}>
            <View style={styles.titleRow}>
              <Text style={styles.name}>{sub.label ?? sub.agent_id}</Text>
              <Text style={styles.status}>{sub.status.toUpperCase()}</Text>
            </View>
            <Text style={styles.meta}>
              {sub.kind} • {sub.agent_id}
              {sub.attention_observed === false ? " • attention unobserved" : ""}
            </Text>
          </View>
        </View>
      ))}
    </View>
  );
}

const styles = StyleSheet.create({
  container: {
    marginTop: spacing.sm,
    paddingTop: spacing.sm,
    borderTopWidth: 1,
    borderTopColor: colors.border,
  },
  heading: {
    fontSize: 11,
    fontWeight: "700",
    color: colors.textMuted,
    marginBottom: spacing.xs,
    textTransform: "uppercase",
    letterSpacing: 0.4,
  },
  row: {
    flexDirection: "row",
    alignItems: "stretch",
    marginBottom: spacing.xs,
  },
  branchLine: {
    width: 2,
    backgroundColor: colors.accent,
    borderRadius: radii.sm,
    marginRight: spacing.sm,
  },
  body: {
    flex: 1,
  },
  titleRow: {
    flexDirection: "row",
    justifyContent: "space-between",
    alignItems: "center",
  },
  name: {
    fontSize: 13,
    fontWeight: "600",
    color: colors.text,
  },
  status: {
    fontSize: 10,
    fontWeight: "700",
    color: colors.accent,
  },
  meta: {
    fontSize: 11,
    color: colors.textDim,
  },
});
