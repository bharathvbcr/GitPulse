import { writable } from "svelte/store";
import { clampTerminalFontSize, TERMINAL_FONT_DEFAULT } from "../terminal/viewControls";
import { LAUNCHERS, type LauncherKind } from "../terminal/tabs";
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
  clampTerminalDockHeight,
  TERMINAL_DOCK_DEFAULT_HEIGHT,
} from "../terminal/dockMetrics";
import { DEFAULT_ACCENT, isAccentId, type AccentId } from "../ui/accents";
import {
  DEFAULT_TAB_WIDTH,
  isDiffLayout,
  isTabWidth,
  type DiffLayout,
  type TabWidth,
} from "../ui/codeDisplay";
import { isTimestampStyle, type TimestampStyle } from "../ui/timestampStyle";

export interface InterfacePrefs {
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
  /** Whether commit times read as "3d ago" or as a `YYYY-MM-DD` date. */
  timestampStyle: TimestampStyle;
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
  showHarnessBadges: boolean;
  /** Author-avatar column in the commit graph gutter. */
  showGraphAvatars: boolean;
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
  /** Word labels beside the header's Open/Clone icons. */
  showHeaderActionLabels: boolean;
  /** Drop the repository tab strip while a single repository is open. */
  autoHideRepoTabs: boolean;
  /** When the header's diagnostics button is drawn. */
  diagnosticsButton: DiagnosticsButtonMode;
  /**
   * Whether the workspace-wide Fleet dashboard is the surface on screen.
   *
   * Persisted so reopening the app returns you where you left, and it lives
   * here rather than in the workspace blob because it is a UI preference, not
   * part of the tab arrangement — bumping the workspace schema version for a
   * boolean would make an older build fall back to its legacy recovery keys
   * and lose the user's tabs.
   */
  fleetOpen: boolean;
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
   * Whether the terminal dock is showing beneath the active view.
   *
   * The terminal was a view until it became clear the shape was wrong: a PTY
   * has to survive a view switch, so App mounted the pane once and hid it
   * thereafter — a page that was never really a page. As a dock it is what it
   * always behaved like, and a Health scan can be read while its command runs.
   *
   * Workspace-wide rather than per-repository, like `fleetOpen`: the session
   * blob holds the tab arrangement, and bumping its schema for a boolean would
   * make an older build fall back to legacy recovery and lose the user's tabs.
   */
  terminalDockOpen: boolean;
  /** Dock height in CSS pixels, clamped on read; the user drags to resize. */
  terminalDockHeight: number;
  terminalFontSize: number;
  terminalLauncher: LauncherKind;
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
   * Opt-in automatic coverage generation. Off by default: generating coverage
   * runs the repository's own test suites, which costs minutes of CPU and
   * writes artifacts (coverage.out, .coverage, lcov.info) into the working
   * tree. GitPulse does not do that to a repository until the user asks.
   */
  autoRunCoverage: boolean;
}

const STORAGE_KEY = "gitpulse_interface_prefs";

const DEFAULTS: InterfacePrefs = {
  accent: DEFAULT_ACCENT,
  reduceMotion: false,
  timestampStyle: "relative",
  diffLayout: "unified",
  diffWordWrap: false,
  diffSyntaxHighlight: true,
  diffIgnoreWhitespace: false,
  tabWidth: DEFAULT_TAB_WIDTH,
  showLanguageBar: true,
  showHarnessBadges: true,
  showGraphAvatars: true,
  graphWidthMode: "balanced",
  graphRefScope: "named",
  uiFontScale: 1.0,
  hiddenViews: [],
  statusBarMode: "full",
  showHeaderActionLabels: true,
  autoHideRepoTabs: false,
  diagnosticsButton: "always",
  fleetOpen: false,
  fleetPulseOpen: true,
  fleetHiddenColumns: [],
  fleetCompact: false,
  terminalDockOpen: false,
  terminalDockHeight: TERMINAL_DOCK_DEFAULT_HEIGHT,
  terminalFontSize: TERMINAL_FONT_DEFAULT,
  terminalLauncher: "shell",
  seenCoachMarks: {},
  checkForUpdates: false,
  lastUpdateCheckAt: 0,
  dismissedUpdateVersion: "",
  autoRunCoverage: false,
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

function readPrefs(): InterfacePrefs {
  if (typeof window === "undefined" || !window.localStorage) return freshDefaults();
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return freshDefaults();
    const parsed = JSON.parse(raw) as Partial<InterfacePrefs>;
    return {
      accent: isAccentId(parsed.accent) ? parsed.accent : DEFAULTS.accent,
      reduceMotion: bool(parsed.reduceMotion, DEFAULTS.reduceMotion),
      timestampStyle: isTimestampStyle(parsed.timestampStyle)
        ? parsed.timestampStyle
        : DEFAULTS.timestampStyle,
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
      showHarnessBadges: bool(parsed.showHarnessBadges, DEFAULTS.showHarnessBadges),
      showGraphAvatars: bool(parsed.showGraphAvatars, DEFAULTS.showGraphAvatars),
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
      showHeaderActionLabels: bool(
        parsed.showHeaderActionLabels,
        DEFAULTS.showHeaderActionLabels,
      ),
      autoHideRepoTabs: bool(parsed.autoHideRepoTabs, DEFAULTS.autoHideRepoTabs),
      diagnosticsButton: isDiagnosticsButtonMode(parsed.diagnosticsButton)
        ? parsed.diagnosticsButton
        : DEFAULTS.diagnosticsButton,
      fleetOpen: bool(parsed.fleetOpen, DEFAULTS.fleetOpen),
      fleetPulseOpen: bool(parsed.fleetPulseOpen, DEFAULTS.fleetPulseOpen),
      fleetHiddenColumns: columnKeys(parsed.fleetHiddenColumns, DEFAULTS.fleetHiddenColumns),
      fleetCompact: bool(parsed.fleetCompact, DEFAULTS.fleetCompact),
      terminalDockOpen: bool(parsed.terminalDockOpen, DEFAULTS.terminalDockOpen),
      terminalFontSize: typeof parsed.terminalFontSize === "number" ? clampTerminalFontSize(parsed.terminalFontSize) : TERMINAL_FONT_DEFAULT,
      terminalLauncher: LAUNCHERS.find((launcher) => launcher.kind === parsed.terminalLauncher)?.kind ?? "shell",
      // Clamped on read, not just on write: a height persisted by another
      // build (or hand-edited) must not be able to render a dock too small
      // to grab or tall enough to swallow the view.
      terminalDockHeight:
        typeof parsed.terminalDockHeight === "number"
          ? clampTerminalDockHeight(parsed.terminalDockHeight)
          : DEFAULTS.terminalDockHeight,
      seenCoachMarks:
        parsed.seenCoachMarks && typeof parsed.seenCoachMarks === "object"
          ? parsed.seenCoachMarks
          : {},
      // Anything other than an explicit `true` leaves the check off. A
      // corrupt or partially-written value must never opt a user in.
      checkForUpdates: parsed.checkForUpdates === true,
      // Same rule, and it matters more here: this one runs the repository's
      // test suites and writes files into the working tree.
      autoRunCoverage: parsed.autoRunCoverage === true,
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

function createInterfaceStore() {
  const initial = readPrefs();
  const { subscribe, set, update } = writable<InterfacePrefs>(initial);

  function commit(next: InterfacePrefs) {
    persist(next);
    set(next);
  }

  /**
   * Every setter is "merge these fields, then write through", so it lives in
   * one place: a setter that forgot to persist used to be a one-word typo
   * away, and each new preference no longer copies the pattern again.
   */
  function patch(
    changes:
      | Partial<InterfacePrefs>
      | ((prefs: InterfacePrefs) => Partial<InterfacePrefs>),
  ) {
    update((prefs) => {
      const next = {
        ...prefs,
        ...(typeof changes === "function" ? changes(prefs) : changes),
      };
      persist(next);
      return next;
    });
  }

  const clampScale = (scale: number) =>
    Math.round(Math.max(0.75, Math.min(1.5, scale)) * 100) / 100;

  return {
    subscribe,
    setAccent: (accent: AccentId) => patch({ accent }),
    setReduceMotion: (reduce: boolean) => patch({ reduceMotion: reduce }),
    setTimestampStyle: (style: TimestampStyle) => patch({ timestampStyle: style }),
    setDiffLayout: (layout: DiffLayout) => patch({ diffLayout: layout }),
    setDiffWordWrap: (wrap: boolean) => patch({ diffWordWrap: wrap }),
    setDiffSyntaxHighlight: (on: boolean) => patch({ diffSyntaxHighlight: on }),
    setDiffIgnoreWhitespace: (ignore: boolean) => patch({ diffIgnoreWhitespace: ignore }),
    setTabWidth: (width: TabWidth) => patch({ tabWidth: width }),
    setShowLanguageBar: (show: boolean) => patch({ showLanguageBar: show }),
    setShowHarnessBadges: (show: boolean) => patch({ showHarnessBadges: show }),
    setShowGraphAvatars: (show: boolean) => patch({ showGraphAvatars: show }),
    setGraphWidthMode: (mode: GraphWidthMode) => patch({ graphWidthMode: mode }),
    setGraphRefScope: (scope: RefScope) => patch({ graphRefScope: scope }),
    toggleGraphAvatars: () =>
      patch((prefs) => ({ showGraphAvatars: !prefs.showGraphAvatars })),
    setStatusBarMode: (mode: StatusBarMode) => patch({ statusBarMode: mode }),
    setShowHeaderActionLabels: (show: boolean) => patch({ showHeaderActionLabels: show }),
    setAutoHideRepoTabs: (hide: boolean) => patch({ autoHideRepoTabs: hide }),
    setDiagnosticsButton: (mode: DiagnosticsButtonMode) =>
      patch({ diagnosticsButton: mode }),
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
    setFleetOpen: (open: boolean) => patch({ fleetOpen: open }),
    toggleFleet: () => patch((prefs) => ({ fleetOpen: !prefs.fleetOpen })),
    toggleFleetPulse: () => patch((prefs) => ({ fleetPulseOpen: !prefs.fleetPulseOpen })),
    toggleFleetColumn: (key: string) =>
      patch((prefs) => ({
        fleetHiddenColumns: prefs.fleetHiddenColumns.includes(key)
          ? prefs.fleetHiddenColumns.filter((column) => column !== key)
          : [...prefs.fleetHiddenColumns, key],
      })),
    showAllFleetColumns: () => patch({ fleetHiddenColumns: [] }),
    toggleFleetCompact: () => patch((prefs) => ({ fleetCompact: !prefs.fleetCompact })),
    setTerminalFontSize: (size: number) => patch({ terminalFontSize: clampTerminalFontSize(size) }),
    setTerminalLauncher: (launcher: LauncherKind) => patch({ terminalLauncher: LAUNCHERS.find((l) => l.kind === launcher)?.kind ?? "shell" }),
    setTerminalDockOpen: (open: boolean) => patch({ terminalDockOpen: open }),
    toggleTerminalDock: () =>
      patch((prefs) => ({ terminalDockOpen: !prefs.terminalDockOpen })),
    setTerminalDockHeight: (px: number) =>
      patch({ terminalDockHeight: clampTerminalDockHeight(px) }),
    setCheckForUpdates: (enabled: boolean) =>
      // Turning the check off also clears the dismissal, so re-enabling it
      // later reports honestly instead of staying silent about a version
      // the user dismissed under different settings.
      patch((prefs) => ({
        checkForUpdates: enabled,
        dismissedUpdateVersion: enabled ? prefs.dismissedUpdateVersion : "",
      })),
    /** Records that an automatic check completed, restarting the throttle. */
    markUpdateChecked: (at: number) => patch({ lastUpdateCheckAt: at }),
    /** Silences the notice for one specific version only. */
    dismissUpdateVersion: (version: string) => patch({ dismissedUpdateVersion: version }),
    dismissCoachMark: (id: string) =>
      patch((prefs) => ({ seenCoachMarks: { ...prefs.seenCoachMarks, [id]: true } })),
    resetCoachMarks: () => patch({ seenCoachMarks: {} }),
    reset: () => commit(freshDefaults()),
  };
}

export const interfaceStore = createInterfaceStore();
