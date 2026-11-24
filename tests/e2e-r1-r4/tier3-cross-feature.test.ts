import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { get } from "svelte/store";
import { render } from "svelte/server";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { clearMocks } from "@tauri-apps/api/mocks";
import { mockIPCWithEvents } from "../../harness/tauriMocks";
import Logo from "../../src/lib/components/Logo.svelte";
import StatusPopover from "../../src/lib/desktop/StatusPopover.svelte";
import {
  canDispatchMenuEvent,
  countAgentsWaiting,
  type MenuAgentRequest,
} from "../../src/lib/desktop/menuState";
import {
  MENU_LIMITS,
  menuStateProblem,
  sendableMenuState,
} from "../../src/lib/desktop/menuContract";
import { nativeMenuState } from "../../src/lib/desktop/menuStateStore";
import {
  dispatchNativeMenu,
  type NativeMenuHandlers,
} from "../../src/lib/desktop/nativeActions";
import { createRepoStore, repoStore, type RepoState } from "../../src/lib/stores/repoStore";
import { interfaceStore } from "../../src/lib/stores/interfaceStore";
import {
  asksForReader,
  sessionActivity,
  type AttentionKind,
} from "../../src/lib/terminal/sessionActivity";
import { projectAgentPlane } from "../../src/lib/agents/plane";
import { planeTerminals, repoAgentPill } from "../../src/lib/agents/repoAttention";
import {
  conflictsFocusRequest,
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
  agyPreInvocation,
  agyStop,
  claudePermissionRequest,
  makeMenuAgentRequest,
  makeMenuState,
  makeMenuStateWithRequests,
  makePlaneProbe,
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
          name: reqRepo === repoB ? "RepoB" : "RepoA",
          is_bare: false,
          common_dir: `${reqRepo}/.git`,
        };
      case "cmd_list_branches":
        return [
          {
            name: reqRepo === repoB ? "feat/agent" : "main",
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
              path: "src/lib.rs",
              status_code: "UU",
              is_staged: false,
              is_conflicted: true,
              additions: 0,
              deletions: 0,
            },
            {
              path: "src/main.rs",
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

describe("[Tier 3] Cross-Feature Pairwise Combinations (R1–R4)", () => {
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

  describe("[Tier 3] [R2] [R4] Pair 1 (F7 <-> F10): Claude blocking vs informational notifications -> MenuState", () => {
    it("only blocking Claude notifications increment agentsWaiting & agentRequests while informational updates agentsInformational", () => {
      const sTurn = trackSession("t3-p1-turn");
      const sIdle = trackSession("t3-p1-idle");
      const sPerm = trackSession("t3-p1-perm");

      expect(
        sessionActivity.announce({
          session: sTurn,
          channel: "hook",
          event: "turn_finished",
          detail: "Completed implementation",
        }),
      ).toBe(true);
      expect(
        sessionActivity.announce({
          session: sIdle,
          channel: "hook",
          event: "idle_prompt",
          detail: "Claude is waiting for your next instruction",
        }),
      ).toBe(true);

      const infoOnly = get(nativeMenuState) as MenuStateFixture;
      expect(infoOnly.agentsWaiting).toBe(0);
      expect(infoOnly.agentRequests).toEqual([]);
      expect(infoOnly.agentsInformational).toBeGreaterThanOrEqual(2);

      expect(
        sessionActivity.announce({
          session: sPerm,
          channel: "hook",
          event: "permission_request",
          detail: "Allow Bash: cargo test",
        }),
      ).toBe(true);

      const withBlocking = get(nativeMenuState) as MenuStateFixture;
      expect(withBlocking.agentsWaiting).toBe(1);
      expect(withBlocking.agentRequests).toBeDefined();
      expect(withBlocking.agentRequests?.length).toBe(1);
      assertMenuRequestMatches(withBlocking.agentRequests?.[0], sPerm, "blocking", "Allow Bash: cargo test");
      expect(withBlocking.agentsInformational).toBeGreaterThanOrEqual(2);

      sessionActivity.announce({
        session: sPerm,
        channel: "hook",
        event: "tool_finished",
        detail: null,
      });

      const cleared = get(nativeMenuState) as MenuStateFixture;
      expect(cleared.agentsWaiting).toBe(0);
      expect(cleared.agentRequests).toEqual([]);
    });
  });

  describe("[Tier 3] [R2] [R4] Pair 2 (F8 <-> F10): Antigravity Stop(NO_TOOL_CALL) vs Stop(ERROR) via gitpulse-hook -> MenuState", () => {
    it("drives MenuState.agentsWaiting & agentRequests from real Antigravity hook payloads without ghost requests", async () => {
      await ensureGitPulseHookBinary();
      const sid = trackSession("t3-p2-agy-1");

      const pre = await runGitPulseHookWithSocket({
        args: ["--host=agy", "notify", "prompt_submitted"],
        env: { GITPULSE_SESSION_ID: sid, GITPULSE_HOST: "antigravity" },
        stdinJson: agyPreInvocation({ conversationId: "91a8ef12-3456-4789-abcd-ef0123456789" }),
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

      const stopClean = await runGitPulseHookWithSocket({
        args: ["--host=agy", "stop-or-notify"],
        env: { GITPULSE_SESSION_ID: sid, GITPULSE_HOST: "antigravity" },
        stdinJson: agyStop({
          terminationReason: "NO_TOOL_CALL",
          message: "Finished turn cleanly",
        }),
      });
      expect(stopClean.exitCode).toBe(0);
      expect(stopClean.receivedJson[0]).toMatchObject({
        event: "turn_finished",
        agent: "agy",
        session: sid,
        fully_idle: true,
      });

      sessionActivity.announce({
        session: sid,
        channel: "hook",
        event: String(stopClean.receivedJson[0].event),
        detail: String(stopClean.receivedJson[0].message ?? ""),
        fully_idle: Boolean(stopClean.receivedJson[0].fully_idle),
      });

      const afterCleanStop = get(nativeMenuState) as MenuStateFixture;
      expect(afterCleanStop.agentsWaiting).toBe(0);
      expect(afterCleanStop.agentRequests).toEqual([]);
      expect(afterCleanStop.agentsInformational).toBeGreaterThanOrEqual(1);

      const stopError = await runGitPulseHookWithSocket({
        args: ["--host=agy", "stop-or-notify"],
        env: { GITPULSE_SESSION_ID: sid, GITPULSE_HOST: "antigravity" },
        stdinJson: agyStop({
          terminationReason: "ERROR",
          error: "Model invocation failed: quota exceeded",
        }),
      });
      expect(stopError.exitCode).toBe(0);
      expect(stopError.receivedJson[0]).toMatchObject({
        event: "error",
        agent: "agy",
        session: sid,
      });

      sessionActivity.announce({
        session: sid,
        channel: "hook",
        event: String(stopError.receivedJson[0].event),
        detail: String(stopError.receivedJson[0].message ?? ""),
      });

      const afterError = get(nativeMenuState) as MenuStateFixture;
      expect(afterError.agentsWaiting).toBe(1);
      expect(afterError.agentRequests?.length).toBe(1);
      assertMenuRequestMatches(afterError.agentRequests?.[0], sid, "error", "quota exceeded");

      const nextTurn = await runGitPulseHookWithSocket({
        args: ["--host=agy", "notify", "prompt_submitted"],
        env: { GITPULSE_SESSION_ID: sid, GITPULSE_HOST: "antigravity" },
        stdinJson: agyPreInvocation(),
      });
      sessionActivity.announce({
        session: sid,
        channel: "hook",
        event: String(nextTurn.receivedJson[0].event),
        detail: null,
      });

      const afterClear = get(nativeMenuState) as MenuStateFixture;
      expect(afterClear.agentsWaiting).toBe(0);
      expect(afterClear.agentRequests).toEqual([]);
    }, 600_000);
  });

  describe("[Tier 3] [R2] [R4] Pair 3 (F7 <-> F9 <-> F10): Cross-surface bellSurfaces agreement", () => {
    const CASES: ReadonlyArray<{
      event: string | null;
      channel: string;
      blocking: boolean;
      signalled?: boolean;
    }> = [
      { event: "permission_request", channel: "hook", blocking: true },
      { event: "permission_prompt", channel: "hook", blocking: true },
      { event: "agent_needs_input", channel: "hook", blocking: true },
      { event: "elicitation_dialog", channel: "hook", blocking: true },
      { event: "error", channel: "hook", blocking: true },
      { event: "turn_finished", channel: "hook", blocking: false },
      { event: "agent_completed", channel: "hook", blocking: false },
      { event: "idle_prompt", channel: "hook", blocking: false },
      { event: null, channel: "bell", blocking: false, signalled: true },
    ];

    for (const bellSetting of [true, false]) {
      for (const item of CASES) {
        it(`agrees 100% across sessionActivity, plane, repoAttention, and MenuState for ${item.event ?? "signalled"} (bell=${bellSetting})`, () => {
          interfaceStore.setBellAsksForReader(bellSetting);
          const sid = trackSession(`t3-p3-${item.event ?? "bell"}-${bellSetting}`);
          const repo = "/code/devtools/GitPulse";
          const wt = `${repo}/.claude/worktrees/alpha`;

          sessionActivity.announce({
            session: sid,
            channel: item.channel,
            event: item.event,
            detail: item.event ? `Detail for ${item.event}` : null,
          });

          const activityMap = get(sessionActivity);
          const entry = activityMap.get(sid);
          expect(entry?.attention).not.toBeNull();

          const expectedBlocking = item.signalled ? bellSetting : item.blocking;
          const kind = entry?.attention?.kind as AttentionKind | undefined;

          const asks = asksForReader(kind);
          const menuWaiting = countAgentsWaiting(activityMap) > 0;

          const terminals = planeTerminals(
            [
              {
                key: `term-${sid}`,
                repoPath: repo,
                label: "Claude Code",
                title: "claude-alpha",
                status: "running",
                sessionId: sid,
              },
            ],
            activityMap,
            new Map([[sid, wt]]),
          );
          const probe = makePlaneProbe(repo, [
            {
              path: repo,
              name: "GitPulse",
              branch: "main",
              is_main: true,
              is_bare: false,
              is_detached: false,
              dirty_files: 0,
              agent_kind: "",
              session_slug: "",
              operation_kind: "",
              operation_ok: true,
              head: null,
              ahead: null,
              behind: null,
              commits_since_base: null,
            },
            {
              path: wt,
              name: "alpha",
              branch: "agent/alpha",
              is_main: false,
              is_bare: false,
              is_detached: false,
              dirty_files: 0,
              agent_kind: "claude",
              session_slug: "alpha",
              operation_kind: "",
              operation_ok: true,
              head: null,
              ahead: null,
              behind: null,
              commits_since_base: null,
            },
          ]);
          const plane = projectAgentPlane({
            probes: [probe],
            terminals,
            tasks: null,
            paths: PATHS,
          });
          const pill = repoAgentPill({
            plane,
            probes: [probe],
            repoPath: repo,
            paths: PATHS,
          });

          expect(asks, "sessionActivity.asksForReader").toBe(expectedBlocking);
          expect(menuWaiting, "menuState.countAgentsWaiting > 0").toBe(expectedBlocking);
          expect(plane.attention.needing > 0, "plane.attention.needing > 0").toBe(expectedBlocking);
          expect(pill.state === "attention", "repoAttention pill.state === attention").toBe(
            expectedBlocking,
          );
        });
      }
    }
  });

  describe("[Tier 3] [R2] [R4] Pair 4 (F9 <-> F10): Dismissing / resolving clears AgentsView & MenuState", () => {
    it("resolving a blocking request clears MenuState and dismissing informational clears AgentsView in Chrome", async () => {
      const sid = trackSession("t3-p4-resolve");
      sessionActivity.announce({
        session: sid,
        channel: "hook",
        event: "permission_request",
        detail: "Allow Write: src/main.ts",
      });
      expect((get(nativeMenuState) as MenuStateFixture).agentsWaiting).toBe(1);

      const answered = sessionActivity.input(sid, "y\n");
      expect(answered).toBe(true);
      const afterResolve = get(nativeMenuState) as MenuStateFixture;
      expect(afterResolve.agentsWaiting).toBe(0);
      expect(afterResolve.agentRequests ?? []).toEqual([]);

      const verdict = await runBrowserPage({
        page: "/tests/e2e-r1-r4/browser/agents-fleet.html",
        query: { scenario: "dismiss" },
      });
      expect(verdict.crashes).toEqual([]);
      expect(verdict.unexpected).toEqual([]);
      const dismissCheck = verdict.results.find((r) =>
        r.name.includes("dismiss action clears informational notification"),
      );
      expect(dismissCheck?.pass, JSON.stringify(verdict.results)).toBe(true);
    }, 120_000);
  });

  describe("[Tier 3] [R2] [R4] Pair 5 (F10 <-> F9): StatusPopover FocusAgent action -> dispatchNativeMenu", () => {
    it("dispatches focus-agent:<id> from StatusPopover and passes Rust actions + dispatchNativeMenu", async () => {
      const verdict = await runBrowserPage<{
        dispatchedActions?: { id: string; repoPath: string | null }[];
      }>({
        page: "/tests/e2e-r1-r4/browser/status-popover.html",
        query: { scenario: "waiting" },
      });
      expect(verdict.crashes).toEqual([]);
      expect(verdict.unexpected).toEqual([]);
      const focusClick = verdict.results.find((r) =>
        r.name.includes("focus-agent:<id>"),
      );
      expect(focusClick?.pass, JSON.stringify(verdict.results)).toBe(true);

      const menuWithRequest = makeMenuStateWithRequests([
        makeMenuAgentRequest({
          id: "term:pty-claude-1",
          source: "terminal",
          provider: "claude",
          agentLabel: "claude-1",
          repoPath: "/Projects/GitPulse",
          repoLabel: "GitPulse",
          urgency: "blocking",
          reason: "Needs your permission",
          detail: "Allow Bash",
        }),
      ]);
      const emptyRepo = get(createRepoStore({ storage: null }));
      const repoState: RepoState = {
        ...emptyRepo,
        currentPath: "/Projects/GitPulse",
        currentBranch: "main",
        isLoading: false,
      };
      const event = { id: "focus-agent", path: "term:pty-claude-1", repo_path: "/Projects/GitPulse" };
      expect(canDispatchMenuEvent(event, menuWithRequest, repoState, PATHS)).toBe(true);

      let focusedId: string | null = null;
      const handlers = makeHandlers({
        focusAgent: (id: string) => {
          focusedId = id;
        },
      });
      const handled = dispatchNativeMenu(event, handlers);
      expect(handled).toBe(true);
      expect(focusedId).toBe("term:pty-claude-1");

      const rustActions = await execCargoSuite(["--lib", "desktop::actions::tests"], {
        timeoutMs: 280_000,
      });
      assertCargoPassed(rustActions, "cargo test --lib desktop::actions::tests");
    }, 300_000);
  });

  describe("[Tier 3] [R2] [R3] Pair 6 (F5 <-> F10): StatusPopover conflict jump -> section:work:overview -> openConflicts", () => {
    it("dispatches section:work:overview from StatusPopover shortcut 3 / Resolve CTA and triggers openConflicts()", async () => {
      const verdict = await runBrowserPage({
        page: "/tests/e2e-r1-r4/browser/status-popover.html",
        query: { scenario: "conflict-jump" },
      });
      expect(verdict.crashes).toEqual([]);
      expect(verdict.unexpected).toEqual([]);
      const jumpCheck = verdict.results.find((r) =>
        r.name.includes("pressing 3 when conflicts exist dispatches section:work:overview"),
      );
      expect(jumpCheck?.pass, JSON.stringify(verdict.results)).toBe(true);

      let navigated: [string, string | undefined] | null = null;
      const handled = dispatchNativeMenu(
        { id: "section:work:overview", repo_path: "/Projects/GitPulse" },
        makeHandlers({
          setTab: (tab: string, section?: string) => {
            navigated = [tab, section];
          },
        }),
      );
      expect(handled).toBe(true);
      expect(navigated).toEqual(["work", "overview"]);

      openConflicts();
      expect(get(conflictsFocusRequest)).toBe(true);
      expect(takeConflictsReveal()).toBe(true);
      expect(takeConflictsFocus()).toBe(true);
    }, 120_000);
  });

  describe("[Tier 3] [R2] [R3] Pair 7 (F6 <-> F10): Multi-repo conflicts in Repo A + waiting agent in Repo B", () => {
    it("preserves both Repo A Resolve conflicts CTA and Repo B waiting agentRequests in MenuState & StatusPopover SSR", async () => {
      const repoA = "/Projects/RepoA";
      const repoB = "/Projects/RepoB";
      const sidB = trackSession("t3-p7-repob-claude");

      await withMockedConflictedAndCleanRepos(repoA, repoB, () => {
        sessionActivity.announce({
          session: sidB,
          channel: "hook",
          event: "elicitation_dialog",
          detail: "Choose staging or production target",
        });

        const built = get(nativeMenuState) as MenuStateFixture;

        // Also verify StatusPopover SSR when Repo B request is attached
        const combined = makeMenuStateWithRequests(
          [
            makeMenuAgentRequest({
              id: `term:${sidB}`,
              source: "terminal",
              provider: "claude",
              agentLabel: "claude-repob",
              repoPath: repoB,
              repoLabel: "RepoB",
              urgency: "blocking",
              reason: "Needs your input",
              detail: "Choose staging or production target",
            }),
          ],
          { conflicts: 2 },
        );

        expect(built.status.conflicts).toBe(2);
        expect(built.status.primaryLabel).toBe("Resolve conflicts");
        expect(built.traySummary.id).toBe("section:work:overview");
        expect(built.agentsWaiting).toBe(1);
        expect(built.agentRequests).toBeDefined();
        expect(built.agentRequests?.length).toBeGreaterThanOrEqual(1);

        expect(menuStateProblem(built)).toBeNull();
        expect(sendableMenuState(combined).problem).toBeNull();

        const { body } = render(StatusPopover, {
          props: { snapshot: combined, onaction: () => {} },
        });
        expect(body).toContain("Resolve conflicts");
        expect(body).toContain("Choose staging or production target");
      });
    });
  });

  describe("[Tier 3] [R1] [R2] Pair 8 (F1 <-> F4): Logo parity across Logo.svelte, StatusPopover, and tray.rs", () => {
    it("renders current G-ring Logo in StatusPopover & tray.rs matching canonical Logo.svelte", () => {
      const canonical = render(Logo, { props: { size: 28, variant: "badge" } }).body;
      expect(canonical).toContain(LOGO_TRUNK_FRAGMENT);

      const iconSvg = readRepoFile("public/gitpulse-icon.svg");
      expect(iconSvg).toContain(LOGO_TRUNK_FRAGMENT);

      const popoverHtml = render(StatusPopover, {
        props: { snapshot: makeMenuState(), onaction: () => {} },
      }).body;
      const markMatch = /class="mark\b[^"]*"/.exec(popoverHtml);
      expect(markMatch, "StatusPopover SSR missing .mark header container").not.toBeNull();
      const markStart = markMatch!.index;
      const markSlice = popoverHtml.slice(markStart, markStart + 3500);
      expect(markSlice).toContain(LOGO_TRUNK_FRAGMENT);
      expect(markSlice).not.toContain("lucide-activity");

      const traySource = readRepoFile("src-tauri/src/desktop/tray.rs");
      assertTrayPulseIconUpdated(traySource);
    });
  });

  describe("[Tier 3] [R1] [R4] Pair 9 (F3 <-> F7/F8): In-app notification surfaces render current <Logo>", () => {
    it("renders current <Logo> across ToastContainer, NativeNotificationBridgeView, SessionNotificationBridge, and AttentionInbox in dark & light themes", async () => {
      for (const theme of ["dark", "light"] as const) {
        const verdict = await runBrowserPage({
          page: "/tests/e2e-r1-r4/browser/notifications-logo.html",
          query: { scenario: "all", theme },
        });
        expect(verdict.crashes).toEqual([]);
        expect(verdict.unexpected).toEqual([]);
        expect(verdict.passed, JSON.stringify(verdict.results)).toBe(verdict.total);
        expect(verdict.total).toBeGreaterThanOrEqual(8);
      }
    }, 120_000);
  });

  describe("[Tier 3] [R1] [R4] Pair 10 (F2 <-> F7): Bundled plugin asset + macOS notification & alerts pipeline parity", () => {
    it("verifies bundled logo.png and app icons are current 256x256 RGBA PNGs under tauri.conf.json and Rust notifications + alerts tests pass", async () => {
      const pluginLogo = readPngInfo("plugins/gitpulse/assets/logo.png");
      const appIcon2x = readPngInfo("src-tauri/icons/128x128@2x.png");
      expect(pluginLogo.width).toBe(256);
      expect(pluginLogo.height).toBe(256);
      expect(pluginLogo.colorType).toBe(6);
      expect(pluginLogo.bitDepth).toBe(8);
      expect(pluginLogo.sha256).not.toBe(LEGACY_PLUGIN_LOGO_SHA256);
      expect(appIcon2x.width).toBe(256);
      expect(appIcon2x.height).toBe(256);
      expect(appIcon2x.colorType).toBe(6);

      const tauriConf = JSON.parse(readRepoFile("src-tauri/tauri.conf.json")) as {
        bundle: { icon: string[]; resources: Record<string, string> };
      };
      expect(tauriConf.bundle.icon).toContain("icons/icon.icns");
      expect(tauriConf.bundle.resources["../plugins/gitpulse/"]).toBe("plugin/");

      const notifRes = await execCargoSuite(["--lib", "workbench::notifications::tests"], {
        timeoutMs: 420_000,
      });
      assertCargoPassed(notifRes, "cargo test --lib workbench::notifications::tests");

      const bridgeRes = await execCargoSuite(["--lib", "alerts::bridge::tests"], {
        timeoutMs: 420_000,
      });
      assertCargoPassed(bridgeRes, "cargo test --lib alerts::bridge::tests");
    }, 600_000);
  });

  describe("[Tier 3] [R3] Pair 11 (F5 <-> F6): Clean Overview -> openConflicts -> Hide -> live conflict transition", () => {
    it("transitions cleanly while preserving single <WorkView /> DOM node identity at desktop and narrow widths", async () => {
      const transition = await runBrowserPage({
        page: "/tests/e2e-r1-r4/browser/overview-conflicts.html",
        query: { scenario: "transition", width: 1280 },
      });
      expect(transition.crashes).toEqual([]);
      expect(transition.unexpected).toEqual([]);
      expect(transition.passed, JSON.stringify(transition.results)).toBe(transition.total);
      expect(transition.data?.workViewPreserved).toBe(true);

      const narrow = await runBrowserPage({
        page: "/tests/e2e-r1-r4/browser/overview-conflicts.html",
        query: { scenario: "clean", width: 640 },
        windowSize: { width: 700, height: 820 },
      });
      expect(narrow.crashes).toEqual([]);
      expect(narrow.unexpected).toEqual([]);
      expect(narrow.passed, JSON.stringify(narrow.results)).toBe(narrow.total);
      expect(narrow.data?.overflowX).toBe(false);
    }, 120_000);
  });

  describe("[Tier 3] [R4] Pair 12 (F7 <-> F8): Concurrent Claude + Antigravity sessions on agent-notify.sock", () => {
    it("routes concurrent Claude permission_request and Antigravity turn_finished without cross-session bleed", async () => {
      await ensureGitPulseHookBinary();
      const claudeSid = trackSession("t3-p12-claude-concurrent");
      const agySid = trackSession("t3-p12-agy-concurrent");

      const [claudeRes, agyRes] = await Promise.all([
        runGitPulseHookWithSocket({
          args: ["notify", "permission_request"],
          env: { GITPULSE_SESSION_ID: claudeSid, GITPULSE_HOST: undefined },
          stdinJson: claudePermissionRequest({
            session_id: "11111111-1111-4111-8111-111111111111",
            message: "Allow Bash: cargo clippy",
          }),
        }),
        runGitPulseHookWithSocket({
          args: ["--host=agy", "stop-or-notify"],
          env: { GITPULSE_SESSION_ID: agySid, GITPULSE_HOST: "antigravity" },
          stdinJson: agyStop({
            conversationId: "22222222-2222-4222-8222-222222222222",
            terminationReason: "NO_TOOL_CALL",
            message: "All tasks done",
          }),
        }),
      ]);

      expect(claudeRes.exitCode).toBe(0);
      expect(agyRes.exitCode).toBe(0);
      expect(claudeRes.receivedJson[0]).toMatchObject({
        event: "permission_request",
        agent: "claude",
        session: claudeSid,
      });
      expect(agyRes.receivedJson[0]).toMatchObject({
        event: "turn_finished",
        agent: "agy",
        session: agySid,
        fully_idle: true,
      });

      sessionActivity.announce({
        session: claudeSid,
        channel: "hook",
        event: String(claudeRes.receivedJson[0].event),
        detail: String(claudeRes.receivedJson[0].message ?? ""),
      });
      sessionActivity.announce({
        session: agySid,
        channel: "hook",
        event: String(agyRes.receivedJson[0].event),
        detail: String(agyRes.receivedJson[0].message ?? ""),
        fully_idle: true,
      });

      const map = get(sessionActivity);
      expect(map.get(claudeSid)?.attention?.kind).toBe("needs-you");
      expect(map.get(claudeSid)?.attention?.detail).toContain("Allow Bash");
      expect(map.get(agySid)?.attention?.kind).toBe("finished");
      expect(map.get(agySid)?.attention?.detail).toContain("All tasks done");
      expect(countAgentsWaiting(map)).toBe(1);
    }, 600_000);
  });

  describe("[Tier 3] [R1] [R2] [R3] [R4] Pair 13 (F1 <-> F3 <-> F9): design-lint --strict compliance on R1–R4 components", () => {
    it("verifies zero color/vocab/layout violations across all R1–R4 UI components", () => {
      const targetFiles = new Set([
        "src/lib/components/ToastContainer.svelte",
        "src/lib/components/NativeNotificationBridgeView.svelte",
        "src/lib/components/SessionNotificationBridge.svelte",
        "src/lib/components/AttentionInbox.svelte",
        "src/lib/components/WorkOverviewPane.svelte",
        "src/lib/components/WorkView.svelte",
        "src/lib/components/AgentsView.svelte",
        "src/lib/desktop/StatusPopover.svelte",
      ]);

      for (const rel of targetFiles) {
        expect(existsSync(repoPath(rel)), `${rel} missing`).toBe(true);
      }

      let rawJson = "";
      try {
        rawJson = execFileSync("node", ["scripts/design-lint.mjs", "--strict", "--json"], {
          cwd: repoPath("."),
          encoding: "utf8",
        });
      } catch (err: unknown) {
        const execErr = err as { status?: number; stdout?: string; stderr?: string };
        if (execErr.status === 1 && execErr.stdout) {
          rawJson = execErr.stdout;
        } else {
          throw err;
        }
      }

      const report = JSON.parse(rawJson) as {
        stale: string[];
        open: { rule: number; file: string; line: number; detail: string }[];
        increases: { rule: number; file: string }[];
      };

      expect(report.stale).toEqual([]);
      const r1r4Open = report.open.filter((v) => targetFiles.has(v.file));
      expect(r1r4Open, JSON.stringify(r1r4Open)).toEqual([]);
      const r1r4Increases = report.increases.filter((v) => targetFiles.has(v.file));
      expect(r1r4Increases, JSON.stringify(r1r4Increases)).toEqual([]);

      // Also assert any remaining open violations across the repo are exclusively pre-existing unrelated dirty BrandMark.svelte
      const unexpectedOpen = report.open.filter(
        (v) => v.file !== "src/lib/components/BrandMark.svelte",
      );
      expect(unexpectedOpen, JSON.stringify(unexpectedOpen)).toEqual([]);
    });
  });

  describe("[Tier 3] [R2] Pair 14 (F4 <-> F10): status-popover 4-command ACL + MenuState bounded agentRequests", () => {
    it("enforces strict 4-command popover ACL and bounded agentRequests in Rust & TS contracts", async () => {
      const aclRes = await execCargoSuite(
        [
          "--test",
          "acl_contract",
          "the_status_popover_is_granted_only_the_commands_it_calls",
        ],
        { timeoutMs: 280_000 },
      );
      assertCargoPassed(aclRes, "cargo test --test acl_contract");

      const cap = JSON.parse(readRepoFile("src-tauri/capabilities/status-popover.json")) as {
        windows: string[];
        permissions: string[];
      };
      expect(cap.windows).toEqual(["status-popover"]);
      const grantedCommands = cap.permissions.filter((p) => p.startsWith("allow-cmd-"));
      expect(grantedCommands.sort()).toEqual([
        "allow-cmd-get-accessibility-display",
        "allow-cmd-get-status-state",
        "allow-cmd-resize-status",
        "allow-cmd-status-action",
      ]);

      const limits = MENU_LIMITS as Record<string, number>;
      expect(limits.agentRequests).toBe(8);

      const overflowRequests = Array.from({ length: 12 }, (_, i) =>
        makeMenuAgentRequest({
          id: `term:pty-${i + 1}`,
          source: "terminal",
          provider: i % 2 === 0 ? "claude" : "agy",
          agentLabel: `agent-${i + 1}`,
          repoPath: "/Projects/GitPulse",
          repoLabel: "GitPulse",
          urgency: "blocking",
          reason: "Needs your permission",
          detail: `Approve step ${i + 1}`,
          atMs: 1_700_000_000_000 + i,
        }),
      );
      const oversized = makeMenuStateWithRequests(overflowRequests, {
        agentRequestsHidden: 0,
      });

      expect(menuStateProblem(oversized)).not.toBeNull();
      const repaired = sendableMenuState(oversized);
      const repairedView = repaired.state as MenuStateFixture;
      expect(repairedView.agentRequests?.length).toBeLessThanOrEqual(8);
      expect(menuStateProblem(repaired.state)).toBeNull();
    }, 300_000);
  });
});
