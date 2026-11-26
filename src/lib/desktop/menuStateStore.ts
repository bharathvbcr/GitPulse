import { get, readable, writable } from "svelte/store";
import { repoStore, type RepoState } from "../stores/repoStore";
import { interfaceStore, type InterfacePrefs } from "../stores/interfaceStore";
import { themeStore, type ThemePreference } from "../stores/themeStore";
import { updateCheckInFlight } from "../updates/updateCheck";
import { diagnostics } from "../diagnostics/diagnostics";
import { sessionActivity, type SessionActivity } from "../terminal/sessionActivity";
import { terminalSessions, type TerminalSessionRecord } from "../terminal/sessionRegistry";
import { buildMenuState, projectMenuAgentState, type MenuState } from "./menuState";
import { fallbackMenuState, sendableMenuState } from "./menuContract";
import { expandedStacks } from "../repos/stackState";
import { collapsedInactiveGroups, explicitlyExpandedGroups } from "../repos/groupState";
import { isTauri } from "../platform";
import { invoke } from "../ipc/invoke";
import { createBoardAgents, type BoardAgents } from "../workbench/boardAgents";
import { subscribeWorkbenchChanges } from "../workbench/client";
import { bindForegroundChanges, readBackgroundDocument } from "../runtime/foreground";
import type { AgentInboxThread, InboxSnapshot } from "../agents/inboxTypes";

/** Dialog/read activity before a Git command starts; the store owns actual mutations. */
export const menuActivity = writable<Record<string, string[]>>({});

const MIN_INBOX_REFRESH_MS = 5_000;
const EMPTY_BOARD: BoardAgents = { runs: [], complete: true, pending: new Map(), readAt: null, error: null };

/**
 * Reported once per distinct fault rather than once per projection.
 *
 * The store republishes on every status tick, so a payload the native side
 * would refuse is not one event — it is one per tick, for as long as the
 * workspace stays in that shape. The shipped build logged five `desktop:menu-state`
 * errors for a single cause, which reads as five faults and named none of them.
 */
const reported = new Set<string>();

function computeMenuState(
  repo: RepoState,
  prefs: InterfacePrefs,
  theme: ThemePreference,
  activity: Record<string, string[]>,
  pending: Record<string, string[]>,
  checking: boolean,
  sessions: ReadonlyMap<string, SessionActivity>,
  records: readonly TerminalSessionRecord[],
  board: BoardAgents,
  inboxThreads: readonly AgentInboxThread[],
): MenuState {
  const agentProjection = projectMenuAgentState(
    sessions,
    records,
    { runs: board.runs, pending: board.pending, clock: board.readAt ?? Date.now() },
    inboxThreads,
    repo.openTabs,
    prefs.bellAsksForReader,
    undefined,
    repo.currentPath,
  );
  const built = buildMenuState(
    repo,
    prefs,
    theme,
    { ...pending, ...activity },
    checking,
    activity,
    undefined,
    agentProjection.agentsWaiting,
    repoStore.jumpStopCount(),
    agentProjection,
  );
  const { state, problem } = sendableMenuState(built);
  if (problem !== null && !reported.has(problem)) {
    reported.add(problem);
    diagnostics.error("desktop:menu-state", `Repaired an unsendable native menu payload: ${problem}`);
  }
  return state;
}

const INITIAL_MENU_STATE: MenuState = fallbackMenuState({
  ...buildMenuState(
    get(repoStore),
    get(interfaceStore),
    get(themeStore.preferenceState),
    {},
    false,
  ),
});

type ScopedInboxThread = AgentInboxThread & { repo_path?: string | null };

export function trustedInboxRepoPaths(repo: RepoState): string[] {
  const paths: string[] = [];
  const seen = new Set<string>();
  for (const tab of repo.openTabs) {
    if (tab.path && !tab.trustRequired && !seen.has(tab.path)) {
      seen.add(tab.path);
      paths.push(tab.path);
    }
  }
  if (
    repo.currentPath &&
    !seen.has(repo.currentPath) &&
    !repo.openTabs.some((tab) => tab.path === repo.currentPath && tab.trustRequired)
  ) {
    paths.push(repo.currentPath);
  }
  return paths;
}

export const nativeMenuState = readable<MenuState>(INITIAL_MENU_STATE, (set) => {
  let repo = get(repoStore);
  let prefs = get(interfaceStore);
  let theme = get(themeStore.preferenceState);
  let activity = get(repoStore.mutationActivity);
  let pending = get(menuActivity);
  let checking = get(updateCheckInFlight);
  let sessions = get(sessionActivity);
  let records = get(terminalSessions);
  let board: BoardAgents = EMPTY_BOARD;
  let inboxThreads: readonly ScopedInboxThread[] = [];
  let inboxReposKey = "";
  let lastInboxAt = 0;
  let inboxInFlight = false;
  let queuedInboxForce = false;
  let disposed = false;
  let started = false;

  const publish = () => {
    if (!started || disposed) return;
    set(computeMenuState(repo, prefs, theme, activity, pending, checking, sessions, records, board, inboxThreads));
  };

  const refreshInbox = async (force: boolean) => {
    if (disposed || !isTauri()) return;
    const targetPaths = trustedInboxRepoPaths(repo);
    const nextReposKey = targetPaths.join("\0");
    if (targetPaths.length === 0) {
      queuedInboxForce = false;
      if (inboxThreads.length > 0 || inboxReposKey !== "") {
        inboxThreads = [];
        inboxReposKey = "";
        publish();
      }
      return;
    }
    const now = Date.now();
    if (!force && inboxReposKey === nextReposKey && now - lastInboxAt < MIN_INBOX_REFRESH_MS) {
      return;
    }
    if (inboxInFlight) {
      if (force) queuedInboxForce = true;
      return;
    }
    inboxInFlight = true;
    try {
      const settled = await Promise.allSettled(
        targetPaths.map(async (targetPath) => {
          const snap = await invoke<InboxSnapshot>("cmd_inbox_read", {
            repoPath: targetPath,
            status: "open",
            limit: 20,
          });
          const resolvedRepo = snap?.repo_path || targetPath;
          const rows = Array.isArray(snap?.threads) ? snap.threads : [];
          return rows.map((thread) => ({ ...thread, repo_path: resolvedRepo }));
        }),
      );
      if (disposed) return;
      const currentKey = trustedInboxRepoPaths(repo).join("\0");
      if (currentKey !== nextReposKey) {
        queuedInboxForce = true;
        return;
      }
      const nextThreads: ScopedInboxThread[] = [];
      for (const result of settled) {
        if (result.status === "fulfilled") {
          nextThreads.push(...result.value);
        }
      }
      inboxReposKey = nextReposKey;
      lastInboxAt = Date.now();
      inboxThreads = nextThreads;
      publish();
    } catch {
      if (!disposed) {
        inboxThreads = [];
        inboxReposKey = nextReposKey;
        publish();
      }
    } finally {
      inboxInFlight = false;
      if (queuedInboxForce && !disposed) {
        queuedInboxForce = false;
        void refreshInbox(true);
      }
    }
  };

  let lastReposKey = trustedInboxRepoPaths(repo).join("\0");
  const stops: Array<() => void> = [
    repoStore.subscribe((value) => {
      repo = value;
      const nextReposKey = trustedInboxRepoPaths(repo).join("\0");
      if (nextReposKey !== lastReposKey) {
        lastReposKey = nextReposKey;
        void refreshInbox(true);
      }
      publish();
    }),
    interfaceStore.subscribe((value) => { prefs = value; publish(); }),
    themeStore.preferenceState.subscribe((value) => { theme = value; publish(); }),
    repoStore.mutationActivity.subscribe((value) => { activity = value; publish(); }),
    menuActivity.subscribe((value) => { pending = value; publish(); }),
    updateCheckInFlight.subscribe((value) => { checking = value; publish(); }),
    sessionActivity.subscribe((value) => { sessions = value; publish(); }),
    terminalSessions.subscribe((value) => { records = value; publish(); }),
    expandedStacks.subscribe(() => publish()),
    collapsedInactiveGroups.subscribe(() => publish()),
    explicitlyExpandedGroups.subscribe(() => publish()),
  ];

  if (isTauri()) {
    const boardWatch = createBoardAgents();
    stops.push(boardWatch.subscribe((value) => { board = value; publish(); }));
    boardWatch.start();
    stops.push(() => boardWatch.stop());

    const unlistenWorkbench = subscribeWorkbenchChanges({
      changed: () => {
        void boardWatch.refresh();
        void refreshInbox(false);
      },
      unavailable: () => {},
    });
    stops.push(unlistenWorkbench);

    if (typeof document !== "undefined" && typeof window !== "undefined") {
      const unbindForeground = bindForegroundChanges(document, window, () => {
        if (!readBackgroundDocument()) {
          boardWatch.wake();
          void refreshInbox(false);
        }
      });
      stops.push(unbindForeground);

      const onInboxChanged = () => {
        void refreshInbox(true);
      };
      window.addEventListener("gitpulse:inbox-changed", onInboxChanged);
      stops.push(() => window.removeEventListener("gitpulse:inbox-changed", onInboxChanged));
    }

    void refreshInbox(true);
  }

  started = true;
  publish();

  return () => {
    disposed = true;
    for (const stop of stops) stop();
  };
});
