import type { RepoState } from "../stores/repoStore";

export interface NativeMenuPayload {
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

const TAB_BY_ID: Record<string, RepoState["activeTab"]> = {
  "tab-history": "history",
  "tab-diff": "diff",
  "tab-conflict": "conflict",
  "tab-blame": "blame",
  "tab-coverage": "coverage",
  "tab-health": "health",
  "tab-stack": "stack",
  "tab-github": "github",
  "tab-reflog": "reflog",
};

export function dispatchNativeMenu(
  payload: NativeMenuPayload,
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
    case "tab-history":
    case "tab-diff":
    case "tab-conflict":
    case "tab-blame":
    case "tab-coverage":
    case "tab-health":
    case "tab-stack":
    case "tab-github":
    case "tab-reflog":
      handlers.setTab(TAB_BY_ID[payload.id]);
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
    default:
      return false;
  }
}
