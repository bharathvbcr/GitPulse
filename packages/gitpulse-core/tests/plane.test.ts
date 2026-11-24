import { describe, expect, it } from "vitest";
import {
  applyAgentTree,
  defaultCapabilitiesForHarness,
  describeControlCapabilities,
  flattenAgentTree,
  planeHeadline,
  projectAgentPlane,
  rollupSummary,
  type PlaneInput,
  type PlaneTerminal,
  type SubagentSummary,
} from "../src/index";

const PATHS = { caseInsensitive: true };

function makeTerminal(overrides: Partial<PlaneTerminal> = {}): PlaneTerminal {
  return {
    key: "term-1",
    repoPath: "/Users/dev/GitPulse",
    label: "claude",
    launcher: "claude",
    title: "Claude main",
    status: "running",
    sessionId: "sess-1",
    taskRunId: "",
    continuesRunId: "",
    cwd: "/Users/dev/GitPulse",
    attention: null,
    ...overrides,
  };
}

describe("packages/gitpulse-core plane projection", () => {
  it("projects active_subagents under parent sessions and rolls up subagent counts and attention", () => {
    const subagents: SubagentSummary[] = [
      {
        agent_id: "sub-protocol",
        parent_session_id: "sess-agy-1",
        kind: "agy",
        status: "running",
        label: "protocol-builder",
        started_at_ms: 1_800_000_000_000,
        updated_at_ms: 1_800_000_002_000,
      },
      {
        agent_id: "sub-review",
        parent_session_id: "sess-agy-1",
        kind: "agy",
        status: "needs_you",
        label: "opus-reviewer",
        started_at_ms: 1_800_000_001_000,
        updated_at_ms: 1_800_000_003_000,
      },
      {
        agent_id: "sub-done",
        parent_session_id: "sess-agy-1",
        kind: "agy",
        status: "completed",
        label: "audit-worker",
        started_at_ms: 1_800_000_000_000,
        updated_at_ms: 1_800_000_004_000,
      },
    ];

    const input: PlaneInput = {
      probes: [],
      terminals: [
        makeTerminal({
          key: "agy-coord",
          label: "agy",
          launcher: "agy",
          title: "Antigravity Coordinator",
          sessionId: "sess-agy-1",
          activeSubagents: subagents,
        }),
      ],
      tasks: null,
      paths: PATHS,
    };

    const plane = projectAgentPlane(input);
    expect(plane.activeSubagents).toBe(3);
    expect(plane.roots).toHaveLength(1);

    const root = plane.roots[0];
    expect(root.activeSubagents).toHaveLength(3);
    expect(root.children).toHaveLength(3);
    expect(root.children.every((c) => c.origin === "subagent")).toBe(true);
    expect(root.rollup.descendants).toBe(3);
    expect(root.rollup.subagents).toBe(3);
    expect(root.rollup.liveSubagents).toBe(2);
    expect(root.rollup.attention).toContain("needs-you");

    // Quiet parent stays visible under "attention" tree filter because a sub-agent asks!
    const attentionRoots = applyAgentTree(plane.roots, "attention");
    expect(attentionRoots).toHaveLength(1);

    const summary = rollupSummary(root);
    expect(summary.presence).toContain("2/3 sub-agents active");
    expect(summary.attention).toContain("Needs you");

    const flat = flattenAgentTree(plane.roots, new Set());
    expect(flat).toHaveLength(4);
    expect(flat[0].level).toBe(1);
    expect(flat[1].level).toBe(2);

    const headline = planeHeadline(plane, plane.rows.length);
    expect(headline).toContain("3 sub-agents");
  });

  it("marks Antigravity sessions as ungated and shows Queued when idle or steering is queued", () => {
    const input: PlaneInput = {
      probes: [],
      terminals: [
        makeTerminal({
          key: "agy-idle",
          label: "agy",
          launcher: "agy",
          title: "Antigravity Idle",
          fullyIdle: true,
        }),
        makeTerminal({
          key: "agy-active-queued",
          label: "agy",
          launcher: "agy",
          title: "Antigravity Active",
          fullyIdle: false,
          queuedSteeringCount: 1,
        }),
        makeTerminal({
          key: "claude-live",
          label: "claude",
          launcher: "claude",
          title: "Claude Code",
          fullyIdle: true,
        }),
      ],
      tasks: null,
      paths: PATHS,
    };

    const plane = projectAgentPlane(input);
    const agyIdle = plane.rows.find((r) => r.liveKey === "agy-idle")!;
    const agyQueued = plane.rows.find((r) => r.liveKey === "agy-active-queued")!;
    const claude = plane.rows.find((r) => r.liveKey === "claude-live")!;

    expect(agyIdle.ungated).toBe(true);
    expect(agyIdle.approvalChannel).toBe("native_only");
    expect(agyIdle.capabilityBadges).toContain("ungated");
    expect(agyIdle.capabilityBadges).toContain("Queued");
    expect(agyIdle.capabilities.supports_remote_approval).toBe(false);
    expect(agyIdle.capabilities.supports_idle_wake).toBe(false);
    expect(agyIdle.capabilities.pty_input_enabled).toBe(false);

    expect(agyQueued.ungated).toBe(true);
    expect(agyQueued.queuedSteeringCount).toBe(1);
    expect(agyQueued.capabilityBadges).toContain("Queued");

    expect(claude.ungated).toBe(false);
    expect(claude.approvalChannel).toBe("presence_gated_hook");
    expect(claude.capabilityBadges).toContain("Presence-Gated");
    expect(claude.capabilityBadges).not.toContain("ungated");
    expect(claude.capabilityBadges).not.toContain("Queued");
  });

  it("derives honest per-session ControlCapability matrices for Codex Tier 1/3 and Manvi", () => {
    const codexAppServer = defaultCapabilitiesForHarness("codex", {
      approvalChannel: "codex_app_server",
    });
    const codexAppDesc = describeControlCapabilities(codexAppServer);
    expect(codexAppDesc.badges).toEqual(["Codex App-Server"]);
    expect(codexAppDesc.canRemoteApprove).toBe(true);
    expect(codexAppDesc.isUngated).toBe(false);
    expect(codexAppDesc.canPtyInput).toBe(false);

    const codexObserve = defaultCapabilitiesForHarness("codex");
    const codexObserveDesc = describeControlCapabilities(codexObserve);
    expect(codexObserveDesc.badges).toContain("Observe Only");
    expect(codexObserveDesc.badges).toContain("ungated");
    expect(codexObserveDesc.canRemoteApprove).toBe(false);

    const manvi = defaultCapabilitiesForHarness("manvi");
    const manviDesc = describeControlCapabilities(manvi);
    expect(manviDesc.badges).toEqual(["Manvi Policy"]);
    expect(manviDesc.canRemoteApprove).toBe(true);
    expect(manviDesc.isUngated).toBe(false);
  });
});
