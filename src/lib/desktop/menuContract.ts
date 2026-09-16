import type { MenuState } from "./menuState";
import { REGISTERED_VIEWS } from "../views/viewRegistry";

/**
 * The frontend half of the native-menu payload contract.
 *
 * `MenuState::validate` in `src-tauri/src/desktop/state.rs` rejects a
 * presentation that breaks any rule below, and a rejection is not a partial
 * update: `cmd_set_menu_state` returns before it touches the menu, so the menu
 * bar, the tray and the status popover all keep whatever they last accepted.
 * One malformed payload therefore freezes every native surface on stale data
 * until some later state happens to pass — which is exactly what the
 * `desktop:menu-state` diagnostic reports, and why the app looked wedged
 * rather than merely wrong.
 *
 * Nothing on this side enforced the rules. `buildMenuState` is now total: it
 * clamps, dedupes and drops so that every payload it returns satisfies the
 * validator by construction, which is what makes a runtime re-check here
 * unnecessary. `menuStateProblem` exists so the test suite can prove that
 * claim against hostile inputs instead of trusting it, and returns the same
 * message Rust would so a failure names the branch that tripped.
 *
 * `scripts/menu-state-contract.test.ts` reads state.rs and actions.rs and
 * asserts this mirror agrees with them on every limit, every enum and the full
 * action vocabulary, so the two cannot drift apart silently.
 */

/** Rust measures `String::len()` in bytes; `String.length` counts UTF-16 units. */
const ENCODER = new TextEncoder();
export function byteLength(text: string): number {
  return ENCODER.encode(text).length;
}

/** Every ceiling state.rs enforces, in the units it enforces them in. */
export const MENU_LIMITS = {
  /** Maximum `enabled` entries. */
  enabled: 128,
  /** Maximum `checked` entries. */
  checked: 64,
  /** Maximum `labels` entries. */
  labels: 64,
  /** Maximum switcher rows. */
  repositories: 64,
  /** Maximum `trayDetails` rows. */
  trayDetails: 16,
  /** Maximum bytes for any single presentation string. */
  text: 2048,
  /** Maximum bytes for a repository path. */
  path: 16384,
  /** Maximum code points in the tray title. */
  trayTitleChars: 24,
  /** Maximum value of a switcher row's changed/conflicts badge. */
  count: 1_000_000,
} as const;

export const MENU_TONES = ["neutral", "warning", "busy", "changed", "clean"] as const;
export const MENU_WATCH_STATUSES = ["unknown", "watching", "degraded"] as const;
/** The only actions the tray summary row may invoke. */
export const TRAY_SUMMARY_IDS = [
  "open",
  "refresh",
  "section:work:resolve",
  "section:work:overview",
  "section:history:graph",
] as const;

export const REPOSITORY_PREFIX = "activate-repo:";
export const RECENT_PREFIX = "open-recent:";

/**
 * Every id `NativeAction::parse` resolves to an action that carries no path.
 *
 * Path-carrying ids (`activate-repo:…`, `open-recent:…`) are deliberately
 * absent: state.rs accepts them nowhere in `enabled`, `checked` or `labels`,
 * because those lists are matched by exact id and a path is not a constant.
 */
export const MENU_ACTION_IDS: readonly string[] = [
  "clear-recents", "check-updates", "stage-all", "unstage-all", "create-branch",
  "rename-branch", "operation-continue", "operation-abort", "operation-skip",
  "copy-repo-path", "copy-branch", "copy-commit", "reveal-repo", "open-remote",
  "open", "clone", "settings", "shortcuts", "diagnostics", "documentation",
  "release-notes", "report-issue", "setup-tools", "zoom-in", "zoom-out",
  "reset-zoom", "refresh", "toggle-theme", "theme-system", "theme-light",
  "theme-dark", "tab-work", "tab-history", "tab-code", "tab-insights", "fleet",
  "terminal-dock", "fetch", "pull", "push", "stash", "stash-pop",
  "quick-commit", "rebase", "palette", "focus-filter", "close-tab",
  "next-repo-tab", "prev-repo-tab", "reopen-repo-tab",
];

/**
 * Section destinations, taken from the registry rather than spelled again.
 *
 * `scripts/view-menu-contract.test.ts` already proves the registry and
 * `actions::SECTION_MENUS` list the same sixteen destinations, so deriving
 * them here inherits that guarantee instead of opening a third place to drift.
 */
export const MENU_SECTION_IDS: readonly string[] = REGISTERED_VIEWS.flatMap((view) =>
  (view.sections ?? []).map((section) => `section:${view.id}:${section.id}`),
);

/** Mirrors the `known` closure in `MenuState::validate`. */
export function isKnownAction(id: string): boolean {
  if (id.startsWith("section:")) return MENU_SECTION_IDS.includes(id);
  return MENU_ACTION_IDS.includes(id);
}

/** Mirrors `state::checkable`. */
export function isCheckable(id: string): boolean {
  return (
    id.startsWith("tab-") ||
    id.startsWith("section:") ||
    id.startsWith(REPOSITORY_PREFIX) ||
    ["theme-system", "theme-light", "theme-dark", "fleet", "terminal-dock"].includes(id)
  );
}

/**
 * The message `MenuState::validate` would return for this payload, or null
 * when it would accept it. Branch order matches the Rust function so a
 * failure here points at the same check.
 */
export function menuStateProblem(state: MenuState): string | null {
  const card = state.status;
  const over = (text: string) => byteLength(text) > MENU_LIMITS.text;
  if (
    !(MENU_TONES as readonly string[]).includes(card.tone) ||
    !(MENU_WATCH_STATUSES as readonly string[]).includes(card.watchStatus) ||
    [card.repository, card.branch, card.headline, card.primaryLabel].some(over) ||
    [card.upstream, card.operation, card.activity].some((text) => text !== null && over(text))
  ) {
    return "Invalid status card presentation";
  }
  if (
    state.enabled.length > MENU_LIMITS.enabled ||
    state.checked.length > MENU_LIMITS.checked ||
    state.labels.length > MENU_LIMITS.labels ||
    state.repositories.length > MENU_LIMITS.repositories
  ) {
    return "Native menu state exceeds its entry limit";
  }
  if (
    state.enabled.some((id) => !isKnownAction(id)) ||
    state.checked.some((id) => !isKnownAction(id) || !isCheckable(id)) ||
    !(TRAY_SUMMARY_IDS as readonly string[]).includes(state.traySummary.id)
  ) {
    return "Native menu state contains an unknown action";
  }
  const labelIds = new Set<string>();
  for (const label of state.labels) {
    if (!isKnownAction(label.id) || over(label.text) || labelIds.has(label.id)) {
      return "Native menu state contains an invalid label";
    }
    labelIds.add(label.id);
  }
  const paths = new Set<string>();
  for (const repo of state.repositories) {
    if (
      !repo.path ||
      byteLength(repo.path) > MENU_LIMITS.path ||
      over(repo.label) ||
      paths.has(repo.path)
    ) {
      return "Native menu state contains an invalid repository";
    }
    paths.add(repo.path);
    if (repo.active !== (state.activePath === repo.path)) {
      return "Native menu active repository does not match its path";
    }
    if (
      (repo.changed !== null && repo.changed > MENU_LIMITS.count) ||
      (repo.conflicts !== null && repo.conflicts > MENU_LIMITS.count)
    ) {
      return "Native menu repository counts exceed their limit";
    }
  }
  if (state.activePath !== null && !paths.has(state.activePath)) {
    return "Native menu active repository is missing from the switcher";
  }
  if (
    (state.activePath !== null && byteLength(state.activePath) > MENU_LIMITS.path) ||
    state.trayDetails.length > MENU_LIMITS.trayDetails ||
    state.trayDetails.some(over) ||
    over(state.traySummary.text) ||
    over(state.trayDetail) ||
    (state.trayTitle !== null &&
      Array.from(state.trayTitle).length > MENU_LIMITS.trayTitleChars)
  ) {
    return "Native menu text exceeds its limit";
  }
  return null;
}

/**
 * Serde rejects the whole payload before `validate` ever runs when a field
 * cannot inhabit its Rust type, so a count that is negative, fractional or
 * past `u32::MAX` wedges the menu just as thoroughly as a broken invariant —
 * and silently, with a deserialization error rather than a named branch.
 * Asserted alongside `menuStateProblem` in the suites.
 */
export function menuStateWireProblem(state: MenuState): string | null {
  const u32 = (value: number | null, field: string) =>
    value === null || (Number.isInteger(value) && value >= 0 && value <= 0xff_ff_ff_ff)
      ? null
      : `${field} is not a u32: ${value}`;
  const card = state.status;
  const fields: [number | null, string][] = [
    [card.changed, "status.changed"],
    [card.staged, "status.staged"],
    [card.conflicts, "status.conflicts"],
    [card.ahead, "status.ahead"],
    [card.behind, "status.behind"],
    [card.stashes, "status.stashes"],
    [card.elsewhere, "status.elsewhere"],
    ...state.repositories.flatMap<[number | null, string]>((repo) => [
      [repo.changed, `repositories[${repo.path}].changed`],
      [repo.conflicts, `repositories[${repo.path}].conflicts`],
    ]),
  ];
  for (const [value, field] of fields) {
    const problem = u32(value, field);
    if (problem) return problem;
  }
  const fetched = card.fetchedAt;
  if (fetched !== null && !Number.isSafeInteger(fetched)) {
    return `status.fetchedAt is not an i64: ${fetched}`;
  }
  return null;
}
