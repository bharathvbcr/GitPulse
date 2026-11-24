import { execFileSync } from "node:child_process";
import { get } from "svelte/store";
import { render } from "svelte/server";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { clearMocks } from "@tauri-apps/api/mocks";
import { mockIPCWithEvents } from "../../harness/tauriMocks";
import StatusPopover from "../../src/lib/desktop/StatusPopover.svelte";
import { canDispatchMenuEvent, type MenuAgentRequest } from "../../src/lib/desktop/menuState";
import { sendableMenuState } from "../../src/lib/desktop/menuContract";
import { nativeMenuState } from "../../src/lib/desktop/menuStateStore";
import {
  dispatchNativeMenu,
  type NativeMenuHandlers,
} from "../../src/lib/desktop/nativeActions";
import { repoStore } from "../../src/lib/stores/repoStore";
import { interfaceStore } from "../../src/lib/stores/interfaceStore";
import {
  asksForReader,
  sessionActivity,
  type SessionAttention,
} from "../../src/lib/terminal/sessionActivity";
import {
  openConflicts,
  takeConflictsFocus,
  takeConflictsReveal,
} from "../../src/lib/ui/workConflicts";
import { runBrowserPage } from "./helpers/browser";
import {
  assertCargoPassed,
  assertTrayPulseIconUpdated,
  ensureGitPulseHookBinary,
  execCargoSuite,
  readPngInfo,
  readRepoFile,
  repoPath,
  runGitPulseHookWithSocket,
} from "./helpers/env";
import {
  agyPostToolUse,
  agyPreInvocation,
  agyStop,
  claudeNotification,
  claudePermissionRequest,
  claudePostToolUse,
  makeMenuAgentRequest,
  makeMenuStateWithRequests,
  type MenuStateFixture,
} from "./helpers/fixtures";

const LOGO_TRUNK_FRAGMENT = "M24.04 9.25A10.5";
const LEGACY_PLUGIN_LOGO_SHA256 =
  "eb41940073257e695b51f513061f56446da9b4fdf995a6aa06d41387ea7e6ea1";
const PATHS = { caseInsensitive: false };

function assertMenuRequestMatches(
  req: MenuAgentRequest | undefined,
  expectedSession: string,
  expectedUrgency: MenuAgentRequest["urgency"],
  expectedDetail?: string,
): void {
  expect(req).toBeDefined();
  expect(req!.id).toBe(`term:${expectedSession}`);
  expect(req!.source).toBe("terminal");
  expect(req!.urgency).toBe(expectedUrgency);
  expect(typeof req!.provider).toBe("string");
  expect(req!.agentLabel.length).toBeGreaterThan(0);
  expect(req!.reason.length).toBeGreaterThan(0);
  expect(req!.repoPath === null || req!.repoPath.length > 0).toBe(true);
  if (expectedDetail !== undefined) {
    expect(req!.detail).toContain(expectedDetail);
  }
}

async function withMockedConflictedAndCleanRepos(
  repoA: string,
  repoB: string,
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
    const reqRepo = String(payload.repoPath ?? payload.path ?? repoA);
    switch (cmd) {
      case "cmd_resolve_repo":
        return {
          path: reqRepo,
          name: reqRepo === repoB ? "DevCouncil" : "GitPulse",
          is_bare: false,
          common_dir: `${reqRepo}/.git`,
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
        if (reqRepo === repoA) {
          return [
            {
              path: "src/merge.ts",
              status_code: "UU",
              is_staged: false,
              is_conflicted: true,
              additions: 0,
              deletions: 0,
            },
            {
              path: "src/conflict.ts",
              status_code: "UU",
              is_staged: false,
              is_conflicted: true,
              additions: 0,
              deletions: 0,
            },
          ];
        }
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
    expect(await repoStore.openRepo(repoB)).toBe(true);
    expect(await repoStore.openRepo(repoA)).toBe(true);
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

function runBunScript(script: string): { ok: boolean; stdout: string; stderr: string } {
  try {
    const stdout = execFileSync("bun", ["run", script], {
      cwd: repoPath("."),
      encoding: "utf8",
      stdio: ["ignore", "pipe", "pipe"],
    });
    return { ok: true, stdout, stderr: "" };
  } catch (err: unknown) {
    const execErr = err as { stdout?: string; stderr?: string };
    return {
      ok: false,
      stdout: execErr.stdout ?? "",
      stderr: execErr.stderr ?? String(err),
    };
  }
}

function makeHandlers(overrides: Partial<NativeMenuHandlers> = {}): NativeMenuHandlers {
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

describe("[Tier 4] End-to-End Real-World Application Scenarios (S1–S7)", () => {
  const trackedSessions = new Set<string>();
  const savedBellSetting = get(interfaceStore).bellAsksForReader;

  function trackSession(id: string): string {
    trackedSessions.add(id);
    return id;
  }

  beforeEach(() => {
    interfaceStore.setBellAsksForReader(false);
    takeConflictsFocus();
    takeConflictsReveal();
  });

  afterEach(() => {
    for (const sid of trackedSessions) {
      sessionActivity.forget(sid);
    }
    trackedSessions.clear();
    interfaceStore.setBellAsksForReader(savedBellSetting);
    takeConflictsFocus();
    takeConflictsReveal();
  });

  describe("[Tier 4] [R1] [R2] [R4] S1: Full Claude Code permission request -> AgentsView -> MenuState & StatusPopover FocusAgent -> PostToolUse clear", () => {
    it("executes the complete Claude permission lifecycle across gitpulse-hook, sessionActivity, AgentsView, MenuState, and StatusPopover", async () => {
      await ensureGitPulseHookBinary();
      const sid = trackSession("t4-s1-claude-perm");
      const claudeUuid = "52d6abcd-6965-454b-a465-56a4a88526e7";

      // 1. Real gitpulse-hook emits PermissionRequest and Notification(permission_prompt)
      const permReq = await runGitPulseHookWithSocket({
        args: ["notify", "permission_request"],
        env: { GITPULSE_SESSION_ID: sid, GITPULSE_HOST: undefined },
        stdinJson: claudePermissionRequest({
          session_id: claudeUuid,
          tool_name: "Bash",
          message: "Allow Bash: git push --dry-run",
        }),
      });
      expect(permReq.exitCode).toBe(0);
      expect(permReq.receivedJson[0]).toMatchObject({
        v: 1,
        event: "permission_request",
        agent: "claude",
        session: sid,
      });

      const permPrompt = await runGitPulseHookWithSocket({
        args: ["notify", "permission_prompt"],
        env: { GITPULSE_SESSION_ID: sid, GITPULSE_HOST: undefined },
        stdinJson: claudeNotification("permission_prompt", {
          session_id: claudeUuid,
          message: "Allow Bash: git push --dry-run",
        }),
      });
      expect(permPrompt.exitCode).toBe(0);
      expect(permPrompt.receivedJson[0]).toMatchObject({
        v: 1,
        event: "permission_prompt",
        agent: "claude",
        session: sid,
      });

      // 2. Feed into sessionActivity and verify canonical R4 fields
      expect(
        sessionActivity.announce({
          session: sid,
          channel: "hook",
          event: String(permPrompt.receivedJson[0].event),
          urgency: "blocking",
          detail: String(permPrompt.receivedJson[0].message ?? ""),
        }),
      ).toBe(true);

      const attention = get(sessionActivity).get(sid)?.attention as SessionAttention & {
        urgency?: string;
        event?: string;
        channel?: string;
      };
      expect(attention.kind).toBe("needs-you");
      expect(attention.detail).toContain("Allow Bash: git push --dry-run");
      expect(attention.at).toBeGreaterThan(0);
      expect(attention.urgency).toBe("blocking");
      expect(attention.event).toBe("permission_prompt");
      expect(attention.channel).toBe("hook");

      // 3. Verify MenuState reflects agentsWaiting >= 1 and populated agentRequests
      const menuWaiting = get(nativeMenuState) as MenuStateFixture;
      expect(menuWaiting.agentsWaiting).toBe(1);
      expect(menuWaiting.agentRequests?.length).toBe(1);
      assertMenuRequestMatches(
        menuWaiting.agentRequests?.[0],
        sid,
        "blocking",
        "Allow Bash: git push --dry-run",
      );

      // 4. Verify AgentsView in real headless Chrome shows Needs Action, LauncherIcon, gp-chip, detail, and Focus
      const agentsVerdict = await runBrowserPage({
        page: "/tests/e2e-r1-r4/browser/agents-fleet.html",
        query: { scenario: "mixed" },
      });
      expect(agentsVerdict.crashes).toEqual([]);
      expect(agentsVerdict.unexpected).toEqual([]);
      expect(agentsVerdict.passed, JSON.stringify(agentsVerdict.results)).toBe(agentsVerdict.total);

      // 5. Verify StatusPopover in real headless Chrome shows <Logo>, request row, and dispatches focus-agent:<id>
      const popoverVerdict = await runBrowserPage({
        page: "/tests/e2e-r1-r4/browser/status-popover.html",
        query: { scenario: "waiting" },
      });
      expect(popoverVerdict.crashes).toEqual([]);
      expect(popoverVerdict.unexpected).toEqual([]);
      expect(popoverVerdict.passed, JSON.stringify(popoverVerdict.results)).toBe(
        popoverVerdict.total,
      );

      // 6. Real gitpulse-hook emits PostToolUse (tool_finished) -> clears attention back to 0
      const resolved = await runGitPulseHookWithSocket({
        args: ["notify", "tool_finished"],
        env: { GITPULSE_SESSION_ID: sid, GITPULSE_HOST: undefined },
        stdinJson: claudePostToolUse({ session_id: claudeUuid, tool_name: "Bash" }),
      });
      expect(resolved.exitCode).toBe(0);
      expect(resolved.receivedJson[0]).toMatchObject({
        event: "tool_finished",
        agent: "claude",
        session: sid,
      });

      sessionActivity.announce({
        session: sid,
        channel: "hook",
        event: String(resolved.receivedJson[0].event),
        detail: null,
      });

      expect(get(sessionActivity).get(sid)?.attention).toBeNull();
      const menuCleared = get(nativeMenuState) as MenuStateFixture;
      expect(menuCleared.agentsWaiting).toBe(0);
      expect(menuCleared.agentRequests ?? []).toEqual([]);
    }, 300_000);
  });

  describe("[Tier 4] [R2] [R4] S2: Full Antigravity (agy) turn lifecycle — informational vs error", () => {
    it("distinguishes informational NO_TOOL_CALL completion from error Stop across hook binary, AgentsView, and MenuState", async () => {
      await ensureGitPulseHookBinary();
      const sid = trackSession("t4-s2-agy-lifecycle");
      const convId = "91a8ef12-3456-4789-abcd-ef0123456789";

      // 1. PreInvocation -> prompt_submitted
      const pre = await runGitPulseHookWithSocket({
        args: ["--host=agy", "notify", "prompt_submitted"],
        env: { GITPULSE_SESSION_ID: sid, GITPULSE_HOST: "antigravity" },
        stdinJson: agyPreInvocation({ conversationId: convId }),
      });
      expect(pre.exitCode).toBe(0);
      expect(pre.receivedJson[0]).toMatchObject({
        event: "prompt_submitted",
        agent: "agy",
        session: sid,
      });
      sessionActivity.announce({
        session: sid,
        channel: "hook",
        event: String(pre.receivedJson[0].event),
        detail: null,
      });

      // 2. PostToolUse -> tool_finished
      const postTool = await runGitPulseHookWithSocket({
        args: ["--host=agy", "notify", "tool_finished"],
        env: { GITPULSE_SESSION_ID: sid, GITPULSE_HOST: "antigravity" },
        stdinJson: agyPostToolUse({ conversationId: convId, toolName: "run_command" }),
      });
      expect(postTool.exitCode).toBe(0);
      expect(postTool.receivedJson[0]).toMatchObject({
        event: "tool_finished",
        agent: "agy",
        session: sid,
      });

      // 3. Stop (NO_TOOL_CALL) -> turn_finished (informational, fully_idle: true)
      const stopInfo = await runGitPulseHookWithSocket({
        args: ["--host=agy", "stop-or-notify"],
        env: { GITPULSE_SESSION_ID: sid, GITPULSE_HOST: "antigravity" },
        stdinJson: agyStop({
          conversationId: convId,
          terminationReason: "NO_TOOL_CALL",
          message: "Completed plan synthesis cleanly",
        }),
      });
      expect(stopInfo.exitCode).toBe(0);
      expect(stopInfo.receivedJson[0]).toMatchObject({
        event: "turn_finished",
        agent: "agy",
        session: sid,
        fully_idle: true,
      });

      sessionActivity.announce({
        session: sid,
        channel: "hook",
        event: String(stopInfo.receivedJson[0].event),
        detail: String(stopInfo.receivedJson[0].message ?? ""),
        fully_idle: true,
      });

      const infoAttention = get(sessionActivity).get(sid)?.attention;
      expect(infoAttention?.kind).toBe("finished");
      expect(asksForReader(infoAttention?.kind)).toBe(false);
      expect((get(nativeMenuState) as MenuStateFixture).agentsWaiting).toBe(0);

      // Verify AgentsView distinguishes Claude blocking vs Antigravity turn_finished in Chrome
      const agentsVerdict = await runBrowserPage({
        page: "/tests/e2e-r1-r4/browser/agents-fleet.html",
        query: { scenario: "claude-vs-agy" },
      });
      expect(agentsVerdict.crashes).toEqual([]);
      expect(agentsVerdict.unexpected).toEqual([]);
      expect(agentsVerdict.passed, JSON.stringify(agentsVerdict.results)).toBe(agentsVerdict.total);

      // Verify StatusPopover renders zero waiting-for-action items when agentsWaiting == 0
      const popoverNone = await runBrowserPage({
        page: "/tests/e2e-r1-r4/browser/status-popover.html",
        query: { scenario: "none" },
      });
      expect(popoverNone.crashes).toEqual([]);
      expect(popoverNone.unexpected).toEqual([]);
      expect(popoverNone.passed, JSON.stringify(popoverNone.results)).toBe(popoverNone.total);

      // 4. Error Stop -> error (asksForReader == true, MenuState.agentsWaiting == 1)
      const stopErr = await runGitPulseHookWithSocket({
        args: ["--host=agy", "stop-or-notify"],
        env: { GITPULSE_SESSION_ID: sid, GITPULSE_HOST: "antigravity" },
        stdinJson: agyStop({
          conversationId: convId,
          terminationReason: "ERROR",
          error: "Model tool call crashed",
        }),
      });
      expect(stopErr.exitCode).toBe(0);
      expect(stopErr.receivedJson[0]).toMatchObject({
        event: "error",
        agent: "agy",
        session: sid,
      });

      sessionActivity.announce({
        session: sid,
        channel: "hook",
        event: String(stopErr.receivedJson[0].event),
        detail: String(stopErr.receivedJson[0].message ?? ""),
      });

      const errAttention = get(sessionActivity).get(sid)?.attention;
      expect(errAttention?.kind).toBe("error");
      expect(asksForReader(errAttention?.kind)).toBe(true);
      expect((get(nativeMenuState) as MenuStateFixture).agentsWaiting).toBe(1);
    }, 300_000);
  });

  describe("[Tier 4] [R3] S3: Clean repository Overview -> Conflicts/Resolve round-trip in Chrome", () => {
    it("opens clean Conflicts/Resolve, focuses #work-conflicts, hides cleanly, handles live conflict arrival & resolution without remounting WorkView", async () => {
      const verdict = await runBrowserPage<{
        layoutBefore?: string;
        layoutAfterOpen?: string;
        layoutAfterClose?: string;
        activeElementId?: string;
        workViewPreserved?: boolean;
        overflowX?: boolean;
      }>({
        page: "/tests/e2e-r1-r4/browser/overview-conflicts.html",
        query: { scenario: "transition", width: 1280 },
      });

      expect(verdict.crashes).toEqual([]);
      expect(verdict.unexpected).toEqual([]);
      expect(verdict.passed, JSON.stringify(verdict.results)).toBe(verdict.total);
      expect(verdict.data?.layoutBefore).toBe("overview");
      expect(verdict.data?.layoutAfterOpen).toBe("with-conflicts");
      expect(verdict.data?.layoutAfterClose).toBe("overview");
      expect(verdict.data?.activeElementId).toBe("work-conflicts");
      expect(verdict.data?.workViewPreserved).toBe(true);
      expect(verdict.data?.overflowX).toBe(false);
    }, 120_000);
  });

  describe("[Tier 4] [R2] [R3] [R4] S4: Multi-repo triage — Repo A conflicts + Repo B waiting Claude elicitation_dialog", () => {
    it("surfaces Repo A Resolve conflicts CTA and Repo B FocusAgent request simultaneously and routes both cleanly", async () => {
      const repoA = "/Projects/GitPulse";
      const repoB = "/Projects/DevCouncil";
      const sidB = trackSession("t4-s4-repob-elicitation");

      await withMockedConflictedAndCleanRepos(repoA, repoB, async () => {
        sessionActivity.announce({
          session: sidB,
          channel: "hook",
          event: "elicitation_dialog",
          detail: "Select deployment target",
        });

        const built = get(nativeMenuState) as MenuStateFixture;
        expect(built.status.conflicts).toBe(2);
        expect(built.status.primaryLabel).toBe("Resolve conflicts");
        expect(built.traySummary.id).toBe("section:work:overview");
        expect(built.agentsWaiting).toBe(1);
        expect(built.agentRequests?.length).toBeGreaterThanOrEqual(1);

        const snapshot = makeMenuStateWithRequests(
          [
            makeMenuAgentRequest({
              id: `term:${sidB}`,
              source: "terminal",
              provider: "claude",
              agentLabel: "claude-devcouncil",
              repoPath: repoB,
              repoLabel: "DevCouncil",
              urgency: "blocking",
              reason: "Needs your input",
              detail: "Select deployment target",
            }),
          ],
          { conflicts: 2 },
        );
        expect(sendableMenuState(snapshot).problem).toBeNull();

        // Browser check on conflict-jump scenario
        const popoverVerdict = await runBrowserPage({
          page: "/tests/e2e-r1-r4/browser/status-popover.html",
          query: { scenario: "conflict-jump" },
        });
        expect(popoverVerdict.crashes).toEqual([]);
        expect(popoverVerdict.unexpected).toEqual([]);
        expect(popoverVerdict.passed, JSON.stringify(popoverVerdict.results)).toBe(
          popoverVerdict.total,
        );

        // Route Resolve CTA -> work/overview -> openConflicts()
        let tabSwitch: [string, string | undefined] | null = null;
        let focusedAgentId: string | null = null;
        const handlers = makeHandlers({
          setTab: (tab: string, section?: string) => {
            tabSwitch = [tab, section];
            if (tab === "work" && section === "overview") openConflicts();
          },
          focusAgent: (id: string) => {
            focusedAgentId = id;
          },
        });

        expect(
          dispatchNativeMenu({ id: "section:work:overview", repo_path: repoA }, handlers),
        ).toBe(true);
        expect(tabSwitch).toEqual(["work", "overview"]);
        expect(takeConflictsReveal()).toBe(true);
        expect(takeConflictsFocus()).toBe(true);

        // Route FocusAgent -> switches/focuses Repo B agent
        const focusEvent = { id: "focus-agent", path: `term:${sidB}`, repo_path: repoA };
        expect(canDispatchMenuEvent(focusEvent, snapshot, get(repoStore), PATHS)).toBe(true);
        expect(dispatchNativeMenu(focusEvent, handlers)).toBe(true);
        expect(focusedAgentId).toBe(`term:${sidB}`);
      });
    }, 120_000);
  });

  describe("[Tier 4] [R1] S5: End-to-end GitPulse brand logo audit across system, in-app, and menu bar surfaces", () => {
    it("verifies current G-ring mark across PNG/ICNS/SVG assets, notifications-logo.html, status-popover.html, and tray.rs", async () => {
      // 1. Asset audit
      const pluginLogo = readPngInfo("plugins/gitpulse/assets/logo.png");
      const icon128x2 = readPngInfo("src-tauri/icons/128x128@2x.png");
      const icon512 = readPngInfo("src-tauri/icons/icon.png");
      expect(pluginLogo.width).toBe(256);
      expect(pluginLogo.height).toBe(256);
      expect(pluginLogo.colorType).toBe(6);
      expect(pluginLogo.bitDepth).toBe(8);
      expect(pluginLogo.sha256).not.toBe(LEGACY_PLUGIN_LOGO_SHA256);
      expect(icon128x2.width).toBe(256);
      expect(icon128x2.height).toBe(256);
      expect(icon512.width).toBe(512);
      expect(icon512.height).toBe(512);

      expect(readRepoFile("public/gitpulse-icon.svg")).toContain(LOGO_TRUNK_FRAGMENT);
      expect(readRepoFile("public/gitpulse-mark.svg")).toContain(LOGO_TRUNK_FRAGMENT);
      expect(readRepoFile("index.html")).toContain(LOGO_TRUNK_FRAGMENT);

      // 2. All 4 in-app notification surfaces in Chrome (dark & light)
      for (const theme of ["dark", "light"] as const) {
        const notifVerdict = await runBrowserPage({
          page: "/tests/e2e-r1-r4/browser/notifications-logo.html",
          query: { scenario: "all", theme },
        });
        expect(notifVerdict.crashes).toEqual([]);
        expect(notifVerdict.unexpected).toEqual([]);
        expect(notifVerdict.passed, JSON.stringify(notifVerdict.results)).toBe(notifVerdict.total);
      }

      // 3. StatusPopover header <Logo> in Chrome
      const popoverVerdict = await runBrowserPage({
        page: "/tests/e2e-r1-r4/browser/status-popover.html",
        query: { scenario: "waiting" },
      });
      const logoCheck = popoverVerdict.results.find((r) =>
        r.name.includes("current GitPulse SVG logo"),
      );
      expect(logoCheck?.pass, JSON.stringify(popoverVerdict.results)).toBe(true);

      // 4. Rust bundled notification icons & tray G-ring glyph tests
      const bundleTest = await execCargoSuite(
        [
          "--lib",
          "workbench::notifications::tests::bundled_notification_icons_and_plugin_logo_match_current_brand",
        ],
        { timeoutMs: 420_000 },
      );
      assertCargoPassed(
        bundleTest,
        "workbench::notifications::tests::bundled_notification_icons_and_plugin_logo_match_current_brand",
      );

      const trayTests = await execCargoSuite(["--lib", "desktop::tray::tests"], {
        timeoutMs: 420_000,
      });
      assertCargoPassed(trayTests, "cargo test --lib desktop::tray::tests");

      const traySource = readRepoFile("src-tauri/src/desktop/tray.rs");
      assertTrayPulseIconUpdated(traySource);
    }, 600_000);
  });

  describe("[Tier 4] [R2] [R4] S6: Noisy multi-agent fleet triage (> 8 overflow + mixed blocking/error/informational/running)", () => {
    it("caps MenuState at MAX_MENU_AGENT_REQUESTS = 8 with agentRequestsHidden, keeps StatusPopover <= 640px, and filters/dismisses in AgentsView", async () => {
      const blockingSessions = Array.from({ length: 10 }, (_, i) => {
        const sid = trackSession(`t4-s6-block-${i + 1}`);
        sessionActivity.announce({
          session: sid,
          channel: "hook",
          event: i % 2 === 0 ? "permission_prompt" : "elicitation_dialog",
          detail: `Blocking request #${i + 1}`,
        });
        return sid;
      });
      const errSid = trackSession("t4-s6-error-1");
      sessionActivity.announce({
        session: errSid,
        channel: "hook",
        event: "error",
        detail: "Rate limit exceeded",
      });

      const infoEvents = ["turn_finished", "agent_completed", "idle_prompt"] as const;
      infoEvents.forEach((ev, idx) => {
        const sid = trackSession(`t4-s6-info-${idx + 1}`);
        sessionActivity.announce({
          session: sid,
          channel: "hook",
          event: ev,
          detail: `Informational notice ${ev}`,
        });
      });

      expect(blockingSessions.length).toBe(10);
      const menuState = get(nativeMenuState) as MenuStateFixture;
      expect(menuState.agentsWaiting).toBe(11);
      expect(menuState.agentRequests).toBeDefined();
      expect(menuState.agentRequests?.length).toBe(8);
      expect(menuState.agentRequestsHidden).toBe(3);
      expect(menuState.agentsInformational).toBeGreaterThanOrEqual(3);

      // Verify overflow StatusPopover in Chrome stays <= 640px and reports hidden requests
      const overflowPopover = await runBrowserPage({
        page: "/tests/e2e-r1-r4/browser/status-popover.html",
        query: { scenario: "overflow" },
      });
      expect(overflowPopover.crashes).toEqual([]);
      expect(overflowPopover.unexpected).toEqual([]);
      expect(overflowPopover.passed, JSON.stringify(overflowPopover.results)).toBe(
        overflowPopover.total,
      );

      // Verify AgentsView filter & dismiss in Chrome
      const fleetDismiss = await runBrowserPage({
        page: "/tests/e2e-r1-r4/browser/agents-fleet.html",
        query: { scenario: "dismiss" },
      });
      expect(fleetDismiss.crashes).toEqual([]);
      expect(fleetDismiss.unexpected).toEqual([]);
      expect(fleetDismiss.passed, JSON.stringify(fleetDismiss.results)).toBe(fleetDismiss.total);

      // Also verify StatusPopover SSR renders cleanly with overflow fixture
      const overflowSnapshot = makeMenuStateWithRequests(
        Array.from({ length: 8 }, (_, i) =>
          makeMenuAgentRequest({
            id: `term:pty-${i + 1}`,
            source: "terminal",
            provider: i % 2 === 0 ? "claude" : "agy",
            agentLabel: `agent-${i + 1}`,
            repoPath: "/Projects/GitPulse",
            repoLabel: "GitPulse",
            urgency: "blocking",
            reason: "Needs your permission",
            detail: `Allow tool #${i + 1}`,
          }),
        ),
        { agentRequestsHidden: 3, agentsRunning: 2, agentsInformational: 3 },
      );
      const { body } = render(StatusPopover, {
        props: { snapshot: overflowSnapshot, onaction: () => {} },
      });
      expect(body).toContain("Allow tool #1");
    }, 120_000);
  });

  describe("[Tier 4] [R1] [R2] [R3] [R4] S7: Full production gate & security boundary workflow", () => {
    it("passes check:ipc, check:types, check:vendor-edits, Rust desktop/ACL contracts, and R1–R4 design-lint", async () => {
      const ipcCheck = runBunScript("check:ipc");
      expect(ipcCheck.ok, ipcCheck.stderr || ipcCheck.stdout).toBe(true);

      const typesCheck = runBunScript("check:types");
      expect(typesCheck.ok, typesCheck.stderr || typesCheck.stdout).toBe(true);

      const vendorCheck = runBunScript("check:vendor-edits");
      expect(vendorCheck.ok, vendorCheck.stderr || vendorCheck.stdout).toBe(true);

      const desktopContracts = await execCargoSuite(
        [
          "--test",
          "acl_contract",
          "--test",
          "desktop_menu_integration",
          "--test",
          "native_menu_main_thread",
        ],
        { timeoutMs: 360_000 },
      );
      assertCargoPassed(
        desktopContracts,
        "acl_contract + desktop_menu_integration + native_menu_main_thread",
      );

      let rawLint = "";
      try {
        rawLint = execFileSync("node", ["scripts/design-lint.mjs", "--strict", "--json"], {
          cwd: repoPath("."),
          encoding: "utf8",
        });
      } catch (err: unknown) {
        const execErr = err as { status?: number; stdout?: string };
        if (execErr.status === 1 && execErr.stdout) {
          rawLint = execErr.stdout;
        } else {
          throw err;
        }
      }
      const report = JSON.parse(rawLint) as {
        stale: string[];
        open: { rule: number; file: string; line: number; detail: string }[];
      };
      expect(report.stale).toEqual([]);
      const nonPreExisting = report.open.filter(
        (v) => v.file !== "src/lib/components/BrandMark.svelte",
      );
      expect(nonPreExisting, JSON.stringify(nonPreExisting)).toEqual([]);
    }, 450_000);
  });
});
