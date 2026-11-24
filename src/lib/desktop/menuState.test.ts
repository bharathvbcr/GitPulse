import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { createRepoStore, type RepoState } from "../stores/repoStore";
import { interfaceStore } from "../stores/interfaceStore";
import { blockedWhilePrompting, buildMenuState, canDispatchMenuEvent, countAgentsWaiting, menuActionEnabled, statusDetailRows, statusInsights, statusKeepsPopover, statusKeyAction, statusShortcuts } from "./menuState";
import { MENU_LIMITS } from "./menuContract";
import { projectMenuAgentState } from "./menuState";
import { parseAttention, type AttentionKind, type SessionActivity } from "../terminal/sessionActivity";
import type { TerminalSessionRecord } from "../terminal/sessionRegistry";
import type { TaskRun } from "../workbench/client";
import type { AgentInboxThread } from "../agents/inboxTypes";

const empty = () => get(createRepoStore({ storage: null }));
const prefs = () => get(interfaceStore);
const loaded = (): RepoState => ({ ...empty(), currentPath: "/r/a", currentBranch: "main", isLoading: false });
const model = (repo = empty(), activity: Record<string, string[]> = {}) =>
  buildMenuState(repo, prefs(), "system", activity, false);

describe("native menu projection", () => {
  it.each(["fleet", "tasks", "agents", "repository"] as const)("projects selection for the %s surface", (globalSurface) => {
    const repo = { ...loaded(), activeTab: "history" as const, viewSections: { history: "diff" } };
    const state = buildMenuState(repo, { ...prefs(), globalSurface }, "system", {}, false);
    expect(state.checked.includes("fleet")).toBe(globalSurface === "fleet");
    expect(state.checked.includes("agents")).toBe(globalSurface === "agents");
    expect(state.checked.includes("tab-history")).toBe(globalSurface === "repository");
    expect(state.checked.includes("section:history:diff")).toBe(globalSurface === "repository");
    expect(menuActionEnabled(state, "tab-history")).toBe(true);
  });
  it("keeps the first status menu compact even for long repository names and branches", () => {
    const state = model({ ...loaded(), currentPath: `/r/${"long-path".repeat(40)}`,
      currentBranch: "feature/".repeat(40) }, { "/r/a": ["fetch"] });
    expect(Array.from(state.trayDetail).length).toBeLessThanOrEqual(72);
    expect(state.traySummary.text).toBe("Clean · 1 running elsewhere");
  });
  it("omits tray title counts unless the preference is on", () => {
    const repo = {
      ...loaded(),
      statuses: [{
        path: "a",
        status_code: " M",
        is_staged: false,
        is_conflicted: false,
        additions: 0,
        deletions: 0,
      }],
      openTabs: [{
        id: "1", path: "/r/a", name: "a", label: "a", pinned: false, isActive: true,
        isBare: false, isDirty: true, isLoading: false, error: null, currentBranch: "main",
        conflictedCount: 0, changedCount: 1,
      }],
      fetchedAt: Date.now() - 120_000,
    };
    expect(buildMenuState(repo, { ...prefs(), statusIconCounts: false }, "system", {}, false).trayTitle).toBeNull();
    expect(buildMenuState(repo, { ...prefs(), statusIconCounts: true }, "system", {}, false).trayTitle).toBe("1");
  });
  it("carries hideDockWhenClosed and fetch age into the projection", () => {
    const state = buildMenuState(
      { ...loaded(), fetchedAt: Date.now() - 4 * 60_000 },
      { ...prefs(), hideDockWhenClosed: false },
      "system",
      {},
      false,
    );
    expect(state.hideDockWhenClosed).toBe(false);
    expect(state.status.fetchedAt).not.toBeNull();
  });
  it("disables repository work at startup while Help and Open remain available", () => {
    const state = model();
    for (const id of ["fetch", "stage-all", "stash-pop", "terminal-dock", "tab-work", "section:history:graph", "copy-repo-path"]) {
      expect(menuActionEnabled(state, id), id).toBe(false);
    }
    expect(menuActionEnabled(state, "open")).toBe(true);
    expect(menuActionEnabled(state, "documentation")).toBe(true);
  });
  it("distinguishes empty, failed, bare, and busy states", () => {
    const repo = loaded();
    expect(menuActionEnabled(model(repo), "stash-pop")).toBe(false);
    for (const patch of [{ error: "unreadable" }, { isLoading: true }, { isBare: true },
      { operation: { operation: null, probeFailed: true } }]) {
      expect(menuActionEnabled(model({ ...repo, ...patch }), "quick-commit")).toBe(false);
    }
    const state = model(repo, { "/r/a": ["fetch"] });
    expect(menuActionEnabled(state, "fetch")).toBe(false);
    expect(state.labels.find((item) => item.id === "fetch")?.text).toBe("Fetching…");
    expect(menuActionEnabled(model(repo, { "/r/b": ["fetch"] }), "fetch")).toBe(true);
  });
  it("shows selection and terminal state, including an explicit theme equal to the system", () => {
    // The dock is per repository tab, so its menu state comes from the repo,
    // not from a workspace preference.
    const repo = { ...loaded(), activeTab: "history" as const, viewSections: { history: "diff" }, terminalOpen: true };
    const state = buildMenuState(repo, prefs(), "light", {}, false);
    expect(state.checked).toEqual(expect.arrayContaining(["theme-light", "tab-history", "section:history:diff", "terminal-dock"]));
    expect(state.checked).not.toContain("theme-system");
    expect(state.labels).toContainEqual({ id: "terminal-dock", text: "Hide Terminal" });
  });
  it("only exposes supported operation actions and permits staging resolutions", () => {
    const repo: RepoState = { ...loaded(), statuses: [{ path: "a", is_staged: false, is_conflicted: true,
      status_code: "UU", additions: 0, deletions: 0 }], operation: { probeFailed: false,
      operation: { kind: "Merge", current_step: null, total_steps: null, head_ref: null, incoming_ref: null,
        conflicted_paths: ["a"], conflicted_total: 1, available: ["abort", "continue"] } } };
    const state = model(repo);
    expect(menuActionEnabled(state, "stage-all")).toBe(true);
    expect(menuActionEnabled(state, "operation-abort")).toBe(true);
    expect(menuActionEnabled(state, "operation-continue")).toBe(false);
    expect(menuActionEnabled(state, "operation-skip")).toBe(false);
    expect(menuActionEnabled(state, "pull")).toBe(false);
    expect(state.traySummary.text).toContain("1 conflict");
  });
  it("never calls an unreadable repository clean and counts files uniquely", () => {
    expect(model({ ...loaded(), error: "denied" }).traySummary.text).toContain("unavailable");
    const file = { path: "same", is_conflicted: false, status_code: "M", additions: 0, deletions: 0 };
    const state = model({ ...loaded(), statuses: [{ ...file, is_staged: true }, { ...file, is_staged: false }] });
    expect(state.traySummary.text).toBe("1 changed · 1 staged");
    expect(state.trayDetails).toContain("1 changed · 1 staged · 0 conflicts");
  });
  it("links status to review, recovery or history and distinguishes other repositories' work", () => {
    expect(model().traySummary.id).toBe("open");
    expect(model(loaded()).traySummary.id).toBe("section:history:graph");
    expect(model({ ...loaded(), error: "denied" }).traySummary.id).toBe("refresh");
    const busy = model(loaded(), { "/r/a": ["fetch"], "/r/b": ["pull"] });
    expect(busy.traySummary.text).toBe("Fetching… · 1 running elsewhere");
    expect(busy.traySummary.id).toBe("section:work:overview");
    const degraded = model({ ...loaded(), watch: { status: "degraded", reason: "watch lost" } });
    expect(degraded.traySummary.text).toContain("Not live");
    expect(degraded.trayDetails).toContain("Polling · live updates unavailable");
  });
  it("labels upstream counts as the last fetch and never infers remote freshness", () => {
    const repo = { ...loaded(), fetchedAt: Date.now() - 4 * 60_000, branches: [{ name: "main", is_current: true, is_remote: false,
      tip_commit_id: "a".repeat(40), ahead_count: 2, behind_count: 3, upstream: "origin/main",
      is_default: true, is_gone: false, last_commit_timestamp: 0, last_author: "", last_summary: "",
      commits_ahead_of_base: 0, commits_behind_base: 0, additions: 0, deletions: 0, files_changed: 0 }] };
    expect(model(repo).traySummary.text).toContain("↑2 ↓3");
    expect(model(repo).trayDetails).toContain("2 ahead · 3 behind origin/main (fetched 4 min ago)");
    const never = { ...repo, fetchedAt: null };
    expect(model(never).trayDetails).toContain("2 ahead · 3 behind origin/main (never fetched)");
    repo.branches[0].is_gone = true;
    expect(model(repo).traySummary.text).not.toContain("↑");
    expect(model(repo).trayDetails).toContain("Upstream no longer exists");
  });
  it("does not label a prompt or clipboard read as running Git work", () => {
    const state = buildMenuState(loaded(), prefs(), "system", { "/r/a": ["copy-commit"] }, false, {});
    expect(state.traySummary.text).toBe("Clean");
    expect(state.trayDetails.join(" ")).not.toContain("running");
    expect(menuActionEnabled(state, "fetch")).toBe(false);
    expect(model(loaded(), { "/r/a": ["unstash"] }).traySummary.text).toBe("Popping Stash…");
  });
  it("projects stash, parked-operation and other-repo activity onto the status card", () => {
    const parked: RepoState = { ...loaded(), stashEntries: [
      { index: 0, selector: "stash@{0}", oid: "a".repeat(40), subject: "wip", message: "wip", branch: "main", timestamp: 0 },
      { index: 1, selector: "stash@{1}", oid: "b".repeat(40), subject: "old", message: "old", branch: null, timestamp: 0 },
    ], operation: { probeFailed: false, operation: { kind: "Merge", current_step: null, total_steps: null,
      head_ref: "main", incoming_ref: null, conflicted_paths: [], conflicted_total: 0, available: ["abort"] } } };
    const state = model(parked, { "/r/b": ["fetch"] });
    expect(state.status.stashes).toBe(2);
    expect(state.status.operation).toContain("Merge in progress");
    expect(state.status.activity).toBeNull();
    expect(state.status.elsewhere).toBe(1);
    expect(statusInsights(state.status).map((item) => item.id)).toEqual(["section:work:overview", "fleet"]);
    expect(statusShortcuts(state).map((item) => item.id)).toEqual(expect.arrayContaining([
      "section:history:graph", "section:insights:pulse", "fleet", "terminal-dock",
    ]));
    expect(statusShortcuts(state).some((item) => item.id === "copy-branch")).toBe(true);
    expect(model({ ...loaded(), stashFailed: true }).status.stashes).toBeNull();
  });
  it("routes compact status keys without mutating Git", () => {
    const snapshot = model({ ...loaded(), statuses: [{ path: "a", is_staged: true, is_conflicted: false,
      status_code: "M", additions: 0, deletions: 0 }] });
    const idle = { choosing: false, expanded: false };
    expect(statusKeyAction({ key: "Escape", metaKey: false, ctrlKey: false, altKey: false }, snapshot, idle))
      .toEqual({ dismiss: true });
    expect(statusKeyAction({ key: "Escape", metaKey: false, ctrlKey: false, altKey: false }, snapshot,
      { choosing: true, expanded: true })).toEqual({ collapse: "chooser" });
    expect(statusKeyAction({ key: "r", metaKey: false, ctrlKey: false, altKey: false }, snapshot, idle))
      .toEqual({ id: "refresh" });
    expect(statusKeyAction({ key: "a", metaKey: false, ctrlKey: false, altKey: false }, snapshot, idle))
      .toEqual({ id: "agents" });
    expect(statusKeyAction({ key: "A", metaKey: false, ctrlKey: false, altKey: false }, snapshot, idle))
      .toEqual({ id: "agents" });
    expect(statusKeyAction({ key: "1", metaKey: false, ctrlKey: false, altKey: false }, snapshot, idle))
      .toEqual({ id: "section:work:overview" });
    expect(statusKeyAction({ key: "3", metaKey: false, ctrlKey: false, altKey: false }, snapshot, idle)).toBeNull();
    expect(statusKeyAction({ key: "r", metaKey: true, ctrlKey: false, altKey: false }, snapshot, idle)).toBeNull();
  });
  it("keeps copy, refresh and appearance in the popover and labels details", () => {
    expect(statusKeepsPopover("copy-branch")).toBe(true);
    expect(statusKeepsPopover("refresh")).toBe(true);
    expect(statusKeepsPopover("toggle-theme")).toBe(true);
    expect(statusKeepsPopover("section:work:overview")).toBe(false);
    expect(statusDetailRows(["Repository: /r/a", "Live updates"])).toEqual([
      { label: "Repository", value: "/r/a" },
      { label: null, value: "Live updates" },
    ]);
  });
});


it("keeps stage-all available for the remaining edits of a partially staged file", () => {
  const state = model({ ...loaded(), statuses: [{ path: "mixed", status_code: "MM", is_staged: true, is_conflicted: false, additions: 3, deletions: 2 }] });
  expect(menuActionEnabled(state, "stage-all")).toBe(true);
});

/**
 * The tray is the one surface that is on screen while GitPulse is not, so it is
 * where an agent blocked on the reader has to show. The count is the shared
 * `asksForReader` rule applied to every tracked session — not a second rule.
 */
describe("agents waiting on the reader", () => {
  const session = (kind: AttentionKind | null): SessionActivity => ({
    lastOutputAt: 1,
    title: "claude",
    attention: kind === null ? null : { kind, label: kind, detail: null, at: 1 },
  });
  const waiting = (count: number) =>
    buildMenuState(empty(), prefs(), "system", {}, false, undefined, undefined, count).agentsWaiting;

  it("counts the sessions that ask for the reader, excluding hook-less bells by default", () => {
    const activity = new Map<string, SessionActivity>([
      ["needs", session("needs-you")],
      ["done", session("finished")],
      ["quiet", session(null)],
      ["broke", session("error")],
      ["bell", session("signalled")],
      ["also-done", session("finished")],
    ]);
    expect(countAgentsWaiting(activity)).toBe(2);
    expect(countAgentsWaiting(new Map())).toBe(0);
    expect(countAgentsWaiting(new Map([["done", session("finished")], ["quiet", session(null)]]))).toBe(0);
  });

  it("counts a signalled-only session only when the bell opt-in is on", () => {
    expect(countAgentsWaiting(new Map([["bell", session("signalled")]]))).toBe(0);
    interfaceStore.setBellAsksForReader(true);
    try {
      expect(countAgentsWaiting(new Map([["bell", session("signalled")]]))).toBe(1);
      expect(countAgentsWaiting(new Map([["bell", session("signalled")], ["needs", session("needs-you")], ["broke", session("error")]]))).toBe(3);
    } finally {
      interfaceStore.reset();
    }
    expect(countAgentsWaiting(new Map([["bell", session("signalled")]]))).toBe(0);
  });

  it("emits zero when nobody is waiting, so an older caller's payload is unchanged", () => {
    expect(model().agentsWaiting).toBe(0);
    expect(model(loaded()).agentsWaiting).toBe(0);
  });

  it("carries the count without moving anything the repository decides", () => {
    const repo = loaded();
    const plain = buildMenuState(repo, prefs(), "system", {}, false);
    const asked = buildMenuState(repo, prefs(), "system", {}, false, undefined, undefined, 2);
    expect(asked.agentsWaiting).toBe(2);
    expect(asked.trayTitle).toBe("2 waiting");
    expect({ ...asked, agentsWaiting: 0, trayTitle: plain.trayTitle }).toEqual(plain);
  });

  it("projects R4 terminal requests, managed decisions, and inbox asks in deterministic order", () => {
    const idlePrompt = parseAttention(
      { session: "s-idle", channel: "hook", event: "idle_prompt", urgency: "informational" },
      100,
    )!.attention!;
    const permission = parseAttention(
      { session: "s-perm", channel: "hook", event: "permission_prompt", detail: "Approve git push" },
      200,
    )!.attention!;
    const err = parseAttention(
      { session: "s-err", channel: "hook", event: "error", detail: "Tool exited 1" },
      150,
    )!.attention!;
    const bareBell: SessionActivity = {
      lastOutputAt: 50,
      title: "Shell",
      attention: { kind: "signalled", urgency: "informational", channel: "bell", label: "Bell", detail: null, at: 50 },
    };
    const activity = new Map<string, SessionActivity>([
      ["s-idle", { lastOutputAt: 100, title: "Claude", attention: idlePrompt }],
      ["s-perm", { lastOutputAt: 200, title: "Claude Code", attention: permission }],
      ["s-err", { lastOutputAt: 150, title: "Antigravity", attention: err }],
      ["s-bell", bareBell],
    ]);
    expect(countAgentsWaiting(activity, false)).toBe(2);

    const records: TerminalSessionRecord[] = [
      { key: "k1", repoPath: "/r/a", label: "Claude", launcher: "claude", status: "running", sessionId: "s-perm", close: async () => {} },
      { key: "k2", repoPath: "/r/b", label: "Antigravity", launcher: "agy", status: "running", sessionId: "s-err", close: async () => {} },
      { key: "k3", repoPath: "/r/a", label: "Shell", launcher: "shell", status: "running", sessionId: "s-bell", close: async () => {} },
    ];
    const runs = [
      {
        id: "run-1",
        task_id: "t-1",
        kind: "managed",
        state: "running",
        session_id: "ms-1",
        provider: "codex",
        task_title: "Review migration",
        cwd: "/r/c",
        updated_at: 0.1,
        expires_at: 9999999999,
      } as unknown as TaskRun,
    ];
    const inbox: AgentInboxThread[] = [
      {
        thread_key: "th-1",
        title: "Choose schema strategy",
        status: "open",
        ask_type: "decide",
        latest_msg: {} as AgentInboxThread["latest_msg"],
        messages: [],
        from_agent: "claude",
        to_agent: "developer",
        updated_at_ms: 180,
      },
      {
        thread_key: "th-dev",
        title: "Self ask",
        status: "open",
        ask_type: "do",
        latest_msg: {} as AgentInboxThread["latest_msg"],
        messages: [],
        from_agent: "developer",
        to_agent: "claude",
        updated_at_ms: 190,
      },
    ];
    const tabs = [
      { path: "/r/a", label: "GitPulse" },
      { path: "/r/b", label: "Manvi" },
    ];
    const projection = projectMenuAgentState(
      activity,
      records,
      { runs, pending: new Map([["run-1", { count: 2, more: false }]]), clock: 1000 },
      inbox,
      tabs,
      false,
      { caseInsensitive: false },
      "/r/a",
    );
    expect(projection.agentsWaiting).toBe(4);
    expect(projection.agentRequestsHidden).toBe(0);
    // 2 running terminal agents (s-perm, s-err; shell excluded) + 1 live managed run not linked to terminal = 3
    expect(projection.agentsRunning).toBe(3);
    // s-idle counts as 1 informational; bare detail-less bell excluded
    expect(projection.agentsInformational).toBe(1);
    // Blocking (oldest first: run-1@100, th-1@180, s-perm@200) before error (s-err@150)
    expect(projection.agentRequests.map((r) => [r.id, r.urgency, r.repoLabel])).toEqual([
      ["run:run-1", "blocking", "c"],
      ["inbox:th-1", "blocking", "GitPulse"],
      ["term:s-perm", "blocking", "GitPulse"],
      ["term:s-err", "error", "Manvi"],
    ]);

    const menu = buildMenuState(
      { ...loaded(), openTabs: [{ id: "1", path: "/r/a", name: "a", label: "GitPulse", pinned: false, isActive: true, isBare: false, isDirty: false, isLoading: false, error: null, currentBranch: "main", conflictedCount: 0, changedCount: 0 }] },
      { ...prefs(), statusIconCounts: true },
      "system",
      {},
      false,
      undefined,
      { caseInsensitive: false },
      undefined,
      0,
      projection,
    );
    expect(menu.trayTitle).toBe("4 waiting · 0");
    expect(canDispatchMenuEvent({ id: "focus-agent", path: "term:s-err", repo_path: "/elsewhere" }, menu, loaded())).toBe(true);
    expect(blockedWhilePrompting("focus-agent")).toBe(true);
  });

  it("caps agent requests at 8 with hidden overflow and prioritizes waiting count in trayTitle", () => {
    const activity = new Map<string, SessionActivity>();
    for (let i = 0; i < 10; i += 1) {
      activity.set(`s-${i}`, {
        lastOutputAt: i + 1,
        title: `Agent ${i}`,
        attention: { kind: "needs-you", urgency: "blocking", label: "Needs input", detail: null, at: i + 1 },
      });
    }
    const projection = projectMenuAgentState(activity, []);
    expect(projection.agentsWaiting).toBe(10);
    expect(projection.agentRequests).toHaveLength(MENU_LIMITS.agentRequests);
    expect(projection.agentRequestsHidden).toBe(2);

    const repo = {
      ...loaded(),
      statuses: Array.from({ length: 1234 }, (_, i) => ({
        path: `f-${i}`,
        status_code: " M",
        is_staged: false,
        is_conflicted: true,
        additions: 0,
        deletions: 0,
      })),
    };
    const menu = buildMenuState(
      repo,
      { ...prefs(), statusIconCounts: true },
      "system",
      {},
      false,
      undefined,
      undefined,
      undefined,
      0,
      projection,
    );
    expect(menu.trayTitle).toBe("10 waiting");
  });

  it("handles fractional attention.at, control-only ID suffixes, hidden Unicode in cleanField, and channel-less signalled notices", () => {
    const activity = new Map<string, SessionActivity>([
      [
        "\u0000\u0007\u009f",
        {
          lastOutputAt: 100,
          title: "Ghost ID",
          attention: { kind: "needs-you", urgency: "blocking", label: "Approve", detail: null, at: 100 },
        },
      ],
      [
        "s-frac",
        {
          lastOutputAt: 200,
          title: "Claude\u202e\u200bCode\u0085",
          attention: {
            kind: "needs-you",
            urgency: "blocking",
            label: "Needs\u202e approval",
            detail: "\u202e\u200b",
            at: 1_700_000_000_123.5,
          },
        },
      ],
      [
        "s-bare-sig",
        {
          lastOutputAt: 300,
          title: "Shell",
          // Synthetic signalled without channel or detail must stay aligned with plane.ts (0 notices)
          attention: {
            kind: "signalled",
            urgency: "informational",
            label: "Signal",
            detail: null,
            at: 300,
          } as unknown as SessionActivity["attention"],
        },
      ],
    ]);
    const runs: TaskRun[] = [
      {
        id: "\u0000\u001b",
        task_id: "t-ctrl",
        kind: "managed",
        state: "running",
        session_id: null,
        provider: "codex",
        task_title: "Ignored control run",
        cwd: "/r/a",
        updated_at: 10,
        expires_at: 9999999999,
      } as unknown as TaskRun,
      {
        id: "run-bidi",
        task_id: "t-bidi",
        kind: "managed",
        state: "running",
        session_id: null,
        provider: "codex",
        task_title: "\u202e\u200b",
        cwd: "/r/a",
        updated_at: 10,
        expires_at: 9999999999,
      } as unknown as TaskRun,
    ];
    const inbox: AgentInboxThread[] = [
      {
        thread_key: "\u0000\u007f",
        title: "Ignored control thread",
        status: "open",
        ask_type: "decide",
        latest_msg: {} as AgentInboxThread["latest_msg"],
        messages: [],
        from_agent: "claude",
        to_agent: "developer",
        updated_at_ms: 400,
      },
      {
        thread_key: "th-bidi",
        title: "Review\u202ePR\u200b",
        status: "open",
        ask_type: "review",
        latest_msg: {} as AgentInboxThread["latest_msg"],
        messages: [],
        from_agent: "claude\u202e",
        to_agent: "developer",
        updated_at_ms: 500,
      },
    ];
    const tabs = [{ path: "/r/a", label: "Repo\u202eName\u200b" }];
    const projection = projectMenuAgentState(
      activity,
      [],
      {
        runs,
        pending: new Map([
          ["\u0000\u001b", { count: 1, more: false }],
          ["run-bidi", { count: 1, more: false }],
        ]),
        clock: 1000,
      },
      inbox,
      tabs,
      false,
      { caseInsensitive: false },
      "/r/a",
    );
    expect(projection.agentsInformational).toBe(0);
    expect(projection.agentRequests.map((r) => r.id)).toEqual([
      "inbox:th-bidi",
      "run:run-bidi",
      "term:s-frac",
    ]);
    const termReq = projection.agentRequests.find((r) => r.id === "term:s-frac")!;
    expect(termReq.atMs).toBe(1_700_000_000_123);
    expect(termReq.agentLabel).toBe("ClaudeCode");
    expect(termReq.reason).toBe("Needs approval");
    expect(termReq.detail).toBeNull();
    expect(termReq.repoLabel).toBe("RepoName");
    const runReq = projection.agentRequests.find((r) => r.id === "run:run-bidi")!;
    expect(runReq.agentLabel).toBe("Codex");
    expect(runReq.detail).toBeNull();
    const inboxReq = projection.agentRequests.find((r) => r.id === "inbox:th-bidi")!;
    expect(inboxReq.agentLabel).toBe("claude");
    expect(inboxReq.detail).toBe("ReviewPR");
  });

  it("bounds the count to a non-negative integer under the ceiling", () => {
    expect(waiting(1)).toBe(1);
    expect(waiting(MENU_LIMITS.agentsWaiting)).toBe(MENU_LIMITS.agentsWaiting);
    expect(waiting(MENU_LIMITS.agentsWaiting + 1)).toBe(MENU_LIMITS.agentsWaiting);
    expect(waiting(1e12)).toBe(MENU_LIMITS.agentsWaiting);
    expect(waiting(-4)).toBe(0);
    expect(waiting(2.9)).toBe(2);
    expect(waiting(Number.NaN)).toBe(0);
    expect(waiting(Number.POSITIVE_INFINITY)).toBe(0);
  });

  it("excludes exited, error, and non-agent (browser/file/shell) terminal records from agentsRunning", () => {
    const records: TerminalSessionRecord[] = [
      { key: "k1", repoPath: "/r/a", label: "Claude", launcher: "claude", status: "running", sessionId: "s1", close: async () => {} },
      { key: "k2", repoPath: "/r/a", label: "Claude", launcher: "claude", status: "exited", sessionId: "s2", close: async () => {} },
      { key: "k3", repoPath: "/r/a", label: "Codex", launcher: "codex", status: "error", sessionId: "s3", close: async () => {} },
      { key: "k4", repoPath: "/r/a", label: "Browser", launcher: "browser" as never, status: "running", sessionId: "s4", close: async () => {} },
      { key: "k5", repoPath: "/r/a", label: "File", launcher: "file" as never, status: "running", sessionId: "s5", close: async () => {} },
      { key: "k6", repoPath: "/r/a", label: "Shell", launcher: "shell", status: "running", sessionId: "s6", close: async () => {} },
    ];
    const projection = projectMenuAgentState(
      new Map(),
      records,
      { runs: [], pending: new Map(), clock: 1000 },
      [],
      [{ path: "/r/a", label: "a" }],
      false,
      { caseInsensitive: false },
      "/r/a",
    );
    expect(projection.agentsRunning).toBe(1);
  });

  it("attributes background open-repo AgentInbox threads to their originating repo instead of activeRepoPath", () => {
    const tabs = [
      { path: "/r/a", label: "a" },
      { path: "/r/b", label: "b" },
    ];
    const projection = projectMenuAgentState(
      new Map(),
      [],
      { runs: [], pending: new Map(), clock: 1000 },
      [
        {
          thread_key: "ask-bg",
          ask_type: "decide",
          title: "Choose migration path",
          status: "open",
          to_agent: "human",
          from_agent: "claude-b",
          updated_at_ms: 900,
          repo_path: "/r/b",
        },
      ],
      tabs,
      false,
      { caseInsensitive: false },
      "/r/a",
    );
    expect(projection.agentRequests[0]?.repoPath).toBe("/r/b");
    expect(projection.agentRequests[0]?.repoLabel).toBe("b");
  });
});

describe("repository-tab digit items", () => {
  const tab = (n: number) => ({
    id: String(n), path: `/r/${n}`, name: `r${n}`, label: `r${n}`, pinned: false, isActive: n === 1,
    isBare: false, isDirty: false, isLoading: false, error: null, currentBranch: "main",
    conflictedCount: 0, changedCount: 0,
  });
  const withTabs = (count: number) => ({
    ...loaded(),
    openTabs: Array.from({ length: count }, (_, i) => tab(i + 1)),
  });
  /** The projection for `count` open tabs drawn as `stops` stops on the strip. */
  const withStops = (count: number, stops: number) =>
    buildMenuState(withTabs(count), prefs(), "system", {}, false, undefined, undefined, 0, stops);

  it("enables digit n only when an nth stop is drawn, and the ninth for any stop", () => {
    const three = withStops(3, 3);
    for (let digit = 1; digit <= 3; digit += 1) expect(menuActionEnabled(three, `repo-tab-${digit}`), `digit ${digit}`).toBe(true);
    for (let digit = 4; digit <= 8; digit += 1) expect(menuActionEnabled(three, `repo-tab-${digit}`), `digit ${digit}`).toBe(false);
    // Nine selects the LAST stop, so it needs one stop, not nine.
    expect(menuActionEnabled(three, "repo-tab-9")).toBe(true);
    const none = withStops(0, 0);
    for (let digit = 1; digit <= 9; digit += 1) expect(menuActionEnabled(none, `repo-tab-${digit}`)).toBe(false);
    const twelve = withStops(12, 12);
    for (let digit = 1; digit <= 9; digit += 1) expect(menuActionEnabled(twelve, `repo-tab-${digit}`)).toBe(true);
  });

  it("counts stops, not open tabs: three tabs folding to two stops leave digit 3 off", () => {
    const folded = withStops(3, 2);
    expect(menuActionEnabled(folded, "repo-tab-1")).toBe(true);
    expect(menuActionEnabled(folded, "repo-tab-2")).toBe(true);
    expect(menuActionEnabled(folded, "repo-tab-3")).toBe(false);
    expect(menuActionEnabled(folded, "repo-tab-9")).toBe(true);
  });

  it("disables every digit when the stop count is not given, rather than guessing", () => {
    const unknown = model(withTabs(5));
    for (let digit = 1; digit <= 9; digit += 1) expect(menuActionEnabled(unknown, `repo-tab-${digit}`)).toBe(false);
  });

  it("is held while a prompt is open and runs in any repository, like next and previous tab", () => {
    for (let digit = 1; digit <= 9; digit += 1) {
      expect(blockedWhilePrompting(`repo-tab-${digit}`)).toBe(true);
    }
    const repo = withTabs(3);
    const state = withStops(3, 3);
    // Workspace-wide: the payload's repository need not be the active one.
    expect(canDispatchMenuEvent({ id: "repo-tab-2", repo_path: "/elsewhere" }, state, repo)).toBe(true);
    expect(canDispatchMenuEvent({ id: "repo-tab-5", repo_path: "/r/1" }, state, repo)).toBe(false);
  });
});

describe("blockedWhilePrompting", () => {
  it.each(["activate-repo", "open", "open-recent", "open-repo", "close-tab", "next-repo-tab", "prev-repo-tab", "reopen-repo-tab", "clone", "create-branch", "rename-branch", "quick-commit", "stash"])(
    "blocks %s while a prompt is open",
    (id) => expect(blockedWhilePrompting(id)).toBe(true),
  );

  it.each(["stage-all", "zoom-in", "check-updates", "fleet"])(
    "leaves %s alone",
    (id) => expect(blockedWhilePrompting(id)).toBe(false),
  );
});
