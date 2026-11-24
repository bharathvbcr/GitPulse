import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it, vi } from "vitest";
import { render } from "svelte/server";
import { get } from "svelte/store";
import { agentDirectories } from "../agents/cwd";
import { agentPlaneStore } from "../agents/store";
import * as ipc from "../ipc/invoke";
import { interfaceStore } from "../stores/interfaceStore";
import { ledgerStatus } from "../ledger/types";
import { sessionActivity, setSignalledAsksForReader } from "../terminal/sessionActivity";
import { terminalSessions } from "../terminal/sessionRegistry";
import { requestInboxFocus, takeInboxFocus } from "../agents/inboxFocus";
import { agentsRepositoryScope, setAgentsRepositoryScope } from "../agents/scope";
import AgentInbox from "./AgentInbox.svelte";
import AgentsView from "./AgentsView.svelte";

const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "AgentsView.svelte"), "utf8");

describe("AgentsView", () => {
  it("does not call an unread task board an empty workspace", () => {
    const html = render(AgentsView).body;
    expect(html).toContain('data-testid="agents-view"');
    expect(html).toContain("Task runs have not been read yet");
    expect(html).toContain("Still reading");
    expect(html).not.toContain("No agent sessions");
    expect(html).not.toContain("couldn't be read");
  });

  it("keeps gaps on the page when the attention column is hidden", () => {
    const gaps = source.indexOf('data-testid="agents-gaps"');
    const table = source.indexOf("<table");
    expect(gaps).toBeGreaterThan(-1);
    expect(table).toBeGreaterThan(gaps);
    expect(source).toContain("Sessions couldn't be read");
    expect(source).toContain("Still reading");
  });

  it("shows a terminal through the focus owner, in the tab that hosts it, not by revealing it by hand", () => {
    // focusTerminalSession owns surface → hosting tab → dock → reveal. A
    // hand-rolled reveal after opening the row's directory skipped the dock
    // and opened the cwd, which is not the tab the terminal lives in.
    const show = source.slice(source.indexOf("async function showTerminal"), source.indexOf("async function openTask"));
    expect(show).toContain("focusTerminalSession(record)");
    expect(show).toContain("showTaskTerminal(run)");
    expect(show).not.toContain("onReady");
    expect(show).not.toContain("openRepo");
    expect(source).not.toContain("reveal?.()");
    expect(source).not.toContain("reveal()");
    // Starting an agent in a checkout is a launch, not a reveal: like the
    // Worktrees fork it waits for the opened tab's canonical path (`onReady`)
    // and hands the launch to that tab's dock.
    const start = source.slice(source.indexOf("async function startIn"), source.indexOf("async function reviewChanges"));
    expect(start).toContain("onReady");
    expect(start).toContain("terminalLaunchRequests.request(at, launcher)");
  });

  it("gives each row explicit, labelled actions", () => {
    // The actions and their labels are decided in agents/rowActions.ts
    // ("Show terminal for …", "Open task for …"; pinned in rowActions.test.ts);
    // the view renders each with its own label and routes it to its handler.
    expect(source).toContain("agentRowActions(row, ");
    expect(source).toContain("aria-label={action.ariaLabel}");
    expect(source).toContain("onclick={() => runAction(action)}");
    expect(source).toMatch(/case "terminal":\s*return void showTerminal\(row\);/);
    expect(source).toMatch(/case "task":\s*return void openTask\(row\);/);
    expect(source).toContain("openTaskForRun(row.taskRunId)");
    expect(source).toContain("repoStore.openRepo(row.checkoutPath)");
    expect(source).toContain("agentKindLabel(row.kind)");
  });

  it("watches task attempts only while this surface is showing", () => {
    expect(source).toContain("createBoardAgents");
    expect(source).toContain("board.start()");
    expect(source).toContain("board.stop()");
    expect(source).toContain('globalSurface === "agents"');
  });

  it("uses the workspace plate so native text keeps its color", () => {
    expect(source).toContain("gp-workspace");
    expect(source).toContain("bg-background");
  });

  it("takes directories from the sweep the tab chip also reads, and does not invent one", () => {
    // The mapping lives in agents/repoAttention.ts (`planeTerminals`), which
    // the header shares; it leaves an unread directory null (repoAttention.test.ts).
    expect(source).toContain("planeTerminals($terminalSessions, $sessionActivity, $agentDirectories)");
    expect(source).not.toContain("cwd: null");
    // The sweep (cwd.ts, createAgentDirectorySweep) owns when to read, and
    // re-reads only when the set of sessions changes (cwd.test.ts). A second
    // reader here would re-run on every registry change.
    expect(source).not.toContain("readAgentCwds");
    expect(source).toContain("agentDirectories.refresh()");
  });

  it("names a task's repository from its registered id", () => {
    expect(source).toContain("repositoryPath: (id) => repoPaths.get(id) ?? null");
    expect(source).toContain("registered.want($board.runs.map((run) => run.repository_id))");
    expect(source).toContain("registered.refresh()");
  });

  it("says when registered repositories could not be read or were capped", () => {
    expect(source).toContain("$registeredStatus.error");
    expect(source).toContain("!$registeredStatus.complete");
  });

  it("ticks its clock on the visibility-aware timer, not a raw interval", () => {
    // ft-0ff882a6: a raw one-second setInterval kept waking the renderer
    // while the window was hidden.
    expect(source).not.toMatch(/\bsetInterval\(/);
    expect(source).toContain("const stopClock = createAdaptiveTimer(() => {");
  });

  it("wires onopenTask on the embedded AgentInbox to requestTaskOpen", () => {
    expect(source).toContain("requestTaskOpen");
    expect(source).toMatch(/<AgentInbox[^>]*onopenTask=\{openInboxTask\}/);
  });
});

describe("AgentsView F11 attention UX and SSR rendering", () => {
  const savedFilter = get(interfaceStore).agentsFilter;
  const slots: ReturnType<typeof terminalSessions.reserve>[] = [];
  const sessions: string[] = [];

  function addTerminal(opts: {
    key: string;
    label: string;
    launcher: "claude" | "agy";
    title: string;
    sessionId: string;
  }) {
    sessions.push(opts.sessionId);
    slots.push(
      terminalSessions.reserve({
        key: opts.key,
        repoPath: "/repo/f11",
        label: opts.label,
        launcher: opts.launcher,
        title: opts.title,
        status: "running",
        sessionId: opts.sessionId,
        close: async () => {},
      }),
    );
  }

  afterEach(async () => {
    setSignalledAsksForReader(false);
    interfaceStore.setAgentsFilter(savedFilter);
    while (sessions.length > 0) sessionActivity.forget(sessions.pop()!);
    while (slots.length > 0) slots.pop()?.release();
    vi.restoreAllMocks();
    await agentPlaneStore.refresh([]);
  });

  it("renders LauncherIcon badges, semantic gp-chip tones, inline detail previews, and timestamps", () => {
    addTerminal({ key: "k-claude", label: "Claude Code", launcher: "claude", title: "claude-ask", sessionId: "s-claude-ask" });
    addTerminal({ key: "k-agy", label: "Antigravity", launcher: "agy", title: "agy-done", sessionId: "s-agy-done" });
    addTerminal({ key: "k-err", label: "Claude Code", launcher: "claude", title: "claude-err", sessionId: "s-claude-err" });
    addTerminal({ key: "k-ok", label: "Antigravity", launcher: "agy", title: "agy-work", sessionId: "s-agy-work" });

    sessionActivity.announce({
      session: "s-claude-ask",
      channel: "hook",
      event: "permission_prompt",
      detail: "Allow Bash: cargo test --locked",
    });
    sessionActivity.announce({
      session: "s-agy-done",
      channel: "hook",
      event: "turn_finished",
      detail: "Synthesized M3b implementation plan",
    });
    sessionActivity.announce({
      session: "s-claude-err",
      channel: "hook",
      event: "error",
      detail: "API rate limit exceeded",
    });
    sessionActivity.output("s-agy-work");

    interfaceStore.setAgentsFilter("all");
    const html = render(AgentsView).body;

    expect(html).toContain('data-launcher-icon="claude"');
    expect(html).toContain('data-launcher-icon="agy"');
    expect(html).toContain('data-tone="warn"');
    expect(html).toContain('data-tone="danger"');
    expect(html).toContain('data-tone="info"');
    expect(html).toContain('data-tone="ok"');
    expect(html).toContain("Allow Bash: cargo test --locked");
    expect(html).toContain("Synthesized M3b implementation plan");
    expect(html).toContain("API rate limit exceeded");
    expect(html).toContain("Working");
    expect(html).toMatch(/<time[^>]*>just now<\/time>/);
  });

  it("exposes Needs action, Notifications, and Gaps filters and separates blocking from informational rows", () => {
    addTerminal({ key: "k-block", label: "Claude Code", launcher: "claude", title: "claude-blocking", sessionId: "s-block" });
    addTerminal({ key: "k-idle", label: "Antigravity", launcher: "agy", title: "agy-idle-notice", sessionId: "s-idle" });

    sessionActivity.announce({ session: "s-block", channel: "hook", event: "permission_prompt", detail: "Approve write" });
    sessionActivity.announce({ session: "s-idle", channel: "hook", event: "idle_prompt", detail: "Ready when you are" });

    interfaceStore.setAgentsFilter("attention");
    const needsActionHtml = render(AgentsView).body;
    expect(needsActionHtml).toContain('data-testid="agents-filter-attention"');
    expect(needsActionHtml).toContain('data-testid="agents-filter-notifications"');
    expect(needsActionHtml).toContain('data-testid="agents-filter-gaps"');
    expect(needsActionHtml).toContain("claude-blocking");
    expect(needsActionHtml).not.toContain("agy-idle-notice");

    interfaceStore.setAgentsFilter("notifications");
    const notificationsHtml = render(AgentsView).body;
    expect(notificationsHtml).toContain("agy-idle-notice");
    expect(notificationsHtml).toContain("Idle, waiting for input");
    expect(notificationsHtml).not.toContain("claude-blocking");
  });

  it("renders Dismiss only on informational notification rows and clears the notice without removing the live row", () => {
    addTerminal({ key: "k-block", label: "Claude Code", launcher: "claude", title: "claude-blocking", sessionId: "s-block-2" });
    addTerminal({ key: "k-info", label: "Antigravity", launcher: "agy", title: "agy-completed", sessionId: "s-info-2" });

    sessionActivity.announce({ session: "s-block-2", channel: "hook", event: "permission_prompt", detail: "Run migration" });
    sessionActivity.announce({ session: "s-info-2", channel: "hook", event: "agent_completed", detail: "Finished verification" });

    interfaceStore.setAgentsFilter("all");
    const html = render(AgentsView).body;
    expect(html).toContain('data-action="dismiss"');
    expect(html).toContain('aria-label="Dismiss notification for agy-completed"');
    expect(html).not.toContain('aria-label="Dismiss notification for claude-blocking"');

    // Dismiss clears informational notice while keeping the live row intact.
    expect(sessionActivity.dismiss("s-block-2")).toBe(false);
    expect(sessionActivity.dismiss("s-info-2")).toBe(true);
    const afterDismissHtml = render(AgentsView).body;
    expect(afterDismissHtml).toContain("agy-completed");
    expect(afterDismissHtml).not.toContain("Finished verification");
    expect(afterDismissHtml).not.toContain('aria-label="Dismiss notification for agy-completed"');
    expect(afterDismissHtml).toContain("Run migration");
  });

  it("renders a dirty checkout with suppressed bare BEL as neutral Uncommitted changes, not info", async () => {
    setSignalledAsksForReader(false);
    const worktreePath = "/repo/f11/.claude/worktrees/dirty-bel";
    vi.spyOn(ipc, "invoke").mockImplementation((async (cmd: string) => {
      if (cmd === "cmd_insights_snapshot") {
        return {
          repo_path: "/repo/f11",
          branch: "main",
          branch_ok: true,
          deadline_expired: false,
          duration_ms: 1,
          worktrees: {
            ok: true,
            error: "",
            count: 1,
            scanned: 1,
            dirty: 1,
            dirty_unknown: 0,
            blocked: 0,
            blocked_unknown: 0,
            truncated: false,
            items: [
              {
                path: worktreePath,
                name: "dirty-bel",
                branch: "agent/dirty-bel",
                is_detached: false,
                is_main: false,
                is_bare: false,
                dirty_files: 2,
                agent_kind: "claude",
                session_slug: "dirty-bel",
                operation_kind: "",
                operation_ok: true,
              },
            ],
          },
          agents: {
            ok: true,
            sessions: 1,
            kinds: [{ kind: "claude", sessions: 1 }],
            truncated: false,
            live: { ok: true, sessions: 0, kinds: [], worktrees: [] },
          },
          changes: {
            ok: true,
            error: "",
            files: 0,
            staged: 0,
            unstaged: 0,
            untracked: 0,
            conflicted: 0,
            additions: 0,
            deletions: 0,
            churn_warnings: 0,
            churn_overflowed: false,
            truncated: false,
          },
          collisions: {
            ok: true,
            error: "",
            overlapping_files: 0,
            worktrees_involved: 0,
            scanned_worktrees: 1,
            unscanned_worktrees: 0,
            failed_worktrees: 0,
            truncated: false,
            shared_worktree_files: 0,
            shared_worktrees: [],
            sessions_ok: true,
            sessions_error: "",
            items: [],
          },
          ledger: ledgerStatus({ recording: true, path: "", dropped: 0, error: "", error_code: "" }),
          codeintel: { available: false, db_path: "" },
        };
      }
      if (cmd === "cmd_terminal_context") {
        return { process: "claude", busy: true, cwd: worktreePath, repo_dir: null };
      }
      return null;
    }) as typeof ipc.invoke);

    await agentPlaneStore.refresh([{ path: "/repo/f11", label: "f11" }]);
    const stopDirs = agentDirectories.subscribe(() => {});
    try {
      addTerminal({
        key: "k-dirty-bel",
        label: "Claude Code",
        launcher: "claude",
        title: "dirty-bel-session",
        sessionId: "s-dirty-bel",
      });
      await new Promise((resolve) => setTimeout(resolve, 20));
      sessionActivity.announce({
        session: "s-dirty-bel",
        channel: "bell",
        event: null,
        detail: null,
      });

      interfaceStore.setAgentsFilter("all");
      const html = render(AgentsView).body;
      expect(html).toContain("Uncommitted changes");
      expect(html).toContain('data-tone="neutral"');
      expect(html).not.toContain('data-tone="info"');
      expect(html).not.toContain('data-action="dismiss"');
    } finally {
      stopDirs();
    }
  });

  it("does not crash when sessionActivity holds an attention timestamp beyond JS Date range (8.64e15)", () => {
    addTerminal({
      key: "k-overflow-ts",
      label: "Claude Code",
      launcher: "claude",
      title: "claude-overflow-ts",
      sessionId: "s-overflow-ts",
    });

    sessionActivity.announce({
      session: "s-overflow-ts",
      channel: "hook",
      event: "permission_prompt",
      detail: "Approve command",
      at: 8_700_000_000_000_000,
    });

    interfaceStore.setAgentsFilter("all");
    const html = render(AgentsView).body;
    expect(html).toContain("claude-overflow-ts");
    expect(html).toContain("Needs your permission");
    expect(html).not.toContain("NaN");
  });

  it("routes focusAgentRequest('inbox:<thread_key>') via inboxFocus to Inbox view and selects the target thread", () => {
    const appSource = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../../App.svelte"), "utf8");
    const inboxSource = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "./AgentInbox.svelte"), "utf8");

    expect(appSource).toContain('if (id.startsWith("inbox:"))');
    expect(appSource).toContain("requestInboxFocus(");
    expect(source).not.toContain("gitpulse:inbox-changed");
    expect(source).toContain("inboxFocus");
    expect(source).toContain('activeView = "inbox"');
    expect(inboxSource).toContain("focusThreadKey");
    expect(inboxSource).toContain("selectedThreadKey");

    // Verify inboxFocus store round-trip and AgentInbox rendering with focusThreadKey.
    requestInboxFocus("ask-target", "/repo/bg");
    const taken = takeInboxFocus();
    expect(taken).toMatchObject({ threadKey: "ask-target", repoPath: "/repo/bg" });
    expect(taken?.seq).toBeGreaterThan(0);
    expect(takeInboxFocus()).toBeNull();

    const { body } = render(AgentInbox, {
      props: {
        repoPath: "/repo/bg",
        embedded: true,
        focusThreadKey: "ask-target",
        focusSeq: 1,
        initialThreads: [
          {
            thread_key: "ask-first",
            title: "First thread",
            status: "open",
            ask_type: "answer",
            from_agent: "claude",
            to_agent: "developer",
            updated_at_ms: 1000,
            latest_msg: { seq: 1, from_agent: "claude", to_agent: "developer", kind: "ask", body: "First?", branch: null, created_at_ms: 1000, thread_key: "ask-first" },
            messages: [{ seq: 1, from_agent: "claude", to_agent: "developer", kind: "ask", body: "First?", branch: null, created_at_ms: 1000, thread_key: "ask-first" }],
          },
          {
            thread_key: "ask-target",
            title: "Target background ask",
            status: "open",
            ask_type: "answer",
            from_agent: "agy",
            to_agent: "developer",
            updated_at_ms: 2000,
            latest_msg: { seq: 2, from_agent: "agy", to_agent: "developer", kind: "ask", body: "Target body detail", branch: null, created_at_ms: 2000, thread_key: "ask-target" },
            messages: [{ seq: 2, from_agent: "agy", to_agent: "developer", kind: "ask", body: "Target body detail", branch: null, created_at_ms: 2000, thread_key: "ask-target" }],
          },
        ],
      },
    });
    expect(body).toContain("Target body detail");
  });

  it("clears stale agentsRepositoryScope when consuming an inboxFocusRequest with repoPath: null", () => {
    setAgentsRepositoryScope("/repo/A");
    expect(get(agentsRepositoryScope)).toBe("/repo/A");
    requestInboxFocus("ask-B", null);
    const req = takeInboxFocus();
    expect(req).toMatchObject({ threadKey: "ask-B", repoPath: null });
    expect(source).toContain("setAgentsRepositoryScope(req.repoPath ?? null)");
    setAgentsRepositoryScope(req?.repoPath ?? null);
    expect(get(agentsRepositoryScope)).toBeNull();
  });
});

