import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { createRepoStore, type FileStatus, type OpenRepoTab, type RepoState } from "../stores/repoStore";
import { interfaceStore, type InterfacePrefs } from "../stores/interfaceStore";
import type { ThemePreference } from "../stores/themeStore";
import type { RepoOperation } from "../repos/operation";
import { MAX_OPEN_TABS } from "../repos/tabModel";
import { buildMenuState, canDispatchMenuEvent } from "./menuState";
import {
  MENU_LIMITS,
  byteLength,
  clampText,
  fallbackMenuState,
  menuStateProblem,
  menuStateWireProblem,
  sendableMenuState,
} from "./menuContract";
import type { MenuState } from "./menuState";

/**
 * `cmd_set_menu_state` validates before it applies, and a refusal applies
 * nothing: the menu bar, the tray and the status popover keep the last state
 * Rust accepted. So a payload this builder cannot get past `MenuState::validate`
 * does not degrade the native surfaces, it freezes them — which is how a single
 * malformed update turned into the `desktop:menu-state` error repeating while
 * the menus stopped tracking the workspace at all.
 *
 * The builder is therefore required to be total: every RepoState, however
 * degraded, must project to a payload Rust accepts. These suites hold it to
 * that against the states the store really does pass through, then against
 * every combination of the flags that feed it, then against generated hostile
 * input.
 */

const base = (): RepoState => get(createRepoStore({ storage: null }));
const prefs = (): InterfacePrefs => get(interfaceStore);

const tab = (over: Partial<OpenRepoTab> = {}): OpenRepoTab => ({
  id: "/r/a",
  path: "/r/a",
  name: "a",
  label: "a",
  pinned: false,
  isActive: false,
  isBare: false,
  isDirty: false,
  isLoading: false,
  error: null,
  currentBranch: "main",
  conflictedCount: 0,
  changedCount: 0,
  ...over,
});

const file = (over: Partial<FileStatus> = {}): FileStatus => ({
  path: "a.txt",
  status_code: " M",
  is_staged: false,
  is_conflicted: false,
  additions: 1,
  deletions: 0,
  ...over,
});

const operation = (over: Partial<RepoOperation> = {}): RepoOperation => ({
  kind: "Rebase",
  current_step: 2,
  total_steps: 5,
  head_ref: "feature",
  incoming_ref: "main",
  conflicted_paths: [],
  conflicted_total: 0,
  available: ["abort", "continue", "skip"],
  ...over,
});

/** Both halves of the wire contract: serde must accept it, then `validate` must. */
function problem(
  repo: RepoState,
  over: Partial<InterfacePrefs> = {},
  theme: ThemePreference = "system",
  activity: Record<string, string[]> = {},
  checking = false,
  gitActivity: Record<string, string[]> = activity,
): string | null {
  const state = buildMenuState(repo, { ...prefs(), ...over }, theme, activity, checking, gitActivity);
  return menuStateWireProblem(state) ?? menuStateProblem(state);
}

describe("the switcher and the active pointer never disagree", () => {
  it("accepts a tab activated before its session exists", () => {
    // `activateTab` sets the workspace pointer and publishes immediately; the
    // session — and so `currentPath` — lands only afterwards. This is the state
    // the shipped build rejected, as "Native menu active repository does not
    // match its path".
    const repo: RepoState = { ...base(), openTabs: [tab({ isActive: true })], currentPath: null };
    const state = buildMenuState(repo, prefs(), "system", {}, false);
    expect(menuStateProblem(state)).toBeNull();
    expect(state.repositories.map((entry) => [entry.path, entry.active])).toEqual([["/r/a", true]]);
    expect(state.activePath).toBe("/r/a");
  });

  it("accepts a session whose tab is gone", () => {
    const repo: RepoState = { ...base(), openTabs: [], currentPath: "/r/a" };
    const state = buildMenuState(repo, prefs(), "system", {}, false);
    expect(menuStateProblem(state)).toBeNull();
    expect(state.activePath).toBeNull();
  });

  it("marks exactly one row for a workspace with several tabs", () => {
    const repo: RepoState = {
      ...base(),
      currentPath: "/r/b",
      openTabs: [tab({ id: "/r/a", path: "/r/a" }), tab({ id: "/r/b", path: "/r/b", isActive: true })],
    };
    const state = buildMenuState(repo, prefs(), "system", {}, false);
    expect(menuStateProblem(state)).toBeNull();
    expect(state.activePath).toBe("/r/b");
    expect(state.repositories.filter((entry) => entry.active)).toHaveLength(1);
  });

  it("falls back to the path when no tab claims the flag", () => {
    const repo: RepoState = {
      ...base(),
      currentPath: "/r/b",
      openTabs: [tab({ id: "/r/a", path: "/r/a" }), tab({ id: "/r/b", path: "/r/b" })],
    };
    const state = buildMenuState(repo, prefs(), "system", {}, false);
    expect(menuStateProblem(state)).toBeNull();
    expect(state.activePath).toBe("/r/b");
  });

  it("drops the pointer rather than the menu when the active row cannot be sent", () => {
    const repo: RepoState = {
      ...base(),
      currentPath: "",
      openTabs: [tab({ id: "", path: "", isActive: true }), tab({ id: "/r/b", path: "/r/b" })],
    };
    const state = buildMenuState(repo, prefs(), "system", {}, false);
    expect(menuStateProblem(state)).toBeNull();
    expect(state.activePath).toBeNull();
    expect(state.repositories.map((entry) => entry.path)).toEqual(["/r/b"]);
  });
});

describe("degraded workspaces still project a sendable payload", () => {
  const cases: [string, RepoState][] = [
    ["an empty path", { ...base(), openTabs: [tab({ path: "" })] }],
    [
      "a path past the byte ceiling",
      { ...base(), openTabs: [tab({ path: `/r/${"p".repeat(MENU_LIMITS.path)}`, isActive: true })] },
    ],
    [
      "a multi-byte path at the ceiling",
      { ...base(), openTabs: [tab({ path: "/r/" + "\u00e9".repeat(MENU_LIMITS.path) })] },
    ],
    [
      "two tabs on one path",
      {
        ...base(),
        currentPath: "/r/a",
        openTabs: [tab({ id: "one", isActive: true }), tab({ id: "two" })],
      },
    ],
    [
      "counts past the switcher ceiling",
      {
        ...base(),
        currentPath: "/r/a",
        openTabs: [tab({ isActive: true, changedCount: 9_000_000, conflictedCount: 2_500_000 })],
      },
    ],
    [
      "counts that are not u32 at all",
      {
        ...base(),
        currentPath: "/r/a",
        openTabs: [tab({ isActive: true, changedCount: -4, conflictedCount: 1.5 })],
      },
    ],
    [
      "counts that are not numbers",
      {
        ...base(),
        currentPath: "/r/a",
        openTabs: [tab({ isActive: true, changedCount: Number.NaN, conflictedCount: Infinity })],
      },
    ],
    [
      "more tabs than the payload may carry",
      {
        ...base(),
        currentPath: "/r/0",
        openTabs: Array.from({ length: MENU_LIMITS.repositories + 40 }, (_, index) =>
          tab({ id: `/r/${index}`, path: `/r/${index}`, isActive: index === 0 }),
        ),
      },
    ],
    [
      "a label full of control characters",
      { ...base(), openTabs: [tab({ label: `a\u0000b\n${"\u007f".repeat(4000)}` })] },
    ],
    [
      "a watch status the backend invented",
      { ...base(), currentPath: "/r/a", watch: { status: "wedged", reason: null } as unknown as RepoState["watch"] },
    ],
    [
      "a branch and upstream longer than the text ceiling",
      {
        ...base(),
        currentPath: "/r/a",
        currentBranch: "\u{1f600}".repeat(4000),
        branches: [
          {
            name: "\u{1f600}".repeat(4000),
            is_current: true,
            upstream: "origin/".concat("u".repeat(4000)),
            is_gone: false,
            ahead_count: 3,
            behind_count: 4,
          } as RepoState["branches"][number],
        ],
      },
    ],
  ];

  for (const [name, repo] of cases) {
    it(`accepts ${name}`, () => {
      expect(problem(repo)).toBeNull();
    });
  }

});

/**
 * The caps applied on the way out are insurance, not corrections: no state
 * reaches them today. Insurance that silently starts firing is worse than
 * none — a seventeenth tray-detail row would be dropped rather than reported —
 * so this measures the headroom instead of the cap, and fails when a new row,
 * action or label eats it.
 */
describe("the payload budget", () => {
  /** The fullest payload the builder can produce: every list at its longest. */
  function fullest() {
    const busy = Object.fromEntries(
      Array.from({ length: 40 }, (_, index) => [`/r/${index}`, ["fetch", "push", "pull"]]),
    );
    const repo: RepoState = {
      ...base(),
      currentPath: "/r/0",
      currentBranch: "main",
      openTabs: Array.from({ length: MAX_OPEN_TABS }, (_, index) =>
        tab({ id: `/r/${index}`, path: `/r/${index}`, isActive: index === 0 }),
      ),
      recentRepos: ["/r/1"],
      lastClosed: ["/r/2"],
      statuses: [file(), file({ path: "b.txt", is_conflicted: true }), file({ path: "c.txt", is_staged: true })],
      stashEntries: [
        { index: 0, selector: "stash@{0}", oid: "abc", subject: "wip", message: "wip", branch: "main", timestamp: 0 },
      ],
      operation: { operation: operation(), probeFailed: false },
      watch: { status: "degraded", reason: "inotify limit" },
      fetchedAt: Date.now() - 60_000,
    };
    return buildMenuState(repo, { ...prefs(), terminalDockOpen: true }, "system", busy, false, busy);
  }

  it("spends every tray-detail row and no more", () => {
    const state = fullest();
    expect(menuStateProblem(state)).toBeNull();
    // Two identity rows, four status rows, the parked operation, the fleet
    // summary and its eight repository lines: exactly the sixteen state.rs
    // allows. Nothing was cut — the last row is still the eighth repository.
    expect(state.trayDetails).toHaveLength(MENU_LIMITS.trayDetails);
    expect(state.trayDetails.at(-1)).toMatch(/^7: /);
    expect(state.trayDetails.at(0)).toBe("Repository: /r/0");
  });

  it("leaves headroom on every other list", () => {
    const state = fullest();
    expect(state.enabled.length).toBeLessThan(MENU_LIMITS.enabled);
    expect(state.checked.length).toBeLessThan(MENU_LIMITS.checked);
    expect(state.labels.length).toBeLessThan(MENU_LIMITS.labels);
    expect(state.repositories).toHaveLength(MAX_OPEN_TABS);
    expect(MAX_OPEN_TABS).toBeLessThanOrEqual(MENU_LIMITS.repositories);
    // Every id is spelled once: state.rs rejects a duplicate label outright,
    // and `apply_presentation` would honour only the first of two anyway.
    expect(new Set(state.enabled).size).toBe(state.enabled.length);
    expect(new Set(state.checked).size).toBe(state.checked.length);
    expect(new Set(state.labels.map((entry) => entry.id)).size).toBe(state.labels.length);
  });
});

describe("every flag combination the builder branches on", () => {
  const themes: ThemePreference[] = ["system", "light", "dark"];
  const surfaces: InterfacePrefs["globalSurface"][] = ["repository", "fleet", "tasks"];
  const watches: RepoState["watch"][] = [
    { status: "unknown", reason: null },
    { status: "watching", reason: null },
    { status: "degraded", reason: "polling" },
  ];

  it("projects a sendable payload for all of them", () => {
    let checked = 0;
    for (const theme of themes) {
      for (const globalSurface of surfaces) {
        for (const watch of watches) {
          for (const isLoading of [false, true]) {
            for (const isBare of [false, true]) {
              for (const probeFailed of [false, true]) {
                for (const error of [null, "Status unavailable"]) {
                  for (const parked of [null, operation()]) {
                    for (const currentPath of [null, "/r/a"]) {
                      for (const statuses of [[], [file(), file({ path: "b", is_conflicted: true, is_staged: true })]]) {
                        for (const busy of [{}, { "/r/a": ["fetch"] }, { "/r/z": ["push", "unstash"] }] as Record<string, string[]>[]) {
                          const repo: RepoState = {
                            ...base(),
                            currentPath,
                            currentBranch: currentPath ? "main" : null,
                            openTabs: currentPath ? [tab({ isActive: true, isBare, isLoading, error })] : [],
                            statuses,
                            isLoading,
                            isBare,
                            error,
                            watch,
                            operation: { operation: parked, probeFailed },
                          };
                          for (const checking of [false, true]) {
                            const found = problem(repo, { globalSurface }, theme, busy, checking);
                            if (found !== null) {
                              throw new Error(
                                `${found} — theme=${theme} surface=${globalSurface} watch=${watch.status} ` +
                                  `loading=${isLoading} bare=${isBare} probeFailed=${probeFailed} ` +
                                  `error=${error} parked=${!!parked} path=${currentPath} ` +
                                  `statuses=${statuses.length} busy=${JSON.stringify(busy)} checking=${checking}`,
                              );
                            }
                            checked += 1;
                          }
                        }
                      }
                    }
                  }
                }
              }
            }
          }
        }
      }
    }
    // Guards the loop against passing because it ran over nothing.
    expect(checked).toBe(3 * 3 * 3 * 2 * 2 * 2 * 2 * 2 * 2 * 2 * 3 * 2);
  });
});

describe("generated hostile workspaces", () => {
  /** Seeded so a failure names one reproducible case rather than a mood. */
  function random(seed: number): () => number {
    let value = seed >>> 0;
    return () => {
      value ^= value << 13;
      value ^= value >>> 17;
      value ^= value << 5;
      value >>>= 0;
      return value / 0x1_0000_0000;
    };
  }

  const PATHS = [
    "",
    " ",
    "/r/a",
    "/r/a ",
    "C:\\Users\\bhara\\Downloads\\Code\\scholarlm",
    "\\\\?\\C:\\Users\\bhara\\Downloads\\Code\\scholarlm",
    "\\\\?\\c:\\users\\bhara\\downloads\\code\\scholarlm",
    "//server/share/repo",
    "/r/\u{1f600}\u{1f600}",
    "/r/\u0000null",
    `/r/${"deep/".repeat(3000)}`,
    `/r/${"\u00e9".repeat(9000)}`,
  ];
  const COUNTS = [0, 1, 7, MENU_LIMITS.count, MENU_LIMITS.count + 1, 4_294_967_296, -1, 0.5, Number.NaN, Infinity];
  const LABELS = ["a", "", "Repo & Co", "\u0000\u001f\u007f", "x".repeat(9000), "\u{1f600}".repeat(3000)];
  const BUSY = ["fetch", "pull", "push", "stash", "unstash", "stage", "unstage", "commit", ""];

  it("projects a sendable payload for 4000 of them", () => {
    for (let seed = 1; seed <= 4000; seed += 1) {
      const next = random(seed);
      const pick = <T,>(list: readonly T[]): T => list[Math.floor(next() * list.length)] as T;
      const count = Math.floor(next() * (MAX_OPEN_TABS + 4));
      const tabs = Array.from({ length: count }, (_, index) =>
        tab({
          id: `t${index}`,
          path: pick(PATHS),
          label: pick(LABELS),
          isActive: next() < 0.25,
          isBare: next() < 0.2,
          isLoading: next() < 0.2,
          error: next() < 0.2 ? "broken" : null,
          changedCount: pick(COUNTS),
          conflictedCount: pick(COUNTS),
        }),
      );
      const activity: Record<string, string[]> = {};
      for (const entry of tabs) {
        if (next() < 0.4) activity[entry.path] = [pick(BUSY), pick(BUSY)];
      }
      const statuses = Array.from({ length: Math.floor(next() * 6) }, (_, index) =>
        file({ path: `f${index}`, is_conflicted: next() < 0.3, is_staged: next() < 0.3 }),
      );
      const repo: RepoState = {
        ...base(),
        openTabs: tabs,
        currentPath: next() < 0.2 ? null : pick(PATHS),
        currentBranch: next() < 0.2 ? null : pick(LABELS),
        statuses,
        isLoading: next() < 0.2,
        isBare: next() < 0.2,
        error: next() < 0.2 ? pick(LABELS) : null,
        stashFailed: next() < 0.2,
        fetchedAt: next() < 0.3 ? null : Date.now() - Math.floor(next() * 9_000_000_000),
        watch: pick([
          { status: "unknown", reason: null },
          { status: "watching", reason: null },
          { status: "degraded", reason: pick(LABELS) },
          { status: "exploded", reason: null } as unknown as RepoState["watch"],
        ]),
        operation: {
          operation: next() < 0.5 ? null : operation({ kind: pick(["Merge", "Rebase", "CherryPick", "Bisect"]) }),
          probeFailed: next() < 0.2,
        },
      };
      const found = problem(repo, {}, pick(["system", "light", "dark"]), activity, next() < 0.5);
      expect(found, `seed ${seed}: ${found}`).toBeNull();
    }
  });
});

/**
 * Windows is the platform this broke on, and the platform no CI runner here is.
 *
 * `openTab` stores `normalizeRepoPath(path)` — backslashes folded to forward
 * slashes — while the session keeps the OS-native string, because that is what
 * every Git command is invoked with and a `\\?\` prefix stops working the
 * moment its separators change. So on Windows a tab's path and `currentPath`
 * are two spellings of one repository, and on macOS and Linux they are the same
 * string. Every comparison between them was written as `===`, which is why the
 * native menu worked on macOS and was inert on Windows.
 *
 * These cases feed the builder both spellings on purpose, with the identity
 * rules pinned rather than read off the host.
 */
describe("a Windows workspace, on whatever host runs this", () => {
  const WINDOWS = { caseInsensitive: true };
  const NATIVE = "C:\\Users\\bhara\\Downloads\\Code\\scholarlm";
  const NORMALIZED = "C:/Users/bhara/Downloads/Code/scholarlm";
  const EXTENDED = "\\\\?\\C:\\Users\\bhara\\Downloads\\Code\\scholarlm";
  const EXTENDED_NORMALIZED = "//?/C:/Users/bhara/Downloads/Code/scholarlm";

  /** The store's own projection: tab path normalised, `currentPath` native. */
  const windows = (native: string, normalized: string, over: Partial<RepoState> = {}): RepoState => ({
    ...base(),
    currentPath: native,
    currentBranch: "main",
    openTabs: [tab({ id: normalized.toLowerCase(), path: normalized, label: "scholarlm", isActive: true })],
    ...over,
  });

  for (const [name, native, normalized] of [
    ["a drive path", NATIVE, NORMALIZED],
    ["an extended-length path", EXTENDED, EXTENDED_NORMALIZED],
  ] as const) {
    it(`sends an accepted payload for ${name}`, () => {
      const repo = windows(native, normalized);
      const state = buildMenuState(repo, prefs(), "system", {}, false, {}, WINDOWS);
      expect(menuStateProblem(state)).toBeNull();
      expect(state.activePath).toBe(normalized);
      expect(state.repositories).toEqual([
        expect.objectContaining({ path: normalized, active: true, label: "scholarlm" }),
      ]);
    });

    it(`shows work running in ${name}`, () => {
      // The activity record is keyed the way `beginMutation` keys it: by the
      // session's native path, not by the tab's normalised one.
      const repo = windows(native, normalized);
      const busy = { [native]: ["fetch"] };
      const state = buildMenuState(repo, prefs(), "system", busy, false, busy, WINDOWS);
      expect(menuStateProblem(state)).toBeNull();
      expect(state.repositories[0].busy).toBe(true);
      expect(state.status.tone).toBe("busy");
      expect(state.status.activity).toBe("Fetching…");
    });

    it(`names ${name} by its tab label rather than a bare basename`, () => {
      const repo = windows(native, normalized, {
        openTabs: [tab({ id: normalized.toLowerCase(), path: normalized, label: "Code/scholarlm", isActive: true })],
      });
      const state = buildMenuState(repo, prefs(), "system", {}, false, {}, WINDOWS);
      expect(state.status.repository).toBe("Code/scholarlm");
    });

    it(`lets a repository-scoped menu action through for ${name}`, () => {
      const repo = windows(native, normalized);
      const state = buildMenuState(repo, prefs(), "system", {}, false, {}, WINDOWS);
      // `repo_path` is whatever Rust last accepted as `activePath`, which is
      // the normalised spelling; `currentPath` is the native one.
      const event = { id: "refresh", repo_path: state.activePath };
      expect(canDispatchMenuEvent(event, state, repo, WINDOWS)).toBe(true);
      expect(canDispatchMenuEvent({ id: "refresh", repo_path: "C:/elsewhere" }, state, repo, WINDOWS)).toBe(false);
      expect(canDispatchMenuEvent({ id: "activate-repo", path: native }, state, repo, WINDOWS)).toBe(true);
      expect(canDispatchMenuEvent({ id: "activate-repo", path: "C:/elsewhere" }, state, repo, WINDOWS)).toBe(false);
    });
  }

  it("treats a case-only difference as one repository where the platform does", () => {
    const repo = windows("c:\\users\\bhara\\downloads\\code\\scholarlm", NORMALIZED);
    const busy = { "C:/USERS/BHARA/DOWNLOADS/CODE/SCHOLARLM": ["push"] };
    const state = buildMenuState(repo, prefs(), "system", busy, false, busy, WINDOWS);
    expect(menuStateProblem(state)).toBeNull();
    expect(state.repositories[0].busy).toBe(true);
    // `refresh` is withheld while work is running, which is the point of the
    // lookup above; `copy-repo-path` stays available, so it is what shows the
    // dispatch guard accepting the two spellings as one repository.
    expect(canDispatchMenuEvent({ id: "copy-repo-path", repo_path: state.activePath }, state, repo, WINDOWS)).toBe(true);
  });

  it("keeps two genuinely different repositories apart on a case-sensitive host", () => {
    const LINUX = { caseInsensitive: false };
    const repo: RepoState = {
      ...base(),
      currentPath: "/r/Alpha",
      currentBranch: "main",
      openTabs: [
        tab({ id: "/r/Alpha", path: "/r/Alpha", isActive: true }),
        tab({ id: "/r/alpha", path: "/r/alpha" }),
      ],
    };
    const busy = { "/r/alpha": ["push"] };
    const state = buildMenuState(repo, prefs(), "system", busy, false, busy, LINUX);
    expect(menuStateProblem(state)).toBeNull();
    expect(state.repositories.map((entry) => entry.busy)).toEqual([false, true]);
    expect(state.activePath).toBe("/r/Alpha");
    expect(canDispatchMenuEvent({ id: "refresh", repo_path: "/r/alpha" }, state, repo, LINUX)).toBe(false);
    expect(canDispatchMenuEvent({ id: "refresh", repo_path: "/r/Alpha" }, state, repo, LINUX)).toBe(true);
  });

  it("still points at the repository when the tab flag has not landed yet", () => {
    // The mirror of the opening case, on Windows: here the session exists and
    // `currentPath` is set, but no tab carries `isActive` — so the pointer has
    // to be recovered from the path, across the two spellings.
    const repo = windows(NATIVE, NORMALIZED, {
      openTabs: [tab({ id: "one", path: NORMALIZED, label: "scholarlm", isActive: false })],
    });
    const state = buildMenuState(repo, prefs(), "system", {}, false, {}, WINDOWS);
    expect(menuStateProblem(state)).toBeNull();
    expect(state.activePath).toBe(NORMALIZED);
    expect(state.repositories[0].active).toBe(true);
  });

  it("opens a recent repository written in either spelling", () => {
    // The recents list is normalised on the way in; an id that came back in the
    // native spelling still names the same repository.
    const repo: RepoState = { ...base(), recentRepos: [NORMALIZED] };
    const state = buildMenuState(repo, prefs(), "system", {}, false, {}, WINDOWS);
    expect(canDispatchMenuEvent({ id: "open-recent", path: NATIVE }, state, repo, WINDOWS)).toBe(true);
    expect(canDispatchMenuEvent({ id: "open-recent", path: EXTENDED }, state, repo, WINDOWS)).toBe(false);
    expect(canDispatchMenuEvent({ id: "open-recent", path: "C:/other" }, state, repo, WINDOWS)).toBe(false);
    expect(canDispatchMenuEvent({ id: "open-recent" }, state, repo, WINDOWS)).toBe(false);
  });

  it("projects a sendable payload for mixed spellings under both identity rules", () => {
    const SPELLINGS = [NATIVE, NORMALIZED, EXTENDED, EXTENDED_NORMALIZED, NATIVE.toLowerCase(), "/r/a", ""];
    for (const caseInsensitive of [true, false]) {
      for (const currentPath of [...SPELLINGS, null]) {
        for (const tabPath of SPELLINGS) {
          for (const activityKey of SPELLINGS) {
            const repo: RepoState = {
              ...base(),
              currentPath,
              currentBranch: "main",
              openTabs: [tab({ id: "one", path: tabPath, isActive: true })],
            };
            const busy = { [activityKey]: ["fetch"] };
            const state = buildMenuState(repo, prefs(), "system", busy, false, busy, { caseInsensitive });
            const found = menuStateWireProblem(state) ?? menuStateProblem(state);
            expect(
              found,
              `${found} — caseInsensitive=${caseInsensitive} current=${currentPath} tab=${tabPath} busy=${activityKey}`,
            ).toBeNull();
          }
        }
      }
    }
  });
});

/**
 * The backstop, which only matters on the day the builder stops being total.
 *
 * `cmd_set_menu_state` applies nothing when it refuses, and refuses
 * deterministically, so a payload it will not take does not cost one stale
 * frame — it costs the menu bar, the tray and the popover until the workspace
 * happens to change shape. `sendableMenuState` is what turns that into a
 * missing repository list instead. It is held to repairing every refusal the
 * validator can raise, because a repair that only covers the faults we thought
 * of is the same freeze with more steps.
 */
describe("an unsendable payload is repaired rather than dropped", () => {
  const valid = (): MenuState =>
    buildMenuState(
      { ...base(), currentPath: "/r/a", currentBranch: "main", openTabs: [tab({ isActive: true })] },
      prefs(),
      "system",
      {},
      false,
    );

  it("passes a sendable payload through untouched", () => {
    const state = valid();
    const result = sendableMenuState(state);
    expect(result.problem).toBeNull();
    expect(result.state).toBe(state);
  });

  /**
   * The first switcher row, restored if an earlier breakage removed it.
   *
   * The combination case below applies these in any subset, so a breakage that
   * assumed the row it wanted was still there would fail on the test's own
   * bookkeeping rather than on the repair it exists to exercise.
   */
  const row = (draft: MenuState) => {
    draft.repositories[0] ??= { path: "/r/a", label: "a", active: false, changed: 0, conflicts: 0, busy: false };
    return draft.repositories[0];
  };

  const breakages: [string, (draft: MenuState) => void][] = [
    ["a switcher row that disagrees with the pointer", (d) => { row(d).active = false; }],
    ["a pointer at no listed repository", (d) => { d.activePath = "/r/missing"; d.repositories = []; }],
    ["a duplicate switcher row", (d) => d.repositories.push({ ...row(d), active: false })],
    ["an empty switcher path", (d) => { d.repositories = [{ ...row(d), path: "", active: false }]; d.activePath = null; }],
    ["a count past the ceiling", (d) => { row(d).changed = MENU_LIMITS.count + 1; }],
    ["a count serde would refuse", (d) => { d.status.changed = -1; }],
    ["a fractional count", (d) => { d.status.stashes = 2.5; }],
    ["an unknown enabled action", (d) => d.enabled.push("not-an-action")],
    ["an uncheckable checked action", (d) => d.checked.push("fetch")],
    ["a tray summary pointing somewhere else", (d) => { d.traySummary.id = "quick-commit"; }],
    ["a duplicate label", (d) => d.labels.push({ id: "fetch", text: "a" }, { id: "fetch", text: "b" })],
    ["an unknown label", (d) => d.labels.push({ id: "nope", text: "a" })],
    ["a tone nobody defined", (d) => { d.status.tone = "excited"; }],
    ["a watch status nobody defined", (d) => { d.status.watchStatus = "wedged"; }],
    ["an oversized headline", (d) => { d.status.headline = "\u{1f600}".repeat(MENU_LIMITS.text); }],
    ["an oversized branch", (d) => { d.status.branch = "b".repeat(MENU_LIMITS.text + 1); }],
    ["an oversized tray detail", (d) => d.trayDetails.push("x".repeat(MENU_LIMITS.text + 1))],
    ["too many tray details", (d) => { d.trayDetails = Array(MENU_LIMITS.trayDetails + 4).fill("row"); }],
    ["an oversized tray title", (d) => { d.trayTitle = "9".repeat(MENU_LIMITS.trayTitleChars + 9); }],
    ["an oversized tray summary", (d) => { d.traySummary.text = "t".repeat(MENU_LIMITS.text + 1); }],
    ["an oversized switcher label", (d) => { row(d).label = "l".repeat(MENU_LIMITS.text + 1); }],
    ["too many rows to send", (d) => {
      const seed = row(d);
      d.repositories = Array.from({ length: MENU_LIMITS.repositories + 5 }, (_, i) => ({
        ...seed, path: `/r/${i}`, active: false,
      }));
      d.activePath = null;
    }],
    ["a fetch time outside i64", (d) => { d.status.fetchedAt = Number.MAX_VALUE; }],
  ];

  for (const [name, breach] of breakages) {
    it(`repairs ${name}`, () => {
      const broken = structuredClone(valid());
      breach(broken);
      const before = menuStateWireProblem(broken) ?? menuStateProblem(broken);
      expect(before, `${name} did not actually break the payload`).not.toBeNull();

      const { state, problem } = sendableMenuState(broken);
      expect(problem).toBe(before);
      expect(menuStateWireProblem(state) ?? menuStateProblem(state)).toBeNull();
      // Repaired, not replaced. One bad field costs the switcher and whatever
      // else could not be clamped; it must not cost the whole projection, or
      // the backstop is just the freeze again with a different shape. No
      // breakage here touches the repository name, so it survives all of them
      // — and it is what the startup fallback would have overwritten.
      expect(state.status.repository).toBe(broken.status.repository);
      expect(state.status.repository).not.toBe(fallbackMenuState(broken).status.repository);
      // The preferences that decide where the app lives survive any repair: a
      // reader who asked for menu-bar-only GitPulse must not get their Dock
      // icon back because a status card was malformed.
      expect(state.showStatusIcon).toBe(broken.showStatusIcon);
      expect(state.hideDockWhenClosed).toBe(broken.hideDockWhenClosed);
    });
  }

  it("repairs anything the generator can break, and keeps the menu usable", () => {
    // Combinations, not one fault at a time: a repair that only holds for
    // single breakages is not a backstop.
    for (let seed = 1; seed <= 600; seed += 1) {
      let value = seed >>> 0;
      const next = () => {
        value ^= value << 13; value ^= value >>> 17; value ^= value << 5;
        value >>>= 0;
        return value / 0x1_0000_0000;
      };
      const broken = structuredClone(valid());
      const applied: string[] = [];
      for (const [name, breach] of breakages) {
        if (next() < 0.3) { breach(broken); applied.push(name); }
      }
      const { state } = sendableMenuState(broken);
      const found = menuStateWireProblem(state) ?? menuStateProblem(state);
      expect(found, `seed ${seed} [${applied.join(", ")}]: ${found}`).toBeNull();
      // A repaired payload is still a working menu, not an empty one.
      expect(state.enabled).toContain("open");
      expect(state.traySummary.id).toBeTruthy();
    }
  });

  it("falls back to a payload the native side accepts at startup", () => {
    const state = fallbackMenuState({ ...valid(), showStatusIcon: true, hideDockWhenClosed: false });
    expect(menuStateWireProblem(state) ?? menuStateProblem(state)).toBeNull();
    expect(state.showStatusIcon).toBe(true);
    expect(state.hideDockWhenClosed).toBe(false);
    expect(state.repositories).toEqual([]);
    expect(state.activePath).toBeNull();
  });

  it("never splits a character when it shortens text", () => {
    expect(clampText("\u{1f600}\u{1f600}", 4)).toBe("\u{1f600}");
    expect(clampText("\u{1f600}\u{1f600}", 7)).toBe("\u{1f600}");
    expect(clampText("\u{1f600}", 3)).toBe("");
    expect(clampText("abc", 10)).toBe("abc");
    expect(byteLength(clampText("é".repeat(5000), MENU_LIMITS.text))).toBeLessThanOrEqual(MENU_LIMITS.text);
  });
});
