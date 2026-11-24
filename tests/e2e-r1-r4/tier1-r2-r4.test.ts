import { get } from "svelte/store";
import { render } from "svelte/server";
import { afterEach, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { clearMocks } from "@tauri-apps/api/mocks";
import { mockIPCWithEvents } from "../../harness/tauriMocks";
import { projectAgentPlane } from "../../src/lib/agents/plane";
import { planeTerminals, repoAgentPill } from "../../src/lib/agents/repoAttention";
import AgentsView from "../../src/lib/components/AgentsView.svelte";
import {
  menuStateProblem,
  menuStateWireProblem,
  sendableMenuState,
} from "../../src/lib/desktop/menuContract";
import {
  countAgentsWaiting,
  type MenuAgentRequest,
  type MenuState,
} from "../../src/lib/desktop/menuState";
import { nativeMenuState } from "../../src/lib/desktop/menuStateStore";
import {
  dispatchNativeMenu,
  type NativeMenuHandlers,
} from "../../src/lib/desktop/nativeActions";
import StatusPopover from "../../src/lib/desktop/StatusPopover.svelte";
import { interfaceStore } from "../../src/lib/stores/interfaceStore";
import { repoStore } from "../../src/lib/stores/repoStore";
import {
  ATTENTION_EVENTS,
  activityToAgentStatus,
  asksForReader,
  createSessionActivity,
  parseAttention,
  sessionActivity,
  setSignalledAsksForReader,
  type SessionActivity,
} from "../../src/lib/terminal/sessionActivity";
import { terminalSessions } from "../../src/lib/terminal/sessionRegistry";
import { runBrowserPage } from "./helpers/browser";
import {
  assertCargoPassed,
  ensureGitPulseHookBinary,
  execCargoSuite,
  readRepoFile,
  runGitPulseHookWithSocket,
  stripRustCommentsAndStrings,
} from "./helpers/env";
import {
  agyNotification,
  agyPreInvocation,
  agySessionEnd,
  agyStop,
  claudeNotification,
  claudePermissionRequest,
  claudeStop,
  makeMenuAgentRequest,
  makeMenuStateWithRequests,
  makePlaneProbe,
  makePlaneTerminal,
} from "./helpers/fixtures";

const PATHS = { caseInsensitive: false };

async function withMockedLoadedRepo(
  currentPath: string,
  fn: () => Promise<void> | void,
): Promise<void> {
  const hadWindow = Object.prototype.hasOwnProperty.call(globalThis, "window");
  const prevWindow = (globalThis as { window?: unknown }).window;
  if (!hadWindow || !prevWindow) {
    (globalThis as { window: unknown }).window = {
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => true,
    };
  }

  mockIPCWithEvents((cmd, args) => {
    if (cmd.startsWith("plugin:")) return null;
    const payload = (args ?? {}) as Record<string, unknown>;
    switch (cmd) {
      case "cmd_resolve_repo":
        return {
          path: String(payload.repoPath ?? payload.path ?? currentPath),
          name: "GitPulse",
          is_bare: false,
          common_dir: `${currentPath}/.git`,
        };
      case "cmd_list_branches":
        return [
          {
            name: "main",
            is_current: true,
            is_remote: false,
            remote_name: null,
            tip_commit_id: "a".repeat(40),
            is_default: true,
            is_gone: false,
            last_commit_timestamp: Math.floor(Date.now() / 1000) - 600,
            last_author: "Alex",
            last_summary: "Work",
            upstream: "origin/main",
            ahead_count: 0,
            behind_count: 0,
          },
        ];
      case "cmd_get_status":
        return [];
      case "cmd_repo_operation":
        return null;
      case "cmd_list_tags":
        return { tags: [], truncated: false };
      case "cmd_stash_list":
        return { entries: [], truncated: false };
      case "cmd_list_worktrees":
        return [];
      case "cmd_get_commit_graph":
        return { nodes: [], edges: [], total_commits: 0, truncated: false };
      case "cmd_branch_stats":
        return { updates: [], capped: false, compute_failures: 0, compared_to: "main" };
      case "cmd_workspace_sync":
        return { repos: [] };
      case "cmd_list_repo_files":
        return [];
      case "cmd_task_view":
        return { available: false, store_path: "", leases: [], error: "" };
      case "cmd_worktree_task":
      case "cmd_watch_repo":
      case "cmd_unwatch_repo":
      case "cmd_last_fetch_at":
      case "cmd_note_tab_activated":
      case "cmd_set_recent_menu":
        return null;
      default:
        return null;
    }
  });

  try {
    const opened = await repoStore.openRepo(currentPath);
    expect(opened).toBe(true);
    await fn();
  } finally {
    let guard = 0;
    while (get(repoStore).activeTabId && guard++ < 10) {
      await repoStore.closeTab(get(repoStore).activeTabId!);
    }
    clearMocks();
    if (!hadWindow) {
      delete (globalThis as { window?: unknown }).window;
    } else {
      (globalThis as { window?: unknown }).window = prevWindow;
    }
  }
}

function createNoopHandlers(
  overrides: Record<string, unknown> = {},
): NativeMenuHandlers & Record<string, unknown> {
  const noop = () => {};
  return {
    activateRepo: noop,
    focusAgent: noop,
    clearRecents: noop,
    checkUpdates: noop,
    stageAll: noop,
    unstageAll: noop,
    createBranch: noop,
    renameBranch: noop,
    operationContinue: noop,
    operationAbort: noop,
    operationSkip: noop,
    copyRepoPath: noop,
    copyBranch: noop,
    copyCommit: noop,
    revealRepo: noop,
    openRemote: noop,
    open: noop,
    clone: noop,
    settings: noop,
    refresh: noop,
    toggleTheme: noop,
    themeSystem: noop,
    themeLight: noop,
    themeDark: noop,
    setTab: noop,
    shortcuts: noop,
    diagnostics: noop,
    documentation: noop,
    releaseNotes: noop,
    reportIssue: noop,
    setupTools: noop,
    zoomIn: noop,
    zoomOut: noop,
    resetZoom: noop,
    fleet: noop,
    agents: noop,
    terminalDock: noop,
    fetch: noop,
    pull: noop,
    push: noop,
    stash: noop,
    stashPop: noop,
    rebase: noop,
    quickCommit: noop,
    palette: noop,
    focusFilter: noop,
    openRecent: noop,
    openRepo: noop,
    closeRepoTab: noop,
    nextRepoTab: noop,
    prevRepoTab: noop,
    reopenRepoTab: noop,
    jumpRepoTab: noop,
    openError: noop,
    ...overrides,
  };
}

describe("[Tier 1] [R4] F7 — Claude Code Attention vs Informational Classification", () => {
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

  it("PermissionRequest and Notification(permission_prompt) emit blocking permission events with session and agent metadata", async () => {
    const providerUuid = "52d6abcd-6965-454b-a465-56a4a88526e7";
    const env = {
      GITPULSE_SESSION_ID: "term-f7-perm-1",
      GITPULSE_AGENT_KIND: undefined,
      GITPULSE_HOST: undefined,
    };

    const reqRes = await runGitPulseHookWithSocket({
      args: ["notify", "permission_request"],
      env,
      stdinJson: claudePermissionRequest({
        session_id: providerUuid,
        tool_name: "Bash",
        tool_input: { command: "git status" },
      }),
    });
    expect(reqRes.exitCode).toBe(0);
    expect(reqRes.stdout).toBe("");
    expect(reqRes.receivedJson).toHaveLength(1);
    expect(reqRes.receivedJson[0]).toMatchObject({
      v: 1,
      event: "permission_request",
      agent: "claude",
      session: "term-f7-perm-1",
      provider_session_id: providerUuid,
      hook_event: "PermissionRequest",
    });
    expect(typeof reqRes.receivedJson[0].subject).toBe("string");
    expect(String(reqRes.receivedJson[0].subject).length).toBeGreaterThan(0);

    const promptRes = await runGitPulseHookWithSocket({
      args: ["notify", "permission_prompt"],
      env,
      stdinJson: claudeNotification("permission_prompt", {
        session_id: providerUuid,
        message: "Claude needs your permission to edit src/main.rs",
      }),
    });
    expect(promptRes.exitCode).toBe(0);
    expect(promptRes.stdout).toBe("");
    expect(promptRes.receivedJson).toHaveLength(1);
    expect(promptRes.receivedJson[0]).toMatchObject({
      v: 1,
      event: "permission_prompt",
      agent: "claude",
      session: "term-f7-perm-1",
      provider_session_id: providerUuid,
      hook_event: "Notification",
      message: "Claude needs your permission to edit src/main.rs",
    });
  }, 300_000);

  it("Notification(agent_needs_input/elicitation_dialog) emit blocking events while Stop and Notification(agent_completed) emit finished events", async () => {
    const env = {
      GITPULSE_SESSION_ID: "term-f7-events-2",
      GITPULSE_AGENT_KIND: undefined,
      GITPULSE_HOST: undefined,
    };

    const cases = [
      {
        arg: "agent_needs_input",
        stdin: claudeNotification("agent_needs_input"),
        expectedEvent: "agent_needs_input",
      },
      {
        arg: "elicitation_dialog",
        stdin: claudeNotification("elicitation_dialog"),
        expectedEvent: "elicitation_dialog",
      },
      {
        arg: "turn_finished",
        stdin: claudeStop({ last_assistant_message: "Finished editing handlers." }),
        expectedEvent: "turn_finished",
      },
      {
        arg: "agent_completed",
        stdin: claudeNotification("agent_completed"),
        expectedEvent: "agent_completed",
      },
    ] as const;

    for (const entry of cases) {
      const res = await runGitPulseHookWithSocket({
        args: ["notify", entry.arg],
        env,
        stdinJson: entry.stdin,
      });
      expect(res.exitCode, entry.arg).toBe(0);
      expect(res.stdout, entry.arg).toBe("");
      expect(res.receivedJson, entry.arg).toHaveLength(1);
      expect(res.receivedJson[0]).toMatchObject({
        v: 1,
        event: entry.expectedEvent,
        agent: "claude",
        session: "term-f7-events-2",
      });
    }
  }, 300_000);

  it("Notification(idle_prompt), turn_finished, and agent_completed emit over socket and classify as informational (not asksForReader / not Needs Action)", async () => {
    const res = await runGitPulseHookWithSocket({
      args: ["notify", "idle_prompt"],
      env: {
        GITPULSE_SESSION_ID: "term-f7-idle-3",
        GITPULSE_AGENT_KIND: undefined,
        GITPULSE_HOST: undefined,
      },
      stdinJson: claudeNotification("idle_prompt"),
    });
    expect(res.exitCode).toBe(0);
    expect(res.receivedJson).toHaveLength(1);
    expect(res.receivedJson[0]).toMatchObject({
      v: 1,
      event: "idle_prompt",
      agent: "claude",
      session: "term-f7-idle-3",
    });

    for (const ev of ["idle_prompt", "turn_finished", "agent_completed"] as const) {
      const parsed = parseAttention({ session: "term-f7-idle-3", channel: "hook", event: ev }, 1_000);
      expect(parsed?.attention, ev).not.toBeNull();
      const att = parsed!.attention as unknown as Record<string, unknown>;
      expect(att.urgency, `${ev} urgency`).toBe("informational");
      expect(asksForReader(parsed!.attention!.kind), `${ev} asksForReader`).toBe(false);
      const status = activityToAgentStatus({
        lastOutputAt: null,
        title: null,
        attention: parsed!.attention,
      });
      expect(status?.state, `${ev} status`).not.toBe("blocked");
    }

    const plane = projectAgentPlane({
      probes: [makePlaneProbe("/repo")],
      terminals: [
        makePlaneTerminal({
          cwd: "/repo/.claude/worktrees/alpha",
          attention: "finished",
        }),
      ],
      tasks: null,
      paths: PATHS,
    });
    expect(plane.attention.needing).toBe(0);
  }, 300_000);

  it("parseAttention preserves urgency, event, channel, detail, and at without dropping them", () => {
    const cases = [
      {
        raw: {
          session: "term-a",
          channel: "hook",
          event: "permission_prompt",
          urgency: "blocking",
          detail: "Allow Bash: cargo test",
        },
        expectedUrgency: "blocking",
        expectedEvent: "permission_prompt",
      },
      {
        raw: {
          session: "term-b",
          channel: "hook",
          event: "error",
          urgency: "error",
          detail: "Rate limit exceeded",
        },
        expectedUrgency: "error",
        expectedEvent: "error",
      },
      {
        raw: {
          session: "term-c",
          channel: "hook",
          event: "idle_prompt",
          urgency: "informational",
          detail: "Waiting for next instruction",
        },
        expectedUrgency: "informational",
        expectedEvent: "idle_prompt",
      },
      {
        raw: {
          session: "term-d",
          channel: "hook",
          event: "turn_finished",
          urgency: "informational",
          detail: "Turn complete.",
        },
        expectedUrgency: "informational",
        expectedEvent: "turn_finished",
      },
      {
        raw: {
          session: "term-e",
          channel: "osc9",
          event: null,
          urgency: "informational",
          detail: "Tests finished",
        },
        expectedUrgency: "informational",
        expectedEvent: null,
      },
    ] as const;

    const activity = createSessionActivity(() => 42_000);
    for (const c of cases) {
      const parsed = parseAttention(c.raw, 42_000);
      expect(parsed?.attention, String(c.raw.session)).toBeTruthy();
      const att = parsed!.attention as unknown as Record<string, unknown>;
      expect(att.urgency, `${c.raw.session} urgency`).toBe(c.expectedUrgency);
      expect(att.event, `${c.raw.session} event`).toBe(c.expectedEvent);
      expect(att.channel, `${c.raw.session} channel`).toBe(c.raw.channel);
      expect(att.detail, `${c.raw.session} detail`).toBe(c.raw.detail);
      expect(att.at, `${c.raw.session} at`).toBe(42_000);

      expect(activity.announce(c.raw)).toBe(true);
      const stored = get(activity).get(c.raw.session)?.attention as unknown as
        | Record<string, unknown>
        | undefined;
      expect(stored?.urgency, `${c.raw.session} stored urgency`).toBe(c.expectedUrgency);
      expect(stored?.event, `${c.raw.session} stored event`).toBe(c.expectedEvent);
      expect(stored?.channel, `${c.raw.session} stored channel`).toBe(c.raw.channel);
    }
  });

  it("Cargo alerts:: unit suite passes including idle_prompt informational urgency and post-HOOK_SHADOW bare BEL guard", async () => {
    const res = await execCargoSuite(["--lib", "alerts::"], { timeoutMs: 300_000 });
    assertCargoPassed(res, "cargo test --lib alerts::");
  }, 300_000);

  it("Cargo hooks notification/socket/route unit tests pass deterministically", async () => {
    const hooksRes = await execCargoSuite(
      [
        "--lib",
        "--",
        "--test-threads=1",
        "hooks::notify",
        "hooks::adv_l5_tests",
        "hooks::adv_w3c_tests",
        "hooks::tests::a_permission_request_and_its_result",
        "hooks::tests::the_shipped_manifest",
        "hooks::tests::what_the_hook_sends_is_what_the_socket_accepts",
        "hooks::tests::antigravity_a2a_tests",
      ],
      { timeoutMs: 300_000 },
    );
    assertCargoPassed(hooksRes, "cargo test --lib hooks notification/socket/route suites");
  }, 300_000);

  it("Cargo agent_notify_socket integration suite passes independently", async () => {
    const socketRes = await execCargoSuite(["--test", "agent_notify_socket"], { timeoutMs: 300_000 });
    assertCargoPassed(socketRes, "cargo test --test agent_notify_socket");
  }, 300_000);
});

describe("[Tier 1] [R4] F8 — Antigravity (agy) Hook Parity & Lifecycle", () => {
  beforeAll(async () => {
    await ensureGitPulseHookBinary();
  }, 300_000);

  it("plugins/gitpulse/hooks.json and plugins/gitpulse-agy/hooks.json align on Antigravity lifecycle hooks and host parity", () => {
    const embedded = JSON.parse(readRepoFile("plugins/gitpulse/hooks.json")) as {
      gitpulse: Record<string, unknown[]>;
    };
    const agyPlugin = JSON.parse(readRepoFile("plugins/gitpulse-agy/hooks.json")) as {
      gitpulse: Record<string, unknown[]>;
    };

    const requiredEvents = [
      "SessionStart",
      "PreInvocation",
      "PostInvocation",
      "PostToolUse",
      "Stop",
    ];

    for (const event of requiredEvents) {
      expect(embedded.gitpulse[event], `plugins/gitpulse/hooks.json missing ${event}`).toBeDefined();
      expect(agyPlugin.gitpulse[event], `plugins/gitpulse-agy/hooks.json missing ${event}`).toBeDefined();
    }

    // Antigravity (`agy plugin validate --strict`) recognizes only SessionStart, PreInvocation,
    // PostInvocation, Stop, PreToolUse, PostToolUse; Notification/SessionEnd fail strict validation,
    // and PreToolUse is intentionally omitted (Antigravity IDE 2.32.1 denies non-"allow" answers).
    for (const disallowed of ["PreToolUse", "Notification", "SessionEnd"]) {
      expect(embedded.gitpulse[disallowed], `plugins/gitpulse/hooks.json unexpected ${disallowed}`).toBeUndefined();
      expect(agyPlugin.gitpulse[disallowed], `plugins/gitpulse-agy/hooks.json unexpected ${disallowed}`).toBeUndefined();
    }

    const extractCommands = (root: Record<string, unknown[]>, event: string) => {
      const items = root[event] as Array<Record<string, unknown>>;
      const out: Array<{ command: string; env?: Record<string, string> }> = [];
      for (const item of items) {
        if (Array.isArray(item.hooks)) {
          for (const nested of item.hooks as Array<Record<string, unknown>>) {
            out.push({
              command: String(nested.command ?? ""),
              env: nested.env as Record<string, string> | undefined,
            });
          }
        } else {
          out.push({
            command: String(item.command ?? ""),
            env: item.env as Record<string, string> | undefined,
          });
        }
      }
      return out;
    };

    expect(extractCommands(embedded.gitpulse, "SessionStart").some((c) => c.command.includes("session-brief"))).toBe(true);
    expect(extractCommands(agyPlugin.gitpulse, "SessionStart").some((c) => c.command.includes("session-brief"))).toBe(true);
    expect(extractCommands(embedded.gitpulse, "SessionStart").some((c) => c.command.includes("peer-inbox"))).toBe(true);
    expect(extractCommands(agyPlugin.gitpulse, "SessionStart").some((c) => c.command.includes("peer-inbox"))).toBe(true);

    // PreInvocation & PostInvocation run peer-inbox (emitting prompt_submitted only on fresh turns).
    for (const invocationEvent of ["PreInvocation", "PostInvocation"] as const) {
      expect(
        extractCommands(embedded.gitpulse, invocationEvent).some((c) => c.command.includes("peer-inbox")),
        `plugins/gitpulse/hooks.json ${invocationEvent} missing peer-inbox`,
      ).toBe(true);
      expect(
        extractCommands(agyPlugin.gitpulse, invocationEvent).some((c) => c.command.includes("peer-inbox")),
        `plugins/gitpulse-agy/hooks.json ${invocationEvent} missing peer-inbox`,
      ).toBe(true);
    }

    expect(extractCommands(embedded.gitpulse, "PostToolUse").some((c) => c.command.includes("notify tool_finished"))).toBe(true);
    expect(extractCommands(agyPlugin.gitpulse, "PostToolUse").some((c) => c.command.includes("notify tool_finished"))).toBe(true);

    // Stop must run stop-or-notify on both manifests.
    expect(extractCommands(embedded.gitpulse, "Stop").some((c) => c.command.includes("stop-or-notify"))).toBe(true);
    expect(extractCommands(agyPlugin.gitpulse, "Stop").some((c) => c.command.includes("stop-or-notify"))).toBe(true);

    // Embedded hooks.json uses --host=agy consistently; gitpulse-agy uses GITPULSE_HOST=antigravity.
    for (const event of requiredEvents) {
      for (const entry of extractCommands(embedded.gitpulse, event)) {
        expect(entry.command, `embedded ${event}`).toContain("--host=agy");
      }
      for (const entry of extractCommands(agyPlugin.gitpulse, event)) {
        expect(entry.env?.GITPULSE_HOST, `agy ${event}`).toBe("antigravity");
      }
    }
  });

  it("gitpulse-hook with agy PreInvocation payload emits prompt_submitted and clears prompt state", async () => {
    const convId = "91a8ef12-3456-4789-abcd-ef0123456789";
    const res = await runGitPulseHookWithSocket({
      args: ["--host=agy", "notify", "prompt_submitted"],
      env: {
        GITPULSE_SESSION_ID: "pty-agy-f8-1",
        GITPULSE_HOST: "antigravity",
        GITPULSE_AGENT_KIND: undefined,
      },
      stdinJson: agyPreInvocation({ conversationId: convId }),
    });
    expect(res.exitCode).toBe(0);
    expect(res.stdout.trim()).toBe("{}");
    expect(res.receivedJson).toHaveLength(1);
    expect(res.receivedJson[0]).toMatchObject({
      v: 1,
      event: "prompt_submitted",
      agent: "agy",
      session: "pty-agy-f8-1",
      hook_event: "PreInvocation",
      provider_session_id: convId,
    });
  }, 300_000);

  it("gitpulse-hook with agy Stop emits turn_finished (fully_idle) for NO_TOOL_CALL and error for failure terminationReason", async () => {
    const env = {
      GITPULSE_SESSION_ID: "pty-agy-f8-stop",
      GITPULSE_HOST: "antigravity",
      GITPULSE_AGENT_KIND: undefined,
    };

    const idleRes = await runGitPulseHookWithSocket({
      args: ["--host=agy", "stop-or-notify"],
      env,
      stdinJson: agyStop({
        terminationReason: "NO_TOOL_CALL",
        fullyIdle: true,
        message: "Ready for review.",
      }),
    });
    expect(idleRes.exitCode).toBe(0);
    expect(idleRes.stdout.trim()).toBe("{}");
    expect(idleRes.receivedJson).toHaveLength(1);
    expect(idleRes.receivedJson[0]).toMatchObject({
      v: 1,
      event: "turn_finished",
      agent: "agy",
      session: "pty-agy-f8-stop",
      fully_idle: true,
    });

    const errRes = await runGitPulseHookWithSocket({
      args: ["--host=agy", "stop-or-notify"],
      env,
      stdinJson: agyStop({
        terminationReason: "ERROR",
        fullyIdle: false,
        error: "Provider RPC timed out",
      }),
    });
    expect(errRes.exitCode).toBe(0);
    expect(errRes.stdout.trim()).toBe("{}");
    expect(errRes.receivedJson).toHaveLength(1);
    expect(errRes.receivedJson[0]).toMatchObject({
      v: 1,
      event: "error",
      agent: "agy",
      session: "pty-agy-f8-stop",
      fully_idle: false,
      message: "Provider RPC timed out",
    });
  }, 300_000);

  it("gitpulse-hook with agy Notification emits distinct blocking and informational events", async () => {
    const env = {
      GITPULSE_SESSION_ID: "pty-agy-f8-notif",
      GITPULSE_HOST: "antigravity",
      GITPULSE_AGENT_KIND: undefined,
    };

    for (const matcher of ["permission_prompt", "agent_completed", "idle_prompt"] as const) {
      const res = await runGitPulseHookWithSocket({
        args: ["--host=agy", "notify", matcher],
        env,
        stdinJson: agyNotification({
          message: `Antigravity ${matcher} notice`,
        }),
      });
      expect(res.exitCode, matcher).toBe(0);
      expect(res.stdout.trim(), matcher).toBe("{}");
      expect(res.receivedJson, matcher).toHaveLength(1);
      expect(res.receivedJson[0]).toMatchObject({
        v: 1,
        event: matcher,
        agent: "agy",
        session: "pty-agy-f8-notif",
        message: `Antigravity ${matcher} notice`,
      });
    }

    const permParsed = parseAttention(
      { session: "pty-agy-f8-notif", channel: "hook", event: "permission_prompt" },
      1_000,
    );
    const doneParsed = parseAttention(
      { session: "pty-agy-f8-notif", channel: "hook", event: "agent_completed" },
      1_000,
    );
    const idleParsed = parseAttention(
      { session: "pty-agy-f8-notif", channel: "hook", event: "idle_prompt" },
      1_000,
    );
    expect(asksForReader(permParsed?.attention?.kind)).toBe(true);
    expect(asksForReader(doneParsed?.attention?.kind)).toBe(false);
    expect(asksForReader(idleParsed?.attention?.kind)).toBe(false);
  }, 300_000);

  it("gitpulse-hook with agy SessionEnd emits session_ended", async () => {
    const res = await runGitPulseHookWithSocket({
      args: ["--host=agy", "notify", "session_ended"],
      env: {
        GITPULSE_SESSION_ID: "pty-agy-f8-end",
        GITPULSE_HOST: "antigravity",
        GITPULSE_AGENT_KIND: undefined,
      },
      stdinJson: agySessionEnd(),
    });
    expect(res.exitCode).toBe(0);
    expect(res.stdout.trim()).toBe("{}");
    expect(res.receivedJson).toHaveLength(1);
    expect(res.receivedJson[0]).toMatchObject({
      v: 1,
      event: "session_ended",
      agent: "agy",
      session: "pty-agy-f8-end",
      hook_event: "SessionEnd",
    });
  }, 300_000);

  it("plugins/gitpulse/hooks/hooks.json, scripts/fixtures/notify-routes.json, bridge::EVENTS, and ATTENTION_EVENTS remain in 1:1 parity", () => {
    const claudeHooks = JSON.parse(readRepoFile("plugins/gitpulse/hooks/hooks.json")) as {
      hooks: Record<string, Array<{ matcher?: string; hooks: Array<{ command?: string }> }>>;
    };
    const routesFixture = JSON.parse(readRepoFile("scripts/fixtures/notify-routes.json")) as {
      routes: Array<{ hook: string; matcher: string | null; event: string; injected: boolean }>;
    };
    const bridgeSrc = readRepoFile("src-tauri/src/alerts/bridge.rs");
    const tableSlice = bridgeSrc.slice(
      bridgeSrc.indexOf("pub const EVENTS"),
      bridgeSrc.indexOf("];", bridgeSrc.indexOf("pub const EVENTS")),
    );
    const bridgeEvents = [...tableSlice.matchAll(/name:\s*"([a-z_]+)"/g)].map((m) => m[1]);

    expect(bridgeEvents.length).toBeGreaterThanOrEqual(12);
    expect(new Set(routesFixture.routes.map((r) => r.event))).toEqual(new Set(bridgeEvents));
    expect(Object.keys(ATTENTION_EVENTS).sort()).toEqual([...bridgeEvents].sort());

    for (const route of routesFixture.routes) {
      const entries = claudeHooks.hooks[route.hook] ?? [];
      const matching = entries.filter((entry) =>
        route.matcher ? entry.matcher === route.matcher : !entry.matcher,
      );
      const hasCommand = matching.some((entry) =>
        entry.hooks.some((h) => String(h.command ?? "").includes(`notify ${route.event}`)),
      );
      expect(hasCommand, `hooks.json missing route ${route.hook}/${route.matcher} -> ${route.event}`).toBe(
        true,
      );
    }
  });
});

describe("[Tier 1] [R4] F9 — Agents Fleet Pane UX/UI & Dismiss/Focus Actions", () => {
  it("Svelte 5 SSR AgentsView renders LauncherIcon markers for Claude and Antigravity agents", () => {
    const slots: ReturnType<typeof terminalSessions.reserve>[] = [];
    try {
      slots.push(
        terminalSessions.reserve({
          key: "ssr-claude-f9",
          repoPath: "/code/GitPulse",
          label: "Claude Code",
          launcher: "claude",
          title: "claude-ssr",
          status: "running",
          sessionId: "pty-ssr-claude-f9",
          close: async () => {},
        }),
      );
      slots.push(
        terminalSessions.reserve({
          key: "ssr-agy-f9",
          repoPath: "/code/GitPulse",
          label: "Antigravity",
          launcher: "agy",
          title: "agy-ssr",
          status: "running",
          sessionId: "pty-ssr-agy-f9",
          close: async () => {},
        }),
      );

      const html = render(AgentsView).body;
      expect(html).toContain("claude-ssr");
      expect(html).toContain("agy-ssr");
      expect(html).toContain('data-launcher-icon="claude"');
      expect(html).toContain('data-launcher-icon="agy"');
    } finally {
      while (slots.length > 0) slots.pop()?.release();
    }
  });

  it("AgentsView visually separates blocking Needs Action from passive Notifications using semantic gp-chip badges and filters", () => {
    const slots: ReturnType<typeof terminalSessions.reserve>[] = [];
    try {
      slots.push(
        terminalSessions.reserve({
          key: "ssr-block-f9",
          repoPath: "/code/GitPulse",
          label: "Claude Code",
          launcher: "claude",
          title: "claude-blocking",
          status: "running",
          sessionId: "pty-ssr-block-f9",
          close: async () => {},
        }),
      );
      slots.push(
        terminalSessions.reserve({
          key: "ssr-info-f9",
          repoPath: "/code/GitPulse",
          label: "Antigravity",
          launcher: "agy",
          title: "agy-informational",
          status: "running",
          sessionId: "pty-ssr-info-f9",
          close: async () => {},
        }),
      );

      sessionActivity.announce({
        session: "pty-ssr-block-f9",
        channel: "hook",
        event: "permission_prompt",
        detail: "Allow Bash: cargo test",
      });
      sessionActivity.announce({
        session: "pty-ssr-info-f9",
        channel: "hook",
        event: "turn_finished",
        detail: "Completed plan synthesis",
      });

      const html = render(AgentsView).body;
      expect(html).toMatch(
        /<[^>]*\bclass="[^"]*\bgp-chip\b[^"]*"[^>]*\bdata-tone="(warn|danger)"|<[^>]*\bdata-tone="(warn|danger)"[^>]*\bclass="[^"]*\bgp-chip\b[^"]*"/,
      );
      expect(html).toMatch(
        /<[^>]*\bclass="[^"]*\bgp-chip\b[^"]*"[^>]*\bdata-tone="(info|neutral)"|<[^>]*\bdata-tone="(info|neutral)"[^>]*\bclass="[^"]*\bgp-chip\b[^"]*"/,
      );
      expect(html).toMatch(/data-testid="agents-filter-(notifications|notices)"/i);
    } finally {
      sessionActivity.forget("pty-ssr-block-f9");
      sessionActivity.forget("pty-ssr-info-f9");
      while (slots.length > 0) slots.pop()?.release();
    }
  });

  it("AgentsView displays inline detail preview and timestamp per agent row", () => {
    const slots: ReturnType<typeof terminalSessions.reserve>[] = [];
    try {
      slots.push(
        terminalSessions.reserve({
          key: "ssr-detail-f9",
          repoPath: "/code/GitPulse",
          label: "Claude Code",
          launcher: "claude",
          title: "claude-detail-row",
          status: "running",
          sessionId: "pty-ssr-detail-f9",
          close: async () => {},
        }),
      );
      sessionActivity.announce({
        session: "pty-ssr-detail-f9",
        channel: "hook",
        event: "permission_prompt",
        detail: "Allow Write to src/lib/agents/plane.ts",
      });

      const html = render(AgentsView).body;
      expect(html).toContain("Allow Write to src/lib/agents/plane.ts");
      expect(html).toMatch(/just now|\b\d+[smhd]\s+ago\b|<time\b/i);
    } finally {
      sessionActivity.forget("pty-ssr-detail-f9");
      while (slots.length > 0) slots.pop()?.release();
    }
  });

  it("AgentsView provides a Dismiss / Acknowledge control for informational notifications without disrupting running terminals", () => {
    const slots: ReturnType<typeof terminalSessions.reserve>[] = [];
    try {
      slots.push(
        terminalSessions.reserve({
          key: "ssr-notice-f9",
          repoPath: "/code/GitPulse",
          label: "Antigravity",
          launcher: "agy",
          title: "agy-notice-row",
          status: "running",
          sessionId: "pty-ssr-notice-f9",
          close: async () => {},
        }),
      );
      sessionActivity.announce({
        session: "pty-ssr-notice-f9",
        channel: "hook",
        event: "agent_completed",
        detail: "Finished code review",
      });

      const html = render(AgentsView).body;
      expect(html).toMatch(
        /data-action="(?:dismiss|acknowledge|clear)"|aria-label="[^"]*(?:dismiss|acknowledge|clear)[^"]*"|<button[^>]*>(?:<[^>]+>|\s)*(?:Dismiss|Acknowledge|Clear)\b/i,
      );
    } finally {
      sessionActivity.forget("pty-ssr-notice-f9");
      while (slots.length > 0) slots.pop()?.release();
    }
  });

  it("plane.ts and repoAttention.ts do not inflate attention.needing or repo blocking asks for informational states", () => {
    const terminals = [
      makePlaneTerminal({
        key: "t-done",
        sessionId: "s-done",
        cwd: "/repo/.claude/worktrees/alpha",
        attention: "finished",
      }),
    ];
    const plane = projectAgentPlane({
      probes: [makePlaneProbe("/repo")],
      terminals,
      tasks: null,
      paths: PATHS,
    });
    expect(plane.attention.needing).toBe(0);

    const mapped = planeTerminals(
      [
        {
          key: "t-done",
          repoPath: "/repo",
          label: "Antigravity",
          status: "running",
          sessionId: "s-done",
        },
      ],
      new Map([
        [
          "s-done",
          {
            title: "Turn finished",
            attention: { kind: "finished" as const },
          },
        ],
      ]),
      new Map([["s-done", "/repo/.claude/worktrees/alpha"]]),
    );
    const pillPlane = projectAgentPlane({
      probes: [makePlaneProbe("/repo")],
      terminals: mapped,
      tasks: null,
      paths: PATHS,
    });
    const pill = repoAgentPill({
      plane: pillPlane,
      probes: [makePlaneProbe("/repo")],
      repoPath: "/repo",
      paths: PATHS,
    });
    // Informational finished state must not appear as blocking "attention" ask.
    expect(pill.state).not.toBe("attention");
  });

  it("Browser E2E agents-fleet.html?scenario=mixed passes all UI, filter, LauncherIcon, gp-chip, and focus checks", async () => {
    const verdict = await runBrowserPage({
      page: "tests/e2e-r1-r4/browser/agents-fleet.html",
      query: { scenario: "mixed" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(verdict.total).toBeGreaterThanOrEqual(7);
    expect(
      verdict.passed,
      `Failed browser checks:\n${verdict.results
        .filter((r) => !r.pass)
        .map((r) => `- ${r.name}${r.detail ? `: ${r.detail}` : ""}`)
        .join("\n")}`,
    ).toBe(verdict.total);
  }, 90_000);
});

describe("[Tier 1] [R2] F10 — Menu Bar Agent Requests, Waiting States & FocusAgent", () => {
  const savedBell = get(interfaceStore).bellAsksForReader;

  beforeEach(() => {
    setSignalledAsksForReader(false);
  });

  afterEach(() => {
    interfaceStore.setBellAsksForReader(savedBell);
  });

  it("MenuState projection includes agentRequests, agentRequestsHidden, agentsRunning, agentsInformational, and agentsWaiting for blocking/error only", async () => {
    const tracked = ["t1-f10-block", "t1-f10-err", "t1-f10-info-1", "t1-f10-info-2", "t1-f10-quiet"];
    try {
      await withMockedLoadedRepo("/Projects/GitPulse", () => {
        sessionActivity.announce({
          session: "t1-f10-block",
          channel: "hook",
          event: "permission_prompt",
          detail: "Allow Bash: git status",
        });
        sessionActivity.announce({
          session: "t1-f10-err",
          channel: "hook",
          event: "error",
          detail: "Tool execution failed",
        });
        sessionActivity.announce({
          session: "t1-f10-info-1",
          channel: "hook",
          event: "turn_finished",
          detail: "Turn complete",
        });
        sessionActivity.announce({
          session: "t1-f10-info-2",
          channel: "hook",
          event: "idle_prompt",
          detail: "Idle reminder",
        });
        sessionActivity.output("t1-f10-quiet");

        const map = get(sessionActivity) as ReadonlyMap<string, SessionActivity>;
        expect(countAgentsWaiting(map)).toBe(2);

        const built = get(nativeMenuState) as unknown as Record<string, unknown>;
        expect(built.agentsWaiting).toBe(2);
        expect(Array.isArray(built.agentRequests)).toBe(true);
        expect((built.agentRequests as unknown[]).length).toBe(2);
        expect(built.agentRequestsHidden).toBe(0);
        expect(typeof built.agentsRunning).toBe("number");
        expect(built.agentsInformational).toBe(2);
      });
    } finally {
      for (const sid of tracked) sessionActivity.forget(sid);
    }
  });

  it("StatusPopover SSR and browser waiting scenario render waiting-for-action agent requests with agent/repo context", async () => {
    const reqs: MenuAgentRequest[] = [
      makeMenuAgentRequest({
        id: "term:pty-claude-1",
        source: "terminal",
        provider: "claude",
        agentLabel: "claude-main",
        repoPath: "/Projects/GitPulse",
        repoLabel: "GitPulse",
        urgency: "blocking",
        reason: "Needs your permission",
        detail: "Allow Bash: cargo test",
        atMs: Date.now() - 10_000,
      }),
      makeMenuAgentRequest({
        id: "term:pty-agy-2",
        source: "terminal",
        provider: "agy",
        agentLabel: "agy-review",
        repoPath: "/Projects/DevCouncil",
        repoLabel: "DevCouncil",
        urgency: "blocking",
        reason: "Needs your input",
        detail: "Choose target deployment environment",
        atMs: Date.now() - 25_000,
      }),
    ];
    const snapshot: MenuState = makeMenuStateWithRequests(reqs, {
      agentsRunning: 3,
      agentsInformational: 1,
    });

    const { body } = render(StatusPopover, {
      props: { snapshot, onaction: () => {} },
    });
    expect(body).toContain("claude-main");
    expect(body).toContain("Needs your permission");
    expect(body).toContain("Allow Bash: cargo test");
    expect(body).toContain("DevCouncil");
    expect(body).toContain("Choose target deployment environment");
    expect(body).toContain("<time");

    const verdict = await runBrowserPage({
      page: "tests/e2e-r1-r4/browser/status-popover.html",
      query: { scenario: "waiting" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(
      verdict.passed,
      `Failed waiting popover checks:\n${verdict.results
        .filter((r) => !r.pass)
        .map((r) => `- ${r.name}${r.detail ? `: ${r.detail}` : ""}`)
        .join("\n")}`,
    ).toBe(verdict.total);
  }, 90_000);

  it("Clicking an agent request dispatches focus-agent:<id> via cmd_status_action and dispatchNativeMenu routes focus-agent", async () => {
    const verdict = await runBrowserPage<{
      dispatchedActions?: Array<{ id: string; repoPath: string | null }>;
    }>({
      page: "tests/e2e-r1-r4/browser/status-popover.html",
      query: { scenario: "waiting" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(
      verdict.data?.dispatchedActions?.some((a) => a.id.startsWith("focus-agent:")),
      `dispatchedActions: ${JSON.stringify(verdict.data?.dispatchedActions)}`,
    ).toBe(true);

    const focused: string[] = [];
    const handled = dispatchNativeMenu(
      { id: "focus-agent", path: "req-1", repo_path: "/Projects/GitPulse" },
      createNoopHandlers({
        focusAgent: (id: string) => focused.push(id),
      }),
    );
    expect(handled).toBe(true);
    expect(focused).toEqual(["req-1"]);
  }, 90_000);

  it("StatusPopover browser none scenario cleanly displays healthy state when zero agents are waiting", async () => {
    const verdict = await runBrowserPage<{ renderedRequestButtons?: number }>({
      page: "tests/e2e-r1-r4/browser/status-popover.html",
      query: { scenario: "none" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(verdict.data?.renderedRequestButtons).toBe(0);
    expect(
      verdict.passed,
      `Failed none popover checks:\n${verdict.results
        .filter((r) => !r.pass)
        .map((r) => `- ${r.name}${r.detail ? `: ${r.detail}` : ""}`)
        .join("\n")}`,
    ).toBe(verdict.total);
  }, 90_000);

  it("Rust acl_contract, desktop_menu_integration, and native_menu_main_thread integration suites pass", async () => {
    const res = await execCargoSuite(
      [
        "--test",
        "acl_contract",
        "--test",
        "desktop_menu_integration",
        "--test",
        "native_menu_main_thread",
      ],
      { timeoutMs: 300_000 },
    );
    assertCargoPassed(res, "acl_contract + desktop_menu_integration + native_menu_main_thread");
  }, 300_000);

  it("Rust desktop::state, desktop::tray, and desktop::popover unit tests pass and tray surfaces agent requests", async () => {
    const res = await execCargoSuite(["--lib", "desktop::"], { timeoutMs: 300_000 });
    assertCargoPassed(res, "cargo test --lib desktop::");

    const traySrc = readRepoFile("src-tauri/src/desktop/tray.rs");
    const trayTestIdx = traySrc.indexOf("#[cfg(test)]");
    const trayProdCode = stripRustCommentsAndStrings(
      trayTestIdx > -1 ? traySrc.slice(0, trayTestIdx) : traySrc,
    );
    expect(trayProdCode).toContain("tray_tooltip");
    expect(trayProdCode).toContain("build_tray_menu");
    expect(trayProdCode).toContain("agent_requests");

    // Verify sendableMenuState keeps valid request-carrying MenuState clean.
    const valid = makeMenuStateWithRequests([
      makeMenuAgentRequest({
        id: "term:s-1",
        source: "terminal",
        provider: "claude",
        agentLabel: "claude-1",
        repoPath: "/Projects/GitPulse",
        repoLabel: "GitPulse",
        urgency: "blocking",
        reason: "Needs your permission",
        detail: "Allow Bash",
        atMs: Date.now(),
      }),
    ]);
    expect(menuStateWireProblem(valid)).toBeNull();
    expect(menuStateProblem(valid)).toBeNull();
    expect(sendableMenuState(valid).problem).toBeNull();
  }, 300_000);
});
