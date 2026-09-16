import type { MenuLabel, MenuState } from "./menuState";
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
 * validator by construction. `menuStateProblem` returns the message Rust would,
 * so the suites can prove that against hostile input instead of trusting it,
 * and `sendableMenuState` applies the same check once more at the IPC boundary
 * — because "total" is a property of today's code, and the cost of it lapsing
 * is a permanently inert menu rather than one wrong row.
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

const DECODER = new TextDecoder();

/**
 * Shortens `text` to at most `max` UTF-8 bytes, never splitting a character.
 *
 * Cuts the encoded bytes and then walks back off any partial sequence, rather
 * than dropping code points and re-measuring: the strings that reach here are
 * the oversized ones by definition, and re-measuring each candidate is
 * quadratic in exactly the case this exists to handle.
 */
export function clampText(text: string, max: number): string {
  const bytes = ENCODER.encode(text);
  if (bytes.length <= max) return text;
  let end = max;
  // A UTF-8 continuation byte is 0b10xxxxxx; walk back to the sequence's lead.
  while (end > 0 && (bytes[end - 1] & 0b1100_0000) === 0b1000_0000) end -= 1;
  if (end > 0) {
    const lead = bytes[end - 1];
    const width = lead < 0x80 ? 1 : lead < 0xe0 ? 2 : lead < 0xf0 ? 3 : 4;
    // Keep that character only when all of it fits inside the budget.
    end = end - 1 + width <= max ? end - 1 + width : end - 1;
  }
  return DECODER.decode(bytes.subarray(0, end));
}

/** A count serde will take as `u32` and `validate` will accept, or null. */
function clampCount(value: number | null, max = MENU_LIMITS.count): number | null {
  if (value === null || !Number.isFinite(value)) return null;
  return Math.min(Math.max(Math.trunc(value), 0), max);
}

/**
 * A payload the native side will accept, repaired if it would not have been.
 *
 * `buildMenuState` is total, and the suites hold it to that — but "total" is a
 * property of today's code, and the cost of it lapsing is out of all proportion
 * to the mistake. A refusal from `cmd_set_menu_state` applies nothing, so the
 * menu bar, the tray and the status popover stop tracking the workspace
 * entirely and stay stopped: every later payload is refused for the same
 * reason. That is how one wrong field became an inert application.
 *
 * So the last thing between the builder and the IPC boundary is a check, and a
 * repair rather than a refusal of our own. The switcher is dropped whole rather
 * than patched row by row, because its rule is an agreement between two fields
 * and a half-corrected switcher is how the original defect looked. Everything
 * else is clamped to something sendable. A reader loses the repository list;
 * they do not lose the menu.
 *
 * The problem is returned rather than swallowed: a repair that nobody hears
 * about is a bug that never gets fixed.
 *
 * The final fall back to `fallbackMenuState` is unreachable while the clamps
 * above cover every field — and that is the point of it. The way this fails
 * again is a new field on `MenuState` that nobody thought to clamp here, which
 * is precisely the case the clamps cannot anticipate and a known-good payload
 * can. Its own test pins that payload as sendable.
 */
export function sendableMenuState(state: MenuState): { state: MenuState; problem: string | null } {
  const problem = menuStateWireProblem(state) ?? menuStateProblem(state);
  if (problem === null) return { state, problem: null };

  const labels: MenuLabel[] = [];
  const seen = new Set<string>();
  for (const label of state.labels) {
    if (!isKnownAction(label.id) || seen.has(label.id)) continue;
    if (labels.length >= MENU_LIMITS.labels) break;
    seen.add(label.id);
    labels.push({ id: label.id, text: clampText(label.text, MENU_LIMITS.text) });
  }
  const card = state.status;
  const repaired: MenuState = {
    ...state,
    enabled: state.enabled.filter(isKnownAction).slice(0, MENU_LIMITS.enabled),
    checked: state.checked
      .filter((id) => isKnownAction(id) && isCheckable(id))
      .slice(0, MENU_LIMITS.checked),
    labels,
    repositories: [],
    activePath: null,
    trayDetails: state.trayDetails
      .slice(0, MENU_LIMITS.trayDetails)
      .map((row) => clampText(row, MENU_LIMITS.text)),
    traySummary: {
      id: (TRAY_SUMMARY_IDS as readonly string[]).includes(state.traySummary.id)
        ? state.traySummary.id
        : "open",
      text: clampText(state.traySummary.text, MENU_LIMITS.text),
    },
    trayDetail: clampText(state.trayDetail, MENU_LIMITS.text),
    trayTitle:
      state.trayTitle === null
        ? null
        : Array.from(state.trayTitle).slice(0, MENU_LIMITS.trayTitleChars).join(""),
    status: {
      ...card,
      repository: clampText(card.repository, MENU_LIMITS.text),
      branch: clampText(card.branch, MENU_LIMITS.text),
      headline: clampText(card.headline, MENU_LIMITS.text),
      primaryLabel: clampText(card.primaryLabel, MENU_LIMITS.text),
      upstream: card.upstream === null ? null : clampText(card.upstream, MENU_LIMITS.text),
      operation: card.operation === null ? null : clampText(card.operation, MENU_LIMITS.text),
      activity: card.activity === null ? null : clampText(card.activity, MENU_LIMITS.text),
      tone: (MENU_TONES as readonly string[]).includes(card.tone) ? card.tone : "neutral",
      watchStatus: (MENU_WATCH_STATUSES as readonly string[]).includes(card.watchStatus)
        ? card.watchStatus
        : "unknown",
      changed: clampCount(card.changed),
      staged: clampCount(card.staged),
      conflicts: clampCount(card.conflicts),
      ahead: clampCount(card.ahead),
      behind: clampCount(card.behind),
      stashes: clampCount(card.stashes),
      elsewhere: clampCount(card.elsewhere) ?? 0,
      fetchedAt: Number.isSafeInteger(card.fetchedAt) ? card.fetchedAt : null,
    },
  };
  const remaining = menuStateWireProblem(repaired) ?? menuStateProblem(repaired);
  return { state: remaining === null ? repaired : fallbackMenuState(state), problem };
}

/**
 * The payload of last resort: what the native side already starts up with.
 *
 * Mirrors `MenuState::default()`, which `validate` accepts by construction, and
 * carries across only the two preferences that decide where the app lives — a
 * reader who asked for a menu-bar-only GitPulse must not get their Dock icon
 * back because a status card was malformed.
 */
export function fallbackMenuState(state: MenuState): MenuState {
  return {
    enabled: [...MENU_ACTION_IDS],
    checked: [],
    labels: [],
    repositories: [],
    activePath: null,
    showStatusIcon: state.showStatusIcon,
    hideDockWhenClosed: state.hideDockWhenClosed,
    trayTitle: null,
    trayDetails: [],
    traySummary: { id: "open", text: "Open a repository…" },
    trayDetail: "GitPulse",
    status: {
      repository: "GitPulse",
      branch: "",
      changed: null,
      staged: null,
      conflicts: null,
      ahead: null,
      behind: null,
      upstream: null,
      headline: "Your work, at a glance",
      tone: "neutral",
      primaryLabel: "Open repository",
      watchStatus: "unknown",
      reduceMotion: state.status.reduceMotion,
      stashes: null,
      operation: null,
      activity: null,
      elsewhere: 0,
      fetchedAt: null,
    },
  };
}
