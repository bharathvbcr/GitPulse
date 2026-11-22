import { writable } from "svelte/store";
import { createDeferredWrite, flushOnPageHide } from "../async/deferredWrite";
import {
  clampTerminalFontSize,
  clampTerminalScrollback,
  TERMINAL_AGENT_SCROLLBACK_DEFAULT,
  TERMINAL_FONT_DEFAULT,
  TERMINAL_SCROLLBACK_DEFAULT,
} from "../terminal/viewControls";
import { SESSION_TYPES, type SessionKind } from "../terminal/tabs";
import { setSignalledAsksForReader } from "../terminal/sessionActivity";
import {
  isGraphWidthMode,
  type GraphWidthMode,
} from "../graph/graphLayout";
import type { ViewTab } from "../repos/persist";
import { isRefScope, type RefScope } from "../graph/refScope";
import { sanitizeHiddenViews } from "../views/viewVisibility";
import { isStatusBarMode, type StatusBarMode } from "../ui/statusBarMode";
import {
  isDiagnosticsButtonMode,
  type DiagnosticsButtonMode,
} from "../ui/diagnosticsButton";
import {
  installHeaderLabels,
  readHeaderLabelMode,
  type HeaderLabelMode,
} from "../ui/headerLabels";
import {
  clampTerminalDockHeight,
  TERMINAL_DOCK_DEFAULT_HEIGHT,
} from "../terminal/dockMetrics";
import { clampPaneHeight, COMMIT_PANE_BOUNDS } from "../ui/paneHeight";
import { DEFAULT_ACCENT, isAccentId, type AccentId } from "../ui/accents";
import {
  APPEARANCE_DEFAULTS,
  FLOAT_BLUR_MAX,
  WALLPAPER_INTENSITY_RANGE,
  installAppearance,
  sanitizeAppearance,
  type AppearancePrefs,
  type FieldLevel,
} from "../ui/appearance/apply";
import type { Appearance } from "../ui/color/derive";
import { densityFromLegacy, type Density } from "../ui/appearance/density";
import type { GitPulseTheme } from "../ui/appearance/themeSchema";
import type { BackdropId } from "../ui/appearance/backdrops";
import { clearThemePatch, presetPatch, themeSelectionPatch, type ThemePreset } from "../ui/appearance/presets";
import { withSavedTheme, withoutSavedTheme, type SaveOutcome } from "../ui/appearance/savedThemes";
import { customWallpaper } from "../ui/appearance/wallpaperStore";
import { installWallpaperMotion } from "../ui/appearance/wallpaperMotion";
import { hostLoadPulse, startHostLoad } from "./hostLoadStore";
import { themeStore } from "./themeStore";
import { hostPlatform } from "./platformStore";
import { isAgentFilter, type AgentFilter } from "../agents/plane";
import {
  DEFAULT_TAB_WIDTH,
  isDiffLayout,
  isTabWidth,
  type DiffLayout,
  type TabWidth,
} from "../ui/codeDisplay";
import { isTimestampStyle, type TimestampStyle } from "../ui/timestampStyle";
import {
  DEFAULT_BRANCH_ROW_LAYOUT,
  isBranchRowLayout,
  type BranchRowLayout,
} from "../sidebar/metrics";
import {
  DEFAULT_TASK_CARD_FIELDS,
  isBoardLayout,
  isTaskDensity,
  sanitizeCardFields,
  sanitizeHiddenStatuses,
  toggleHiddenStatus,
  type BoardLayout,
  type TaskCardField,
  type TaskDensity,
} from "../ui/taskView";
import type { TaskStatus } from "../workbench/vocabulary";
import { sanitizeHiddenRepositories, toggleHiddenRepository } from "../workbench/taskHiddenRepositories";
import {
  DEFAULT_TASK_REPOSITORY_SORT,
  isTaskRepositorySort,
  type TaskRepositorySort,
} from "../workbench/taskRepositories";
import {
  deleteView,
  isSwimlane,
  isTaskOrder,
  sanitizeBoards,
  saveView,
  setWipLimit,
  type BoardPrefs,
  type SaveViewResult,
  type SavedTaskView,
  type Swimlane,
  type TaskOrder,
  type TaskViewSnapshot,
} from "../ui/taskBoardViews";
import { defaultHandoff, reconcileHandoff, sanitizeHandoff, type HandoffSettings } from "../workbench/taskHandoff";
import type { CodePercentageMode } from "../language/barStats";

export type GlobalSurface = "repository" | "fleet" | "tasks" | "agents";

/**
 * How much of a worktree the macOS FSEvents stream covers. "narrow" leaves out
 * the build and dependency trees git ignores (cargo `target/`, `node_modules/`,
 * nested worktree folders); "whole" covers everything. Other platforms ignore it.
 */
export type WatchScope = "narrow" | "whole";

export function isWatchScope(value: unknown): value is WatchScope {
  return value === "narrow" || value === "whole";
}

/**
 * The editor "Open in editor" hands a file to. These four names are the whole
 * vocabulary: the backend maps one to a fixed launcher, so a stored value that
 * is not one of them reads as "auto" instead of reaching the backend.
 */
export type EditorChoice = "auto" | "code" | "cursor" | "zed";
export const EXTERNAL_EDITOR_LABELS: Readonly<Record<EditorChoice, string>> = {
  auto: "Auto-detect",
  code: "VS Code",
  cursor: "Cursor",
  zed: "Zed",
};

export function isExternalEditor(value: unknown): value is EditorChoice {
  return typeof value === "string" && Object.hasOwn(EXTERNAL_EDITOR_LABELS, value);
}

export interface InterfacePrefs extends AppearancePrefs {
  /**
   * Which accent tint the window wears. Applied as an inline `--c-accent`
   * override on `<html>`; see ui/accents.ts for why it is a per-theme pair
   * rather than one colour.
   */
  accent: AccentId;
  /**
   * The in-app half of "reduce motion". One-way on purpose: `false` means
   * "follow the system", never "animate anyway" — a reader whose OS asked for
   * less motion cannot be overridden from in here.
   */
  reduceMotion: boolean;
  /**
   * Ask macOS 26+ for its Liquid Glass window material in place of the standard
   * one. Off by default. `true` is a request, not a fact: the backend answers
   * with what is in effect and why not (`desktop/nativeRendering.ts`).
   */
  nativeGlass: boolean;
  /**
   * Ask WebKit to render above 60 Hz on the main webview. Off by default; a
   * request like `nativeGlass`, and a no-op on a 60 Hz display.
   */
  highRefresh: boolean;
  /** Whether commit times read as "3d ago" or as a `YYYY-MM-DD` date. */
  timestampStyle: TimestampStyle;
  /**
   * Whether a sidebar branch gets one, two or three lines. Three is the
   * default: on a default-width sidebar the single line handed the name
   * whatever the chips and buttons left over, which was often three
   * characters, and two lines still packed the numbers and the attribution
   * together.
   */
  branchRowLayout: BranchRowLayout;
  /**
   * Which default `branchRowLayout` was last reconciled against. Stored
   * "two-line" was the old default far more often than a choice, so a read
   * that finds it under an older revision moves it to three lines once;
   * `BRANCH_ROW_LAYOUT_REV` is written back with every save, which is what
   * lets a later, deliberate "two-line" stay.
   */
  branchRowLayoutRev: number;
  /** Layout a diff opens in; the toolbar still switches the one on screen. */
  diffLayout: DiffLayout;
  /** Whether a diff opens with long lines wrapped. */
  diffWordWrap: boolean;
  /** Whether a diff opens with syntax colouring on. */
  diffSyntaxHighlight: boolean;
  /** Whether a newly opened repository ignores whitespace-only changes. */
  diffIgnoreWhitespace: boolean;
  /** Columns a literal tab advances to, everywhere code is drawn. */
  tabWidth: TabWidth;
  showLanguageBar: boolean;
  /** Whether code percentage counts only code files or includes markdowns & notes. */
  codePercentageMode: CodePercentageMode;
  showHarnessBadges: boolean;
  /** Author-avatar column in the commit graph gutter. */
  showGraphAvatars: boolean;
  /** GitHub profile pictures instead of initials, resolved through the GitHub CLI. */
  showGithubAvatars: boolean;
  /** Maximum share of the graph view used by the lane viewport. */
  graphWidthMode: GraphWidthMode;
  /**
   * Which refs the commit graph walks and labels. `named` — branches,
   * remotes, tags and HEAD — is the default because a lane the graph cannot
   * name is unreadable; `all` restores git's `--all` for repositories that
   * deliberately park history in a custom namespace.
   */
  graphRefScope: RefScope;
  /** Global UI font / zoom scale factor (e.g. 1.0 = 100%, 0.9 = 90%, 1.1 = 110%). */
  uiFontScale: number;
  /**
   * Views the header nav leaves out. Hiding is cosmetic: the pane, the
   * command palette and the native View menu are unaffected, and the active
   * view (plus Resolve while conflicts exist) is shown regardless.
   */
  hiddenViews: ViewTab[];
  /** How much of the bottom status bar is drawn. */
  statusBarMode: StatusBarMode;
  /** Drop the repository tab strip while a single repository is open. */
  autoHideRepoTabs: boolean;
  /** Draw the worktrees of one repository as one tab with a switcher. */
  stackWorktreeTabs: boolean;
  /** Collapse inactive tabs in tab groups to focus on active tabs. */
  collapseInactiveGroupTabs: boolean;
  /** When collapsing inactive tabs in groups, collapse even tabs running active shells (default: true). */
  collapseInactiveIncludeShells: boolean;
  /** Keep a status icon available; closing the window hides it while enabled. */
  showStatusIcon: boolean;
  /** Hide the Dock icon while the main window is closed and the status icon is on. */
  hideDockWhenClosed: boolean;
  /** Show changed/conflict counts beside the menu-bar glyph. Off by default. */
  statusIconCounts: boolean;
  /** When the header's diagnostics button is drawn. */
  diagnosticsButton: DiagnosticsButtonMode;
  /**
   * Whether header buttons carry words. "auto" lets the header's width decide;
   * "icons" holds the icon-only rendering at every width. Stored before the
   * header rebuild as the boolean `showHeaderActionLabels`, which is still read.
   */
  headerLabels: HeaderLabelMode;
  /**
   * One exclusive application surface, independent of repository tab lifetime.
   *
   * Persisted so reopening the app returns you where you left, and it lives
   * here rather than in the workspace blob because it is a UI preference, not
   * part of the tab arrangement — bumping the workspace schema version for a
   * boolean would make an older build fall back to its legacy recovery keys
   * and lose the user's tabs.
   */
  globalSurface: GlobalSurface;
  /**
   * Whether the Fleet Pulse panel is expanded above the Fleet grid.
   *
   * On by default, and cheap to leave on: the panel reads only what the Tier 1
   * sweep already fetched, so collapsing it saves layout, never work. Persisted
   * here for the same reason `fleetOpen` is — it is a UI preference, not part
   * of the tab arrangement.
   */
  fleetPulseOpen: boolean;
  /**
   * Fleet grid columns the reader has hidden, by sort key.
   *
   * Eleven columns is a lot, and someone auditing dependencies does not need
   * Storage. Hiding one never hides a *failure*, though: the grid reports any
   * failed cell in a hidden column above the table, because a column you
   * cannot see is still a column that could not be read.
   */
  fleetHiddenColumns: string[];
  /** Compact rows fit roughly twice as many repositories on screen. */
  fleetCompact: boolean;
  /**
   * Agents columns the reader has hidden. The session name is not one of
   * them. A hidden column does not hide a gap: a read that failed stays
   * above the table.
   */
  agentsHiddenColumns: string[];
  /** Compact rows on the Agents plane. */
  agentsCompact: boolean;
  /** Which sessions the Agents plane lists. Anything else reads as all. */
  agentsFilter: AgentFilter;
  /**
   * Layout the Tasks board opens in. The header still switches the one on
   * screen; this is what a fresh window starts with.
   */
  taskLayout: BoardLayout;
  /** Card density on the Tasks board, matching `fleetCompact`'s intent. */
  taskDensity: TaskDensity;
  /**
   * Status columns the Tasks board leaves out, by status key.
   *
   * Hiding is cosmetic and never silent: a hidden column still holds its
   * tasks, still accepts Move from the context menu, and the board reports
   * how much work is off screen. `sanitizeHiddenStatuses` refuses a stored
   * value that would hide every column.
   */
  taskHiddenColumns: TaskStatus[];
  /**
   * Repositories whose tasks the global and workspace boards leave out, by
   * repository id (`taskHiddenRepositories.ts`). A task goes by its primary
   * repository; the repository's own board still shows everything, and the
   * board says how many tasks it is keeping off screen.
   */
  taskHiddenRepositories: string[];
  /**
   * Chips a board card carries. Title and the priority pip are not optional —
   * a card without them cannot be read or triaged at a glance.
   */
  taskCardFields: TaskCardField[];
  /**
   * How the Tasks board groups cards into lanes. Saved with a view, like the
   * layout; `effectiveSwimlane` decides what the current layout can draw.
   */
  taskSwimlane: Swimlane;
  /**
   * How the Tasks board orders cards when no search ranks them: most urgent
   * first (the default) or the board's own order, the only one a card can be
   * moved within. Saved with a view, like the lanes.
   */
  taskOrder: TaskOrder;
  /**
   * Saved views and work-in-progress limits, per board (`boardKey`): the
   * global board, each workspace's and each repository's. Bounded and
   * sanitized by `sanitizeBoards`.
   */
  taskBoards: Record<string, BoardPrefs>;
  /** Whether the scope navigator lists archived workspaces. */
  taskShowArchivedWorkspaces: boolean;
  /**
   * Whether the Tasks scope navigator is tucked away. A collapsed navigator
   * is opened from the board toolbar as an overlay, so the choice gives the
   * board its full width without removing the scopes.
   */
  taskNavigatorCollapsed: boolean;
  /** How the Tasks scope navigator orders its Repositories list. */
  taskRepositorySort: TaskRepositorySort;
  /**
   * What Return does in the board's quick-add line.
   *
   * `"assist"` saves the typed line exactly as `"manual"` does and then asks
   * the configured model for a title and description to review. Off by
   * default, and stored rather than derived: it changes what a keystroke does,
   * so the reader has to choose it once, not every time.
   *
   * Note the asymmetry with the *engine* choice, which deliberately does not
   * persist (see `TaskManviAssist`). Both modes always work — the manual one
   * unconditionally, the drafting one degrading to "task saved, and here is
   * why the model could not run" — whereas a remembered engine can become a
   * dead button on a machine that later turned it off.
   */
  taskQuickAddAssist: boolean;
  /**
   * The agent handoff a task launch starts from: provider, connection and
   * permission mode.
   *
   * The checkout path is deliberately *not* here. It is derived per launch
   * from the repository's own identity and the tabs currently open, so a
   * remembered path can never point an agent at a directory that has since
   * moved. `sanitizeHandoff` also refuses to restore `bypass`: see its note.
   */
  taskHandoff: HandoffSettings;
  /*
   * Whether the terminal dock is showing is NOT here: it is per repository
   * tab, on `RepoSession.terminalOpen`. It lived here as one workspace-wide
   * boolean on the reasoning that per-tab state would need a `version` bump
   * of the session blob and so push older builds into legacy recovery — but
   * `PersistedTab.viewSections`, added in the same commit, shows an optional
   * field needs no bump at all. The workspace-wide flag meant opening a shell
   * in one repository opened the dock over every other repository visited,
   * and started a PTY in each.
   */
  /** Dock height in CSS pixels, clamped on read; the user drags to resize. */
  terminalDockHeight: number;
  /** Commit details pane height in CSS pixels, clamped on read; the user drags to resize. */
  commitPaneHeight: number;
  terminalFontSize: number;
  /**
   * Rows of history a shell terminal keeps, and the (smaller) amount an agent
   * CLI or task-run terminal keeps. xterm allocates `cols * 12` bytes per row
   * whether or not it holds text, and every visited repository tab keeps its
   * terminals mounted; see `TERMINAL_SCROLLBACK_MIN`. Read when a terminal is
   * created, and applied to live ones when changed.
   */
  terminalScrollback: number;
  terminalAgentScrollback: number;
  /**
   * Builds xterm's accessibility tree — a row list plus a live region that
   * announces new output — so VoiceOver and other assistive technology can
   * read the terminal at all. Without it the grid is a canvas of glyphs with
   * no accessible text behind it.
   *
   * Opt-in rather than always on because the tree is rebuilt as output
   * arrives, which is a real cost on a session that prints continuously, and
   * the web gives no reliable signal that a screen reader is running. It does
   * NOT make the shell's current input line an editable accessible field:
   * xterm's focused textarea stays empty by design, so tools that expand text
   * by reading and rewriting the focused field still cannot act here.
   */
  terminalScreenReader: boolean;
  /**
   * macOS only: treat Option as Meta in terminals, so Option+B / Option+F send
   * ESC b / ESC f (word motion) instead of composing a glyph. On by default.
   * Turn off to type characters your layout puts on Option, such as `@` on a
   * German keyboard. Applied to live terminals.
   */
  terminalOptionAsMeta: boolean;
  /**
   * Glide a mouse-wheel notch over about 150 ms instead of moving several rows
   * at once (`terminal/scrollFeel.ts`). On by default; a trackpad always
   * scrolls directly, and the system's reduced-motion preference turns the
   * glide off whatever this says. Applied to live terminals.
   */
  terminalSmoothWheel: boolean;
  /**
   * Draw terminals on screen through WebGL rather than the DOM (see
   * `terminal/gpuRenderer.ts`). On by default; turning it off returns every
   * live session to the DOM renderer at once, for a GPU or driver that draws
   * it wrong. Hidden sessions never hold a context either way.
   */
  terminalGpu: boolean;
  /**
   * Automatically copies non-whitespace terminal text selections to the
   * clipboard on mouse/keyboard selection completion. On by default.
   */
  terminalCopyOnSelect: boolean;
  terminalLauncher: SessionKind;
  /** Map of dismissed coach mark IDs. */
  seenCoachMarks: Record<string, boolean>;
  /**
   * Opt-in automatic release check. Off by default: GitPulse makes no network
   * request about itself until the user asks for one.
   */
  checkForUpdates: boolean;
  /** Epoch ms of the last completed automatic check; 0 means never. */
  lastUpdateCheckAt: number;
  /** Version whose update notice the user dismissed ("" means none). */
  dismissedUpdateVersion: string;
  /**
   * Whether a release check may announce a beta (`X.Y.Z-beta.N`) as well as
   * a stable release. Off by default: nobody is offered a beta unasked.
   */
  includeBetaUpdates: boolean;
  /**
   * Opt-in automatic coverage generation. Off by default: generating coverage
   * runs the repository's own test suites, which costs minutes of CPU and
   * writes artifacts (coverage.out, .coverage, lcov.info) into the working
   * tree. GitPulse does not do that to a repository until the user asks.
   */
  autoRunCoverage: boolean;
  /**
   * Automatic GitHub Dependabot and code scanning fetch when a repository
   * opens. On by default: the same `gh` CLI path the Health panel already
   * uses. Critical and high findings toast; a check that did not run does
   * not. Stored `false` stays off.
   */
  autoScanGithubAlerts: boolean;
  /**
   * Whether a terminal bell or OSC notice counts as an agent waiting on the
   * reader, on the board, the task pane, the Agents view and the menu-bar
   * count. Off by default (opt-in): turning this on counts terminal bells from
   * agents without hooks on those surfaces. While off, the Agents view also
   * stops listing a bell as a reason (the task pane and the board still show
   * the bell and only stop counting it). A request an agent reports through
   * its hooks (`needs-you`, `error`) counts either way. Owned by
   * `sessionActivity`, which cannot import this store, so every change is
   * pushed to it from here.
   */
  bellAsksForReader: boolean;
  /**
   * Which default `bellAsksForReader` was last reconciled against. Pre-revision-1
   * `true` was the old default written by every preference save rather than an
   * opt-in, so a read that finds it under an older revision moves it to `false`
   * once; `BELL_ASKS_FOR_READER_REV` is written back on every save, so turning
   * the opt-in on afterwards stays `true`.
   */
  bellAsksForReaderRev: number;
  /**
   * Watch scope sent with `cmd_watch_repo`. Narrow by default; an unknown
   * stored value reads as narrow. Changing it re-registers every live watch
   * (`repoStore` subscribes).
   */
  watchScope: WatchScope;
  /**
   * Editor for "Open in editor". "auto" takes the first installed of VS Code,
   * Cursor and Zed. An unknown stored value reads as "auto".
   */
  externalEditor: EditorChoice;
}

const STORAGE_KEY = "gitpulse_interface_prefs";

/**
 * Revision of the branch row layout default. Bumped to 1 when the default
 * moved from two lines to three; see `branchRowLayoutRev`.
 */
export const BRANCH_ROW_LAYOUT_REV = 1;

/**
 * The layout to use for a stored value, reconciled once against the new
 * default.
 *
 * - Not a layout (garbage, missing): the default.
 * - "two-line" saved before revision 1: the old default rather than a
 *   choice, so it becomes the new default. Made once: every save writes the
 *   revision, so a "two-line" picked afterwards is kept.
 * - Anything else, including "one-line" at any revision: as stored.
 */
export function migrateBranchRowLayout(stored: unknown, storedRev: unknown): BranchRowLayout {
  if (!isBranchRowLayout(stored)) return DEFAULT_BRANCH_ROW_LAYOUT;
  const reconciled = typeof storedRev === "number" && storedRev >= BRANCH_ROW_LAYOUT_REV;
  if (stored === "two-line" && !reconciled) return DEFAULT_BRANCH_ROW_LAYOUT;
  return stored;
}

/**
 * Revision of the `bellAsksForReader` default. Bumped to 1 when the default
 * moved from on to opt-in (`false`); see `bellAsksForReaderRev`.
 */
export const BELL_ASKS_FOR_READER_REV = 1;

/**
 * Whether a stored `bellAsksForReader` opts in, reconciled once against the
 * opt-in default.
 *
 * - Not `true` (`false`, missing, corrupt): `false` — an unreadable value
 *   never opts a reader in.
 * - `true` saved before revision 1: the old default rather than a choice, so
 *   it becomes `false` once. Every save writes `BELL_ASKS_FOR_READER_REV`, so
 *   turning the setting on afterwards is kept.
 * - `true` at or after revision 1: `true`.
 */
export function migrateBellAsksForReader(stored: unknown, storedRev: unknown): boolean {
  if (stored !== true) return false;
  return typeof storedRev === "number" && storedRev >= BELL_ASKS_FOR_READER_REV;
}

const DEFAULTS: InterfacePrefs = {
  ...APPEARANCE_DEFAULTS,
  accent: DEFAULT_ACCENT,
  reduceMotion: false,
  nativeGlass: false,
  highRefresh: false,
  timestampStyle: "relative",
  branchRowLayout: DEFAULT_BRANCH_ROW_LAYOUT,
  branchRowLayoutRev: BRANCH_ROW_LAYOUT_REV,
  diffLayout: "unified",
  diffWordWrap: false,
  diffSyntaxHighlight: true,
  diffIgnoreWhitespace: false,
  tabWidth: DEFAULT_TAB_WIDTH,
  showLanguageBar: true,
  codePercentageMode: "code-only",
  showHarnessBadges: true,
  showGraphAvatars: true,
  showGithubAvatars: true,
  graphWidthMode: "balanced",
  graphRefScope: "named",
  uiFontScale: 1.0,
  hiddenViews: [],
  statusBarMode: "full",
  autoHideRepoTabs: false,
  stackWorktreeTabs: true,
  collapseInactiveGroupTabs: false,
  collapseInactiveIncludeShells: true,
  showStatusIcon: false,
  hideDockWhenClosed: true,
  statusIconCounts: false,
  diagnosticsButton: "always",
  headerLabels: "auto",
  globalSurface: "repository",
  fleetPulseOpen: true,
  fleetHiddenColumns: [],
  fleetCompact: false,
  agentsHiddenColumns: [],
  agentsCompact: false,
  agentsFilter: "all",
  taskLayout: "board",
  taskDensity: "comfortable",
  taskHiddenColumns: [],
  taskHiddenRepositories: [],
  taskCardFields: [...DEFAULT_TASK_CARD_FIELDS],
  taskSwimlane: "none",
  taskOrder: "priority",
  taskBoards: {},
  taskShowArchivedWorkspaces: true,
  taskNavigatorCollapsed: false,
  taskRepositorySort: DEFAULT_TASK_REPOSITORY_SORT,
  taskQuickAddAssist: false,
  taskHandoff: defaultHandoff(),
  terminalDockHeight: TERMINAL_DOCK_DEFAULT_HEIGHT,
  commitPaneHeight: COMMIT_PANE_BOUNDS.fallback,
  terminalFontSize: TERMINAL_FONT_DEFAULT,
  terminalScrollback: TERMINAL_SCROLLBACK_DEFAULT,
  terminalAgentScrollback: TERMINAL_AGENT_SCROLLBACK_DEFAULT,
  terminalScreenReader: false,
  terminalOptionAsMeta: true,
  terminalSmoothWheel: true,
  terminalGpu: true,
  terminalCopyOnSelect: true,
  terminalLauncher: "shell",
  seenCoachMarks: {},
  checkForUpdates: false,
  lastUpdateCheckAt: 0,
  dismissedUpdateVersion: "",
  includeBetaUpdates: false,
  autoRunCoverage: false,
  autoScanGithubAlerts: true,
  bellAsksForReader: false,
  bellAsksForReaderRev: BELL_ASKS_FOR_READER_REV,
  watchScope: "narrow",
  externalEditor: "auto",
};

/**
 * A copy no caller can use to mutate DEFAULTS. The nested array and record
 * survive a spread by reference, so `{ ...DEFAULTS }` alone would hand every
 * reset the same `hiddenViews` instance.
 */
function freshDefaults(): InterfacePrefs {
  return {
    ...DEFAULTS,
    hiddenViews: [...DEFAULTS.hiddenViews],
    fleetHiddenColumns: [...DEFAULTS.fleetHiddenColumns],
    agentsHiddenColumns: [...DEFAULTS.agentsHiddenColumns],
    taskHiddenColumns: [...DEFAULTS.taskHiddenColumns],
    taskHiddenRepositories: [...DEFAULTS.taskHiddenRepositories],
    taskCardFields: [...DEFAULTS.taskCardFields],
    taskBoards: {},
    taskHandoff: { ...DEFAULTS.taskHandoff },
    seenCoachMarks: { ...DEFAULTS.seenCoachMarks },
  };
}

/** Stored preferences are user data: a wrong type falls back, never throws. */
function bool(value: unknown, fallback: boolean): boolean {
  return typeof value === "boolean" ? value : fallback;
}

/**
 * Upper bound on stored column keys.
 *
 * The grid has eleven columns; anything past a generous multiple of that came
 * from something other than the toggle, and an unbounded array read out of
 * `localStorage` on every load is a slow leak nobody would notice.
 */
const MAX_STORED_COLUMNS = 32;

/**
 * Column keys from storage: strings only, deduped, bounded.
 *
 * Deliberately does not check the keys against the grid's own column list.
 * Doing so would make this store depend on the Fleet view's internals to read
 * a preference, and an unknown key is already harmless — the grid hides a
 * column by looking its key up, so a stale one from an older build simply
 * matches nothing. What must not happen is an unbounded or non-string array,
 * and that is what this rejects.
 */
function columnKeys(value: unknown, fallback: string[]): string[] {
  if (!Array.isArray(value)) return [...fallback];
  const seen = new Set<string>();
  for (const entry of value) {
    if (typeof entry !== "string" || entry === "") continue;
    seen.add(entry);
    if (seen.size >= MAX_STORED_COLUMNS) break;
  }
  return [...seen];
}

const LEGACY_DENSITY_KEY = "gitpulse_density_mode";

function readLegacyDensity(): Density {
  try {
    return densityFromLegacy(window.localStorage.getItem(LEGACY_DENSITY_KEY));
  } catch {
    return densityFromLegacy(null);
  }
}

function readPrefs(): InterfacePrefs {
  if (typeof window === "undefined" || !window.localStorage) return freshDefaults();
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    // No stored preferences yet: a reader who only ever changed the old
    // density setting still has it under its own key.
    if (!raw) return { ...freshDefaults(), density: readLegacyDensity() };
    const parsed = JSON.parse(raw) as Partial<InterfacePrefs> & { fleetOpen?: unknown };
    return {
      ...sanitizeAppearance({
        ...parsed,
        // Before the density merge the preference lived under its own key.
        density: parsed.density ?? readLegacyDensity(),
      }),
      accent: isAccentId(parsed.accent) ? parsed.accent : DEFAULTS.accent,
      reduceMotion: bool(parsed.reduceMotion, DEFAULTS.reduceMotion),
      // Strictly `true`: a stored value this build cannot read must not turn a
      // private-API switch on.
      nativeGlass: parsed.nativeGlass === true,
      highRefresh: parsed.highRefresh === true,
      timestampStyle: isTimestampStyle(parsed.timestampStyle)
        ? parsed.timestampStyle
        : DEFAULTS.timestampStyle,
      branchRowLayout: migrateBranchRowLayout(parsed.branchRowLayout, parsed.branchRowLayoutRev),
      branchRowLayoutRev: BRANCH_ROW_LAYOUT_REV,
      diffLayout: isDiffLayout(parsed.diffLayout) ? parsed.diffLayout : DEFAULTS.diffLayout,
      diffWordWrap: bool(parsed.diffWordWrap, DEFAULTS.diffWordWrap),
      diffSyntaxHighlight: bool(
        parsed.diffSyntaxHighlight,
        DEFAULTS.diffSyntaxHighlight,
      ),
      diffIgnoreWhitespace: bool(
        parsed.diffIgnoreWhitespace,
        DEFAULTS.diffIgnoreWhitespace,
      ),
      tabWidth: isTabWidth(parsed.tabWidth) ? parsed.tabWidth : DEFAULTS.tabWidth,
      showLanguageBar: bool(parsed.showLanguageBar, DEFAULTS.showLanguageBar),
      codePercentageMode:
        parsed.codePercentageMode === "all" ? "all" : "code-only",
      showHarnessBadges: bool(parsed.showHarnessBadges, DEFAULTS.showHarnessBadges),
      showGraphAvatars: bool(parsed.showGraphAvatars, DEFAULTS.showGraphAvatars),
      showGithubAvatars: bool(parsed.showGithubAvatars, DEFAULTS.showGithubAvatars),
      graphWidthMode: isGraphWidthMode(parsed.graphWidthMode)
        ? parsed.graphWidthMode
        : DEFAULTS.graphWidthMode,
      graphRefScope: isRefScope(parsed.graphRefScope)
        ? parsed.graphRefScope
        : DEFAULTS.graphRefScope,
      uiFontScale:
        typeof parsed.uiFontScale === "number" &&
        parsed.uiFontScale >= 0.75 &&
        parsed.uiFontScale <= 1.5
          ? parsed.uiFontScale
          : DEFAULTS.uiFontScale,
      hiddenViews: sanitizeHiddenViews(parsed.hiddenViews),
      statusBarMode: isStatusBarMode(parsed.statusBarMode)
        ? parsed.statusBarMode
        : DEFAULTS.statusBarMode,
      autoHideRepoTabs: bool(parsed.autoHideRepoTabs, DEFAULTS.autoHideRepoTabs),
      stackWorktreeTabs: bool(parsed.stackWorktreeTabs, DEFAULTS.stackWorktreeTabs),
      collapseInactiveGroupTabs: bool(parsed.collapseInactiveGroupTabs, DEFAULTS.collapseInactiveGroupTabs),
      collapseInactiveIncludeShells: bool(parsed.collapseInactiveIncludeShells, DEFAULTS.collapseInactiveIncludeShells),
      showStatusIcon: parsed.showStatusIcon === true,
      hideDockWhenClosed: bool(parsed.hideDockWhenClosed, DEFAULTS.hideDockWhenClosed),
      statusIconCounts: parsed.statusIconCounts === true,
      diagnosticsButton: isDiagnosticsButtonMode(parsed.diagnosticsButton)
        ? parsed.diagnosticsButton
        : DEFAULTS.diagnosticsButton,
      headerLabels: readHeaderLabelMode(parsed, DEFAULTS.headerLabels),
      globalSurface: parsed.globalSurface === "tasks" || parsed.globalSurface === "fleet" || parsed.globalSurface === "agents" || parsed.globalSurface === "repository"
        ? parsed.globalSurface
        : parsed.fleetOpen === true ? "fleet" : "repository",
      fleetPulseOpen: bool(parsed.fleetPulseOpen, DEFAULTS.fleetPulseOpen),
      fleetHiddenColumns: columnKeys(parsed.fleetHiddenColumns, DEFAULTS.fleetHiddenColumns),
      fleetCompact: bool(parsed.fleetCompact, DEFAULTS.fleetCompact),
      agentsHiddenColumns: columnKeys(parsed.agentsHiddenColumns, DEFAULTS.agentsHiddenColumns),
      agentsCompact: bool(parsed.agentsCompact, DEFAULTS.agentsCompact),
      agentsFilter: isAgentFilter(parsed.agentsFilter) ? parsed.agentsFilter : DEFAULTS.agentsFilter,
      taskLayout: isBoardLayout(parsed.taskLayout) ? parsed.taskLayout : DEFAULTS.taskLayout,
      taskDensity: isTaskDensity(parsed.taskDensity) ? parsed.taskDensity : DEFAULTS.taskDensity,
      // Both sanitizers bound and normalize the stored array; neither can
      // return a board with no columns or a card shape the board cannot draw.
      taskHiddenColumns: sanitizeHiddenStatuses(parsed.taskHiddenColumns),
      taskHiddenRepositories: sanitizeHiddenRepositories(parsed.taskHiddenRepositories),
      taskCardFields: sanitizeCardFields(parsed.taskCardFields),
      taskSwimlane: isSwimlane(parsed.taskSwimlane) ? parsed.taskSwimlane : DEFAULTS.taskSwimlane,
      taskOrder: isTaskOrder(parsed.taskOrder) ? parsed.taskOrder : DEFAULTS.taskOrder,
      taskBoards: sanitizeBoards(parsed.taskBoards),
      taskShowArchivedWorkspaces: bool(
        parsed.taskShowArchivedWorkspaces,
        DEFAULTS.taskShowArchivedWorkspaces,
      ),
      taskNavigatorCollapsed: bool(parsed.taskNavigatorCollapsed, DEFAULTS.taskNavigatorCollapsed),
      taskRepositorySort: isTaskRepositorySort(parsed.taskRepositorySort)
        ? parsed.taskRepositorySort
        : DEFAULTS.taskRepositorySort,
      // Anything unrecognizable falls back to manual. A stored value this
      // build cannot read must not be the one that spends a model call.
      taskQuickAddAssist: bool(parsed.taskQuickAddAssist, DEFAULTS.taskQuickAddAssist),
      taskHandoff: sanitizeHandoff(parsed.taskHandoff),
      terminalFontSize: typeof parsed.terminalFontSize === "number" ? clampTerminalFontSize(parsed.terminalFontSize) : TERMINAL_FONT_DEFAULT,
      terminalScrollback: clampTerminalScrollback(parsed.terminalScrollback, TERMINAL_SCROLLBACK_DEFAULT),
      terminalAgentScrollback: clampTerminalScrollback(
        parsed.terminalAgentScrollback,
        TERMINAL_AGENT_SCROLLBACK_DEFAULT,
      ),
      terminalScreenReader: bool(parsed.terminalScreenReader, DEFAULTS.terminalScreenReader),
      terminalOptionAsMeta: bool(parsed.terminalOptionAsMeta, DEFAULTS.terminalOptionAsMeta),
      terminalSmoothWheel: bool(parsed.terminalSmoothWheel, DEFAULTS.terminalSmoothWheel),
      terminalGpu: bool(parsed.terminalGpu, DEFAULTS.terminalGpu),
      terminalCopyOnSelect: bool(parsed.terminalCopyOnSelect, DEFAULTS.terminalCopyOnSelect),
      terminalLauncher: SESSION_TYPES.find((launcher) => launcher.kind === parsed.terminalLauncher)?.kind ?? "shell",
      // Clamped on read, not just on write: a height persisted by another
      // build (or hand-edited) must not be able to render a dock too small
      // to grab or tall enough to swallow the view.
      terminalDockHeight:
        typeof parsed.terminalDockHeight === "number"
          ? clampTerminalDockHeight(parsed.terminalDockHeight)
          : DEFAULTS.terminalDockHeight,
      // Same rule as the dock height: validated on read as well as on write.
      commitPaneHeight:
        typeof parsed.commitPaneHeight === "number"
          ? clampPaneHeight(parsed.commitPaneHeight, COMMIT_PANE_BOUNDS)
          : DEFAULTS.commitPaneHeight,
      seenCoachMarks:
        parsed.seenCoachMarks && typeof parsed.seenCoachMarks === "object"
          ? parsed.seenCoachMarks
          : {},
      // Anything other than an explicit `true` leaves the check off. A
      // corrupt or partially-written value must never opt a user in.
      checkForUpdates: parsed.checkForUpdates === true,
      // Same rule: only an explicit `true` puts a user on the beta channel.
      includeBetaUpdates: parsed.includeBetaUpdates === true,
      // Same rule, and it matters more here: this one runs the repository's
      // test suites and writes files into the working tree.
      autoRunCoverage: parsed.autoRunCoverage === true,
      // Missing key follows the default (on). Only an explicit false
      // disables launch scans; a corrupt value does not quietly opt out
      // of a check the Health panel still offers by hand.
      autoScanGithubAlerts: bool(
        parsed.autoScanGithubAlerts,
        DEFAULTS.autoScanGithubAlerts,
      ),
      bellAsksForReader: migrateBellAsksForReader(
        parsed.bellAsksForReader,
        parsed.bellAsksForReaderRev,
      ),
      bellAsksForReaderRev: BELL_ASKS_FOR_READER_REV,
      watchScope: isWatchScope(parsed.watchScope) ? parsed.watchScope : DEFAULTS.watchScope,
      externalEditor: isExternalEditor(parsed.externalEditor)
        ? parsed.externalEditor
        : DEFAULTS.externalEditor,
      lastUpdateCheckAt:
        typeof parsed.lastUpdateCheckAt === "number" &&
        Number.isFinite(parsed.lastUpdateCheckAt) &&
        parsed.lastUpdateCheckAt >= 0
          ? parsed.lastUpdateCheckAt
          : DEFAULTS.lastUpdateCheckAt,
      dismissedUpdateVersion:
        typeof parsed.dismissedUpdateVersion === "string"
          ? parsed.dismissedUpdateVersion
          : DEFAULTS.dismissedUpdateVersion,
    };
  } catch {
    /* corrupt or unavailable storage falls back to defaults */
    return freshDefaults();
  }
}

function persist(prefs: InterfacePrefs) {
  if (typeof window === "undefined" || !window.localStorage) return;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(prefs));
  } catch {
    /* ignore quota / private-mode failures */
  }
}

/**
 * How long a changed preference may wait before it is saved. A dock drag or a
 * settings slider changes a preference on every pointer move; each save
 * serializes every preference and writes it synchronously. Moves inside one
 * window share one save, and the page-hide flush covers the window the app
 * is closed in.
 */
export const INTERFACE_PERSIST_DELAY_MS = 250;

function createInterfaceStore() {
  let current = readPrefs();
  setSignalledAsksForReader(current.bellAsksForReader);
  const { subscribe, set } = writable<InterfacePrefs>(current);
  const pendingSave = createDeferredWrite(() => persist(current), INTERFACE_PERSIST_DELAY_MS);
  flushOnPageHide(() => pendingSave.flush());

  function commit(next: InterfacePrefs) {
    pendingSave.cancel();
    current = next;
    persist(next);
    setSignalledAsksForReader(next.bellAsksForReader);
    set(next);
  }

  /**
   * Every setter is "merge these fields, then write through", so it lives in
   * one place: a setter that forgot to persist used to be a one-word typo
   * away, and each new preference no longer copies the pattern again.
   *
   * A patch that changes nothing is dropped before it reaches the store: a
   * writable notifies on every object it is given, equal or not, so a drag
   * held still — or clamped at its limit — republished the preferences to
   * every subscriber on each pointer move.
   */
  function patch(
    changes:
      | Partial<InterfacePrefs>
      | ((prefs: InterfacePrefs) => Partial<InterfacePrefs>),
  ) {
    const partial = typeof changes === "function" ? changes(current) : changes;
    const keys = Object.keys(partial) as (keyof InterfacePrefs)[];
    if (keys.every((key) => Object.is(current[key], partial[key]))) return;
    current = { ...current, ...partial };
    setSignalledAsksForReader(current.bellAsksForReader);
    set(current);
    pendingSave.schedule();
  }

  const clampScale = (scale: number) =>
    Math.round(Math.max(0.75, Math.min(1.5, scale)) * 100) / 100;

  return {
    subscribe,
    /** Saves a pending change now; the page-hide handler does the same. */
    flush: () => pendingSave.flush(),
    setAccent: (accent: AccentId) => patch({ accent, customAccent: "" }),
    /** A custom accent (`#rrggbb`); the contrast guard decides what it becomes. `""` returns to the preset. */
    setCustomAccent: (hex: string) => patch({ customAccent: hex }),
    setFontUi: (family: string) => patch({ fontUi: family }),
    setFontMono: (family: string) => patch({ fontMono: family }),
    setDensity: (density: Density) => patch({ density }),
    setRadiusScale: (scale: number) => patch({ radiusScale: scale }),
    /** Sets one appearance's intensity; the other keeps its own. */
    setMaterialIntensity: (theme: Appearance, intensity: number) =>
      patch((prefs) => ({ materialIntensity: { ...prefs.materialIntensity, [theme]: intensity } })),
    setFloatBlur: (px: number) => patch({ floatBlur: Math.round(Math.min(FLOAT_BLUR_MAX, Math.max(0, px))) }),
    setFieldLevel: (level: FieldLevel) => patch({ fieldLevel: level }),
    setFieldHue: (degrees: number) => patch({ fieldHue: degrees }),
    setReduceTransparency: (reduce: boolean) => patch({ reduceTransparency: reduce }),
    /**
     * Takes the theme out of force, with whatever it stamped that is still as it
     * left it: a preset's wallpaper and material, any theme's corner scale and fonts.
     */
    clearTheme: () => patch((prefs) => clearThemePatch(prefs)),
    /** Applies several appearance fields at once (an imported theme), as one save. */
    patchAppearance: (changes: Partial<AppearancePrefs>) => patch(changes),
    /**
     * Puts every Appearance preference back without touching the rest. The
     * reader's saved themes are a library, not a setting, so they stay.
     */
    resetAppearance: () =>
      patch((prefs) => ({
        ...APPEARANCE_DEFAULTS,
        savedThemes: prefs.savedThemes,
        accent: DEFAULT_ACCENT,
        nativeGlass: false,
        highRefresh: false,
      })),
    setBackdrop: (backdrop: BackdropId) => patch({ backdrop }),
    /** 0.1..1; held to the range, so a stray value can only fade, never brighten. */
    setWallpaperIntensity: (intensity: number) =>
      patch({
        wallpaperIntensity: Math.min(
          WALLPAPER_INTENSITY_RANGE.max,
          Math.max(WALLPAPER_INTENSITY_RANGE.min, Number.isFinite(intensity) ? intensity : 1),
        ),
      }),
    setWallpaperMotion: (on: boolean) => patch({ wallpaperMotion: on }),
    /**
     * Applies a built-in preset: its theme, wallpaper and material, and switches
     * the window to the preset's appearance, because a theme only draws in its own.
     */
    applyPreset: (preset: ThemePreset) => {
      patch((prefs) => presetPatch(preset, prefs));
      themeStore.setTheme(preset.theme.appearance);
    },
    /** Applies a theme from the reader's shelf, in its own appearance. The wallpaper is left as it is. */
    applyTheme: (theme: GitPulseTheme) => {
      patch(themeSelectionPatch(theme));
      themeStore.setTheme(theme.appearance);
    },
    /** Puts `theme` on the shelf. A full shelf refuses rather than dropping someone's theme. */
    saveTheme: (theme: GitPulseTheme): SaveOutcome => {
      let outcome: SaveOutcome = "full";
      patch((prefs) => {
        const next = withSavedTheme(prefs.savedThemes, theme);
        outcome = next.outcome;
        return { savedThemes: next.list };
      });
      return outcome;
    },
    removeSavedTheme: (theme: Pick<GitPulseTheme, "name" | "appearance">) =>
      patch((prefs) => ({ savedThemes: withoutSavedTheme(prefs.savedThemes, theme) })),
    setReduceMotion: (reduce: boolean) => patch({ reduceMotion: reduce }),
    setNativeGlass: (on: boolean) => patch({ nativeGlass: on }),
    setHighRefresh: (on: boolean) => patch({ highRefresh: on }),
    setTimestampStyle: (style: TimestampStyle) => patch({ timestampStyle: style }),
    setBranchRowLayout: (layout: BranchRowLayout) => patch({ branchRowLayout: layout }),
    setDiffLayout: (layout: DiffLayout) => patch({ diffLayout: layout }),
    setDiffWordWrap: (wrap: boolean) => patch({ diffWordWrap: wrap }),
    setDiffSyntaxHighlight: (on: boolean) => patch({ diffSyntaxHighlight: on }),
    setDiffIgnoreWhitespace: (ignore: boolean) => patch({ diffIgnoreWhitespace: ignore }),
    setTabWidth: (width: TabWidth) => patch({ tabWidth: width }),
    setShowLanguageBar: (show: boolean) => patch({ showLanguageBar: show }),
    setCodePercentageMode: (mode: CodePercentageMode) => patch({ codePercentageMode: mode }),
    setShowHarnessBadges: (show: boolean) => patch({ showHarnessBadges: show }),
    setShowGraphAvatars: (show: boolean) => patch({ showGraphAvatars: show }),
    setShowGithubAvatars: (show: boolean) => patch({ showGithubAvatars: show }),
    setGraphWidthMode: (mode: GraphWidthMode) => patch({ graphWidthMode: mode }),
    setGraphRefScope: (scope: RefScope) => patch({ graphRefScope: scope }),
    toggleGraphAvatars: () =>
      patch((prefs) => ({ showGraphAvatars: !prefs.showGraphAvatars })),
    setStatusBarMode: (mode: StatusBarMode) => patch({ statusBarMode: mode }),
    setShowStatusIcon: (show: boolean) => patch({ showStatusIcon: show }),
    setHideDockWhenClosed: (hide: boolean) => patch({ hideDockWhenClosed: hide }),
    setStatusIconCounts: (show: boolean) => patch({ statusIconCounts: show }),
    setAutoHideRepoTabs: (hide: boolean) => patch({ autoHideRepoTabs: hide }),
    setStackWorktreeTabs: (stack: boolean) => patch({ stackWorktreeTabs: stack }),
    setCollapseInactiveGroupTabs: (collapse: boolean) => patch({ collapseInactiveGroupTabs: collapse }),
    toggleCollapseInactiveGroupTabs: () =>
      patch((prefs) => ({ collapseInactiveGroupTabs: !prefs.collapseInactiveGroupTabs })),
    setCollapseInactiveIncludeShells: (include: boolean) => patch({ collapseInactiveIncludeShells: include }),
    toggleCollapseInactiveIncludeShells: () =>
      patch((prefs) => ({ collapseInactiveIncludeShells: !prefs.collapseInactiveIncludeShells })),
    setDiagnosticsButton: (mode: DiagnosticsButtonMode) =>
      patch({ diagnosticsButton: mode }),
    setHeaderLabels: (mode: HeaderLabelMode) => patch({ headerLabels: mode }),
    /** Adds or removes one view from the header's hidden list. */
    setViewHidden: (view: ViewTab, hidden: boolean) =>
      patch((prefs) => ({
        hiddenViews: hidden
          ? prefs.hiddenViews.includes(view)
            ? prefs.hiddenViews
            : [...prefs.hiddenViews, view]
          : prefs.hiddenViews.filter((entry) => entry !== view),
      })),
    showAllViews: () => patch({ hiddenViews: [] }),
    setFontScale: (scale: number) => patch({ uiFontScale: clampScale(scale) }),
    zoomIn: () => patch((prefs) => ({ uiFontScale: clampScale(prefs.uiFontScale + 0.05) })),
    zoomOut: () => patch((prefs) => ({ uiFontScale: clampScale(prefs.uiFontScale - 0.05) })),
    resetZoom: () => patch({ uiFontScale: DEFAULTS.uiFontScale }),
    setAutoRunCoverage: (enabled: boolean) => patch({ autoRunCoverage: enabled }),
    setAutoScanGithubAlerts: (enabled: boolean) =>
      patch({ autoScanGithubAlerts: enabled }),
    setGlobalSurface: (globalSurface: GlobalSurface) => patch({ globalSurface }),
    setFleetOpen: (open: boolean) => patch({ globalSurface: open ? "fleet" : "repository" }),
    toggleFleet: () => patch((prefs) => ({ globalSurface: prefs.globalSurface === "fleet" ? "repository" : "fleet" })),
    setTasksOpen: (open: boolean) => patch({ globalSurface: open ? "tasks" : "repository" }),
    toggleFleetPulse: () => patch((prefs) => ({ fleetPulseOpen: !prefs.fleetPulseOpen })),
    toggleFleetColumn: (key: string) =>
      patch((prefs) => ({
        fleetHiddenColumns: prefs.fleetHiddenColumns.includes(key)
          ? prefs.fleetHiddenColumns.filter((column) => column !== key)
          : [...prefs.fleetHiddenColumns, key],
      })),
    showAllFleetColumns: () => patch({ fleetHiddenColumns: [] }),
    toggleFleetCompact: () => patch((prefs) => ({ fleetCompact: !prefs.fleetCompact })),
    setAgentsOpen: (open: boolean) => patch({ globalSurface: open ? "agents" : "repository" }),
    toggleAgents: () => patch((prefs) => ({ globalSurface: prefs.globalSurface === "agents" ? "repository" : "agents" })),
    toggleAgentsColumn: (key: string) =>
      patch((prefs) => ({
        agentsHiddenColumns: prefs.agentsHiddenColumns.includes(key)
          ? prefs.agentsHiddenColumns.filter((column) => column !== key)
          : [...prefs.agentsHiddenColumns, key],
      })),
    showAllAgentsColumns: () => patch({ agentsHiddenColumns: [] }),
    toggleAgentsCompact: () => patch((prefs) => ({ agentsCompact: !prefs.agentsCompact })),
    setAgentsFilter: (agentsFilter: AgentFilter) => patch({ agentsFilter }),
    setTaskLayout: (taskLayout: BoardLayout) => patch({ taskLayout }),
    setTaskDensity: (taskDensity: TaskDensity) => patch({ taskDensity }),
    /** Adds or removes one status column; never hides the last visible one. */
    toggleTaskColumn: (status: TaskStatus) =>
      patch((prefs) => ({ taskHiddenColumns: toggleHiddenStatus(prefs.taskHiddenColumns, status) })),
    showAllTaskColumns: () => patch({ taskHiddenColumns: [] }),
    /** Hides one repository's tasks from the global and workspace boards, or shows them again. */
    toggleTaskRepositoryHidden: (repositoryId: string) =>
      patch((prefs) => ({ taskHiddenRepositories: toggleHiddenRepository(prefs.taskHiddenRepositories, repositoryId) })),
    showAllTaskRepositories: () => patch({ taskHiddenRepositories: [] }),
    setTaskCardFields: (fields: readonly TaskCardField[]) =>
      patch({ taskCardFields: sanitizeCardFields([...fields]) }),
    toggleTaskCardField: (field: TaskCardField) =>
      patch((prefs) => ({
        taskCardFields: sanitizeCardFields(
          prefs.taskCardFields.includes(field)
            ? prefs.taskCardFields.filter((entry) => entry !== field)
            : [...prefs.taskCardFields, field],
        ),
      })),
    setTaskSwimlane: (lane: Swimlane) => patch({ taskSwimlane: isSwimlane(lane) ? lane : "none" }),
    setTaskOrder: (order: TaskOrder) => patch({ taskOrder: isTaskOrder(order) ? order : DEFAULTS.taskOrder }),
    /** A column's work-in-progress limit on one board; null removes it. */
    setTaskWipLimit: (board: string, status: TaskStatus, limit: number | null) =>
      patch((prefs) => ({ taskBoards: setWipLimit(prefs.taskBoards, board, status, limit) })),
    /**
     * Save the board's current view under a name, or replace the view of
     * that name. The result says what happened, including a refusal.
     */
    saveTaskView: (board: string, name: string, snapshot: TaskViewSnapshot, newId: () => string): SaveViewResult => {
      let result: SaveViewResult = { ok: false, reason: "The view was not saved." };
      patch((prefs) => {
        result = saveView(prefs.taskBoards, board, name, snapshot, newId);
        return result.ok ? { taskBoards: result.boards } : {};
      });
      return result;
    },
    deleteTaskView: (board: string, id: string) =>
      patch((prefs) => ({ taskBoards: deleteView(prefs.taskBoards, board, id) })),
    /** Put a saved view's layout preferences back; the board restores its own filters. */
    applyTaskView: (view: SavedTaskView) =>
      patch({
        taskLayout: view.layout,
        taskDensity: view.density,
        taskSwimlane: view.swimlane,
        taskOrder: view.order,
        taskHiddenColumns: [...view.hiddenColumns],
        taskCardFields: [...view.cardFields],
      }),
    setTaskShowArchivedWorkspaces: (show: boolean) => patch({ taskShowArchivedWorkspaces: show }),
    setTaskNavigatorCollapsed: (collapsed: boolean) => patch({ taskNavigatorCollapsed: collapsed }),
    setTaskRepositorySort: (sort: TaskRepositorySort) =>
      patch({ taskRepositorySort: isTaskRepositorySort(sort) ? sort : DEFAULTS.taskRepositorySort }),
    setTaskQuickAddAssist: (assist: boolean) => patch({ taskQuickAddAssist: assist }),
    /**
     * Remembers the last handoff a reader chose.
     *
     * Sanitized on the way in as well as on the way out, so a caller cannot
     * persist `bypass` or an impossible provider/connection pair even by
     * passing one directly.
     */
    setTaskHandoff: (settings: HandoffSettings) =>
      patch({ taskHandoff: sanitizeHandoff(reconcileHandoff(settings)) }),
    /** Restores every Tasks board view preference without touching the rest. */
    resetTaskView: () =>
      patch({
        taskLayout: DEFAULTS.taskLayout,
        taskDensity: DEFAULTS.taskDensity,
        taskHiddenColumns: [],
        taskCardFields: [...DEFAULT_TASK_CARD_FIELDS],
        // Saved views and limits are the reader's records, not a view
        // setting, so resetting the view keeps them. So are hidden
        // repositories: a reset about layout must not put back a list of
        // repositories the reader took off the board one by one.
        taskSwimlane: DEFAULTS.taskSwimlane,
        taskOrder: DEFAULTS.taskOrder,
        taskShowArchivedWorkspaces: DEFAULTS.taskShowArchivedWorkspaces,
        taskNavigatorCollapsed: DEFAULTS.taskNavigatorCollapsed,
        taskRepositorySort: DEFAULTS.taskRepositorySort,
        taskQuickAddAssist: DEFAULTS.taskQuickAddAssist,
        taskHandoff: defaultHandoff(),
      }),
    setTerminalFontSize: (size: number) => patch({ terminalFontSize: clampTerminalFontSize(size) }),
    setTerminalScrollback: (rows: number) =>
      patch({ terminalScrollback: clampTerminalScrollback(rows, TERMINAL_SCROLLBACK_DEFAULT) }),
    setTerminalAgentScrollback: (rows: number) =>
      patch({ terminalAgentScrollback: clampTerminalScrollback(rows, TERMINAL_AGENT_SCROLLBACK_DEFAULT) }),
    setTerminalScreenReader: (on: boolean) => patch({ terminalScreenReader: on }),
    setTerminalOptionAsMeta: (on: boolean) => patch({ terminalOptionAsMeta: on }),
    setTerminalSmoothWheel: (on: boolean) => patch({ terminalSmoothWheel: on }),
    setTerminalGpu: (on: boolean) => patch({ terminalGpu: on }),
    setTerminalCopyOnSelect: (on: boolean) => patch({ terminalCopyOnSelect: on }),
    setTerminalLauncher: (launcher: SessionKind) => patch({ terminalLauncher: SESSION_TYPES.find((l) => l.kind === launcher)?.kind ?? "shell" }),
    setTerminalDockHeight: (px: number) =>
      patch({ terminalDockHeight: clampTerminalDockHeight(px) }),
    setCommitPaneHeight: (px: number) =>
      patch({ commitPaneHeight: clampPaneHeight(px, COMMIT_PANE_BOUNDS) }),
    setCheckForUpdates: (enabled: boolean) =>
      // Turning the check off also clears the dismissal, so re-enabling it
      // later reports honestly instead of staying silent about a version
      // the user dismissed under different settings.
      patch((prefs) => ({
        checkForUpdates: enabled,
        dismissedUpdateVersion: enabled ? prefs.dismissedUpdateVersion : "",
      })),
    /**
     * Switches the release check between stable releases only and stable
     * plus betas. The dismissal is cleared, since it was made about the other
     * channel's answer.
     */
    setIncludeBetaUpdates: (enabled: boolean) =>
      patch({ includeBetaUpdates: enabled, dismissedUpdateVersion: "" }),
    /** Records that an automatic check completed, restarting the throttle. */
    markUpdateChecked: (at: number) => patch({ lastUpdateCheckAt: at }),
    /** Silences the notice for one specific version only. */
    dismissUpdateVersion: (version: string) => patch({ dismissedUpdateVersion: version }),
    dismissCoachMark: (id: string) =>
      patch((prefs) => ({ seenCoachMarks: { ...prefs.seenCoachMarks, [id]: true } })),
    resetCoachMarks: () => patch({ seenCoachMarks: {} }),
    setBellAsksForReader: (on: boolean) => patch({ bellAsksForReader: on }),
    setWatchScope: (scope: WatchScope) => patch({ watchScope: isWatchScope(scope) ? scope : "narrow" }),
    setExternalEditor: (editor: EditorChoice) =>
      patch({ externalEditor: isExternalEditor(editor) ? editor : "auto" }),
    reset: () => commit(freshDefaults()),
  };
}

export const interfaceStore = createInterfaceStore();

// The document follows the preferences from here on, whichever window or test
// harness imported the store first; there is one subscription and it lives as
// long as the page. It is a no-op without a document.
installAppearance({
  prefs: interfaceStore,
  theme: themeStore,
  platform: hostPlatform,
  customBackdrop: customWallpaper,
});
// The moving wallpaper, beside the still: it decides on its own inputs (the
// host's power and load among them) and owns the one layer that moves.
installWallpaperMotion({
  prefs: interfaceStore,
  platform: hostPlatform,
  custom: customWallpaper,
  pulse: hostLoadPulse,
  startPulse: startHostLoad,
});
installHeaderLabels(interfaceStore);
