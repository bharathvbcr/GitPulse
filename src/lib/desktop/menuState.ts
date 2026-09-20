import type { RepoState } from "../stores/repoStore";
import type { InterfacePrefs } from "../stores/interfaceStore";
import type { ThemePreference } from "../stores/themeStore";
import { REGISTERED_VIEWS, resolveSection } from "../views/viewRegistry";
import { actionLabel, blocksOtherMutations, headline } from "../repos/operation";
import type { NativeEvent } from "./nativeActions";
import { hasUnstagedChanges } from "../files/fileStatus";
import { byteLength, MENU_LIMITS, MENU_WATCH_STATUSES } from "./menuContract";
import {
  identityKey,
  isCaseInsensitiveFs,
  isPathAmong,
  sameRepo,
  type PathIdentityOptions,
} from "../repos/paths";

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

/**
 * A switcher path state.rs will accept, or null when the row must be dropped.
 *
 * Trimming the path is not an option: it is the row's identity, both for the
 * `activate-repo:<path>` menu id and for `canDispatchMenuEvent`, so a shortened
 * one would render a row that activates nothing. Dropping it costs one entry in
 * the switcher; keeping it costs the entire native menu, because state.rs
 * rejects the whole payload rather than the offending row.
 */
function switcherPath(raw: string): string | null {
  return raw.length > 0 && byteLength(raw) <= MENU_LIMITS.path ? raw : null;
}

/**
 * A switcher badge inside the ceiling state.rs enforces, and inside `u32`.
 *
 * Serde refuses a negative or fractional count before `validate` ever runs, so
 * an impossible number from a degraded status read would wedge the menu with a
 * deserialization error rather than a named branch. A clamped badge on a
 * repository with a million pending changes is a rounding error next to that.
 */
function switcherCount(value: number): number | null {
  if (!Number.isFinite(value)) return null;
  return Math.min(Math.max(Math.trunc(value), 0), MENU_LIMITS.count);
}

/**
 * Per-repository activity, reachable by any spelling of the repository.
 *
 * The activity records are keyed by the *session* path — the OS-native string
 * the Git commands are invoked with, which on Windows keeps its backslashes and
 * any `\\?\` prefix. The switcher rows are keyed by the *tab* path, which
 * `openTab` normalised to forward slashes. On macOS and Linux the two spellings
 * are identical and a plain lookup worked; on Windows they never matched, so
 * every switcher row read as idle no matter what was running in it.
 *
 * Indexing by identity is what the rest of the app already does with repository
 * paths, and it is the reason this is a lookup rather than a comparison.
 */
function activityIndex(
  activity: Record<string, string[]>,
  options: PathIdentityOptions,
): (path: string | null) => string[] {
  const index = new Map<string, string[]>();
  for (const [path, actions] of Object.entries(activity)) {
    const key = identityKey(path, options);
    if (!key) continue;
    const existing = index.get(key);
    // Two spellings of one repository are one repository's work.
    if (existing) existing.push(...actions);
    else index.set(key, [...actions]);
  }
  return (path) => (path === null ? [] : index.get(identityKey(path, options)) ?? []);
}

export function buildMenuState(
  repo: RepoState,
  prefs: InterfacePrefs,
  theme: ThemePreference,
  activity: Record<string, string[]>,
  checkingUpdate: boolean,
  gitActivity: Record<string, string[]> = activity,
  // Repository identity is a platform property: Windows and macOS fold case,
  // Linux does not. Injectable so the suites can pin either answer rather than
  // testing only whichever host they happen to run on — the divergence this
  // guards against never reproduced on the Linux and macOS machines CI uses.
  options: PathIdentityOptions = { caseInsensitive: isCaseInsensitiveFs() },
): MenuState {
  const enabled = new Set(GLOBAL_ACTIONS);
  const checked = new Set([`theme-${theme}`]);
  // Keyed by id: state.rs rejects a payload that labels the same item twice,
  // and `apply_presentation` would silently honour only the first row anyway.
  const labels = new Map<string, MenuLabel>();
  const allow = (id: string, condition: boolean) => { if (condition) enabled.add(id); };
  const label = (id: string, text: string) => labels.set(id, { id, text: menuText(text) });
  const labelText = (id: string) => labels.get(id)?.text;
  const menuBusy = activityIndex(activity, options);
  const repoBusy = activityIndex(gitActivity, options);
  const hasRepo = !!repo.currentPath;
  const busy = menuBusy(repo.currentPath);
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
  allow("stage-all", worktree && repo.statuses.some(hasUnstagedChanges));
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
  // The dock belongs to the active repository tab, so the menu reads the repo
  // state rather than a workspace preference: switching to a repository whose
  // terminal is closed must say "Show Terminal", not keep offering to hide a
  // dock that is not on screen.
  label("terminal-dock", repo.terminalOpen ? "Hide Terminal" : "Show Terminal");
  label("copy-commit", repo.selectedCommitId ? "Copy Selected Commit SHA" : "Copy HEAD SHA");
  if (repo.terminalOpen && hasRepo) checked.add("terminal-dock");
  if (prefs.globalSurface === "fleet") checked.add("fleet");
  for (const view of REGISTERED_VIEWS) {
    allow(`tab-${view.id}`, hasRepo);
    if (hasRepo && prefs.globalSurface === "repository" && repo.activeTab === view.id) checked.add(`tab-${view.id}`);
    for (const section of view.sections ?? []) {
      const id = `section:${view.id}:${section.id}`;
      allow(id, hasRepo);
      if (hasRepo && prefs.globalSurface === "repository" && repo.activeTab === view.id && resolveSection(view.id, repo.viewSections[view.id]) === section.id) checked.add(id);
    }
  }
  const gitBusy = repoBusy(repo.currentPath);
  const workingRepos = Object.entries(gitActivity).filter(([, actions]) => actions.length);
  const totalBusy = workingRepos.reduce((sum, [, actions]) => sum + actions.length, 0);
  const status = !hasRepo ? "No repository" : repo.isLoading ? "Loading…"
    : repo.error ? "Status unavailable" : repo.operation.probeFailed ? "Operation unavailable"
    : repo.isBare ? "Bare repository" : [changes ? `${changes} changed` : "Clean",
      conflicts ? `${conflicts} conflict${conflicts === 1 ? "" : "s"}` : ""].filter(Boolean).join(" · ");
  // By identity, not by spelling: the tab carries the normalised path and
  // `currentPath` the OS-native one, so on Windows this matched nothing and the
  // card fell back to a bare basename instead of the disambiguated tab label.
  const activeRepo = repo.openTabs.find((tab) => sameRepo(tab.path, repo.currentPath ?? "", options));
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
    : gitBusy.length ? labelText(gitBusy[0] === "unstash" ? "stash-pop" : gitBusy[0]) ?? "Git action running…"
    : changes ? `${changes} changed${staged ? ` · ${staged} staged` : ""}`
    : operation ? headline(operation) : status;
  const elsewhere = totalBusy - gitBusy.length;
  // The switcher rows and the active pointer have to agree exactly: state.rs
  // rejects the whole payload when a row's `active` flag disagrees with
  // `activePath`, and a rejected payload updates nothing, so the menu bar, the
  // tray and the popover all freeze on whatever they last accepted.
  //
  // They used to come from different places — `active` from the workspace tab
  // list, `activePath` from the active session — and those disagree on every
  // activation that runs ahead of its session. `activateTab` publishes as soon
  // as the tab is current, while `currentPath` stays null until a session for
  // it exists, so each such publish sent a row claiming `active` against a null
  // pointer. Deriving both from the rows that actually survive keeps the pair
  // consistent by construction, including when a row is dropped below.
  const activeTab =
    repo.openTabs.find((tab) => tab.isActive) ??
    repo.openTabs.find((tab) => sameRepo(tab.path, repo.currentPath ?? "", options)) ??
    null;
  const repositories: MenuRepository[] = [];
  const switcherPaths = new Set<string>();
  for (const tab of repo.openTabs) {
    const path = switcherPath(tab.path);
    // A duplicate path is rejected outright by state.rs, and the identity that
    // made two rows distinct is gone by the time the payload is built, so the
    // second row could never be activated anyway.
    if (path === null || switcherPaths.has(path)) continue;
    if (repositories.length >= MENU_LIMITS.repositories) break;
    switcherPaths.add(path);
    const countsKnown = !tab.isLoading && !tab.error && !tab.isBare;
    repositories.push({
      path,
      label: menuText(tab.label),
      active: tab === activeTab,
      changed: countsKnown ? switcherCount(tab.changedCount) : null,
      conflicts: countsKnown ? switcherCount(tab.conflictedCount) : null,
      busy: repoBusy(tab.path).length > 0 || tab.isLoading,
    });
  }
  // Read back from the surviving rows rather than from `currentPath`: a pointer
  // at a repository the switcher does not list is the other half of the same
  // rejection.
  const activePath = repositories.find((entry) => entry.active)?.path ?? null;
  const rawTitle = !prefs.statusIconCounts || !hasRepo || !loaded || repo.isBare
    ? null
    : [String(changes), conflicts ? `⚠${conflicts}` : null].filter(Boolean).join(" · ");
  const trayTitle = rawTitle ? menuText(rawTitle, 24) : null;
  return {
    // Every list is capped on the way out. The counts below sit well inside
    // their ceilings today, but the ceilings belong to state.rs, and a payload
    // that outgrows one is refused whole rather than trimmed.
    enabled: [...enabled].slice(0, MENU_LIMITS.enabled),
    checked: [...checked].slice(0, MENU_LIMITS.checked),
    labels: [...labels.values()].slice(0, MENU_LIMITS.labels),
    repositories,
    activePath,
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
      headline: menuText(!hasRepo ? "Your work, at a glance" : !loaded || repo.operation.probeFailed ? status
        : conflicts ? "Conflicts need your attention" : gitBusy.length ? brief
        : operation ? headline(operation) : repo.isBare ? "Bare repository" : changes ? "Ready to review" : "All changes committed", 200),
      tone: !loaded ? (repo.error ? "warning" : "neutral") : conflicts || repo.operation.probeFailed ? "warning"
        : gitBusy.length ? "busy" : changes || operation ? "changed" : "clean",
      primaryLabel: !hasRepo ? "Open repository" : trayTarget === "refresh" ? "Try again"
        : conflicts ? "Resolve conflicts" : trayTarget === "section:history:graph" ? "View history" : "Review changes",
      // A watch state is a backend payload, and state.rs accepts exactly three
      // spellings of it. An unrecognised one reads as "not known yet", which is
      // what it is, rather than taking the whole menu down with it.
      watchStatus: (MENU_WATCH_STATUSES as readonly string[]).includes(repo.watch.status)
        ? repo.watch.status : "unknown",
      reduceMotion: prefs.reduceMotion,
      stashes: loaded && !repo.isBare && !repo.stashFailed ? repo.stashEntries.length : null,
      operation: operation ? menuText(headline(operation), 120) : null,
      activity: gitBusy.length
        ? menuText(labelText(gitBusy[0] === "unstash" ? "stash-pop" : gitBusy[0])
          ?? "Git action running…", 72)
        : null,
      elsewhere: Math.max(0, elsewhere),
      fetchedAt: loaded ? repo.fetchedAt : null,
    },
    trayDetails: details.slice(0, MENU_LIMITS.trayDetails).map((detail) => menuText(detail)),
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

/**
 * Whether a native event may be acted on, given what the menu was showing.
 *
 * The repository check is an anti-staleness guard: a menu item clicked after
 * the workspace moved on must not run against the repository it was drawn for.
 * `repo_path` is the `activePath` from the payload Rust last accepted, so it is
 * a normalised path, while `currentPath` is the OS-native one the Git commands
 * use. Comparing the two as strings made the guard reject everything on
 * Windows — every repository-scoped menu item and shortcut silently did
 * nothing — while passing on macOS and Linux, where the two spellings coincide.
 * Identity is what the guard always meant, and it is what the rest of the app
 * uses to decide whether two paths are one repository.
 */
export function canDispatchMenuEvent(
  event: NativeEvent,
  state: MenuState,
  repo: RepoState,
  options: PathIdentityOptions = { caseInsensitive: isCaseInsensitiveFs() },
): boolean {
  if (event.id === "open-recent") {
    return !!event.path && isPathAmong(event.path, repo.recentRepos, options);
  }
  if (event.id === "activate-repo") {
    return !!event.path && state.repositories.some((entry) => sameRepo(entry.path, event.path ?? "", options));
  }
  if (!menuActionEnabled(state, event.id)) return false;
  const global = GLOBAL_ACTIONS.includes(event.id) || ["zoom-in", "zoom-out", "reset-zoom", "check-updates",
    "clear-recents", "next-repo-tab", "prev-repo-tab", "reopen-repo-tab"].includes(event.id);
  return global || sameRepo(event.repo_path ?? "", repo.currentPath ?? "", options);
}
