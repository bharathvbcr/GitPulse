import { describe, expect, it } from "vitest";
import type { TerminalContext } from "./sessionContext";
import { terminalPillView, type PillSession } from "./statusPill";
import {
  filterPillSessions,
  highlightSearchText,
  MAX_PILL_QUICK_LAUNCHES,
  pillActionId,
  pillFilterCounts,
  pillSearchActions,
  resolvePillActionIndex,
  searchTerminalPill,
} from "./pillSearch";

const identity = { caseInsensitive: false };

function shell(overrides: Partial<PillSession> = {}): PillSession {
  return {
    key: "a",
    repoPath: "/repo/app",
    label: "Shell",
    status: "running",
    canReveal: true,
    ...overrides,
  };
}

const idle: TerminalContext = {
  process: "zsh",
  busy: false,
  jobs: false,
  cwd: "/repo/app/src",
  repo_dir: "src",
};

describe("terminal pill search & quick launch", () => {
  const openTabs = [
    { path: "/repo/GitPulse", label: "GitPulse", currentBranch: "main", group: "devtools" },
    { path: "/repo/DevCouncil", label: "DevCouncil", currentBranch: "feat/rust-parser", group: "devtools" },
    { path: "/repo/Manvi", label: "Manvi", currentBranch: "main", group: "devtools" },
    { path: "/repo/Jarvis", label: "Jarvis", currentBranch: "main", group: "apps" },
    {
      path: "/repo/GitPulse/.claude/worktrees/fix-sidebar",
      label: "fix-sidebar",
      familyRoot: "/repo/GitPulse",
      currentBranch: "fix/sidebar",
      group: "devtools",
    },
  ];

  function pillSessionsFixture() {
    return terminalPillView(
      [
        shell({
          key: "gp-claude",
          repoPath: "/repo/GitPulse",
          checkout: "/repo/GitPulse/.claude/worktrees/fix-sidebar",
          label: "Claude",
          launcher: "claude",
          title: "Improve sidebar navigation",
          isActive: true,
          splitSlot: 1,
        }),
        shell({
          key: "dc-claude",
          repoPath: "/repo/DevCouncil",
          label: "Claude",
          launcher: "claude",
          title: "Fix Rust parser",
          attention: "Needs input",
        }),
        shell({
          key: "mv-codex",
          repoPath: "/repo/Manvi",
          label: "Codex",
          launcher: "codex",
          title: "Add unit tests",
        }),
        shell({
          key: "gp-shell",
          repoPath: "/repo/GitPulse",
          label: "Shell",
          launcher: "shell",
        }),
        shell({
          key: "dc-agy",
          repoPath: "/repo/DevCouncil",
          label: "Antigravity",
          launcher: "agy",
          title: "Audit hooks",
        }),
      ],
      {
        open: true,
        repoPath: "/repo/GitPulse",
        identity,
        checkouts: openTabs,
        activity: { "gp-shell": idle },
      },
    ).sessions;
  }

  it("keeps the full session list and emits no quick launches when the query is empty or whitespace", () => {
    const sessions = pillSessionsFixture();
    for (const blank of ["", "   ", "\u0000\t", null, undefined]) {
      const result = searchTerminalPill({
        sessions,
        query: blank,
        openTabs,
        currentRepoPath: "/repo/GitPulse",
        defaultLauncher: "claude",
        identity,
      });
      expect(result.query).toBe("");
      expect(result.sessions.map((s) => s.key)).toEqual(sessions.map((s) => s.key));
      expect(result.launches).toEqual([]);
      expect(result.totalLaunches).toBe(0);
      expect(result.hiddenLaunches).toBe(0);
    }
  });

  it("finds running agents across repos by agent kind, repository, worktree, title, attention, or multi-token query", () => {
    const sessions = pillSessionsFixture();
    const base = {
      sessions,
      openTabs,
      currentRepoPath: "/repo/GitPulse",
      defaultLauncher: "claude" as const,
      identity,
    };

    expect(searchTerminalPill({ ...base, query: "devcouncil claude" }).sessions.map((s) => s.key)).toEqual([
      "dc-claude",
    ]);
    expect(searchTerminalPill({ ...base, query: "claude devcouncil" }).sessions.map((s) => s.key)).toEqual([
      "dc-claude",
    ]);
    expect(searchTerminalPill({ ...base, query: "agy" }).sessions.map((s) => s.key)).toEqual([
      "dc-agy",
    ]);
    expect(searchTerminalPill({ ...base, query: "needs input" }).sessions.map((s) => s.key)).toEqual([
      "dc-claude",
    ]);
    expect(searchTerminalPill({ ...base, query: "sidebar" }).sessions.map((s) => s.key)).toEqual([
      "gp-claude",
    ]);
    expect(searchTerminalPill({ ...base, query: "#1" }).sessions.map((s) => s.key)).toEqual([
      "gp-claude",
    ]);
    expect(searchTerminalPill({ ...base, query: "active" }).sessions.map((s) => s.key)).toEqual([
      "gp-claude",
    ]);
    expect(searchTerminalPill({ ...base, query: "idle" }).sessions.map((s) => s.key)).toEqual([
      "gp-shell",
    ]);
  });

  it("ranks prefix agent matches above loose subsequence matches and avoids false DevCouncil launch on 'cl'", () => {
    const sessions = pillSessionsFixture();
    const result = searchTerminalPill({
      sessions,
      query: "cl",
      openTabs,
      currentRepoPath: "/repo/GitPulse",
      defaultLauncher: "shell",
      identity,
    });
    expect(result.sessions.map((s) => s.key)).toEqual(["gp-claude", "dc-claude"]);
    expect(result.launches.map((l) => `${l.launcher}@${l.repository}`)).toEqual(["claude@GitPulse"]);
    expect(result.launches[0]?.label).toBe("New Claude session");
  });

  it("never matches hidden .claude worktree path segments as a repository or session match for 'claude'", () => {
    const codexInClaudeWorktree = terminalPillView(
      [
        shell({
          key: "codex-wt",
          repoPath: "/repo/GitPulse",
          checkout: "/repo/GitPulse/.claude/worktrees/fix-sidebar",
          label: "Codex",
          launcher: "codex",
          title: "Refactor tests",
        }),
      ],
      { open: true, repoPath: "/repo/GitPulse", identity, checkouts: openTabs },
    ).sessions;
    const result = searchTerminalPill({
      sessions: codexInClaudeWorktree,
      query: "claude",
      openTabs,
      currentRepoPath: "/repo/GitPulse",
      defaultLauncher: "shell",
      identity,
    });
    expect(result.sessions).toEqual([]);
    expect(result.launches).toHaveLength(1);
    expect(result.launches[0]).toMatchObject({
      repoPath: "/repo/GitPulse",
      launcher: "claude",
      label: "New Claude session",
    });
  });

  it("supports quick-launching any launcher in any open repo in either word order and via camelCase initials", () => {
    const sessions = pillSessionsFixture();
    const base = {
      sessions,
      openTabs,
      currentRepoPath: "/repo/GitPulse",
      defaultLauncher: "claude" as const,
      identity,
    };

    const devCodex = searchTerminalPill({ ...base, query: "devcouncil codex" });
    expect(devCodex.sessions).toEqual([]);
    expect(devCodex.launches[0]).toMatchObject({
      repoPath: "/repo/DevCouncil",
      repository: "DevCouncil",
      launcher: "codex",
      label: "New Codex in DevCouncil",
      isCurrentRepo: false,
    });

    const codexDev = searchTerminalPill({ ...base, query: "codex devcouncil" });
    expect(codexDev.launches[0]).toMatchObject({
      repoPath: "/repo/DevCouncil",
      launcher: "codex",
      label: "New Codex in DevCouncil",
    });

    const initials = searchTerminalPill({ ...base, query: "dc agy" });
    expect(initials.launches[0]).toMatchObject({
      repoPath: "/repo/DevCouncil",
      launcher: "agy",
      label: "New Antigravity in DevCouncil",
    });

    const repoOnly = searchTerminalPill({ ...base, query: "jarvis" });
    expect(repoOnly.sessions).toEqual([]);
    expect(repoOnly.launches).toHaveLength(1);
    expect(repoOnly.launches[0]).toMatchObject({
      repoPath: "/repo/Jarvis",
      launcher: "claude",
      label: "New Claude in Jarvis",
    });

    // Ambiguous name ("manvi" is both a launcher and an open repo): offers both distinct launches
    // and never falsely matches scattered letters in unrelated titles like "Improve sidebar navigation".
    const manvi = searchTerminalPill({ ...base, query: "manvi" });
    expect(manvi.sessions.map((s) => s.key)).toEqual(["mv-codex"]);
    expect(manvi.launches.map((l) => l.label)).toEqual([
      "New Manvi session",
      "New Claude in Manvi",
    ]);

    // Deduplicates when current repo and default launcher collide ("manvi" in Manvi with default Manvi).
    const deduped = searchTerminalPill({
      sessions: [],
      query: "manvi",
      openTabs,
      currentRepoPath: "/repo/Manvi",
      defaultLauncher: "manvi",
      identity,
    });
    expect(deduped.launches).toHaveLength(1);
    expect(deduped.launches[0]?.label).toBe("New Manvi session");
  });

  it("supports launch verbs ('new', 'new grok', 'launch jarvis') and browser capability gating", () => {
    const base = {
      sessions: [],
      openTabs,
      currentRepoPath: "/repo/GitPulse",
      defaultLauncher: "claude" as const,
      identity,
    };

    const allNew = searchTerminalPill({ ...base, query: "new", browserSessions: false });
    expect(allNew.launches[0]?.launcher).toBe("claude");
    expect(allNew.launches.map((l) => l.launcher)).not.toContain("browser");
    expect(allNew.launches).toHaveLength(6);

    const withBrowser = searchTerminalPill({ ...base, query: "browser", browserSessions: true });
    expect(withBrowser.launches[0]?.launcher).toBe("browser");
    const withoutBrowser = searchTerminalPill({ ...base, query: "browser", browserSessions: false });
    expect(withoutBrowser.launches).toEqual([]);

    const newGrok = searchTerminalPill({ ...base, query: "new grok" });
    expect(newGrok.launches[0]).toMatchObject({ launcher: "grok", label: "New Grok session" });

    const launchJarvis = searchTerminalPill({ ...base, query: "launch jarvis" });
    expect(launchJarvis.launches[0]).toMatchObject({
      repoPath: "/repo/Jarvis",
      launcher: "claude",
      label: "New Claude in Jarvis",
    });
  });

  it("caps quick-launch results honestly with totalLaunches and hiddenLaunches, and skips unrevealable sessions in keyboard actions", () => {
    const manyTabs = Array.from({ length: MAX_PILL_QUICK_LAUNCHES + 4 }, (_, i) => ({
      path: `/repo/service-${i + 1}`,
      label: `service-${i + 1}`,
    }));
    const sessions = terminalPillView(
      [
        shell({ key: "dead-1", repoPath: "/repo/service-1", label: "Claude", launcher: "claude", canReveal: false }),
        shell({ key: "live-1", repoPath: "/repo/service-1", label: "Claude", launcher: "claude", canReveal: true }),
      ],
      { open: true, repoPath: "/repo/service-1", identity, checkouts: manyTabs },
    ).sessions;

    const result = searchTerminalPill({
      sessions,
      query: "service",
      openTabs: manyTabs,
      currentRepoPath: "/repo/service-1",
      defaultLauncher: "shell",
      identity,
    });
    expect(result.launches).toHaveLength(MAX_PILL_QUICK_LAUNCHES);
    expect(result.totalLaunches).toBe(MAX_PILL_QUICK_LAUNCHES + 4);
    expect(result.hiddenLaunches).toBe(4);

    const actions = pillSearchActions(result);
    expect(actions[0]).toMatchObject({ kind: "session", key: "live-1", optionId: "terminal-pill-opt-0" });
    expect(actions.some((a) => a.key === "dead-1")).toBe(false);
    expect(actions[1]).toMatchObject({ kind: "launch", optionId: "terminal-pill-opt-1" });
    expect(actions).toHaveLength(1 + MAX_PILL_QUICK_LAUNCHES);
  });

  it("fails closed on hostile or missing inputs without throwing", () => {
    const result = searchTerminalPill({
      sessions: null as never,
      query: "claude",
      openTabs: null as never,
      currentRepoPath: null,
      defaultLauncher: "not-a-launcher",
      identity,
    });
    expect(result).toEqual({
      query: "claude",
      sessions: [],
      launches: [],
      totalLaunches: 0,
      hiddenLaunches: 0,
    });
  });

  it("computes smart filter chip counts and composes session filters with text search", () => {
    const sessions = pillSessionsFixture();
    const counts = pillFilterCounts(sessions);
    expect(counts).toEqual({
      total: 5,
      agents: 4,
      attention: 1,
      current: 2,
      showBar: true,
    });

    // Single-repo uniform shells with no attention need no filter chip bar.
    const uniformShells = terminalPillView(
      [
        shell({ key: "s1", repoPath: "/repo/GitPulse", label: "Shell", launcher: "shell" }),
        shell({ key: "s2", repoPath: "/repo/GitPulse", label: "Shell", launcher: "shell" }),
      ],
      { open: true, repoPath: "/repo/GitPulse", identity, checkouts: openTabs },
    ).sessions;
    expect(pillFilterCounts(uniformShells).showBar).toBe(false);

    expect(filterPillSessions(sessions, "agents").map((s) => s.key)).toEqual([
      "gp-claude",
      "dc-claude",
      "mv-codex",
      "dc-agy",
    ]);
    expect(filterPillSessions(sessions, "attention").map((s) => s.key)).toEqual(["dc-claude"]);
    expect(filterPillSessions(sessions, "current").map((s) => s.key)).toEqual([
      "gp-claude",
      "gp-shell",
    ]);

    // Filter composes with search query cleanly.
    const currentAgents = searchTerminalPill({
      sessions,
      query: "claude",
      filter: "current",
      openTabs,
      currentRepoPath: "/repo/GitPulse",
      defaultLauncher: "claude",
      identity,
    });
    expect(currentAgents.sessions.map((s) => s.key)).toEqual(["gp-claude"]);
  });

  it("matches workspace tab groups across sessions and quick launches, and includes recent repositories", () => {
    const sessions = pillSessionsFixture();
    const recentRepos = ["/repo/GoSree", "/repo/DevCouncil", "/repo/MarkDev"];

    // Workspace tab group "devtools" matches live sessions and open repos in that group.
    const devtoolsClaude = searchTerminalPill({
      sessions,
      query: "devtools claude",
      openTabs,
      recentRepos,
      currentRepoPath: "/repo/GitPulse",
      defaultLauncher: "shell",
      identity,
    });
    expect(devtoolsClaude.sessions.map((s) => s.key)).toEqual(["gp-claude", "dc-claude"]);
    expect(devtoolsClaude.launches.some((l) => l.repository === "DevCouncil" && l.launcher === "claude")).toBe(true);
    expect(devtoolsClaude.launches.some((l) => l.repository === "Jarvis")).toBe(false);

    // Recent closed repository ("GoSree") is launchable directly and tagged source: "recent".
    const recentHit = searchTerminalPill({
      sessions,
      query: "gosree codex",
      openTabs,
      recentRepos,
      currentRepoPath: "/repo/GitPulse",
      defaultLauncher: "claude",
      identity,
    });
    expect(recentHit.sessions).toEqual([]);
    expect(recentHit.launches).toHaveLength(1);
    expect(recentHit.launches[0]).toMatchObject({
      repoPath: "/repo/GoSree",
      repository: "GoSree",
      launcher: "codex",
      source: "recent",
      label: "New Codex in GoSree",
    });

    // Open tab "/repo/DevCouncil" deduplicates against the identical recent path and stays source: "open".
    const openWins = searchTerminalPill({
      sessions: [],
      query: "devcouncil",
      openTabs,
      recentRepos,
      currentRepoPath: "/repo/GitPulse",
      defaultLauncher: "claude",
      identity,
    });
    expect(openWins.launches).toHaveLength(1);
    expect(openWins.launches[0]).toMatchObject({
      repoPath: "/repo/DevCouncil",
      source: "open",
    });
  });

  it("preserves keyboard selection by stable action id across live session churn", () => {
    const sessions = pillSessionsFixture();
    const first = searchTerminalPill({
      sessions,
      query: "claude",
      openTabs,
      currentRepoPath: "/repo/GitPulse",
      defaultLauncher: "claude",
      identity,
    });
    const actionsBefore = pillSearchActions(first);
    const chosenId = pillActionId(actionsBefore[1]); // dc-claude
    expect(resolvePillActionIndex(actionsBefore, chosenId)).toBe(1);

    // Simulate gp-claude exiting while menu is open: selection stays locked on dc-claude (now index 0).
    const afterExit = searchTerminalPill({
      sessions: sessions.filter((s) => s.key !== "gp-claude"),
      query: "claude",
      openTabs,
      currentRepoPath: "/repo/GitPulse",
      defaultLauncher: "claude",
      identity,
    });
    const actionsAfter = pillSearchActions(afterExit);
    expect(resolvePillActionIndex(actionsAfter, chosenId)).toBe(0);
    expect(actionsAfter[0]?.key).toBe("dc-claude");
    expect(resolvePillActionIndex(actionsAfter, "session:gone")).toBe(0);
    expect(resolvePillActionIndex([], chosenId)).toBe(-1);
  });

  it("highlights multi-token substring and camelCase initial matches safely without regex", () => {
    expect(highlightSearchText("Improve sidebar navigation", "side nav")).toEqual([
      { text: "Improve ", matched: false },
      { text: "side", matched: true },
      { text: "bar ", matched: false },
      { text: "nav", matched: true },
      { text: "igation", matched: false },
    ]);
    expect(highlightSearchText("DevCouncil", "dc")).toEqual([
      { text: "D", matched: true },
      { text: "ev", matched: false },
      { text: "C", matched: true },
      { text: "ouncil", matched: false },
    ]);
    expect(highlightSearchText("New Claude in DevCouncil", "dev claude")).toEqual([
      { text: "New ", matched: false },
      { text: "Claude", matched: true },
      { text: " in ", matched: false },
      { text: "Dev", matched: true },
      { text: "Council", matched: false },
    ]);
    expect(highlightSearchText("Shell", "")).toEqual([{ text: "Shell", matched: false }]);
    expect(highlightSearchText("İstanbul", "is")).toEqual([{ text: "İstanbul", matched: false }]);
    expect(highlightSearchText("", "claude")).toEqual([]);
  });
});
