import { spawn } from "node:child_process";
import { existsSync, mkdirSync, readFileSync } from "node:fs";
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
  byteLength,
  menuStateProblem,
  menuStateWireProblem,
  sendableMenuState,
} from "../../src/lib/desktop/menuContract";
import {
  canDispatchMenuEvent,
  countAgentsWaiting,
  statusKeyAction,
  type MenuState,
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
  buildCargoEnv,
  ensureGitPulseHookBinary,
  readRepoFile,
  runCargoTest,
  runGitPulseHookWithSocket,
} from "./helpers/env";
import {
  agyNotification,
  agyPostToolUse,
  agyPreInvocation,
  agyStop,
  claudeNotification,
  claudePermissionRequest,
  claudePostToolUse,
  claudeStopFailure,
  makeMenuStateWithRequests,
  makePlaneProbe,
  makePlaneTerminal,
  type MenuAgentRequestFixture,
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

function assertCargoPassed(
  res: { ok: boolean; exitCode: number; stdout: string; stderr: string },
  label: string,
): void {
  expect(res.ok, `${label} failed (exit ${res.exitCode}):\n${res.stdout}\n${res.stderr}`).toBe(true);
  expect(res.exitCode).toBe(0);
  const combined = `${res.stdout}\n${res.stderr}`;
  const passedMatch = /(\d+)\s+passed/.exec(combined);
  if (passedMatch) {
    expect(Number(passedMatch[1]), `${label} matched 0 tests`).toBeGreaterThan(0);
  }
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

  it("post-HOOK_SHADOW (>10s) bare terminal BEL does NOT overwrite a cleared hook state or resurrect a standing blocking alert", async () => {
    const res = await runCargoTest(
      ["--lib", "alerts::tests::post_hook_shadow_bare_bell"],
      { timeoutMs: 300_000 },
    );
    assertCargoPassed(res, "post_hook_shadow_bare_bell");

    const bellRes = await runCargoTest(
      ["--lib", "alerts::tests::bare_bell_does_not_erase_standing_osc_detail"],
      { timeoutMs: 300_000 },
    );
    assertCargoPassed(bellRes, "bare_bell_does_not_erase_standing_osc_detail");
  }, 300_000);

  it("rapid duplicate twin hooks (<500ms) deduplicate cleanly and stale seq reports are rejected out-of-order", async () => {
    const intakeRes = await runCargoTest(["--lib", "alerts::intake::tests"], {
      timeoutMs: 300_000,
    });
    assertCargoPassed(intakeRes, "alerts::intake::tests");

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

    const rustCheck = await runCargoTest(
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
    const res = await runCargoTest(["--lib", "workbench::agy_plugin"], {
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

    const verdict = await runBrowserPage<{ overflowX?: boolean }>({
      page: "tests/e2e-r1-r4/browser/agents-fleet.html",
      query: { scenario: "narrow", width: 640 },
      windowSize: { width: 640, height: 820 },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
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
  it("MenuState overflow (> MAX_MENU_AGENT_REQUESTS = 8): sendableMenuState clamps to 8 and sets agentRequestsHidden = total - 8", async () => {
    const now = Date.now();
    const eleven: MenuAgentRequestFixture[] = Array.from({ length: 11 }, (_, i) => ({
      id: `req-ov-${i + 1}`,
      sessionId: `pty-${i + 1}`,
      agent: i % 2 === 0 ? "claude" : "agy",
      kind: i % 2 === 0 ? "claude" : "agy",
      label: `agent-${i + 1}`,
      repoPath: "/Projects/GitPulse",
      repoLabel: "GitPulse",
      urgency: "blocking",
      event: "permission_prompt",
      detail: `Allow tool #${i + 1}`,
      at: now - i * 1_000,
    }));
    const raw = makeMenuStateWithRequests(eleven, {
      agentsRunning: 2,
      agentsInformational: 1,
    }) as unknown as MenuState;

    const { state: repaired } = sendableMenuState(raw);
    const repObj = repaired as unknown as Record<string, unknown>;
    expect(Array.isArray(repObj.agentRequests)).toBe(true);
    expect((repObj.agentRequests as unknown[]).length).toBeLessThanOrEqual(8);
    expect(repObj.agentRequestsHidden).toBe(3);

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
        {
          id: "req-big",
          sessionId: "pty-big",
          agent: "claude",
          kind: "claude",
          label: "L".repeat(3000),
          repoPath: "/Projects/GitPulse",
          repoLabel: "GitPulse",
          urgency: "blocking",
          event: "permission_prompt",
          detail: "D".repeat(3000),
          at: Date.now(),
        },
      ],
      { agentsRunning: 2 },
    );
    oversized.agentsWaiting = 2500;
    oversized.trayTitle = "T".repeat(40);
    oversized.status.headline = "H".repeat(3000);

    const { state: repaired, problem } = sendableMenuState(oversized as unknown as MenuState);
    expect(problem).not.toBeNull();
    expect(menuStateWireProblem(repaired)).toBeNull();
    expect(menuStateProblem(repaired)).toBeNull();
    expect(repaired.agentsWaiting).toBeLessThanOrEqual(999);
    expect(Array.from(repaired.trayTitle ?? "").length).toBeLessThanOrEqual(24);
    expect(byteLength(repaired.status.headline)).toBeLessThanOrEqual(2048);
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

    const state = makeMenuStateWithRequests([], { conflicts: 1 }) as unknown as MenuState;
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
    const verdict = await runBrowserPage<{ shellHeight?: number }>({
      page: "tests/e2e-r1-r4/browser/status-popover.html",
      query: { scenario: "overflow" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(verdict.data?.shellHeight).toBeGreaterThan(0);
    expect(verdict.data?.shellHeight).toBeLessThanOrEqual(640);
  }, 90_000);

  it("Keyboard navigation in StatusPopover (Escape, R, Enter, 1..4) works alongside agent request rows", async () => {
    const snapshot = makeMenuStateWithRequests(
      [
        {
          id: "req-kbd-1",
          sessionId: "pty-1",
          agent: "claude",
          kind: "claude",
          label: "claude-1",
          repoPath: "/Projects/GitPulse",
          repoLabel: "GitPulse",
          urgency: "blocking",
          event: "permission_prompt",
          detail: "Allow Edit",
          at: Date.now(),
        },
      ],
      { conflicts: 2 },
    ) as unknown as MenuState;
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
    const res = await runCargoTest(["--lib", "desktop::state::tests"], {
      timeoutMs: 300_000,
    });
    assertCargoPassed(res, "desktop::state::tests");

    const stateSrc = readRepoFile("src-tauri/src/desktop/state.rs");
    for (const field of [
      "agent_requests",
      "agent_requests_hidden",
      "agents_running",
      "agents_informational",
    ]) {
      const idx = stateSrc.indexOf(`pub ${field}:`);
      expect(idx, `state.rs missing field ${field}`).toBeGreaterThan(-1);
      const prefix = stateSrc.slice(Math.max(0, idx - 160), idx);
      expect(prefix, `state.rs field ${field} missing #[serde(default)]`).toContain(
        "#[serde(default)]",
      );
    }
  }, 300_000);
});
