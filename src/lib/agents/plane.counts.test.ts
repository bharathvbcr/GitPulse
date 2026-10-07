import { describe, expect, it } from "vitest";
import type { FleetRepoFacet } from "../fleet/types";
import { buildFleetRows } from "../fleet/row";
import { unknownFacts } from "../repos/facts";
import { WATCH_ACTIVE } from "../repos/watchState";
import { agentCheckoutCount } from "../work/projection";
import { applyAgentFilter, liveAgentCount, planeHeadline } from "./plane";
import { listing, probe, project, snapshot, tasks, task, terminal, worktree } from "./__tests__/fixtures";

/**
 * Three surfaces count agents, and they count two different things:
 *
 * - agent checkouts on disk: Fleet's work cell, the Work view's tile and the
 *   plane's headline;
 * - live agent terminals this window started: the tab chip and the plane's
 *   headline.
 *
 * Each quantity has one definition. These tests feed one fixture to every
 * surface and require the same number back.
 */

const items = [
  worktree({ path: "/repo", name: "repo", agent_kind: "", session_slug: "", is_main: true }),
  worktree(),
  worktree({ path: "/repo/.codex/worktrees/b", agent_kind: "codex", session_slug: "b", name: "b" }),
  // A worktree at the container itself: an agent layout with no slug.
  worktree({ path: "/repo/.claude/worktrees", agent_kind: "claude", session_slug: "", name: "worktrees" }),
  worktree({ path: "/repo/feature", agent_kind: "", session_slug: "", name: "feature" }),
];

// What `agent_summary` in src-tauri/src/insights/mod.rs computes: every item with a kind.
const facetSessions = items.filter((item) => item.agent_kind !== "").length;

const records = [
  { key: "a", label: "Claude", status: "running", repoPath: "/repo", sessionId: "s-a" },
  { key: "a", label: "Claude", status: "running", repoPath: "/repo", sessionId: "s-a" },
  { key: "b", label: "Shell", status: "running", repoPath: "/repo", sessionId: "s-b" },
  { key: "c", label: "Shell", status: "running", repoPath: "/repo", sessionId: "s-c" },
  { key: "d", label: "Codex", status: "exited", repoPath: "/repo", sessionId: "s-d" },
];
const directories = new Map([["s-b", "/repo/.codex/worktrees/b/src"], ["s-c", "/repo"]]);

function facet(): FleetRepoFacet {
  return {
    repo_path: "/repo", ok: true, error: "", worktrees_ok: true, worktrees_error: "",
    worktrees: items.length,
    agents: { ok: true, sessions: facetSessions, kinds: [], truncated: false },
    last_commit_ok: true, last_commit_epoch: 1, commits_ok: false, commits_error: "", commits: null,
    metrics_ok: false, metrics_error: "", metrics: null,
  };
}

describe("one definition of each agent count", () => {
  it("Fleet, the Work view, the plane and the tab chip agree on one fixture", () => {
    const plane = project({
      probes: [probe({ snapshot: snapshot({ worktrees: listing(items) }) })],
      terminals: records.map((record) => terminal({
        key: record.key,
        label: record.label,
        status: record.status,
        repoPath: record.repoPath,
        sessionId: record.sessionId,
        cwd: directories.get(record.sessionId) ?? null,
      })),
    });
    const [fleet] = buildFleetRows({
      open: [{ ...unknownFacts("/repo", "repo"), hydrated: true, watch: WATCH_ACTIVE, branch: "main" }],
      recents: [],
      snapshot: { repos: [facet()], requested: 1, scanned: 1, anchor_epoch: 1, truncated: false, duration_ms: 1 },
      snapshotError: null,
      scanFailures: {},
      now: 1,
    });
    expect(fleet.work.kind).toBe("read");
    const fleetSessions = fleet.work.kind === "read" ? fleet.work.value.agentSessions : -1;

    // Agent checkouts on disk.
    expect(plane.checkouts).toBe(facetSessions);
    expect(fleetSessions).toBe(facetSessions);
    expect(agentCheckoutCount(items.map((item) => item.path))).toBe(facetSessions);
    expect(plane.rows.filter((row) => row.origin === "checkout")).toHaveLength(facetSessions);

    // Live agent terminals: the tab chip, the plane's headline and its rows.
    expect(liveAgentCount(records, directories)).toBe(2);
    expect(plane.live).toBe(liveAgentCount(records, directories));
    expect(plane.rows.filter((row) => row.presence === "live")).toHaveLength(plane.live);
    expect(planeHeadline(plane, plane.rows.length)).toMatch(/^3 agent checkouts · 2 live terminals · /);
  });

  it("the tab chip counts a key once and reads the directory it is given", () => {
    const twice = [records[0], records[0]];
    expect(liveAgentCount(twice)).toBe(1);
    expect(liveAgentCount([records[2]])).toBe(0);
    expect(liveAgentCount([records[2]], directories)).toBe(1);
  });
});

describe("P7: the headline says what the rows are", () => {
  it("names checkouts, live terminals and task attempts, and pluralizes each", () => {
    expect(planeHeadline(project(), 1)).toBe("1 agent checkout · 0 live terminals · 0 need attention");
    const watched = project({
      terminals: [terminal({ cwd: "/repo/.claude/worktrees/alpha" })],
      tasks: tasks({ tasks: [task(), task({ runId: "run-2", cwd: "/nowhere", tone: "needs-you" })] }),
    });
    expect(planeHeadline(watched, watched.rows.length))
      .toBe("1 agent checkout · 1 live terminal · 2 task attempts · 1 need attention");
    expect(planeHeadline(watched, watched.rows.length)).not.toMatch(/session/);
  });

  it("says a task list that was not read is not zero attempts", () => {
    const unread = project({ tasks: tasks({ ok: false, unread: true }) });
    expect(planeHeadline(unread, unread.rows.length)).toContain("task attempts not read");
    const capped = project({ tasks: tasks({ complete: false, tasks: [task({ cwd: "/nowhere", tone: "needs-you" })] }) });
    expect(planeHeadline(capped, capped.rows.length)).toContain("at least 1 task attempt");
  });
});

describe("the attention filter and the headline", () => {
  it("count unread rows apart from rows that need the reader, and the filter holds both", () => {
    const plane = project({
      probes: [probe({
        snapshot: snapshot({
          worktrees: listing([
            worktree(),
            worktree({ path: "/repo/.claude/worktrees/beta", session_slug: "beta", name: "beta" }),
            worktree({ path: "/repo/.claude/worktrees/gamma", session_slug: "gamma", name: "gamma", operation_ok: false }),
          ]),
          // A partial collision scan marks every checkout `unscanned`.
          collisions: {
            ok: true, error: "", overlapping_files: 0, worktrees_involved: 0, scanned_worktrees: 2,
            unscanned_worktrees: 1, failed_worktrees: 0, truncated: false, items: [],
          },
        }),
      })],
      terminals: [terminal({ cwd: "/repo/.claude/worktrees/alpha", attention: "needs-you" })],
    });
    const headline = planeHeadline(plane, plane.rows.length);
    expect(headline).toContain("1 need attention");
    expect(headline).toContain("2 not fully read");
    expect(plane.attention).toEqual({ needing: 1, unread: 2 });
    expect(applyAgentFilter(plane.rows, "attention")).toHaveLength(plane.attention.needing + plane.attention.unread);
  });
});
