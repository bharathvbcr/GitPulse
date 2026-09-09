import { get } from "svelte/store";
import { Bug, CircleUserRound, Download, FileCode, FolderGit2, FolderOpen, GitBranch, GitCommit, Keyboard, Layers, LayoutGrid, Moon, Percent, Plug, Plus, RefreshCw, Search, Settings, Terminal, Upload, Wrench, X } from "@lucide/svelte";
import { repoStore, type RepoState } from "../stores/repoStore";
import { themeStore } from "../stores/themeStore";
import { interfaceStore } from "../stores/interfaceStore";
import { askText } from "../stores/modalStore";
import { promptQuickCommit } from "../commit/quickCommit";
import { VIEW_REGISTRY } from "../views/viewRegistry";
import { displayName, isCaseInsensitiveFs, isPathAmong, sameRepo } from "../repos/paths";
import { openSetupWizard } from "../tools/onboardingStore";
import { PALETTE_MODES, type PaletteItem, type PaletteMode } from "./model";

const icons = { work: GitBranch, code: FileCode, history: Search, insights: Percent };
const dispatch = (event: string) => { window.dispatchEvent(new CustomEvent(event)); };

export function repoUnavailable(state: RepoState): string | undefined {
  return !state.currentPath ? "Open a repository first." : state.isLoading ? "Wait for the repository to finish loading." : undefined;
}

export function worktreeUnavailable(state: RepoState): string | undefined {
  return repoUnavailable(state) ?? (state.isBare ? "This bare repository has no working tree." :
    state.operation.probeFailed ? "Operation status is unavailable. Refresh the repository first." :
    state.operation.operation ? "Finish the current Git operation in Work → Resolve first." :
    state.statuses.some(file => file.is_conflicted) ? "Resolve conflicted files first." : undefined);
}

export interface PaletteHostActions {
  onClone?: () => void;
  onRebase?: () => void;
}

export function buildCommands(state: RepoState, changeMode: (mode: PaletteMode) => void, host: PaletteHostActions = {}): PaletteItem[] {
  const unavailable = repoUnavailable(state);
  const worktree = worktreeUnavailable(state);
  const active = state.openTabs.find(tab => tab.isActive);
  const activeIndex = state.openTabs.findIndex(tab => tab.isActive);
  const checkOrigin = () => {
    if (get(repoStore).activeTabId !== state.activeTabId || get(repoStore).currentPath !== state.currentPath) {
      throw Error("The active repository changed. Run the command again in the intended repository.");
    }
  };
  const navigate = (path: string) => async () => {
    const opened = await repoStore.openRepo(path);
    const current = get(repoStore);
    if (!opened || !current.currentPath || !sameRepo(current.currentPath, path, { caseInsensitive: isCaseInsensitiveFs() })) {
      throw Error(current.error || "The repository could not be opened.");
    }
  };
  const views: PaletteItem[] = Object.values(VIEW_REGISTRY).flatMap(view => [
    { id: view.id, label: view.paletteCommand ?? `Open ${view.label}`, description: view.summary, category: "Views", icon: icons[view.id], disabledReason: unavailable, action: () => repoStore.setActiveTab(view.id) },
    ...(view.sections ?? []).map(section => ({
      id: `${view.id}:${section.id}`, label: section.paletteCommand ?? `Open ${view.label} ${section.label}`,
      description: `${view.label} → ${section.label} · ${section.summary}`, keywords: `${view.label} ${section.label}`, category: "Views", icon: icons[view.id], disabledReason: unavailable,
      action: () => repoStore.setActiveTab(view.id, section.id),
    })),
  ]);
  return [
    { id: "open_repo", label: "Open Repository…", description: "Choose a local Git repository", category: "Repositories", icon: FolderOpen, shortcut: "⌘T", closeBefore: true, action: () => repoStore.pickAndOpenRepo() },
    { id: "clone_repo", label: "Clone Repository…", description: "Clone a remote repository to a local folder", category: "Repositories", icon: Download, closeBefore: true, disabledReason: host.onClone ? undefined : "Cloning is unavailable in this window.", action: () => host.onClone?.() },
    { id: "fleet", label: "Open Fleet — every repository at a glance", category: "Workspace", icon: LayoutGrid, action: () => interfaceStore.setFleetOpen(true) },
    { id: "terminal-dock", label: "Toggle Terminal — the shell, docked under the current view", category: "Workspace", icon: Terminal, shortcut: "⌃`", disabledReason: unavailable, action: () => interfaceStore.toggleTerminalDock() },
    { id: "refresh", label: "Refresh Repository Status", category: "Repository", icon: RefreshCw, shortcut: "⌘R", disabledReason: unavailable, action: () => repoStore.refresh() },
    { id: "quick_commit", label: "Quick Commit…", description: "Stage all changes and commit with a message", category: "Git actions", icon: GitCommit, shortcut: "⌘Enter", closeBefore: true, disabledReason: worktree ?? (state.statuses.length ? undefined : "Nothing to commit."), action: () => promptQuickCommit() },
    { id: "new_branch", label: "Create New Branch…", category: "Git actions", icon: Plus, closeBefore: true, disabledReason: worktree, action: async () => {
      const name = await askText({ title: "Create New Branch", message: "New branch name", placeholder: "feat/name", confirmLabel: "Create" });
      if (!name?.trim()) return { ok: false };
      checkOrigin();
      return repoStore.createBranch(name.trim());
    } },
    { id: "rename_branch", label: "Rename Current Branch…", description: state.currentBranch ?? "Detached HEAD", category: "Git actions", icon: GitBranch, closeBefore: true, disabledReason: worktree ?? (!state.currentBranch ? "Check out a local branch first." : undefined), action: async () => {
      const current = state.currentBranch;
      if (!current) return { ok: false };
      const name = await askText({ title: "Rename branch", message: current, initialValue: current, confirmLabel: "Rename" });
      if (!name?.trim() || name.trim() === current) return { ok: false };
      checkOrigin();
      return repoStore.renameBranch(current, name.trim());
    } },
    { id: "fetch", label: "Fetch All Remotes", category: "Git actions", icon: Download, disabledReason: unavailable, action: () => repoStore.fetch() },
    { id: "pull", label: "Pull (fast-forward)", category: "Git actions", icon: Download, disabledReason: worktree ?? (!state.currentBranch ? "Check out a branch first." : undefined), action: () => repoStore.pull() },
    { id: "push", label: "Push Current Branch", category: "Git actions", icon: Upload, disabledReason: unavailable ?? (!state.currentBranch ? "Check out a branch first." : undefined), action: () => repoStore.push() },
    { id: "rebase", label: "Interactive Rebase…", description: "Open the rebase planner to review commits before applying changes", category: "Git actions", icon: GitBranch, closeBefore: true, disabledReason: worktree ?? (!state.currentBranch ? "Check out a branch first." : !host.onRebase ? "The rebase planner is unavailable in this window." : undefined), action: () => host.onRebase?.() },
    { id: "stash", label: "Stash Working Tree", category: "Git actions", icon: Layers, disabledReason: worktree ?? (state.statuses.length ? undefined : "The working tree is clean."), action: () => repoStore.stashSave() },
    { id: "stash_pop", label: "Pop Stash", description: state.stashEntries[0]?.subject ?? "Apply and remove the latest stash", category: "Git actions", icon: Layers, disabledReason: worktree ?? (state.stashFailed ? "Stash status is unavailable. Refresh first." : !state.stashEntries.length ? "No stash entries in this repository." : undefined), action: () => repoStore.stashPop() },
    ...views,
    ...PALETTE_MODES.filter(entry => entry.mode !== "commands").map(entry => ({ id: `search:${entry.mode}`, label: `Search ${entry.label}`, description: entry.hint, category: "Search", icon: Search, keepOpen: true, action: () => changeMode(entry.mode) })),
    { id: "close_tab", label: "Close Repository Tab", category: "Repositories", icon: X, shortcut: "⌘⇧W", disabledReason: unavailable, action: () => repoStore.closeActiveTab() },
    { id: "next_tab", label: "Next Repository Tab", category: "Repositories", icon: FolderGit2, shortcut: "Ctrl+Tab", disabledReason: state.openTabs.length < 2 ? "Open another repository tab first." : undefined, action: () => repoStore.nextTab() },
    { id: "prev_tab", label: "Previous Repository Tab", category: "Repositories", icon: FolderGit2, shortcut: "Ctrl+⇧+Tab", disabledReason: state.openTabs.length < 2 ? "Open another repository tab first." : undefined, action: () => repoStore.prevTab() },
    { id: "reopen_tab", label: "Reopen Closed Repository", category: "Repositories", icon: FolderGit2, disabledReason: state.lastClosed.length ? undefined : "No closed repository to reopen.", action: () => repoStore.reopenLastClosed() },
    { id: "move_tab_left", label: "Move Repository Tab Left", category: "Repositories", icon: FolderGit2, shortcut: "Ctrl+⇧+←", disabledReason: activeIndex <= 0 ? "This tab is already first." : undefined, action: () => { if (active) repoStore.moveTabBy(active.id, -1); } },
    { id: "move_tab_right", label: "Move Repository Tab Right", category: "Repositories", icon: FolderGit2, shortcut: "Ctrl+⇧+→", disabledReason: activeIndex < 0 || activeIndex === state.openTabs.length - 1 ? "This tab is already last." : undefined, action: () => { if (active) repoStore.moveTabBy(active.id, 1); } },
    ...state.openTabs.map(tab => ({ id: `switch:${tab.id}`, label: `Switch to ${tab.label}`, description: tab.path, category: "Open repositories", icon: FolderGit2, disabledReason: tab.isActive ? "This repository is already active." : undefined, action: navigate(tab.path) })),
    ...state.recentRepos.filter(path => !isPathAmong(path, state.openTabs.map(tab => tab.path), { caseInsensitive: isCaseInsensitiveFs() })).map(path => ({ id: `recent:${path}`, label: `Open recent ${displayName(path)}`, description: path, category: "Recent repositories", icon: FolderGit2, action: navigate(path) })),
    { id: "theme", label: "Toggle Dark / Light Theme", category: "Appearance", icon: Moon, keywords: "appearance color", action: () => themeStore.toggle() },
    { id: "theme_system", label: "Use System Appearance", category: "Appearance", icon: Moon, action: () => themeStore.setPreference("system") },
    { id: "theme_light", label: "Use Light Appearance", category: "Appearance", icon: Moon, action: () => themeStore.setPreference("light") },
    { id: "theme_dark", label: "Use Dark Appearance", category: "Appearance", icon: Moon, action: () => themeStore.setPreference("dark") },
    { id: "toggle_author_avatars", label: "Toggle Author Avatars", category: "Appearance", icon: CircleUserRound, action: () => interfaceStore.toggleGraphAvatars() },
    { id: "zoom_in", label: "Increase Interface Size", keywords: "zoom in", category: "Appearance", icon: Plus, shortcut: "⌘+", action: () => interfaceStore.zoomIn() },
    { id: "zoom_out", label: "Decrease Interface Size", keywords: "zoom out", category: "Appearance", icon: Search, shortcut: "⌘−", action: () => interfaceStore.zoomOut() },
    { id: "zoom_reset", label: "Reset Interface Size", keywords: "zoom reset", category: "Appearance", icon: RefreshCw, shortcut: "⌘0", action: () => interfaceStore.resetZoom() },
    { id: "shortcuts", label: "Keyboard Shortcuts Cheat Sheet", category: "Help", icon: Keyboard, shortcut: "⌘/", closeBefore: true, action: () => dispatch("gitpulse:shortcuts") },
    { id: "diagnostics", label: "Open Diagnostics", category: "Help", icon: Bug, closeBefore: true, action: () => dispatch("gitpulse:diagnostics") },
    { id: "settings", label: "Open Settings", category: "Settings", icon: Settings, shortcut: "⌘,", closeBefore: true, action: () => dispatch("gitpulse:settings") },
    { id: "mcp_setup", label: "Connect an agent (MCP 2.0 / Agent Plugins)", category: "Settings", icon: Plug, closeBefore: true, action: () => dispatch("gitpulse:settings") },
    { id: "optional_tools_setup", label: "Set up optional tools (devmap / manvi)", category: "Settings", icon: Wrench, closeBefore: true, action: () => openSetupWizard("devmap", "explain") },
  ];
}

export function helpCommands(changeMode: (mode: PaletteMode) => void): PaletteItem[] {
  return [
    ...PALETTE_MODES.filter(entry => entry.mode !== "help").map(entry => ({ id: `help_${entry.mode}`, label: `Type ${entry.prefix} to search ${entry.label.toLowerCase()}`, description: entry.hint, category: "Search modes", icon: Search, keepOpen: true, action: () => changeMode(entry.mode) })),
    { id: "help_shortcuts", label: "View All Keyboard Shortcuts", category: "Help", icon: Keyboard, closeBefore: true, action: () => dispatch("gitpulse:shortcuts") },
    { id: "help_map_docs", label: "Open Map for docs search, doc graph, and cross-repo link candidates", category: "Help", icon: FileCode, disabledReason: repoUnavailable(get(repoStore)), action: () => repoStore.setActiveTab("code", "map") },
  ];
}
