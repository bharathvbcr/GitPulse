import type { RepoState } from "../stores/repoStore";
import { REGISTERED_VIEWS, viewTabForMenuId } from "../views/viewRegistry";

/**
 * Payload for every native-shell event, not just menu ones — `gitpulse-menu`
 * and `gitpulse-open-repo` both carry it. Named for the Rust struct it
 * mirrors, which is what lets check:types compare them; the old name
 * (NativeMenuPayload) described only half its uses.
 */
export interface NativeEvent {
  id: string;
  path?: string | null;
  repo_path?: string | null;
}

/**
 * The native id for the workspace-wide Fleet dashboard.
 *
 * Exported so `scripts/fleet-surface-contract.test.ts` can assert the Rust and
 * TypeScript sides agree on it, rather than spelling the string twice and
 * letting a rename pass both halves silently. Deliberately outside the `tab-`
 * namespace: `viewTabForMenuId` claims everything in there.
 *
 * The switch below still spells the literal, because `view-menu-contract`
 * scans this file for `case "<id>":` to prove no native menu id is clickable
 * without a handler. A constant in the case label would be invisible to it —
 * which is why that contract test also asserts the two agree.
 */
export const FLEET_ACTION_ID = "fleet";

/**
 * The native id for the terminal dock.
 *
 * Outside the `tab-` namespace for the same reason as Fleet: `viewTabForMenuId`
 * claims everything in there, and the terminal is no longer a view. It is a
 * dock beneath whichever view is on screen, so routing it through `setTab`
 * would ask the app to navigate somewhere that does not exist.
 */
export const TERMINAL_DOCK_ACTION_ID = "terminal-dock";

export interface NativeMenuHandlers {
  canDispatch?: (event: NativeEvent) => boolean;
  activateRepo: (path: string) => void;
  clearRecents: () => void;
  checkUpdates: () => void;
  stageAll: () => void;
  unstageAll: () => void;
  createBranch: () => void;
  renameBranch: () => void;
  operationContinue: () => void;
  operationAbort: () => void;
  operationSkip: () => void;
  copyRepoPath: () => void;
  copyBranch: () => void;
  copyCommit: () => void;
  revealRepo: () => void;
  openRemote: () => void;

  open: () => void;
  clone: () => void;
  settings: () => void;
  refresh: () => void;
  toggleTheme: () => void;
  themeSystem: () => void;
  themeLight: () => void;
  themeDark: () => void;
  setTab: (tab: RepoState["activeTab"], section?: string) => void;
  shortcuts: () => void;
  diagnostics: () => void;
  documentation: () => void;
  releaseNotes: () => void;
  reportIssue: () => void;
  setupTools: () => void;
  zoomIn: () => void;
  zoomOut: () => void;
  resetZoom: () => void;
  /** Opens the workspace-wide Fleet dashboard. Not a view: see actions.rs. */
  fleet: () => void;
  /** Shows or hides the terminal dock. Not a view: see actions.rs. */
  terminalDock: () => void;
  fetch: () => void;
  pull: () => void;
  push: () => void;
  stash: () => void;
  stashPop: () => void;
  rebase: () => void;
  quickCommit: () => void;
  palette: () => void;
  focusFilter: () => void;
  openRecent: (path: string) => void;
  openRepo: (path: string) => void;
  closeRepoTab: () => void;
  nextRepoTab: () => void;
  prevRepoTab: () => void;
  reopenRepoTab: () => void;
  openError: (message: string) => void;
  setDropActive?: (active: boolean) => void;
}

export function dispatchNativeMenu(
  payload: NativeEvent,
  handlers: NativeMenuHandlers,
): boolean {
  if (handlers.canDispatch && !handlers.canDispatch(payload)) return false;
  switch (payload.id) {
    case "activate-repo":
      if (!payload.path) return false;
      handlers.activateRepo(payload.path);
      return true;
    case "clear-recents":
      handlers.clearRecents();
      return true;
    case "check-updates":
      handlers.checkUpdates();
      return true;
    case "stage-all":
      handlers.stageAll();
      return true;
    case "unstage-all":
      handlers.unstageAll();
      return true;
    case "create-branch":
      handlers.createBranch();
      return true;
    case "rename-branch":
      handlers.renameBranch();
      return true;
    case "operation-continue":
      handlers.operationContinue();
      return true;
    case "operation-abort":
      handlers.operationAbort();
      return true;
    case "operation-skip":
      handlers.operationSkip();
      return true;
    case "copy-repo-path":
      handlers.copyRepoPath();
      return true;
    case "copy-branch":
      handlers.copyBranch();
      return true;
    case "copy-commit":
      handlers.copyCommit();
      return true;
    case "reveal-repo":
      handlers.revealRepo();
      return true;
    case "open-remote":
      handlers.openRemote();
      return true;

    case "shortcuts":
      handlers.shortcuts();
      return true;
    case "diagnostics":
      handlers.diagnostics();
      return true;
    case "documentation":
      handlers.documentation();
      return true;
    case "release-notes":
      handlers.releaseNotes();
      return true;
    case "report-issue":
      handlers.reportIssue();
      return true;
    case "setup-tools":
      handlers.setupTools();
      return true;
    case "zoom-in":
      handlers.zoomIn();
      return true;
    case "zoom-out":
      handlers.zoomOut();
      return true;
    case "reset-zoom":
      handlers.resetZoom();
      return true;
    case "open":
      handlers.open();
      return true;
    case "clone":
      handlers.clone();
      return true;
    case "settings":
      handlers.settings();
      return true;
    case "refresh":
      handlers.refresh();
      return true;
    case "toggle-theme":
      handlers.toggleTheme();
      return true;
    case "theme-system":
      handlers.themeSystem();
      return true;
    case "theme-light":
      handlers.themeLight();
      return true;
    case "theme-dark":
      handlers.themeDark();
      return true;
    case "fetch":
      handlers.fetch();
      return true;
    case "pull":
      handlers.pull();
      return true;
    case "push":
      handlers.push();
      return true;
    case "stash":
      handlers.stash();
      return true;
    case "stash-pop":
      handlers.stashPop();
      return true;
    case "rebase":
      handlers.rebase();
      return true;
    case "quick-commit":
      handlers.quickCommit();
      return true;
    case "fleet":
      handlers.fleet();
      return true;
    case "terminal-dock":
      handlers.terminalDock();
      return true;
    case "palette":
      handlers.palette();
      return true;
    case "focus-filter":
      handlers.focusFilter();
      return true;
    case "open-recent":
      if (payload.path) handlers.openRecent(payload.path);
      return Boolean(payload.path);
    case "open-repo":
      if (payload.path) handlers.openRepo(payload.path);
      return Boolean(payload.path);
    case "close-tab":
      handlers.closeRepoTab();
      return true;
    case "next-repo-tab":
      handlers.nextRepoTab();
      return true;
    case "prev-repo-tab":
      handlers.prevRepoTab();
      return true;
    case "reopen-repo-tab":
      handlers.reopenRepoTab();
      return true;
    default: {
      // Match the complete destination against the live registry. Unknown or
      // retired sections must not silently navigate to a view's default.
      if (payload.id.startsWith("section:")) {
        for (const view of REGISTERED_VIEWS) {
          const section = view.sections?.find(
            (entry) => payload.id === `section:${view.id}:${entry.id}`,
          );
          if (section) {
            handlers.setTab(view.id, section.id);
            return true;
          }
        }
        return false;
      }
      // `tab-<view>` ids resolve through the view registry, so a newly
      // registered view is navigable without touching this file (this is how
      // 'tab-manvi' went missing).
      const tab = viewTabForMenuId(payload.id);
      if (!tab) return false;
      handlers.setTab(tab);
      return true;
    }
  }
}
