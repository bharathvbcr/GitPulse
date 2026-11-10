import { get } from "svelte/store";
import { Bot, Bug, CircleUserRound, Download, FileCode, FolderGit2, FolderOpen, GitBranch, GitCommit, Keyboard, Layers, LayoutGrid, Moon, Percent, Plug, Plus, RefreshCw, Search, Settings, Terminal, Trash2, Upload, Wrench, X } from "@lucide/svelte";
import { repoStore, type MutationTarget, type RepoState } from "../stores/repoStore";
import { themeStore } from "../stores/themeStore";
import { interfaceStore } from "../stores/interfaceStore";
import { askText } from "../stores/modalStore";
import { promptQuickCommit } from "../commit/quickCommit";
import { VIEW_REGISTRY, describeDestination } from "../views/viewRegistry";
import { setEditorLens, showExplorerRail } from "../files/explorerLens";
import { displayName, isCaseInsensitiveFs, isPathAmong, sameRepo } from "../repos/paths";
import { openSetupWizard } from "../tools/onboardingStore";
import { SESSION_TYPES } from "../terminal/tabs";
import { hostPlatform } from "../stores/platformStore";
import { terminalLaunchRequests } from "../terminal/launchRequests";
import { PALETTE_MODES, type PaletteItem, type PaletteMode } from "./model";
import { MANVI_FOCUS_LIST, type ManviFocusId } from "../ui/manviFocus";
import { openManviFocus } from "../ui/openManviFocus";
import { openConflicts } from "../ui/workConflicts";
import { policyActivityVisible } from "../harness/policyPresentation";
import { harnessStore } from "../stores/harnessStore";

const icons = { work: GitBranch, code: FileCode, history: Search, insights: Percent };
/** Palette phrases for the Manvi cards Policy used to hold, by focus target. */
const MANVI_PALETTE_LABELS: Readonly<Record<ManviFocusId, string>> = {
  harness: "Open Manvi policy settings — connection and capability boundary",
  model: "Open local model servers — the models Manvi and agents can use",
  activity: "Open policy activity — Manvi verdicts, grants and the agent journal",
  cleanup: "Open branch and tag cleanup — merged branches and tags safe to delete",
};
const dispatch = (event: string) => { window.dispatchEvent(new CustomEvent(event)); };

export function repoUnavailable(state: RepoState): string | undefined {
  return !state.currentPath ? "Open a repository first." : state.isLoading ? "Wait for the repository to finish loading." : undefined;
}

export function worktreeUnavailable(state: RepoState): string | undefined {
  return repoUnavailable(state) ?? (state.isBare ? "This bare repository has no working tree." :
    state.operation.probeFailed ? "Operation status is unavailable. Refresh the repository first." :
    state.operation.operation ? `Finish the current Git operation in ${describeDestination("work", "overview")} first.` :
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
  // The repository this palette was built for. Every command that mutates it
  // passes this to the store, which runs there or refuses; a command that does
  // not go through the store (the terminal launch) checks it with `checkOrigin`.
  const target: MutationTarget | null = state.activeTabId && state.currentPath ? { tabId: state.activeTabId, path: state.currentPath } : null;
  const checkOrigin = () => {
    if (!repoStore.targetIsActive(target)) {
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
  // Read once per build, like `state`: the palette is rebuilt when it opens.
  const harness = get(harnessStore);
  const views: PaletteItem[] = Object.values(VIEW_REGISTRY).flatMap(view => [
    { id: view.id, label: view.paletteCommand ?? `Open ${view.label}`, description: view.summary, category: "Views", icon: icons[view.id], disabledReason: unavailable, action: () => {
      interfaceStore.setGlobalSurface("repository");
      repoStore.setActiveTab(view.id);
    } },
    ...(view.sections ?? []).map(section => ({
      id: `${view.id}:${section.id}`, label: section.paletteCommand ?? `Open ${view.label} ${section.label}`,
      description: `${view.label} → ${section.label} · ${section.summary}`, keywords: `${view.label} ${section.label}`, category: "Views", icon: icons[view.id], disabledReason: unavailable,
      action: () => {
        interfaceStore.setGlobalSurface("repository");
        repoStore.setActiveTab(view.id, section.id);
      },
    })),
    // Reflog was a History section, and the palette was one of its doors.
    // It folded into Graph as a list (views/historyList.ts), so this door
    // lands on Graph and switches the list; the registry derives no command
    // for it because it is no longer a section.
    ...(view.id === "history" ? [{
      id: "history:head-moves", label: "Open Reflog — HEAD movements and recovery points",
      description: "History → Graph → HEAD moves · Every movement of HEAD, including commits a reset or amend left behind.",
      keywords: "History Graph reflog HEAD moves recovery undo reset", category: "Views", icon: icons.history, disabledReason: unavailable,
      action: () => {
        interfaceStore.setGlobalSurface("repository");
        repoStore.setActiveTab("history", "graph");
        repoStore.setHistoryList("moves");
      },
    }] : []),
    // Resolve and Policy were sections of Work, and the palette was one of
    // their doors. Their content moved to the surfaces that own its subject,
    // so the registry derives no command for them; these keep a reader who
    // types "resolve", "conflicts", "policy" or "cleanup" landing on the card
    // that answers it rather than on nothing.
    ...(view.id === "work" ? [
      {
        id: "work:conflicts", label: "Resolve conflicts — finish a parked merge or rebase",
        description: "Work → Overview · The conflict editor leads Overview while a merge, rebase or cherry-pick is parked.",
        keywords: "Work Overview Resolve conflicts merge rebase cherry-pick parked continue abort markers", category: "Views", icon: icons.work,
        disabledReason: unavailable ?? (state.operation.operation || state.operation.probeFailed || state.statuses.some(file => file.is_conflicted) ? undefined : "No merge, rebase or cherry-pick is waiting on you."),
        action: () => {
          interfaceStore.setGlobalSurface("repository");
          openConflicts();
        },
      },
      ...MANVI_FOCUS_LIST.map(target => ({
        id: `manvi:${target.id}`, label: MANVI_PALETTE_LABELS[target.id],
        description: `${target.destination.label} → ${target.label}`,
        keywords: `Manvi Policy ${target.destination.label.replace("→", "")} ${target.label}`, category: "Views", icon: icons.work,
        disabledReason: target.destination.kind === "settings" ? undefined
          : unavailable ?? (target.id === "activity" && !policyActivityVisible(harness.harness, harness.actions.length, harness.error)
            ? "Manvi is not installed and nothing has been recorded." : undefined),
        action: () => {
          if (target.destination.kind === "view") interfaceStore.setGlobalSurface("repository");
          openManviFocus(target.id);
        },
      })),
    ] : []),
  ]);
  // Search and Blame were sections of Code; they are toggles inside Explorer
  // now (`files/explorerLens.ts`), so the registry no longer generates their
  // doors. They keep the ids and labels they had, so a remembered command
  // still finds them, and each lands on Explorer already showing its toggle.
  const explorerToggles: PaletteItem[] = [
    { id: "code:search", label: "Open Search — find text across the repository", description: "Code → Explorer → Search · Search file contents across the working tree or a revision, beside the editor. A capped, cut or cancelled result says it is partial.", keywords: "Code Explorer Search find grep text contents", category: "Views", icon: icons.code, disabledReason: unavailable, action: () => {
      interfaceStore.setGlobalSurface("repository");
      repoStore.setActiveTab("code", "explorer");
      showExplorerRail("search");
    } },
    { id: "code:blame", label: "Open Blame — line authorship and code age", description: "Code → Explorer → Blame · Who last touched each line of the open file, and how old that code is.", keywords: "Code Explorer Blame authorship annotate age", category: "Views", icon: icons.code, disabledReason: unavailable, action: () => {
      interfaceStore.setGlobalSurface("repository");
      repoStore.setActiveTab("code", "explorer");
      setEditorLens("blame");
    } },
  ];
  return [
    { id: "open_repo", label: "Open Repository…", description: "Choose a local Git repository", category: "Repositories", icon: FolderOpen, shortcut: "⌘T", closeBefore: true, action: () => repoStore.pickAndOpenRepo() },
    { id: "clone_repo", label: "Clone Repository…", description: "Clone a remote repository to a local folder", category: "Repositories", icon: Download, closeBefore: true, disabledReason: host.onClone ? undefined : "Cloning is unavailable in this window.", action: () => host.onClone?.() },
    { id: "tasks", label: "Open Tasks — global and workspace Kanban boards", category: "Workspace", icon: LayoutGrid, action: () => interfaceStore.setGlobalSurface("tasks") },
    { id: "fleet", label: "Open Fleet — every repository at a glance", category: "Workspace", icon: LayoutGrid, action: () => interfaceStore.setFleetOpen(true) },
    { id: "agents", label: "Open Agents — sessions, worktrees, and what needs you", category: "Workspace", icon: Bot, shortcut: "⌘⇧A", action: () => interfaceStore.setAgentsOpen(true) },
    { id: "terminal-dock", label: "Toggle Terminal — the shell, docked under this repository", category: "Workspace", icon: Terminal, shortcut: "⌃`", disabledReason: unavailable, action: () => repoStore.toggleTerminal() },
    // Starting a session had exactly one door: open the dock, then find the
    // launcher dropdown and the + beside it. Naming each launcher here makes
    // "give me Claude in this repository" one phrase, and it works whether or
    // not the dock is already showing.
    ...SESSION_TYPES.map((launcher) => ({
      id: `terminal-new-${launcher.kind}`,
      label: launcher.kind === "shell"
        ? "New Terminal Session — a shell in this repository"
        : launcher.kind === "browser"
          ? "New Browser Session — a local dev server in the terminal dock"
          : `New ${launcher.label} Session — in this repository`,
      keywords: launcher.kind === "browser"
        ? "terminal session new browser web page localhost dev server preview inspect"
        : `terminal shell session new ${launcher.label}`,
      category: "Workspace",
      icon: Terminal,
      disabledReason: launcher.kind === "browser" && !get(hostPlatform).browser_sessions
        ? "Browser sessions are not available on this computer yet."
        : unavailable,
      action: async () => {
        // The palette captured `state` when it was built; refuse rather than
        // start a shell in whichever repository is in front NOW.
        checkOrigin();
        const path = get(repoStore).currentPath;
        if (!path) throw Error("Open a repository first.");
        // Request first, then show the dock: the request is a bounded handoff
        // that waits for the panel, which may still be loading lazily.
        const launched = terminalLaunchRequests.request(path, launcher.kind);
        repoStore.setTerminalOpen(true);
        await launched;
      },
    })),
    { id: "refresh", label: "Refresh Repository Status", category: "Repository", icon: RefreshCw, shortcut: "⌘R", disabledReason: unavailable, action: () => repoStore.refresh() },
    { id: "quick_commit", label: "Quick Commit…", description: "Stage all changes and commit with a message", category: "Git actions", icon: GitCommit, shortcut: "⌘Enter", closeBefore: true, disabledReason: worktree ?? (state.statuses.length ? undefined : "Nothing to commit."), action: () => promptQuickCommit() },
    { id: "new_branch", label: "Create New Branch…", category: "Git actions", icon: Plus, closeBefore: true, disabledReason: worktree, action: async () => {
      const name = await askText({ title: "Create New Branch", message: "New branch name", placeholder: "feat/name", confirmLabel: "Create" });
      if (!name?.trim()) return { ok: false };
      return repoStore.createBranch(name.trim(), undefined, target);
    } },
    { id: "rename_branch", label: "Rename Current Branch…", description: state.currentBranch ?? "Detached HEAD", category: "Git actions", icon: GitBranch, closeBefore: true, disabledReason: worktree ?? (!state.currentBranch ? "Check out a local branch first." : undefined), action: async () => {
      const current = state.currentBranch;
      if (!current) return { ok: false };
      const name = await askText({ title: "Rename branch", message: current, initialValue: current, confirmLabel: "Rename" });
      if (!name?.trim() || name.trim() === current) return { ok: false };
      return repoStore.renameBranch(current, name.trim(), target);
    } },
    { id: "fetch", label: "Fetch All Remotes", category: "Git actions", icon: Download, disabledReason: unavailable, action: () => repoStore.fetch(undefined, target) },
    { id: "pull", label: "Pull (fast-forward)", category: "Git actions", icon: Download, disabledReason: worktree ?? (!state.currentBranch ? "Check out a branch first." : undefined), action: () => repoStore.pull(undefined, undefined, undefined, target) },
    { id: "pull-rebase", label: "Pull (rebase local commits)", description: "git pull --rebase: replay your unpushed commits on top of the upstream instead of merging", category: "Git actions", icon: Download, disabledReason: worktree ?? (!state.currentBranch ? "Check out a branch first." : undefined), action: () => repoStore.pull(undefined, undefined, true, target) },
    { id: "push", label: "Push Current Branch", category: "Git actions", icon: Upload, disabledReason: unavailable ?? (!state.currentBranch ? "Check out a branch first." : undefined), action: () => repoStore.push(undefined, undefined, false, target) },
    { id: "rebase", label: "Interactive Rebase…", description: "Open the rebase planner to review commits before applying changes", category: "Git actions", icon: GitBranch, closeBefore: true, disabledReason: worktree ?? (!state.currentBranch ? "Check out a branch first." : !host.onRebase ? "The rebase planner is unavailable in this window." : undefined), action: () => host.onRebase?.() },
    { id: "clean_branches", label: "Clean Stale / Dead Branches…", description: "Identify and safely prune merged and stale branches with deadbranch", category: "Git actions", icon: Trash2, closeBefore: true, disabledReason: unavailable, action: () => dispatch("gitpulse:branch-cleanup") },
    { id: "stash", label: "Stash Working Tree", category: "Git actions", icon: Layers, disabledReason: worktree ?? (state.statuses.length ? undefined : "The working tree is clean."), action: () => repoStore.stashSave(undefined, undefined, target) },
    { id: "stash_pop", label: "Pop Stash", description: state.stashEntries[0]?.subject ?? "Apply and remove the latest stash", category: "Git actions", icon: Layers, disabledReason: worktree ?? (state.stashFailed ? "Stash status is unavailable. Refresh first." : !state.stashEntries.length ? "No stash entries in this repository." : undefined), action: () => repoStore.stashPop(target) },
    ...views,
    ...explorerToggles,
    ...PALETTE_MODES.filter(entry => entry.mode !== "commands").map(entry => ({ id: `search:${entry.mode}`, label: `Search ${entry.label}`, description: entry.hint, category: "Search", icon: Search, keepOpen: true, action: () => changeMode(entry.mode) })),
    { id: "close_tab", label: "Close Repository Tab", category: "Repositories", icon: X, shortcut: "⌘⇧W", disabledReason: unavailable, action: () => repoStore.closeActiveTab() },
    { id: "next_tab", label: "Next Repository Tab", category: "Repositories", icon: FolderGit2, shortcut: "Ctrl+Tab", disabledReason: state.openTabs.length < 2 ? "Open another repository tab first." : undefined, action: () => repoStore.nextTab() },
    { id: "prev_tab", label: "Previous Repository Tab", category: "Repositories", icon: FolderGit2, shortcut: "Ctrl+⇧+Tab", disabledReason: state.openTabs.length < 2 ? "Open another repository tab first." : undefined, action: () => repoStore.prevTab() },
    { id: "reopen_tab", label: "Reopen Closed Repository", category: "Repositories", icon: FolderGit2, disabledReason: state.lastClosed.length ? undefined : "No closed repository to reopen.", action: () => repoStore.reopenLastClosed() },
    { id: "move_tab_left", label: "Move Repository Tab Left", category: "Repositories", icon: FolderGit2, shortcut: "Ctrl+⇧+←", disabledReason: !active || !repoStore.canMoveTab(active.id, -1) ? "This tab is already first." : undefined, action: () => { if (active) repoStore.moveTabBy(active.id, -1); } },
    { id: "move_tab_right", label: "Move Repository Tab Right", category: "Repositories", icon: FolderGit2, shortcut: "Ctrl+⇧+→", disabledReason: !active || !repoStore.canMoveTab(active.id, 1) ? "This tab is already last." : undefined, action: () => { if (active) repoStore.moveTabBy(active.id, 1); } },
    { id: "group_by_parent_folder", label: "Group All Repositories by Parent Folder", category: "Repositories", icon: FolderGit2, disabledReason: state.openTabs.length < 2 ? "Open multiple repositories first." : undefined, action: () => repoStore.groupByParentFolder() },
    { id: "collapse_all_groups", label: "Collapse All Repository Groups", category: "Repositories", icon: FolderGit2, disabledReason: state.openTabs.every(t => !t.group) ? "No repositories are currently grouped." : undefined, action: () => repoStore.collapseAllGroups() },
    { id: "collapse_inactive_groups", label: "Collapse Inactive Repository Groups", category: "Repositories", icon: FolderGit2, disabledReason: state.openTabs.every(t => !t.group) ? "No repositories are currently grouped." : undefined, action: () => repoStore.collapseInactiveGroups() },
    { id: "collapse_inactive_group_tabs", label: "Collapse Inactive Tabs in Groups", category: "Repositories", icon: FolderGit2, disabledReason: state.openTabs.every(t => !t.group) ? "No repositories are currently grouped." : undefined, action: () => interfaceStore.setCollapseInactiveGroupTabs(!get(interfaceStore).collapseInactiveGroupTabs) },
    { id: "collapse_inactive_include_shells", label: "Toggle Collapsing Active Shells in Inactive Tabs", category: "Repositories", icon: FolderGit2, action: () => interfaceStore.toggleCollapseInactiveIncludeShells() },
    { id: "expand_all_groups", label: "Expand All Repository Groups", category: "Repositories", icon: FolderGit2, disabledReason: (!state.collapsedGroups || state.collapsedGroups.length === 0) ? "No repository groups are currently collapsed." : undefined, action: () => repoStore.expandAllGroups() },
    { id: "ungroup_all", label: "Ungroup All Repositories", category: "Repositories", icon: FolderGit2, disabledReason: state.openTabs.every(t => !t.group) ? "No repositories are currently grouped." : undefined, action: () => repoStore.ungroupTabs() },
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
