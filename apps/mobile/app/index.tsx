import { useMemo, useState } from "react";
import { Pressable, ScrollView, StyleSheet, Text, View } from "react-native";
import { useRouter } from "expo-router";
import { CapabilityBadge } from "@/components/CapabilityBadge";
import { ProbeGapCard } from "@/components/ProbeGapCard";
import { SubagentTree } from "@/components/SubagentTree";
import {
  deriveCapabilityBadges,
  type HarnessKind,
  type SessionSnapshot,
} from "@/protocol/types";
import { useControlPlane } from "@/state/ControlPlaneContext";
import { colors, radii, spacing } from "@/theme/tokens";

type InboxFilter = "all" | "approvals" | "attention" | "gaps" | HarnessKind;

const FILTERS: ReadonlyArray<{ id: InboxFilter; label: string }> = [
  { id: "all", label: "All" },
  { id: "approvals", label: "Approvals" },
  { id: "attention", label: "Attention" },
  { id: "gaps", label: "Gaps" },
  { id: "claude", label: "Claude" },
  { id: "antigravity", label: "Antigravity" },
  { id: "codex", label: "Codex" },
  { id: "manvi", label: "Manvi" },
];

export default function UnifiedInboxScreen() {
  const router = useRouter();
  const { state, dismissBanner, resyncNow } = useControlPlane();
  const [filter, setFilter] = useState<InboxFilter>("all");

  const filteredSessions = useMemo<readonly SessionSnapshot[]>(() => {
    if (filter === "all" || filter === "gaps") return state.sessions;
    if (filter === "approvals") {
      return state.sessions.filter((s) => s.pending_approval_ids.length > 0);
    }
    if (filter === "attention") {
      return state.sessions.filter(
        (s) => Boolean(s.attention) || s.pending_approval_ids.length > 0,
      );
    }
    return state.sessions.filter((s) => s.harness === filter);
  }, [state.sessions, filter]);

  return (
    <ScrollView style={styles.screen} contentContainerStyle={styles.content}>
      {/* Connection & Header Bar */}
      <View style={styles.topBar}>
        <View>
          <Text style={styles.connStatus}>
            Relay: {state.connectionState.toUpperCase()}
            {state.resyncing ? " • RESYNCING" : ""}
          </Text>
          <Text style={styles.epochMeta}>
            epoch {state.hostEpoch} • seq {state.lastSeq}
          </Text>
        </View>
        <View style={styles.topActions}>
          <Pressable style={styles.smallBtn} onPress={resyncNow}>
            <Text style={styles.smallBtnText}>Resync</Text>
          </Pressable>
          <Pressable
            style={[styles.smallBtn, styles.accentBtn]}
            onPress={() => router.push("/pair")}
          >
            <Text style={styles.smallBtnText}>Pair / Devices</Text>
          </Pressable>
        </View>
      </View>

      {/* Banners (not_applied late decisions, steering_expired, cursor_gap, revoked) */}
      {state.banners.map((banner) => (
        <View
          key={banner.id}
          style={[
            styles.banner,
            banner.severity === "error"
              ? styles.bannerError
              : banner.severity === "warning"
                ? styles.bannerWarn
                : styles.bannerInfo,
          ]}
        >
          <View style={styles.bannerHeader}>
            <Text style={styles.bannerTitle}>{banner.title}</Text>
            <Pressable onPress={() => dismissBanner(banner.id)}>
              <Text style={styles.dismiss}>Dismiss</Text>
            </Pressable>
          </View>
          <Text style={styles.bannerMsg}>{banner.message}</Text>
        </View>
      ))}

      {/* Pending Approvals Queue */}
      {state.openTickets.length > 0 && (
        <View style={styles.section}>
          <Text style={styles.sectionTitle}>
            Pending Hardware-Signed Approvals ({state.openTickets.length})
          </Text>
          {state.openTickets.map((ticket) => (
            <Pressable
              key={ticket.ticket_id}
              style={styles.ticketCard}
              onPress={() =>
                router.push({
                  pathname: "/approvals/[ticketId]",
                  params: { ticketId: ticket.ticket_id },
                })
              }
            >
              <View style={styles.rowBetween}>
                <Text style={styles.ticketTool}>{ticket.tool_name}</Text>
                <Text style={styles.ticketHarness}>{ticket.harness.toUpperCase()}</Text>
              </View>
              <Text style={styles.ticketSummary} numberOfLines={2}>
                {ticket.summary ?? ticket.cwd}
              </Text>
              <Text style={styles.ticketHash}>
                sha256: {ticket.input_sha256.slice(0, 16)}… • Tap to inspect full bytes
              </Text>
            </Pressable>
          ))}
        </View>
      )}

      {/* ProbeGap Warning Cards — always visible when gaps exist */}
      {state.probeGaps.length > 0 && (
        <View style={styles.section}>
          <Text style={styles.sectionTitle}>Probe Gaps ({state.probeGaps.length})</Text>
          {state.probeGaps.map((gap) => (
            <ProbeGapCard key={gap.id} gap={gap} />
          ))}
        </View>
      )}

      {/* Filter Bar */}
      <ScrollView
        horizontal
        showsHorizontalScrollIndicator={false}
        style={styles.filterScroll}
        contentContainerStyle={styles.filterRow}
      >
        {FILTERS.map((f) => (
          <Pressable
            key={f.id}
            style={[styles.filterChip, filter === f.id && styles.filterChipActive]}
            onPress={() => setFilter(f.id)}
          >
            <Text style={[styles.filterText, filter === f.id && styles.filterTextActive]}>
              {f.label}
            </Text>
          </Pressable>
        ))}
      </ScrollView>

      {/* Cross-Repo / Cross-Harness Session Rows */}
      <View style={styles.section}>
        <Text style={styles.sectionTitle}>
          Active Sessions ({filteredSessions.length})
        </Text>
        {filteredSessions.length === 0 ? (
          <View style={styles.emptyCard}>
            <Text style={styles.emptyTitle}>No sessions match "{filter}"</Text>
            <Text style={styles.emptySub}>
              Pair a desktop running `wayloded --relay` (Geek / Squad / Enterprise) to view
              live Claude, Antigravity, Codex, and Manvi sessions.
            </Text>
          </View>
        ) : (
          filteredSessions.map((session) => {
            const badges = deriveCapabilityBadges(session);
            return (
              <Pressable
                key={session.session_key}
                style={styles.sessionCard}
                onPress={() =>
                  router.push({
                    pathname: "/sessions/[key]",
                    params: { key: encodeURIComponent(session.session_key) },
                  })
                }
              >
                <View style={styles.rowBetween}>
                  <Text style={styles.repoName}>{session.repo_label}</Text>
                  <Text style={styles.harnessTag}>
                    {session.harness.toUpperCase()} • {session.presence}
                  </Text>
                </View>

                <Text style={styles.sessionTitle}>{session.title}</Text>
                <Text style={styles.branchText}>
                  {session.branch ?? "detached"} • {session.worktree_path}
                </Text>

                <View style={styles.badgesWrap}>
                  {badges.map((badge) => (
                    <CapabilityBadge key={badge.label} badge={badge} />
                  ))}
                </View>

                {session.attention && (
                  <Text style={styles.attentionLine}>
                    Attention: {session.attention}
                    {session.attention_detail ? ` — ${session.attention_detail}` : ""}
                  </Text>
                )}

                <SubagentTree subagents={session.active_subagents} />
              </Pressable>
            );
          })
        )}
      </View>
    </ScrollView>
  );
}

const styles = StyleSheet.create({
  screen: { flex: 1, backgroundColor: colors.bg },
  content: { padding: spacing.lg, paddingBottom: 48 },
  topBar: {
    flexDirection: "row",
    justifyContent: "space-between",
    alignItems: "center",
    marginBottom: spacing.md,
  },
  connStatus: { fontSize: 12, fontWeight: "800", color: colors.accent },
  epochMeta: { fontSize: 11, fontFamily: "Courier", color: colors.textDim },
  topActions: { flexDirection: "row", gap: spacing.sm },
  smallBtn: {
    backgroundColor: colors.surfaceElevated,
    borderWidth: 1,
    borderColor: colors.border,
    borderRadius: radii.sm,
    paddingHorizontal: spacing.md,
    paddingVertical: spacing.xs + 2,
  },
  accentBtn: { borderColor: colors.accent },
  smallBtnText: { color: colors.text, fontSize: 12, fontWeight: "700" },
  banner: {
    borderRadius: radii.md,
    borderWidth: 1,
    padding: spacing.md,
    marginBottom: spacing.sm,
  },
  bannerWarn: { backgroundColor: colors.warningBg, borderColor: colors.warning },
  bannerError: { backgroundColor: colors.dangerBg, borderColor: colors.danger },
  bannerInfo: { backgroundColor: colors.infoBg, borderColor: colors.info },
  bannerHeader: { flexDirection: "row", justifyContent: "space-between", marginBottom: 4 },
  bannerTitle: { color: colors.text, fontWeight: "800", fontSize: 13 },
  dismiss: { color: colors.textMuted, fontSize: 12 },
  bannerMsg: { color: colors.text, fontSize: 12, lineHeight: 17 },
  section: { marginTop: spacing.md },
  sectionTitle: {
    fontSize: 12,
    fontWeight: "800",
    color: colors.textMuted,
    textTransform: "uppercase",
    letterSpacing: 0.4,
    marginBottom: spacing.sm,
  },
  ticketCard: {
    backgroundColor: colors.warningBg,
    borderColor: colors.warning,
    borderWidth: 1,
    borderRadius: radii.md,
    padding: spacing.md,
    marginBottom: spacing.sm,
  },
  ticketTool: { color: colors.warning, fontWeight: "800", fontSize: 15 },
  ticketHarness: { color: colors.textMuted, fontWeight: "700", fontSize: 11 },
  ticketSummary: { color: colors.text, fontSize: 13, marginTop: 4 },
  ticketHash: { color: colors.textMuted, fontFamily: "Courier", fontSize: 11, marginTop: 6 },
  filterScroll: { marginTop: spacing.sm },
  filterRow: { gap: spacing.xs },
  filterChip: {
    backgroundColor: colors.surface,
    borderWidth: 1,
    borderColor: colors.border,
    borderRadius: radii.pill,
    paddingHorizontal: spacing.md,
    paddingVertical: 6,
  },
  filterChipActive: { backgroundColor: colors.infoBg, borderColor: colors.accent },
  filterText: { color: colors.textMuted, fontSize: 12, fontWeight: "600" },
  filterTextActive: { color: colors.text },
  sessionCard: {
    backgroundColor: colors.surface,
    borderWidth: 1,
    borderColor: colors.border,
    borderRadius: radii.md,
    padding: spacing.md,
    marginBottom: spacing.md,
  },
  rowBetween: { flexDirection: "row", justifyContent: "space-between", alignItems: "center" },
  repoName: { color: colors.text, fontWeight: "800", fontSize: 15 },
  harnessTag: { color: colors.accent, fontSize: 11, fontWeight: "700" },
  sessionTitle: { color: colors.text, fontSize: 13, marginTop: 4 },
  branchText: { color: colors.textDim, fontFamily: "Courier", fontSize: 11, marginTop: 2 },
  badgesWrap: { flexDirection: "row", flexWrap: "wrap", gap: spacing.xs, marginTop: spacing.sm },
  attentionLine: { color: colors.warning, fontSize: 12, marginTop: spacing.sm, fontWeight: "600" },
  emptyCard: {
    backgroundColor: colors.surface,
    borderWidth: 1,
    borderColor: colors.border,
    borderRadius: radii.md,
    padding: spacing.lg,
  },
  emptyTitle: { color: colors.text, fontSize: 15, fontWeight: "700" },
  emptySub: { color: colors.textMuted, fontSize: 13, marginTop: 4, lineHeight: 19 },
});
