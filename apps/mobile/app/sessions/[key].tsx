import { Pressable, ScrollView, StyleSheet, Text, View } from "react-native";
import { useLocalSearchParams, useRouter } from "expo-router";
import { BlastRadiusCard } from "@/components/BlastRadiusCard";
import { CapabilityBadge } from "@/components/CapabilityBadge";
import { DiffHunkList } from "@/components/DiffHunkList";
import { SteeringBar } from "@/components/SteeringBar";
import { SubagentTree } from "@/components/SubagentTree";
import { deriveCapabilityBadges } from "@/protocol/types";
import { useControlPlane } from "@/state/ControlPlaneContext";
import { colors, radii, spacing } from "@/theme/tokens";

export default function SessionDetailScreen() {
  const { key } = useLocalSearchParams<{ key: string }>();
  const router = useRouter();
  const { state } = useControlPlane();

  const decodedKey = key ? decodeURIComponent(key) : "";
  const session = state.sessions.find((s) => s.session_key === decodedKey);

  if (!session) {
    return (
      <View style={styles.emptyWrap}>
        <Text style={styles.emptyTitle}>Session Not Found</Text>
        <Text style={styles.emptyBody}>Session `{decodedKey}` is no longer active.</Text>
      </View>
    );
  }

  const badges = deriveCapabilityBadges(session);
  const sessionTickets = state.openTickets.filter((t) =>
    session.pending_approval_ids.includes(t.ticket_id),
  );

  return (
    <ScrollView style={styles.screen} contentContainerStyle={styles.content}>
      <View style={styles.headerCard}>
        <View style={styles.rowBetween}>
          <Text style={styles.repoLabel}>{session.repo_label}</Text>
          <Text style={styles.harness}>{session.harness.toUpperCase()}</Text>
        </View>
        <Text style={styles.title}>{session.title}</Text>
        <Text style={styles.meta}>
          {session.branch ?? "detached"} • {session.worktree_path}
        </Text>

        <View style={styles.badgesWrap}>
          {badges.map((b) => (
            <CapabilityBadge key={b.label} badge={b} />
          ))}
        </View>

        <SubagentTree subagents={session.active_subagents} />

        <Pressable
          style={styles.ptyBtn}
          onPress={() =>
            router.push({
              pathname: "/pty/[sessionKey]",
              params: { sessionKey: encodeURIComponent(session.session_key) },
            })
          }
        >
          <Text style={styles.ptyBtnText}>
            Open Opt-In PTY Drawer (Read-Only Default)
          </Text>
        </Pressable>
      </View>

      {sessionTickets.length > 0 && (
        <View style={styles.section}>
          <Text style={styles.sectionTitle}>Pending Approvals</Text>
          {sessionTickets.map((ticket) => (
            <Pressable
              key={ticket.ticket_id}
              style={styles.approvalPill}
              onPress={() =>
                router.push({
                  pathname: "/approvals/[ticketId]",
                  params: { ticketId: ticket.ticket_id },
                })
              }
            >
              <Text style={styles.approvalTitle}>
                {ticket.tool_name} • {ticket.input_sha256.slice(0, 12)}…
              </Text>
              <Text style={styles.approvalSub}>Tap to open native approval sheet</Text>
            </Pressable>
          ))}
        </View>
      )}

      {session.blast_radius && <BlastRadiusCard summary={session.blast_radius} />}

      <View style={styles.section}>
        <Text style={styles.sectionTitle}>Uncommitted Diff Hunks (FlashList)</Text>
        <DiffHunkList files={session.diff_files ?? []} />
      </View>

      <SteeringBar session={session} />
    </ScrollView>
  );
}

const styles = StyleSheet.create({
  screen: { flex: 1, backgroundColor: colors.bg },
  content: { padding: spacing.lg, paddingBottom: 48 },
  emptyWrap: { flex: 1, backgroundColor: colors.bg, justifyContent: "center", padding: spacing.xl },
  emptyTitle: { color: colors.text, fontSize: 18, fontWeight: "800" },
  emptyBody: { color: colors.textMuted, fontSize: 13, marginTop: 6 },
  headerCard: {
    backgroundColor: colors.surface,
    borderWidth: 1,
    borderColor: colors.border,
    borderRadius: radii.md,
    padding: spacing.md,
    marginBottom: spacing.md,
  },
  rowBetween: { flexDirection: "row", justifyContent: "space-between", alignItems: "center" },
  repoLabel: { color: colors.text, fontSize: 17, fontWeight: "800" },
  harness: { color: colors.accent, fontSize: 12, fontWeight: "800" },
  title: { color: colors.text, fontSize: 14, marginTop: 4 },
  meta: { color: colors.textDim, fontFamily: "Courier", fontSize: 11, marginTop: 2 },
  badgesWrap: { flexDirection: "row", flexWrap: "wrap", gap: spacing.xs, marginTop: spacing.sm },
  ptyBtn: {
    marginTop: spacing.md,
    backgroundColor: colors.surfaceElevated,
    borderWidth: 1,
    borderColor: colors.border,
    borderRadius: radii.sm,
    paddingVertical: spacing.sm + 2,
    alignItems: "center",
  },
  ptyBtnText: { color: colors.accent, fontSize: 13, fontWeight: "700" },
  section: { marginBottom: spacing.md },
  sectionTitle: {
    fontSize: 12,
    fontWeight: "800",
    color: colors.textMuted,
    textTransform: "uppercase",
    marginBottom: spacing.sm,
  },
  approvalPill: {
    backgroundColor: colors.warningBg,
    borderWidth: 1,
    borderColor: colors.warning,
    borderRadius: radii.md,
    padding: spacing.md,
    marginBottom: spacing.sm,
  },
  approvalTitle: { color: colors.warning, fontWeight: "800", fontSize: 14 },
  approvalSub: { color: colors.text, fontSize: 12, marginTop: 2 },
});
