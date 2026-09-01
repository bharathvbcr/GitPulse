import type { RepoState } from "../stores/repoStore";
import { viewTabForMenuId } from "../views/viewRegistry";

/**
 * Payload for every native-shell event, not just menu ones — `gitpulse-menu`
 * and `gitpulse-open-repo` both carry it. Named for the Rust struct it
 * mirrors, which is what lets check:types compare them; the old name
 * (NativeMenuPayload) described only half its uses.
 */
export interface NativeEvent {
  id: string;
  path?: string | null;
}

export interface NativeMenuHandlers {
  open: () => void;
  clone: () => void;
  settings: () => void;
  refresh: () => void;
  toggleTheme: () => void;
  themeSystem: () => void;
  themeLight: () => void;
  themeDark: () => void;
  setTab: (tab: RepoState["activeTab"]) => void;
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
  switch (payload.id) {
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
