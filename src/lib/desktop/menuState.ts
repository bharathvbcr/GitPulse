import type { RepoState } from "../stores/repoStore";
import type { InterfacePrefs } from "../stores/interfaceStore";
import type { ThemePreference } from "../stores/themeStore";
import { REGISTERED_VIEWS, resolveSection } from "../views/viewRegistry";
import { actionLabel, blocksOtherMutations, headline } from "../repos/operation";
import type { NativeEvent } from "./nativeActions";

export interface MenuLabel { id: string; text: string }
export interface MenuRepository {
  path: string;
  label: string;
  active: boolean;
  changed: number | null;
  conflicts: number | null;
  busy: boolean;
}
export interface StatusCard {
  repository: string;
  branch: string;
  changed: number | null;
  staged: number | null;
  conflicts: number | null;
  ahead: number | null;
  behind: number | null;
  upstream: string | null;
  headline: string;
  tone: string;
  primaryLabel: string;
  watchStatus: string;
  reduceMotion: boolean;
  stashes: number | null;
  operation: string | null;
  activity: string | null;
  elsewhere: number;
  fetchedAt: number | null;
}

export interface StatusShortcut {
  id: string;
  label: string;
  group: "go" | "tool";
}

export interface StatusInsight {
  id: string;
  text: string;
  tone: "warning" | "busy";
}

const STATUS_SHORTCUTS: StatusShortcut[] = [
  { id: "section:history:graph", label: "History", group: "go" },
  { id: "section:insights:pulse", label: "Pulse", group: "go" },
  { id: "fleet", label: "Fleet", group: "go" },
  { id: "terminal-dock", label: "Terminal", group: "go" },
  { id: "copy-branch", label: "Copy branch", group: "tool" },
  { id: "reveal-repo", label: "Reveal", group: "tool" },
  { id: "open-remote", label: "Remote", group: "tool" },
  { id: "toggle-theme", label: "Appearance", group: "tool" },
];
/** Bounded presentation, shared with desktop::state. Git authorization stays in the existing commands. */
export interface MenuState {
  enabled: string[];
  checked: string[];
  labels: MenuLabel[];
  repositories: MenuRepository[];
  activePath: string | null;
  showStatusIcon: boolean;
  hideDockWhenClosed: boolean;
  trayTitle: string | null;
  trayDetails: string[];
  traySummary: MenuLabel;
  trayDetail: string;
  status: StatusCard;
}

const GLOBAL_ACTIONS = ["open", "clone", "settings", "shortcuts", "diagnostics", "documentation",
  "release-notes", "report-issue", "setup-tools", "toggle-theme", "theme-system", "theme-light",
  "theme-dark", "fleet", "palette"];

/** Menu titles must not turn path control characters into accelerators or extra rows. */
export function menuText(value: string, max = 240): string {
  const text = value.replace(/[\u0000-\u001f\u007f]/g, " ");
  return Array.from(text).length > max ? Array.from(text).slice(0, max - 1).join("") + "…" : text;
}

export function buildMenuState(
  repo: RepoState,
  prefs: InterfacePrefs,
  theme: ThemePreference,
  activity: Record<string, string[]>,
  checkingUpdate: boolean,
  gitActivity: Record<string, string[]> = activity,
): MenuState {
  const enabled = [...GLOBAL_ACTIONS];
  const checked = [`theme-${theme}`];
  const labels: MenuLabel[] = [];
  const allow = (id: string, condition: boolean) => { if (condition) enabled.push(id); };
  const label = (id: string, text: string) => labels.push({ id, text: menuText(text) });
  const hasRepo = !!repo.currentPath;
  const busy = repo.currentPath ? activity[repo.currentPath] ?? [] : [];
  const loaded = hasRepo && !repo.isLoading && !repo.error;
  const idle = loaded && busy.length === 0;
  const worktree = idle && !repo.isBare && !repo.operation.probeFailed;
  const conflicts = new Set(repo.statuses.filter((file) => file.is_conflicted).map((file) => file.path)).size;
  const changes = new Set(repo.statuses.map((file) => file.path)).size;
  const mutable = worktree && !blocksOtherMutations(repo.operation) && conflicts === 0;
  const operation = repo.operation.operation;
  allow("check-updates", !checkingUpdate);
  label("check-updates", checkingUpdate ? "Checking for Updates…" : "Check for Updates…");
  allow("zoom-in", prefs.uiFontScale < 1.5);
  allow("zoom-out", prefs.uiFontScale > 0.75);
  allow("reset-zoom", prefs.uiFontScale !== 1);
  allow("clear-recents", repo.recentRepos.length > 0);
  allow("close-tab", hasRepo);
  allow("next-repo-tab", repo.openTabs.length > 1);
  allow("prev-repo-tab", repo.openTabs.length > 1);
  allow("reopen-repo-tab", repo.lastClosed.length > 0);
  for (const id of ["copy-repo-path", "reveal-repo", "terminal-dock", "focus-filter"]) allow(id, hasRepo);
  allow("copy-branch", loaded && !!repo.currentBranch);
  allow("copy-commit", loaded);
  allow("open-remote", idle);
  allow("refresh", hasRepo && !repo.isLoading && busy.length === 0);
  allow("fetch", idle);
  for (const id of ["pull", "push", "rebase", "create-branch"]) allow(id, mutable);
  allow("rename-branch", mutable && !!repo.currentBranch);
  allow("stash", mutable && changes > 0);
  allow("stash-pop", mutable && !repo.stashFailed && repo.stashEntries.length > 0);
  allow("quick-commit", mutable && changes > 0);
  allow("stage-all", worktree && repo.statuses.some((file) => !file.is_staged));
  allow("unstage-all", worktree && repo.statuses.some((file) => file.is_staged));
  for (const action of ["continue", "abort", "skip"] as const) {
    allow(`operation-${action}`, worktree && !!operation?.available.includes(action)
      && (action !== "continue" || conflicts === 0));
    label(`operation-${action}`, operation ? `${actionLabel(operation.kind, action)}${action === "continue" ? "" : "…"}`
      : `${action[0].toUpperCase()}${action.slice(1)} Operation${action === "continue" ? "" : "…"}`);
  }
  for (const [id, active, text] of [["fetch", "Fetching…", "Fetch"], ["pull", "Pulling…", "Pull"],
    ["push", "Pushing…", "Push"], ["stash", "Stashing…", "Stash Working Tree"],
    ["stash-pop", "Popping Stash…", "Pop Stash"], ["stage-all", "Staging…", "Stage All"],
    ["unstage-all", "Unstaging…", "Unstage All"]]) {
    const kind = id === "stage-all" ? "stage" : id === "unstage-all" ? "unstage" : id;
    label(id, busy.includes(kind) || busy.includes(id) || (id === "stash-pop" && busy.includes("unstash")) ? active : text);
  }
  label("terminal-dock", prefs.terminalDockOpen ? "Hide Terminal" : "Show Terminal");
  label("copy-commit", repo.selectedCommitId ? "Copy Selected Commit SHA" : "Copy HEAD SHA");
  if (prefs.terminalDockOpen && hasRepo) checked.push("terminal-dock");
  if (prefs.globalSurface === "fleet") checked.push("fleet");
  for (const view of REGISTERED_VIEWS) {
    allow(`tab-${view.id}`, hasRepo);
    if (hasRepo && prefs.globalSurface === "repository" && repo.activeTab === view.id) checked.push(`tab-${view.id}`);
    for (const section of view.sections ?? []) {
      const id = `section:${view.id}:${section.id}`;
      allow(id, hasRepo);
      if (hasRepo && prefs.globalSurface === "repository" && repo.activeTab === view.id && resolveSection(view.id, repo.viewSections[view.id]) === section.id) checked.push(id);
    }
  }
  const gitBusy = repo.currentPath ? gitActivity[repo.currentPath] ?? [] : [];
  const workingRepos = Object.entries(gitActivity).filter(([, actions]) => actions.length);
  const totalBusy = workingRepos.reduce((sum, [, actions]) => sum + actions.length, 0);
  const status = !hasRepo ? "No repository" : repo.isLoading ? "Loading…"
    : repo.error ? "Status unavailable" : repo.operation.probeFailed ? "Operation unavailable"
    : repo.isBare ? "Bare repository" : [changes ? `${changes} changed` : "Clean",
      conflicts ? `${conflicts} conflict${conflicts === 1 ? "" : "s"}` : ""].filter(Boolean).join(" · ");
  const activeRepo = repo.openTabs.find((tab) => tab.path === repo.currentPath);
  const branch = repo.currentBranch ?? (hasRepo ? "Detached / unborn HEAD" : "");
  const staged = new Set(repo.statuses.filter((file) => file.is_staged).map((file) => file.path)).size;
  const branchInfo = repo.branches.find((entry) => entry.is_current && entry.name === repo.currentBranch);
  const tracking = loaded && branchInfo?.upstream && !branchInfo.is_gone ? branchInfo : null;
  const sync = tracking && (tracking.ahead_count || tracking.behind_count)
    ? `↑${tracking.ahead_count} ↓${tracking.behind_count}` : "";
  const watch = repo.watch.status === "degraded" ? "Polling · live updates unavailable" :
    repo.watch.status === "watching" ? "Live updates" : "Live updates pending";
  const details = hasRepo ? [`Repository: ${repo.currentPath}`, `Branch: ${branch}`,
    ...(!loaded ? [status] : [`${changes} changed · ${staged} staged · ${conflicts} conflicts`,
      repo.stashFailed ? "Stashes unavailable" : `${repo.stashEntries.length} listed stashes`,
      tracking ? `${tracking.ahead_count} ahead · ${tracking.behind_count} behind ${tracking.upstream} (${
        repo.fetchedAt == null ? "never fetched" : `fetched ${formatFetchAge(repo.fetchedAt)}`
      })`
        : branchInfo?.is_gone ? "Upstream no longer exists" : branchInfo ? "No upstream configured" : "Upstream status unavailable", watch]),
    ...(operation ? [headline(operation)] : [])] : [];
  if (totalBusy) details.push(`${totalBusy} Git action${totalBusy === 1 ? "" : "s"} running${workingRepos.length > 8 ? ` · showing 8 of ${workingRepos.length} repositories` : ""}`,
    ...workingRepos
      .slice(0, 8).map(([path, actions]) => `${path.split(/[\\/]/).pop()}: ${actions.join(", ")}`));
  const trayTarget = !hasRepo ? "open" : !loaded || repo.operation.probeFailed ? "refresh"
    : conflicts ? "section:work:resolve" : changes || operation || gitBusy.length ? "section:work:overview" : "section:history:graph";
  const brief = !hasRepo ? "Open a repository…" : !loaded || repo.operation.probeFailed ? status
    : conflicts ? `Resolve ${conflicts} conflict${conflicts === 1 ? "" : "s"}`
    : gitBusy.length ? labels.find((entry) => entry.id === (gitBusy[0] === "unstash" ? "stash-pop" : gitBusy[0]))?.text ?? "Git action running…"
    : changes ? `${changes} changed${staged ? ` · ${staged} staged` : ""}`
    : operation ? headline(operation) : status;
  const elsewhere = totalBusy - gitBusy.length;
  const repositories = repo.openTabs.map((tab) => {
    const tabBusy = (gitActivity[tab.path] ?? []).length > 0 || tab.isLoading;
    const countsKnown = !tab.isLoading && !tab.error && !tab.isBare;
    return {
      path: tab.path,
      label: menuText(tab.label),
      active: tab.isActive,
      changed: countsKnown ? tab.changedCount : null,
      conflicts: countsKnown ? tab.conflictedCount : null,
      busy: tabBusy,
    };
  });
  const rawTitle = !prefs.statusIconCounts || !hasRepo || !loaded || repo.isBare
    ? null
    : [String(changes), conflicts ? `⚠${conflicts}` : null].filter(Boolean).join(" · ");
  const trayTitle = rawTitle ? menuText(rawTitle, 24) : null;
  return {
    enabled, checked, labels,
    repositories,
    activePath: repo.currentPath,
    showStatusIcon: prefs.showStatusIcon,
    hideDockWhenClosed: prefs.hideDockWhenClosed,
    trayTitle,
    status: {
      repository: menuText(activeRepo?.label ?? repo.currentPath?.split(/[\\/]/).pop() ?? "GitPulse", 100),
      branch: menuText(branch, 120),
      changed: loaded && !repo.isBare ? changes : null,
      staged: loaded && !repo.isBare ? staged : null,
      conflicts: loaded && !repo.operation.probeFailed ? conflicts : null,
      ahead: tracking?.ahead_count ?? null, behind: tracking?.behind_count ?? null,
      upstream: tracking?.upstream ? menuText(tracking.upstream, 120) : null,
      headline: !hasRepo ? "Your work, at a glance" : !loaded || repo.operation.probeFailed ? status
        : conflicts ? "Conflicts need your attention" : gitBusy.length ? brief
        : operation ? headline(operation) : repo.isBare ? "Bare repository" : changes ? "Ready to review" : "All changes committed",
      tone: !loaded ? (repo.error ? "warning" : "neutral") : conflicts || repo.operation.probeFailed ? "warning"
        : gitBusy.length ? "busy" : changes || operation ? "changed" : "clean",
      primaryLabel: !hasRepo ? "Open repository" : trayTarget === "refresh" ? "Try again"
        : conflicts ? "Resolve conflicts" : trayTarget === "section:history:graph" ? "View history" : "Review changes",
      watchStatus: repo.watch.status,
      reduceMotion: prefs.reduceMotion,
      stashes: loaded && !repo.isBare && !repo.stashFailed ? repo.stashEntries.length : null,
      operation: operation ? menuText(headline(operation), 120) : null,
      activity: gitBusy.length
        ? menuText(labels.find((entry) => entry.id === (gitBusy[0] === "unstash" ? "stash-pop" : gitBusy[0]))?.text
          ?? "Git action running…", 72)
        : null,
      elsewhere: Math.max(0, elsewhere),
      fetchedAt: loaded ? repo.fetchedAt : null,
    },
    trayDetails: details.map((detail) => menuText(detail)),
    traySummary: { id: trayTarget, text: menuText([brief, sync,
      repo.watch.status === "degraded" ? "Not live" : "", elsewhere ? `${elsewhere} running elsewhere` : ""].filter(Boolean).join(" · "), 72) },
    trayDetail: hasRepo ? [menuText(activeRepo?.label ?? repo.currentPath?.split(/[\\/]/).pop() ?? "Repository", 32),
      menuText(branch, 36)].join(" · ") : "GitPulse",
  };
}

/** Compact relative age for the tray/popover; empty when the clock is wrong. */
export function formatFetchAge(fetchedAtMs: number, nowMs = Date.now()): string {
  const delta = Math.max(0, nowMs - fetchedAtMs);
  const minutes = Math.floor(delta / 60_000);
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 48) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}

export function menuActionEnabled(state: MenuState, id: string): boolean {
  return state.enabled.includes(id);
}

export function statusShortcuts(state: MenuState): StatusShortcut[] {
  return STATUS_SHORTCUTS.filter((item) => state.enabled.includes(item.id));
}

export function statusInsights(card: StatusCard): StatusInsight[] {
  const items: StatusInsight[] = [];
  if (card.operation) items.push({ id: "section:work:overview", text: card.operation, tone: "warning" });
  if (card.activity) items.push({ id: "section:work:overview", text: card.activity, tone: "busy" });
  if (card.elsewhere > 0) {
    items.push({
      id: "fleet",
      text: `${card.elsewhere} running elsewhere`,
      tone: "busy",
    });
  }
  return items;
}

/** Copy, refresh and appearance stay in the popover; navigation still opens GitPulse. */
export function statusKeepsPopover(id: string): boolean {
  return id === "refresh" || id === "toggle-theme" || id.startsWith("copy-") || id.startsWith("activate-repo:");
}

export function statusDetailRows(lines: string[]): { label: string | null; value: string }[] {
  return lines.map((line) => {
    const at = line.indexOf(": ");
    if (at > 0 && at <= 18) return { label: line.slice(0, at), value: line.slice(at + 2) };
    return { label: null, value: line };
  });
}

export function statusKeyAction(
  event: { key: string; metaKey: boolean; ctrlKey: boolean; altKey: boolean },
  snapshot: MenuState | null,
  ui: { choosing: boolean; expanded: boolean },
): { dismiss?: true; collapse?: "chooser" | "details"; id?: string } | null {
  if (event.metaKey || event.ctrlKey || event.altKey) return null;
  if (event.key === "Escape") {
    if (ui.choosing) return { collapse: "chooser" };
    if (ui.expanded) return { collapse: "details" };
    return { dismiss: true };
  }
  if (!snapshot || ui.choosing) return null;
  const can = (id: string) => snapshot.enabled.includes(id);
  if (event.key === "r" || event.key === "R") return can("refresh") ? { id: "refresh" } : null;
  if (event.key === "Enter") return can(snapshot.traySummary.id) ? { id: snapshot.traySummary.id } : null;
  const card = snapshot.status;
  if (event.key === "1" && can("section:work:overview") && card.changed) return { id: "section:work:overview" };
  if (event.key === "2" && can("section:work:overview") && card.staged) return { id: "section:work:overview" };
  if (event.key === "3" && can("section:work:resolve") && card.conflicts) return { id: "section:work:resolve" };
  if (event.key === "4" && can("section:work:overview") && card.stashes) return { id: "section:work:overview" };
  return null;
}

export function canDispatchMenuEvent(event: NativeEvent, state: MenuState, repo: RepoState): boolean {
  if (event.id === "open-recent") return !!event.path && repo.recentRepos.includes(event.path);
  if (event.id === "activate-repo") return state.repositories.some((entry) => entry.path === event.path);
  if (!menuActionEnabled(state, event.id)) return false;
  const global = GLOBAL_ACTIONS.includes(event.id) || ["zoom-in", "zoom-out", "reset-zoom", "check-updates",
    "clear-recents", "next-repo-tab", "prev-repo-tab", "reopen-repo-tab"].includes(event.id);
  return global || event.repo_path === repo.currentPath;
}
