import { spawn } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, realpathSync } from "node:fs";
import net from "node:net";
import path from "node:path";
import { get } from "svelte/store";
import { render } from "svelte/server";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";
import {
  applyAgentTree,
  flattenAgentTree,
  projectAgentPlane,
} from "../../src/lib/agents/plane";
import AgentsView from "../../src/lib/components/AgentsView.svelte";
import {
  MENU_LIMITS,
  byteLength,
  menuStateProblem,
  menuStateWireProblem,
  sendableMenuState,
} from "../../src/lib/desktop/menuContract";
import {
  buildMenuState,
  canDispatchMenuEvent,
  countAgentsWaiting,
  projectMenuAgentState,
  statusKeyAction,
  type MenuAgentRequest,
} from "../../src/lib/desktop/menuState";
import { interfaceStore } from "../../src/lib/stores/interfaceStore";
import { createRepoStore, type RepoState } from "../../src/lib/stores/repoStore";
import {
  activityToAgentStatus,
  asksForReader,
  clean,
  createSessionActivity,
  parseAttention,
  setSignalledAsksForReader,
  type SessionActivity,
} from "../../src/lib/terminal/sessionActivity";
import { terminalSessions } from "../../src/lib/terminal/sessionRegistry";
import { runBrowserPage } from "./helpers/browser";
import {
  TempDirManager,
  assertCargoPassed,
  buildCargoEnv,
  ensureGitPulseHookBinary,
  execCargoSuite,
  readRepoFile,
  runGitPulseHookWithSocket,
  stripRustCommentsAndStrings,
} from "./helpers/env";
import {
  agyNotification,
  agyPostToolUse,
  agyPreInvocation,
  agyStop,
  claudeNotification,
  claudePermissionRequest,
  claudePostToolUse,
  claudeStop,
  claudeStopFailure,
  makeMenuAgentRequest,
  makeMenuStateWithRequests,
  makePlaneProbe,
  makePlaneTerminal,
} from "./helpers/fixtures";

const PATHS = { caseInsensitive: false };

function emptyRepo(): RepoState {
  return get(createRepoStore({ storage: null }));
}

function loadedRepo(currentPath = "/Projects/GitPulse"): RepoState {
  return {
    ...emptyRepo(),
    currentPath,
    currentBranch: "main",
    isLoading: false,
    openTabs: [
      {
        id: "tab-1",
        path: currentPath,
        name: "GitPulse",
        label: "GitPulse",
        pinned: false,
        isActive: true,
        isBare: false,
        isDirty: false,
        isLoading: false,
        error: null,
        currentBranch: "main",
        conflictedCount: 0,
        changedCount: 0,
      },
    ],
  };
}

function spawnHook(
  binPath: string,
  args: string[],
  env: NodeJS.ProcessEnv,
  stdinText?: string,
): Promise<{ exitCode: number; stdout: string; stderr: string }> {
  return new Promise((resolve, reject) => {
    const child = spawn(binPath, args, { env, stdio: ["pipe", "pipe", "pipe"] });
    const out: Buffer[] = [];
    const err: Buffer[] = [];
    const timer = setTimeout(() => {
      child.kill("SIGKILL");
      reject(new Error(`gitpulse-hook timed out for ${args.join(" ")}`));
    }, 15_000);
    child.stdout.on("data", (c: Buffer) => out.push(c));
    child.stderr.on("data", (c: Buffer) => err.push(c));
    child.once("error", (e) => {
      clearTimeout(timer);
      reject(e);
    });
    child.once("close", (code, signal) => {
      clearTimeout(timer);
      resolve({
        exitCode: code ?? (signal ? 1 : 0),
        stdout: Buffer.concat(out).toString("utf8"),
        stderr: Buffer.concat(err).toString("utf8"),
      });
    });
    if (stdinText !== undefined) child.stdin.write(stdinText);
    child.stdin.end();
  });
}

describe("[Tier 2] [R4] F7 — Claude Code Notification Boundaries", () => {
  const savedBell = get(interfaceStore).bellAsksForReader;

  beforeAll(async () => {
    await ensureGitPulseHookBinary();
  }, 300_000);

  beforeEach(() => {
    setSignalledAsksForReader(false);
  });

  afterEach(() => {
    interfaceStore.setBellAsksForReader(savedBell);
  });

  it("post-HOOK_SHADOW (>10s) bare terminal BEL and informational OSC do NOT overwrite standing blocking hook or OSC detail", async () => {
    const res = await execCargoSuite(
      ["--lib", "alerts::tests::post_hook_shadow_bare_bell"],
      { timeoutMs: 300_000 },
    );
    assertCargoPassed(res, "post_hook_shadow_bare_bell");

    const bellRes = await execCargoSuite(
      ["--lib", "alerts::tests::bare_bell_does_not_erase_standing_osc_detail"],
      { timeoutMs: 300_000 },
    );
    assertCargoPassed(bellRes, "bare_bell_does_not_erase_standing_osc_detail");
  }, 300_000);

  it("rapid duplicate twin hooks (<500ms) deduplicate cleanly and stale seq reports are rejected out-of-order", async () => {
    const intakeRes = await execCargoSuite(["--lib", "alerts::intake_tests::"], {
      timeoutMs: 300_000,
    });
    assertCargoPassed(intakeRes, "alerts::intake_tests::");

    const twinRes = await execCargoSuite(["--lib", "alerts::intake_twin_attack_tests::"], {
      timeoutMs: 300_000,
    });
    assertCargoPassed(twinRes, "alerts::intake_twin_attack_tests::");

    // Verify real binary treats app-side `stale` refusal silently with exit 0 and empty stdout.
    const hookRes = await runGitPulseHookWithSocket({
      args: ["notify", "agent_completed"],
      env: {
        GITPULSE_SESSION_ID: "term-t2-stale",
        GITPULSE_AGENT_KIND: undefined,
        GITPULSE_HOST: undefined,
      },
      stdinJson: claudeNotification("agent_completed"),
      reply: '{"ok":false,"reason":"stale"}\n',
    });
    expect(hookRes.exitCode).toBe(0);
    expect(hookRes.stdout).toBe("");
    expect(hookRes.stderr).not.toContain("refused");
  }, 300_000);

  it("gitpulse-hook with missing GITPULSE_SESSION_ID for Role::State exits 0 silently; malformed stdin and >8192B message degrade gracefully", async () => {
    // 1. Missing GITPULSE_SESSION_ID for Role::State (tool_finished) must exit 0 without connecting.
    const noSession = await runGitPulseHookWithSocket({
      args: ["notify", "tool_finished"],
      env: {
        GITPULSE_SESSION_ID: undefined,
        GITPULSE_AGENT_KIND: undefined,
        GITPULSE_HOST: undefined,
      },
      stdinJson: claudePostToolUse(),
    });
    expect(noSession.exitCode).toBe(0);
    expect(noSession.stdout).toBe("");
    expect(noSession.receivedRaw).toHaveLength(0);

    // 2. Malformed JSON stdin must exit 0 and write nothing to stdout.
    const malformed = await runGitPulseHookWithSocket({
      args: ["notify", "permission_prompt"],
      env: {
        GITPULSE_SESSION_ID: "term-t2-malformed",
        GITPULSE_AGENT_KIND: undefined,
        GITPULSE_HOST: undefined,
      },
      stdinJson: "{this is not valid json",
    });
    expect(malformed.exitCode).toBe(0);
    expect(malformed.stdout).toBe("");

    // 3. Oversized >8192B message must be truncated so serialized socket payload stays <= 8192 bytes.
    const oversized = await runGitPulseHookWithSocket({
      args: ["notify", "permission_prompt"],
      env: {
        GITPULSE_SESSION_ID: "term-t2-oversized",
        GITPULSE_AGENT_KIND: undefined,
        GITPULSE_HOST: undefined,
      },
      stdinJson: claudeNotification("permission_prompt", {
        message: "M".repeat(12_000),
      }),
    });
    expect(oversized.exitCode).toBe(0);
    expect(oversized.stdout).toBe("");
    expect(oversized.receivedRaw).toHaveLength(1);
    expect(Buffer.byteLength(oversized.receivedRaw[0], "utf8")).toBeLessThanOrEqual(8192);
    expect(oversized.receivedJson).toHaveLength(1);
    expect(String(oversized.receivedJson[0].message ?? "").length).toBeLessThanOrEqual(1024);
  }, 300_000);

  it("missing/unreachable socket exits 0 cleanly and closed-without-reply records .drops beside socket", async () => {
    const binPath = await ensureGitPulseHookBinary();
    const temps = new TempDirManager();
    try {
      const dir = temps.createShortSocketDir("gpd-");
      const logDir = path.join(dir, "logs");
      mkdirSync(logDir, { mode: 0o700 });
      const missingSock = path.join(dir, "nonexistent.sock");

      const missingRes = await spawnHook(
        binPath,
        ["notify", "permission_prompt"],
        buildCargoEnv({
          GITPULSE_NOTIFY_SOCKET: missingSock,
          GITPULSE_LOG_DIR: logDir,
          GITPULSE_SESSION_ID: "term-t2-missing-sock",
          GITPULSE_AGENT_KIND: undefined,
          GITPULSE_HOST: undefined,
        }),
        JSON.stringify(claudeNotification("permission_prompt")),
      );
      expect(missingRes.exitCode).toBe(0);
      expect(missingRes.stdout).toBe("");

      // Now bind a socket that accepts and immediately closes without replying -> triggers NotifyDrops record.
      const silentSock = path.join(dir, "silent.sock");
      const server = net.createServer((socket) => {
        socket.on("data", () => {
          socket.destroy();
        });
      });
      await new Promise<void>((resolve, reject) => {
        server.once("error", reject);
        server.listen(silentSock, () => resolve());
      });
      try {
        const dropRes = await spawnHook(
          binPath,
          ["notify", "permission_prompt"],
          buildCargoEnv({
            GITPULSE_NOTIFY_SOCKET: silentSock,
            GITPULSE_LOG_DIR: logDir,
            GITPULSE_SESSION_ID: "term-t2-drop-sock",
            GITPULSE_AGENT_KIND: undefined,
            GITPULSE_HOST: undefined,
          }),
          JSON.stringify(claudeNotification("permission_prompt")),
        );
        expect(dropRes.exitCode).toBe(0);
        expect(dropRes.stdout).toBe("");
        const dropsPath = `${silentSock}.drops`;
        expect(existsSync(dropsPath), `missing drop record at ${dropsPath}`).toBe(true);
        const dropsBody = readFileSync(dropsPath, "utf8");
        expect(dropsBody).toContain('"event":"permission_prompt"');
        expect(dropsBody).toContain('"session":"term-t2-drop-sock"');
      } finally {
        await new Promise<void>((resolve) => server.close(() => resolve()));
      }
    } finally {
      temps.cleanup();
    }
  }, 300_000);

  it("StopFailure / error urgency stays 'error' (asksForReader === true), distinct from idle_prompt (informational, asksForReader === false)", async () => {
    const errSocket = await runGitPulseHookWithSocket({
      args: ["notify", "error"],
      env: {
        GITPULSE_SESSION_ID: "term-t2-err",
        GITPULSE_AGENT_KIND: undefined,
        GITPULSE_HOST: undefined,
      },
      stdinJson: claudeStopFailure({ error: "API 503 unavailable" }),
    });
    expect(errSocket.exitCode).toBe(0);
    expect(errSocket.receivedJson[0]).toMatchObject({
      event: "error",
      agent: "claude",
      message: "API 503 unavailable",
    });

    const errParsed = parseAttention(
      { session: "term-t2-err", channel: "hook", event: "error", detail: "API 503 unavailable" },
      1_000,
    );
    expect(errParsed?.attention).toBeTruthy();
    expect((errParsed!.attention as unknown as Record<string, unknown>).urgency).toBe("error");
    expect(asksForReader(errParsed!.attention!.kind)).toBe(true);
    expect(
      activityToAgentStatus({
        lastOutputAt: null,
        title: null,
        attention: errParsed!.attention,
      })?.state,
    ).toBe("error");

    const idleParsed = parseAttention(
      { session: "term-t2-idle", channel: "hook", event: "idle_prompt" },
      1_000,
    );
    expect(idleParsed?.attention).toBeTruthy();
    expect((idleParsed!.attention as unknown as Record<string, unknown>).urgency).toBe("informational");
    expect(asksForReader(idleParsed!.attention!.kind)).toBe(false);
  }, 300_000);

  it("bellAsksForReader toggle: bare BEL (signalled) increments blocking asks ONLY when bellAsksForReader === true", () => {
    const activity = createSessionActivity(() => 5_000);
    activity.announce({ session: "s-bell", channel: "bell", event: null });
    const map = get(activity) as ReadonlyMap<string, SessionActivity>;

    interfaceStore.setBellAsksForReader(false);
    expect(asksForReader("signalled")).toBe(false);
    expect(countAgentsWaiting(map)).toBe(0);
    expect(
      projectAgentPlane({
        probes: [makePlaneProbe("/repo")],
        terminals: [
          makePlaneTerminal({
            cwd: "/repo/.claude/worktrees/alpha",
            attention: "signalled",
          }),
        ],
        tasks: null,
        paths: PATHS,
      }).attention.needing,
    ).toBe(0);

    interfaceStore.setBellAsksForReader(true);
    expect(asksForReader("signalled")).toBe(true);
    expect(countAgentsWaiting(map)).toBe(1);
    expect(
      projectAgentPlane({
        probes: [makePlaneProbe("/repo")],
        terminals: [
          makePlaneTerminal({
            cwd: "/repo/.claude/worktrees/alpha",
            attention: "signalled",
          }),
        ],
        tasks: null,
        paths: PATHS,
      }).attention.needing,
    ).toBe(1);
  });

  it("subagent turn_finished / agent_completed emits subagent: true on wire and alerts worker isolates parent Blocking prompts and working state", async () => {
    const subStop = await runGitPulseHookWithSocket({
      args: ["notify", "turn_finished"],
      env: {
        GITPULSE_SESSION_ID: "term-t2-subagent-parent",
        GITPULSE_AGENT_KIND: undefined,
        GITPULSE_HOST: undefined,
      },
      stdinJson: claudeStop({ agent_id: "subagent-1", last_assistant_message: "Subagent done" }),
    });
    expect(subStop.exitCode).toBe(0);
    expect(subStop.stdout).toBe("");
    expect(subStop.receivedJson).toHaveLength(1);
    expect(subStop.receivedJson[0]).toMatchObject({
      event: "turn_finished",
      subagent: true,
      agent_id: "subagent-1",
      session: "term-t2-subagent-parent",
    });

    const subAlerts = await execCargoSuite(["--lib", "alerts::tests::subagent_"], {
      timeoutMs: 300_000,
    });
    assertCargoPassed(subAlerts, "alerts::tests::subagent_");
  }, 300_000);
});

describe("[Tier 2] [R4] F8 — Antigravity (agy) Hook Boundaries", () => {
  beforeAll(async () => {
    await ensureGitPulseHookBinary();
  }, 300_000);

  it("Antigravity PreInvocation clears prior standing notification on new user turn without dropping providerSessionId", async () => {
    const convId = "91a8ef12-3456-4789-abcd-ef0123456789";
    const activity = createSessionActivity(() => 20_000);

    activity.announce({
      session: "pty-agy-turn",
      channel: "hook",
      event: "agent_completed",
      detail: "Previous turn done",
      fully_idle: true,
      provider_session_id: convId,
    });
    expect(get(activity).get("pty-agy-turn")?.attention).not.toBeNull();
    expect(get(activity).get("pty-agy-turn")?.fullyIdle).toBe(true);

    const preRes = await runGitPulseHookWithSocket({
      args: ["--host=agy", "notify", "prompt_submitted"],
      env: {
        GITPULSE_SESSION_ID: "pty-agy-turn",
        GITPULSE_HOST: "antigravity",
        GITPULSE_AGENT_KIND: undefined,
      },
      stdinJson: agyPreInvocation({ conversationId: convId }),
    });
    expect(preRes.exitCode).toBe(0);
    expect(preRes.stdout.trim()).toBe("{}");
    expect(preRes.receivedJson[0]).toMatchObject({
      event: "prompt_submitted",
      agent: "agy",
      session: "pty-agy-turn",
      provider_session_id: convId,
    });

    activity.announce({
      session: "pty-agy-turn",
      channel: "hook",
      event: preRes.receivedJson[0].event,
      provider_session_id: preRes.receivedJson[0].provider_session_id,
    });
    const entry = get(activity).get("pty-agy-turn");
    expect(entry?.attention).toBeNull();
    expect(entry?.fullyIdle).toBe(false);
    expect(entry?.dirtyPrompt).toBe(false);
    expect(entry?.providerSessionId).toBe(convId);
  }, 300_000);

  it("Antigravity Stop handles USER_CANCELED, ERROR, MAX_TOKEN_BUDGET_EXCEEDED, and NO_TOOL_CALL accurately", async () => {
    const env = {
      GITPULSE_SESSION_ID: "pty-agy-stop-reasons",
      GITPULSE_HOST: "antigravity",
      GITPULSE_AGENT_KIND: undefined,
    };

    const canceled = await runGitPulseHookWithSocket({
      args: ["--host=agy", "stop-or-notify"],
      env,
      stdinJson: agyStop({ terminationReason: "USER_CANCELED", fullyIdle: false }),
    });
    expect(canceled.exitCode).toBe(0);
    expect(canceled.stdout.trim()).toBe("{}");
    expect(canceled.receivedJson[0].event).toBe("turn_finished");

    const errored = await runGitPulseHookWithSocket({
      args: ["--host=agy", "stop-or-notify"],
      env,
      stdinJson: agyStop({ terminationReason: "ERROR", fullyIdle: false }),
    });
    expect(errored.exitCode).toBe(0);
    expect(errored.stdout.trim()).toBe("{}");
    expect(errored.receivedJson[0].event).toBe("error");

    const budget = await runGitPulseHookWithSocket({
      args: ["--host=agy", "stop-or-notify"],
      env,
      stdinJson: agyStop({ terminationReason: "MAX_TOKEN_BUDGET_EXCEEDED", fullyIdle: false }),
    });
    expect(budget.exitCode).toBe(0);
    expect(budget.stdout.trim()).toBe("{}");
    expect(budget.receivedJson[0].event).toBe("error");

    const cleanStop = await runGitPulseHookWithSocket({
      args: ["--host=agy", "stop-or-notify"],
      env,
      stdinJson: agyStop({ terminationReason: "NO_TOOL_CALL", fullyIdle: true }),
    });
    expect(cleanStop.exitCode).toBe(0);
    expect(cleanStop.stdout.trim()).toBe("{}");
    expect(cleanStop.receivedJson[0].event).toBe("turn_finished");
    expect(cleanStop.receivedJson[0].fully_idle).toBe(true);
  }, 300_000);

  it("PostToolUse clears permission_request only on matching tool subject (never mismatched subject)", async () => {
    const env = {
      GITPULSE_SESSION_ID: "pty-agy-subj",
      GITPULSE_HOST: "antigravity",
      GITPULSE_AGENT_KIND: undefined,
    };

    const req = await runGitPulseHookWithSocket({
      args: ["notify", "permission_request"],
      env,
      stdinJson: claudePermissionRequest({
        tool_name: "Bash",
        tool_input: { command: "git status" },
      }),
    });
    const otherTool = await runGitPulseHookWithSocket({
      args: ["--host=agy", "notify", "tool_finished"],
      env,
      stdinJson: agyPostToolUse({ toolName: "run_command", toolSummary: "echo hi" }),
    });
    expect(req.receivedJson[0].subject).toBeTruthy();
    expect(otherTool.receivedJson[0].event).toBe("tool_finished");

    const rustCheck = await execCargoSuite(
      ["--lib", "alerts::tests::a_permission_ends_only_on_its_own_tool_call"],
      { timeoutMs: 300_000 },
    );
    assertCargoPassed(rustCheck, "a_permission_ends_only_on_its_own_tool_call");
  }, 300_000);

  it("Antigravity Notification with unknown or empty matcher falls back safely with exit 0 and '{}' stdout", async () => {
    const env = {
      GITPULSE_SESSION_ID: "pty-agy-unknown-matcher",
      GITPULSE_HOST: "antigravity",
      GITPULSE_AGENT_KIND: undefined,
    };

    const unknownRes = await runGitPulseHookWithSocket({
      args: ["--host=agy", "notify", "unknown_future_matcher"],
      env,
      stdinJson: agyNotification({ message: "Future matcher" }),
    });
    expect(unknownRes.exitCode).toBe(0);
    expect(unknownRes.stdout.trim()).toBe("{}");
    expect(unknownRes.receivedJson).toHaveLength(0);

    const emptyRes = await runGitPulseHookWithSocket({
      args: ["--host=agy", "notify"],
      env,
      stdinJson: agyNotification({ message: "Missing matcher" }),
    });
    expect(emptyRes.exitCode).toBe(0);
    expect(emptyRes.stdout.trim()).toBe("{}");
    expect(emptyRes.receivedJson).toHaveLength(0);
  }, 300_000);

  it("workbench::agy_plugin Rust tests verify .gitpulse-managed marker preservation and configure_agents = false cleanup", async () => {
    const res = await execCargoSuite(["--lib", "workbench::agy_plugin"], {
      timeoutMs: 300_000,
    });
    assertCargoPassed(res, "workbench::agy_plugin");
  }, 300_000);
});

describe("[Tier 2] [R4] F9 — Agents Fleet Pane Boundaries", () => {
  it("AgentsView with 0 agents renders clean empty state without error", () => {
    const html = render(AgentsView).body;
    expect(html).toContain('data-testid="agents-view"');
    expect(html).toContain('data-testid="agents-empty"');
  });

  it("AgentsView with long detail (>200 chars) truncates/wraps without horizontal overflow in narrow viewport", async () => {
    const bounded = clean("A".repeat(320), 240);
    expect(bounded).not.toBeNull();
    expect(bounded!.length).toBeLessThanOrEqual(240);

    const verdict = await runBrowserPage<{
      overflowX?: boolean;
      detailLength?: number;
      narrowDetailRendered?: boolean;
      narrowDetailBounded?: boolean;
    }>({
      page: "tests/e2e-r1-r4/browser/agents-fleet.html",
      query: { scenario: "narrow", width: 390 },
      windowSize: { width: 390, height: 820 },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(verdict.data?.detailLength).toBeGreaterThan(200);
    expect(verdict.data?.narrowDetailRendered).toBe(true);
    expect(verdict.data?.narrowDetailBounded).toBe(true);
    expect(verdict.data?.overflowX).toBe(false);
    expect(
      verdict.passed,
      `Failed narrow viewport checks:\n${verdict.results
        .filter((r) => !r.pass)
        .map((r) => `- ${r.name}${r.detail ? `: ${r.detail}` : ""}`)
        .join("\n")}`,
    ).toBe(verdict.total);
  }, 90_000);

  it("Dismissing an informational notification in agents-fleet.html?scenario=dismiss clears notice while preserving live terminal row", async () => {
    const verdict = await runBrowserPage<{ hasDismissButton?: boolean }>({
      page: "tests/e2e-r1-r4/browser/agents-fleet.html",
      query: { scenario: "dismiss" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(verdict.data?.hasDismissButton).toBe(true);
    expect(
      verdict.passed,
      `Failed dismiss scenario checks:\n${verdict.results
        .filter((r) => !r.pass)
        .map((r) => `- ${r.name}${r.detail ? `: ${r.detail}` : ""}`)
        .join("\n")}`,
    ).toBe(verdict.total);
  }, 90_000);

  it("AgentsView with partial/failed probe gaps surfaces agents-gaps cleanly alongside agent attention states", () => {
    const plane = projectAgentPlane({
      probes: [
        {
          path: "/repo/failed",
          label: "FailedRepo",
          snapshot: null,
          error: "git status timed out",
          skipped: false,
          skipReason: "",
        },
        makePlaneProbe("/repo/healthy"),
      ],
      terminals: [
        makePlaneTerminal({
          repoPath: "/repo/healthy",
          cwd: "/repo/healthy/.claude/worktrees/alpha",
          attention: "needs-you",
        }),
      ],
      tasks: null,
      paths: PATHS,
    });
    expect(plane.gaps.length).toBeGreaterThanOrEqual(1);
    expect(plane.attention.needing).toBe(1);

    const viewSrc = readRepoFile("src/lib/components/AgentsView.svelte");
    const gapsIdx = viewSrc.indexOf('data-testid="agents-gaps"');
    const tableIdx = viewSrc.indexOf("<table");
    expect(gapsIdx).toBeGreaterThan(-1);
    expect(tableIdx).toBeGreaterThan(gapsIdx);
  });

  it("Claude + Antigravity mixed fleet keeps treegrid keyboard navigation and roving tabindex functional", async () => {
    const verdict = await runBrowserPage({
      page: "tests/e2e-r1-r4/browser/agents-fleet.html",
      query: { scenario: "claude-vs-agy" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(
      verdict.passed,
      `Failed claude-vs-agy checks:\n${verdict.results
        .filter((r) => !r.pass)
        .map((r) => `- ${r.name}${r.detail ? `: ${r.detail}` : ""}`)
        .join("\n")}`,
    ).toBe(verdict.total);

    const slots: ReturnType<typeof terminalSessions.reserve>[] = [];
    try {
      slots.push(
        terminalSessions.reserve({
          key: "t2-tree-claude",
          repoPath: "/repo/tree",
          label: "Claude Code",
          launcher: "claude",
          title: "tree-claude",
          status: "running",
          sessionId: "pty-t2-tree-claude",
          close: async () => {},
        }),
      );
      slots.push(
        terminalSessions.reserve({
          key: "t2-tree-agy",
          repoPath: "/repo/tree",
          label: "Antigravity",
          launcher: "agy",
          title: "tree-agy",
          status: "running",
          sessionId: "pty-t2-tree-agy",
          close: async () => {},
        }),
      );
      const html = render(AgentsView).body;
      expect(html).toContain('role="treegrid"');
      expect(html).toContain('aria-level="1"');
      expect(html).toContain('tabindex="0"');
      expect(html).toContain('tabindex="-1"');

      // Verify tree flattening preserves levels cleanly.
      const plane = projectAgentPlane({
        probes: [],
        terminals: [
          makePlaneTerminal({ key: "a", sessionId: "sa", title: "claude-a" }),
          makePlaneTerminal({ key: "b", sessionId: "sb", label: "Antigravity", title: "agy-b" }),
        ],
        tasks: null,
        paths: PATHS,
      });
      const flat = flattenAgentTree(applyAgentTree(plane.roots, "all"), new Set());
      expect(flat.length).toBe(2);
      expect(flat.map((e) => e.posInSet)).toEqual([1, 2]);
      expect(flat.every((e) => e.setSize === 2)).toBe(true);
    } finally {
      while (slots.length > 0) slots.pop()?.release();
    }
  }, 90_000);
});

describe("[Tier 2] [R2] F10 — Menu Bar Overhaul Boundaries", () => {
  it("MenuState overflow (> MAX_MENU_AGENT_REQUESTS = 8): projectMenuAgentState + buildMenuState caps at 8 with agentRequestsHidden = total - 8 and sendableMenuState enforces bounds", async () => {
    const now = Date.now();
    const activity = new Map<string, SessionActivity>();
    for (let i = 0; i < 11; i += 1) {
      activity.set(`pty-${i + 1}`, {
        lastOutputAt: now - i * 1_000,
        title: `agent-${i + 1}`,
        attention: {
          kind: "needs-you",
          urgency: "blocking",
          label: "Needs your permission",
          detail: `Allow tool #${i + 1}`,
          at: now - i * 1_000,
        },
      });
    }
    const records = Array.from({ length: 11 }, (_, i) => ({
      sessionId: `pty-${i + 1}`,
      repoPath: "/Projects/GitPulse",
      launcher: (i % 2 === 0 ? "claude" : "agy") as "claude" | "agy",
      title: `agent-${i + 1}`,
      label: i % 2 === 0 ? "Claude Code" : "Antigravity",
    }));

    const projection = projectMenuAgentState(
      activity,
      records,
      undefined,
      [],
      [{ path: "/Projects/GitPulse", label: "GitPulse" }],
      false,
      PATHS,
      "/Projects/GitPulse",
    );
    expect(projection.agentsWaiting).toBe(11);
    expect(projection.agentRequests).toHaveLength(MENU_LIMITS.agentRequests);
    expect(projection.agentRequestsHidden).toBe(3);

    const built = buildMenuState(
      loadedRepo("/Projects/GitPulse"),
      get(interfaceStore),
      "system",
      {},
      false,
      {},
      PATHS,
      projection.agentsWaiting,
      0,
      projection,
    );
    expect(built.agentsWaiting).toBe(11);
    expect(built.agentRequests).toHaveLength(MENU_LIMITS.agentRequests);
    expect(built.agentRequestsHidden).toBe(3);
    expect(menuStateWireProblem(built)).toBeNull();
    expect(menuStateProblem(built)).toBeNull();

    const { state: validated, problem: validProblem } = sendableMenuState(built);
    expect(validProblem).toBeNull();
    expect(validated.agentRequests).toHaveLength(MENU_LIMITS.agentRequests);
    expect(validated.agentRequestsHidden).toBe(3);

    // Adversarial unsendable > 8 payload: sendableMenuState clamps to 8 and keeps valid state
    const eleven: MenuAgentRequest[] = Array.from({ length: 11 }, (_, i) =>
      makeMenuAgentRequest({
        id: `term:req-ov-${i + 1}`,
        source: "terminal",
        provider: i % 2 === 0 ? "claude" : "agy",
        agentLabel: `agent-${i + 1}`,
        repoPath: "/Projects/GitPulse",
        repoLabel: "GitPulse",
        urgency: "blocking",
        reason: "Needs your permission",
        detail: `Allow tool #${i + 1}`,
        atMs: now - i * 1_000,
      }),
    );
    const oversized = makeMenuStateWithRequests(eleven, {
      agentsRunning: 2,
      agentsInformational: 1,
      agentRequestsHidden: 3,
    });
    expect(menuStateProblem(oversized)).not.toBeNull();
    const { state: repaired } = sendableMenuState(oversized);
    expect(repaired.agentRequests).toHaveLength(MENU_LIMITS.agentRequests);
    expect(repaired.agentRequestsHidden).toBe(3);
    expect(menuStateWireProblem(repaired)).toBeNull();
    expect(menuStateProblem(repaired)).toBeNull();

    // Dishonest hidden > 0 when list < 8 is rejected by menuStateProblem and repaired to 0
    const dishonest = makeMenuStateWithRequests(eleven.slice(0, 3), {
      agentRequestsHidden: 2,
    });
    expect(menuStateProblem(dishonest)).not.toBeNull();
    const { state: repairedDishonest } = sendableMenuState(dishonest);
    expect(repairedDishonest.agentRequestsHidden).toBe(0);
    expect(menuStateProblem(repairedDishonest)).toBeNull();

    const verdict = await runBrowserPage<{ shellHeight?: number }>({
      page: "tests/e2e-r1-r4/browser/status-popover.html",
      query: { scenario: "overflow" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(
      verdict.passed,
      `Failed overflow popover checks:\n${verdict.results
        .filter((r) => !r.pass)
        .map((r) => `- ${r.name}${r.detail ? `: ${r.detail}` : ""}`)
        .join("\n")}`,
    ).toBe(verdict.total);
  }, 90_000);

  it("Oversized MenuState strings (>2048B), trayTitle (>24 chars), and agentsWaiting (>999) are repaired by sendableMenuState", () => {
    const oversized = makeMenuStateWithRequests(
      [
        makeMenuAgentRequest({
          id: "term:req-big",
          source: "terminal",
          provider: "claude",
          agentLabel: "L".repeat(3000),
          repoPath: "/Projects/GitPulse",
          repoLabel: "GitPulse",
          urgency: "blocking",
          reason: "Needs your permission",
          detail: "D".repeat(3000),
          atMs: Date.now(),
        }),
      ],
      { agentsRunning: 2 },
    );
    oversized.agentsWaiting = 2500;
    oversized.trayTitle = "T".repeat(40);
    oversized.status.headline = "H".repeat(3000);

    const { state: repaired, problem } = sendableMenuState(oversized);
    expect(problem).not.toBeNull();
    expect(menuStateWireProblem(repaired)).toBeNull();
    expect(menuStateProblem(repaired)).toBeNull();
    expect(repaired.agentsWaiting).toBeLessThanOrEqual(999);
    expect(Array.from(repaired.trayTitle ?? "").length).toBeLessThanOrEqual(24);
    expect(byteLength(repaired.status.headline)).toBeLessThanOrEqual(2048);
    expect(byteLength(repaired.agentRequests[0]?.agentLabel ?? "")).toBeLessThanOrEqual(2048);
    expect(byteLength(repaired.agentRequests[0]?.detail ?? "")).toBeLessThanOrEqual(2048);
  });

  it("Stale repository mismatch rejects/disables stale action safely without mutating wrong repo", async () => {
    const verdict = await runBrowserPage<{ actionError?: string | null }>({
      page: "tests/e2e-r1-r4/browser/status-popover.html",
      query: { scenario: "stale-repo" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(verdict.data?.actionError).toContain("Repository changed");
    expect(
      verdict.passed,
      `Failed stale-repo checks:\n${verdict.results
        .filter((r) => !r.pass)
        .map((r) => `- ${r.name}${r.detail ? `: ${r.detail}` : ""}`)
        .join("\n")}`,
    ).toBe(verdict.total);

    const state = makeMenuStateWithRequests([], { conflicts: 1 });
    const repo = loadedRepo("/Projects/GitPulse");
    expect(
      canDispatchMenuEvent(
        { id: "section:work:overview", repo_path: "/Projects/StaleRepo" },
        state,
        repo,
        PATHS,
      ),
    ).toBe(false);
    expect(
      canDispatchMenuEvent(
        { id: "section:work:overview", repo_path: "/Projects/GitPulse" },
        state,
        repo,
        PATHS,
      ),
    ).toBe(true);
  }, 90_000);

  it("StatusPopover height stays <= 640px (MAX_HEIGHT) even with max 8 agent requests + metrics", async () => {
    const verdict = await runBrowserPage<{
      shellHeight?: number;
      renderedRequestButtons?: number;
    }>({
      page: "tests/e2e-r1-r4/browser/status-popover.html",
      query: { scenario: "overflow" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(verdict.data?.shellHeight).toBeGreaterThan(0);
    expect(verdict.data?.shellHeight).toBeLessThanOrEqual(640);
    expect(verdict.data?.renderedRequestButtons).toBe(8);
    expect(
      verdict.passed,
      `Failed overflow height checks:\n${verdict.results
        .filter((r) => !r.pass)
        .map((r) => `- ${r.name}${r.detail ? `: ${r.detail}` : ""}`)
        .join("\n")}`,
    ).toBe(verdict.total);
  }, 90_000);

  it("Keyboard navigation in StatusPopover (Escape, R, Enter, 1..4) works alongside agent request rows", async () => {
    const snapshot = makeMenuStateWithRequests(
      [
        makeMenuAgentRequest({
          id: "term:req-kbd-1",
          source: "terminal",
          provider: "claude",
          agentLabel: "claude-1",
          repoPath: "/Projects/GitPulse",
          repoLabel: "GitPulse",
          urgency: "blocking",
          reason: "Needs your permission",
          detail: "Allow Edit",
          atMs: Date.now(),
        }),
      ],
      { conflicts: 2 },
    );
    snapshot.status.changed = 3;
    snapshot.status.staged = 1;
    snapshot.status.stashes = 2;

    const kbd = (key: string, ui = { choosing: false, expanded: false }) =>
      statusKeyAction({ key, metaKey: false, ctrlKey: false, altKey: false }, snapshot, ui);

    expect(kbd("r")).toEqual({ id: "refresh" });
    expect(kbd("Enter")).toEqual({ id: "section:work:overview" });
    expect(kbd("3")).toEqual({ id: "section:work:overview" });
    expect(kbd("4")).toEqual({ id: "section:work:overview" });
    expect(kbd("Escape", { choosing: true, expanded: true })).toEqual({ collapse: "chooser" });
    expect(kbd("Escape", { choosing: false, expanded: true })).toEqual({ collapse: "details" });
    expect(kbd("Escape", { choosing: false, expanded: false })).toEqual({ dismiss: true });

    const verdict = await runBrowserPage({
      page: "tests/e2e-r1-r4/browser/status-popover.html",
      query: { scenario: "conflict-jump" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(
      verdict.passed,
      `Failed conflict-jump keyboard checks:\n${verdict.results
        .filter((r) => !r.pass)
        .map((r) => `- ${r.name}${r.detail ? `: ${r.detail}` : ""}`)
        .join("\n")}`,
    ).toBe(verdict.total);
  }, 90_000);

  it("MenuState Rust serde backward-compatibility deserializes payloads omitting agentRequests / agentsRunning / agentsInformational", async () => {
    const res = await execCargoSuite(["--lib", "desktop::state::tests"], {
      timeoutMs: 300_000,
    });
    assertCargoPassed(res, "desktop::state::tests");

    const stateSrc = readRepoFile("src-tauri/src/desktop/state.rs");
    const stateTestIdx = stateSrc.indexOf("#[cfg(test)]");
    const stateCode = stripRustCommentsAndStrings(
      stateTestIdx > -1 ? stateSrc.slice(0, stateTestIdx) : stateSrc,
    );
    for (const field of [
      "agent_requests",
      "agent_requests_hidden",
      "agents_running",
      "agents_informational",
    ]) {
      const idx = stateCode.indexOf(`pub ${field}:`);
      expect(idx, `state.rs missing field ${field}`).toBeGreaterThan(-1);
      const boundary = Math.max(stateCode.lastIndexOf(",", idx), stateCode.lastIndexOf("{", idx));
      const prefix = stateCode.slice(boundary + 1, idx);
      expect(prefix, `state.rs field ${field} missing #[serde(default)]`).toContain(
        "#[serde(default)]",
      );
    }
  }, 300_000);
});

describe("[Tier 2] [R4] Antigravity peer-inbox --host=agy vs GITPULSE_HOST=antigravity parity", () => {
  it("routes PreInvocation invocationNum 1 (prompt_submitted, fully_idle=false) vs 2 (no-op) identically under --host=agy alone and GITPULSE_HOST=antigravity alone", async () => {
    await ensureGitPulseHookBinary();

    const matrix = [
      {
        label: "flag-only-inv1",
        args: ["--host=agy", "peer-inbox"],
        envHost: undefined as string | undefined,
        invocationNum: 1,
        expectPromptSubmitted: true,
      },
      {
        label: "flag-only-inv2",
        args: ["--host=agy", "peer-inbox"],
        envHost: undefined as string | undefined,
        invocationNum: 2,
        expectPromptSubmitted: false,
      },
      {
        label: "env-only-inv1",
        args: ["peer-inbox"],
        envHost: "antigravity" as string | undefined,
        invocationNum: 1,
        expectPromptSubmitted: true,
      },
      {
        label: "env-only-inv2",
        args: ["peer-inbox"],
        envHost: "antigravity" as string | undefined,
        invocationNum: 2,
        expectPromptSubmitted: false,
      },
    ];

    for (const item of matrix) {
      const sid = `t5-agy-peer-${item.label}`;
      const res = await runGitPulseHookWithSocket({
        args: item.args,
        env: { GITPULSE_SESSION_ID: sid, GITPULSE_HOST: item.envHost },
        stdinJson: agyPreInvocation({ invocationNum: item.invocationNum }),
      });

      expect(res.exitCode, item.label).toBe(0);
      expect(res.stdout.trim(), item.label).toBe("{}");
      if (item.expectPromptSubmitted) {
        expect(res.receivedJson, item.label).toHaveLength(1);
        expect(res.receivedJson[0], item.label).toMatchObject({
          agent: "agy",
          event: "prompt_submitted",
          fully_idle: false,
          hook_event: "PreInvocation",
          session: sid,
        });
      } else {
        expect(res.receivedJson, item.label).toEqual([]);
      }
    }
  }, 60_000);
});

describe("[Tier 2] [R4] gitpulse-hook adversarial stdin, flag order, oversized Stop & concurrent socket stress", () => {
  it("keeps Antigravity stdout non-empty '{}' on malformed/empty JSON regardless of --host=agy position, retains oversized stop error, handles hostile control+emoji, and survives 16-way concurrent burst", async () => {
    await ensureGitPulseHookBinary();

    for (const badPayload of ["{not json", ""]) {
      for (const variant of [
        { args: ["--host=agy", "peer-inbox"], envHost: undefined },
        { args: ["peer-inbox", "--host=agy"], envHost: undefined },
        { args: ["peer-inbox"], envHost: "antigravity" },
      ]) {
        const res = await runGitPulseHookWithSocket({
          args: variant.args,
          env: { GITPULSE_SESSION_ID: "t5-bad-json", GITPULSE_HOST: variant.envHost },
          stdinJson: badPayload,
        });
        expect(res.exitCode, `${variant.args.join(" ")}:${badPayload}`).toBe(0);
        expect(res.stdout.trim(), `${variant.args.join(" ")}:${badPayload}`).toBe("{}");
        expect(res.receivedJson).toEqual([]);
      }
    }

    // Oversized (>16 MiB) Stop payload still preserves terminationReason="ERROR"
    const oversizedStop = JSON.stringify({
      hookEventName: "Stop",
      conversationId: "91a8ef12-3456-4789-abcd-ef0123456789",
      terminationReason: "ERROR",
      error: "Oversized stop error preserved",
      pad: "x".repeat(16 * 1024 * 1024 + 256),
    });
    const stopRes = await runGitPulseHookWithSocket({
      args: ["--host=agy", "stop-or-notify"],
      env: { GITPULSE_SESSION_ID: "t5-oversized-stop", GITPULSE_HOST: undefined },
      stdinJson: oversizedStop,
    });
    expect(stopRes.exitCode).toBe(0);
    expect(stopRes.stdout.trim()).toBe("{}");
    expect(stopRes.receivedJson).toHaveLength(1);
    expect(stopRes.receivedJson[0]).toMatchObject({
      agent: "agy",
      event: "error",
      session: "t5-oversized-stop",
    });

    // Hostile bidi + ESC + BEL + 3000 multi-byte emoji stays valid JSON <= 8192B
    const hostileSid = "t5-hostile-msg";
    const hostile = await runGitPulseHookWithSocket({
      args: ["notify", "permission_request"],
      env: { GITPULSE_SESSION_ID: hostileSid, GITPULSE_HOST: undefined },
      stdinJson: claudePermissionRequest({
        message: `\u001b[31m\u202eAllow\u0007\n\r${"🔥".repeat(3000)}`,
      }),
    });
    expect(hostile.exitCode).toBe(0);
    expect(hostile.receivedJson).toHaveLength(1);
    expect(Buffer.byteLength(JSON.stringify(hostile.receivedJson[0]), "utf8")).toBeLessThanOrEqual(8192);

    // 16-way concurrent burst across Claude & Antigravity routes
    const burst = await Promise.all(
      Array.from({ length: 16 }, (_, i) => {
        const sid = `t5-burst-${i}`;
        if (i % 4 === 0) {
          return runGitPulseHookWithSocket({
            args: ["--host=agy", "peer-inbox"],
            env: { GITPULSE_SESSION_ID: sid, GITPULSE_HOST: undefined },
            stdinJson: agyPreInvocation({ invocationNum: 1 }),
          });
        }
        if (i % 4 === 1) {
          return runGitPulseHookWithSocket({
            args: ["peer-inbox"],
            env: { GITPULSE_SESSION_ID: sid, GITPULSE_HOST: "antigravity" },
            stdinJson: agyPreInvocation({ invocationNum: 2 }),
          });
        }
        if (i % 4 === 2) {
          return runGitPulseHookWithSocket({
            args: ["notify", "permission_request"],
            env: { GITPULSE_SESSION_ID: sid, GITPULSE_HOST: undefined },
            stdinJson: claudePermissionRequest({ message: `Burst ask ${i}` }),
          });
        }
        return runGitPulseHookWithSocket({
          args: ["stop-or-notify"],
          env: { GITPULSE_SESSION_ID: sid, GITPULSE_HOST: "antigravity" },
          stdinJson: agyStop({ terminationReason: "NO_TOOL_CALL", message: `Done ${i}` }),
        });
      }),
    );

    for (const [i, res] of burst.entries()) {
      expect(res.exitCode, `burst[${i}]`).toBe(0);
      if (i % 4 === 1) {
        expect(res.receivedJson, `burst[${i}] inv=2`).toEqual([]);
      } else {
        expect(res.receivedJson, `burst[${i}]`).toHaveLength(1);
      }
    }
  }, 120_000);

  it("gitpulse-worktree.sh fallback extracts top-level name without shadowing by nested objects", async () => {
    const temps = new TempDirManager();
    const repoDir = realpathSync(temps.create("gp-wt-nested-"));
    const runGit = (args: string[], cwd: string) =>
      new Promise<{ code: number; stderr: string }>((resolve, reject) => {
        const child = spawn("git", args, { cwd, stdio: ["ignore", "pipe", "pipe"] });
        const err: Buffer[] = [];
        child.stderr.on("data", (c: Buffer) => err.push(c));
        child.once("error", reject);
        child.once("close", (code) => resolve({ code: code ?? 1, stderr: Buffer.concat(err).toString("utf8") }));
      });
    try {
      expect((await runGit(["init", "-b", "main"], repoDir)).code).toBe(0);
      expect((await runGit(["config", "user.email", "t@example.com"], repoDir)).code).toBe(0);
      expect((await runGit(["config", "user.name", "Test"], repoDir)).code).toBe(0);
      expect((await runGit(["commit", "--allow-empty", "-m", "init"], repoDir)).code).toBe(0);

      for (const [idx, payload] of [
        JSON.stringify({ name: "wt-top-first", cwd: repoDir, nested: { name: "wrong-name" } }),
        JSON.stringify({ nested: { name: "wrong-name" }, name: "wt-top-second", cwd: repoDir }),
      ].entries()) {
        const expectedName = idx === 0 ? "wt-top-first" : "wt-top-second";
        const res = await new Promise<{ code: number; stdout: string; stderr: string }>((resolve, reject) => {
          const child = spawn(
            "sh",
            [path.join(process.cwd(), "plugins/gitpulse/hooks/gitpulse-worktree.sh"), "/nonexistent/gitpulse-hook", "worktree-create"],
            { cwd: repoDir, stdio: ["pipe", "pipe", "pipe"] },
          );
          const out: Buffer[] = [];
          const err: Buffer[] = [];
          child.stdout.on("data", (c: Buffer) => out.push(c));
          child.stderr.on("data", (c: Buffer) => err.push(c));
          child.once("error", reject);
          child.once("close", (code) =>
            resolve({
              code: code ?? 1,
              stdout: Buffer.concat(out).toString("utf8"),
              stderr: Buffer.concat(err).toString("utf8"),
            }),
          );
          child.stdin.write(payload);
          child.stdin.end();
        });
        expect(res.code, res.stderr).toBe(0);
        expect(res.stdout.trim()).toBe(path.join(repoDir, ".claude/worktrees", expectedName));
        expect(existsSync(path.join(repoDir, ".claude/worktrees", expectedName))).toBe(true);
        expect(existsSync(path.join(repoDir, ".claude/worktrees/wrong-name"))).toBe(false);
      }
    } finally {
      temps.cleanup();
    }
  }, 60_000);
});
