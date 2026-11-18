import { describe, expect, it } from "vitest";
import type { TerminalContext } from "./sessionContext";
import { activityPhrase, terminalPillView, type PillSession } from "./statusPill";

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

const busy: TerminalContext = {
  process: "bun",
  busy: true,
  jobs: true,
  cwd: "/repo/app",
  repo_dir: "",
};

describe("terminal pill copy", () => {
  it("says nothing about a command when the dock is closed and no session is published", () => {
    const view = terminalPillView([], { open: false, repoPath: "/repo/app", identity });
    expect(view.summary).toBeNull();
    expect(view.detail).toBe("No shell · dock closed");
    expect(view.alive).toBe(0);
    expect(view.focusKey).toBeNull();
  });

  it("says No shell when the dock is open and the host has published nothing", () => {
    const view = terminalPillView([], { open: true, repoPath: "/repo/app", identity });
    expect(view.summary).toBe("No shell");
    expect(view.detail).toBe("No shell");
    expect(view.summary).not.toMatch(/bun|npm|zsh|Idle|Running/);
  });

  it("does not treat a live PTY as a running command or as idle", () => {
    expect(activityPhrase("running", null)).toBeNull();
    const view = terminalPillView([shell()], { open: false, repoPath: "/repo/app", identity });
    expect(view.summary).toBe("Shell");
    expect(view.detail).toBe("Shell");
    expect(view.summary).not.toMatch(/bun|npm|Idle|Running/);
    expect(view.sessions[0]?.cwd).toBeNull();
  });

  it("says Idle only when the host proved the shell idle, and names a program only when the host said it is busy", () => {
    expect(activityPhrase("running", idle)).toBe("Idle");
    expect(activityPhrase("running", busy)).toBe("bun");
    expect(activityPhrase("running", { ...busy, process: null })).toBe("Busy");
    expect(activityPhrase("running", { ...busy, process: "   " })).toBe("Busy");
    const idling = terminalPillView([shell()], {
      open: true,
      repoPath: "/repo/app",
      identity,
      activity: { a: idle },
    });
    expect(idling.summary).toBe("Shell · Idle");
    expect(idling.sessions[0]?.cwd).toBe("/repo/app/src");
    expect(idling.sessions[0]?.where).toBe("src");
    const running = terminalPillView([shell()], {
      open: true,
      repoPath: "/repo/app",
      identity,
      activity: { a: busy },
    });
    expect(running.summary).toBe("Shell · bun");
  });

  it("prefers the session title and a non-running lifecycle status the registry published", () => {
    const view = terminalPillView(
      [shell({ title: "Fix importer", status: "starting", checkout: "/repo/app/.claude/worktrees/fix" })],
      { open: true, repoPath: "/repo/app", identity },
    );
    expect(view.summary).toBe("Fix importer · Starting");
    expect(view.sessions[0]?.cwd).toBe("/repo/app/.claude/worktrees/fix");
    expect(view.sessions[0]?.where).toBe("fix");
    expect(view.focusKey).toBe("a");
  });

  it("lists every session when several are alive, current repository first, and focuses only a single revealable one", () => {
    const here = shell({ key: "here", label: "Shell" });
    const elsewhere = shell({ key: "else", repoPath: "/repo/other", label: "Claude", title: "Review" });
    const hidden = shell({ key: "hidden", canReveal: false });
    const many = terminalPillView([elsewhere, here], { open: true, repoPath: "/repo/app", identity });
    expect(many.alive).toBe(2);
    expect(many.summary).toBeNull();
    expect(many.detail).toBe("2 sessions");
    expect(many.focusKey).toBeNull();
    expect(many.sessions.map((session) => session.key)).toEqual(["here", "else"]);
    expect(many.sessions[1]?.name).toBe("Review");
    const one = terminalPillView([hidden], { open: true, repoPath: "/repo/app", identity });
    expect(one.focusKey).toBeNull();
    expect(one.alive).toBe(1);
  });

  it("highlights the active or urgent session when several are alive and reports non-PTY open dock tabs", () => {
    const activeClaude = shell({
      key: "c1",
      label: "Claude",
      title: "Auditing split header",
      isActive: true,
      splitSlot: 1,
    });
    const peerCodex = shell({
      key: "c2",
      label: "Codex",
      title: "Writing unit tests",
      splitSlot: 2,
    });
    const activeView = terminalPillView([peerCodex, activeClaude], {
      open: true,
      repoPath: "/repo/app",
      identity,
    });
    expect(activeView.alive).toBe(2);
    expect(activeView.summary).toBe("Auditing split header");
    expect(activeView.detail).toBe("2 sessions");

    // Urgent attention beats isActive across repositories
    const blockedElsewhere = shell({
      key: "c3",
      repoPath: "/repo/other",
      label: "Claude",
      title: "Deploy check",
      attention: "Needs input",
    });
    const urgentView = terminalPillView([activeClaude, blockedElsewhere], {
      open: true,
      repoPath: "/repo/app",
      identity,
    });
    expect(urgentView.summary).toBe("Deploy check · Needs input");
    expect(urgentView.detail).toBe("2 sessions · Deploy check · Needs input");

    // Non-PTY open tabs (Browser/File) when alive === 0
    const browserDockOpen = terminalPillView([], {
      open: true,
      repoPath: "/repo/app",
      identity,
      openTabs: 2,
    });
    expect(browserDockOpen.summary).toBe("2 tabs");
    expect(browserDockOpen.detail).toBe("2 tabs");
    const browserDockClosed = terminalPillView([], {
      open: false,
      repoPath: "/repo/app",
      identity,
      openTabs: 1,
    });
    expect(browserDockClosed.summary).toBeNull();
    expect(browserDockClosed.detail).toBe("1 tab · dock closed");
  });

  it("enriches every session view with repository name, worktree checkout, launcher kind, agent label, and distinct displayTitle", () => {
    const claudeInWorktree = shell({
      key: "wt-1",
      repoPath: "/repo/GitPulse",
      checkout: "/repo/GitPulse/.claude/worktrees/fix-sidebar",
      label: "Claude",
      launcher: "claude",
      title: "Improve sidebar navigation",
      isActive: true,
      splitSlot: 1,
    });
    const plainClaudeElsewhere = shell({
      key: "other-1",
      repoPath: "/repo/DevCouncil",
      label: "Claude",
      launcher: "claude",
    });

    const view = terminalPillView([plainClaudeElsewhere, claudeInWorktree], {
      open: true,
      repoPath: "/repo/GitPulse",
      identity,
      activity: {
        "wt-1": { process: "claude", busy: false, jobs: true, cwd: "/repo/GitPulse/.claude/worktrees/fix-sidebar", repo_dir: "" },
      },
      checkouts: [
        { path: "/repo/GitPulse", label: "GitPulse" },
        { path: "/repo/DevCouncil", label: "DevCouncil" },
      ],
    });

    expect(view.sessions).toHaveLength(2);
    expect(view.sessions[0]).toMatchObject({
      key: "wt-1",
      repository: "GitPulse",
      checkoutName: "fix-sidebar",
      launcherKind: "claude",
      agentLabel: "Claude",
      displayTitle: "Improve sidebar navigation",
      isCurrentRepo: true,
      phrase: null,
    });
    expect(view.sessions[1]).toMatchObject({
      key: "other-1",
      repository: "DevCouncil",
      checkoutName: null,
      launcherKind: "claude",
      agentLabel: "Claude",
      displayTitle: null,
      isCurrentRepo: false,
    });
  });

  it("never mislabels agent CLIs as Unix shell Background job or Idle, and collapses generic default OSC titles", () => {
    const ctx = (process: string, busyFlag: boolean, jobsFlag: boolean): TerminalContext => ({
      process,
      busy: busyFlag,
      jobs: jobsFlag,
      cwd: null,
      repo_dir: null,
    });
    // Direct activityPhrase calls: agent CLIs with MCP/helper child processes (jobs: true) or no children (jobs: false)
    for (const kind of ["claude", "codex", "manvi", "grok", "agy"] as const) {
      expect(activityPhrase("running", ctx(kind, false, true), kind)).toBeNull();
      expect(activityPhrase("running", ctx(kind, false, false), kind)).toBeNull();
      expect(activityPhrase("running", ctx("node", false, true), kind)).toBeNull();
    }
    expect(activityPhrase("running", ctx("claude", false, true))).toBeNull();
    expect(activityPhrase("running", ctx("claude", false, false))).toBeNull();
    // Genuine Unix shell background job and foreground subcommands remain distinct
    expect(activityPhrase("running", ctx("zsh", false, true), "shell")).toBe("Background job");
    expect(activityPhrase("running", ctx("claude", true, true), "shell")).toBe("claude");
    expect(activityPhrase("running", ctx("cargo", true, true), "claude")).toBe("cargo");

    // Generic startup OSC titles ("✳ Claude Code", "Claude Code", "OpenAI Codex") collapse cleanly
    const defaultClaude = shell({
      key: "gen-claude",
      repoPath: "/repo/GitPulse",
      label: "Claude",
      launcher: "claude",
      title: "✳ Claude Code",
      isActive: true,
    });
    const defaultCodex = shell({
      key: "gen-codex",
      repoPath: "/repo/GitPulse",
      label: "Codex",
      launcher: "codex",
      title: "OpenAI Codex",
    });
    const realClaude = shell({
      key: "real-claude",
      repoPath: "/repo/GitPulse",
      label: "Claude",
      launcher: "claude",
      title: "✳ Improve sidebar UX",
    });
    const pill = terminalPillView([defaultClaude, defaultCodex, realClaude], {
      open: true,
      repoPath: "/repo/GitPulse",
      identity,
      activity: {
        "gen-claude": { process: "claude", busy: false, jobs: true, cwd: "/repo/GitPulse", repo_dir: "" },
      },
    });
    expect(pill.summary).toBe("Claude");
    expect(pill.sessions[0]).toMatchObject({
      name: "Claude",
      displayTitle: null,
      agentLabel: "Claude",
      phrase: null,
    });
    expect(pill.sessions[1]).toMatchObject({
      name: "Codex",
      displayTitle: null,
      agentLabel: "Codex",
    });
    expect(pill.sessions[2]).toMatchObject({
      name: "Improve sidebar UX",
      displayTitle: "Improve sidebar UX",
      agentLabel: "Claude",
    });
  });

  it("resolves sibling worktree checkouts (<repo>.wt-<slug>), strips CLI spinner glyphs, clips emoji safely, and preserves nested repo_dir", () => {
    const siblingWt = shell({
      key: "sib-1",
      repoPath: "/repo/GitPulse",
      checkout: "/repo/GitPulse.wt-sidebar-fix",
      label: "Antigravity",
      launcher: "agy",
      title: `⠋ Ship ${"🚀".repeat(30)}`,
      isActive: true,
    });
    const nestedDirShell = shell({
      key: "nested-1",
      repoPath: "/repo/manvi",
      label: "Shell",
      launcher: "shell",
    });

    const view = terminalPillView([siblingWt, nestedDirShell], {
      open: true,
      repoPath: "/repo/GitPulse",
      identity,
      activity: {
        "nested-1": {
          process: "zsh",
          busy: false,
          jobs: false,
          cwd: "/repo/manvi/cmd/manvi",
          repo_dir: "cmd/manvi",
        },
      },
      checkouts: [
        { path: "/repo/GitPulse", label: "GitPulse" },
        { path: "/repo/manvi", label: "manvi" },
      ],
    });

    expect(view.highlightedLauncher).toBe("agy");
    expect(view.sessions[0]).toMatchObject({
      repository: "GitPulse",
      checkoutName: "wt-sidebar-fix",
      launcherKind: "agy",
      agentLabel: "Antigravity",
    });
    expect(view.sessions[0]?.displayTitle?.startsWith("Ship 🚀")).toBe(true);
    expect(view.sessions[0]?.displayTitle?.isWellFormed()).toBe(true);
    expect(view.sessions[0]?.name.isWellFormed()).toBe(true);
    expect(view.summary?.isWellFormed()).toBe(true);

    // Subdirectory whose leaf matches repository name ("manvi/cmd/manvi") stays visible via repo_dir
    expect(view.sessions[1]?.where).toBe("cmd/manvi");
  });

  it("ranks adopted worktree sessions with the current repository family, marks isCurrentRepo, and resolves logoPath to the parent repo tab", () => {
    const otherRepoSession = shell({
      key: "other-repo",
      repoPath: "/repo/DevCouncil",
      label: "Shell",
      launcher: "shell",
    });
    const adoptedWorktreeSession = shell({
      key: "adopted-wt",
      repoPath: "/repo/GitPulse/.claude/worktrees/sidebar-fix",
      label: "Claude",
      launcher: "claude",
      isActive: true,
    });
    const otherRepoWorktree = shell({
      key: "other-wt",
      repoPath: "/repo/DevCouncil/.claude/worktrees/lane-2",
      label: "Codex",
      launcher: "codex",
    });

    const view = terminalPillView([otherRepoSession, adoptedWorktreeSession, otherRepoWorktree], {
      open: true,
      repoPath: "/repo/GitPulse",
      identity,
      checkouts: [
        { path: "/repo/GitPulse", label: "GitPulse" },
        { path: "/repo/DevCouncil", label: "DevCouncil" },
      ],
    });

    expect(view.sessions.map((s) => s.key)).toEqual(["adopted-wt", "other-repo", "other-wt"]);
    expect(view.sessions[0]).toMatchObject({
      key: "adopted-wt",
      repository: "GitPulse",
      checkoutName: "sidebar-fix",
      isCurrentRepo: true,
      logoPath: "/repo/GitPulse",
    });
    expect(view.sessions[2]).toMatchObject({
      key: "other-wt",
      repository: "DevCouncil",
      checkoutName: "lane-2",
      isCurrentRepo: false,
      logoPath: "/repo/DevCouncil",
    });
    expect(view.summary).toBe("Claude");
  });

  it("says where a session waits in the build queue, and says nothing when the queue was not read", () => {
    const waiter = (terminal_session: string) =>
      ({
        name: terminal_session,
        record: { terminal_session } as never,
      }) as never;
    const queued = shell({ sessionId: "term-b" });
    const queue = [waiter("term-a"), waiter("term-b")];
    const view = terminalPillView([queued], { open: true, repoPath: "/repo/app", identity, buildQueue: queue });
    expect(view.sessions[0].phrase).toBe("Waiting for a build slot (2nd)");
    expect(view.summary).toContain("Waiting for a build slot (2nd)");
    const unread = terminalPillView([queued], { open: true, repoPath: "/repo/app", identity });
    expect(unread.sessions[0].phrase).toBeNull();
    const other = terminalPillView([shell({ sessionId: "term-z" })], { open: true, repoPath: "/repo/app", identity, buildQueue: queue });
    expect(other.sessions[0].phrase).toBeNull();
  });
});
