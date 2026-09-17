import { describe, it, expect } from "vitest";
import { dispatchNativeMenu, type NativeMenuHandlers } from "./nativeActions";
import { repoWindowTitle } from "./windowChrome";
import { REGISTERED_VIEWS } from "../views/viewRegistry";

function handlers(): NativeMenuHandlers & { calls: string[] } {
  const calls: string[] = [];
  return {
    calls,
    activateRepo: (path) => calls.push(`activate:${path}`),
    clearRecents: () => calls.push("clearRecents"),
    checkUpdates: () => calls.push("checkUpdates"),
    stageAll: () => calls.push("stageAll"),
    unstageAll: () => calls.push("unstageAll"),
    createBranch: () => calls.push("createBranch"),
    renameBranch: () => calls.push("renameBranch"),
    operationContinue: () => calls.push("operationContinue"),
    operationAbort: () => calls.push("operationAbort"),
    operationSkip: () => calls.push("operationSkip"),
    copyRepoPath: () => calls.push("copyRepoPath"),
    copyBranch: () => calls.push("copyBranch"),
    copyCommit: () => calls.push("copyCommit"),
    revealRepo: () => calls.push("revealRepo"),
    openRemote: () => calls.push("openRemote"),

    open: () => calls.push("open"),
    clone: () => calls.push("clone"),
    settings: () => calls.push("settings"),
    refresh: () => calls.push("refresh"),
    toggleTheme: () => calls.push("toggleTheme"),
    themeSystem: () => calls.push("themeSystem"),
    themeLight: () => calls.push("themeLight"),
    themeDark: () => calls.push("themeDark"),
    setTab: (tab, section?: string) => calls.push(section ? `tab:${tab}:${section}` : `tab:${tab}`),
    shortcuts: () => calls.push("shortcuts"),
    diagnostics: () => calls.push("diagnostics"),
    documentation: () => calls.push("documentation"),
    releaseNotes: () => calls.push("releaseNotes"),
    reportIssue: () => calls.push("reportIssue"),
    setupTools: () => calls.push("setupTools"),
    zoomIn: () => calls.push("zoomIn"),
    zoomOut: () => calls.push("zoomOut"),
    resetZoom: () => calls.push("resetZoom"),
    fleet: () => calls.push("fleet"),
    terminalDock: () => calls.push("terminalDock"),
    fetch: () => calls.push("fetch"),
    pull: () => calls.push("pull"),
    push: () => calls.push("push"),
    stash: () => calls.push("stash"),
    stashPop: () => calls.push("stashPop"),
    rebase: () => calls.push("rebase"),
    quickCommit: () => calls.push("quickCommit"),
    palette: () => calls.push("palette"),
    focusFilter: () => calls.push("focusFilter"),
    openRecent: (path) => calls.push(`recent:${path}`),
    openRepo: (path) => calls.push(`repo:${path}`),
    closeRepoTab: () => calls.push("closeTab"),
    nextRepoTab: () => calls.push("nextTab"),
    prevRepoTab: () => calls.push("prevTab"),
    reopenRepoTab: () => calls.push("reopenTab"),
    openError: (message) => calls.push(`error:${message}`),
    setDropActive: (active) => calls.push(`drop:${active}`),
  };
}

describe("dispatchNativeMenu", () => {
  it("opens every registered section without losing the parent view", () => {
    const h = handlers();
    const expected: string[] = [];
    for (const view of REGISTERED_VIEWS) {
      for (const section of view.sections ?? []) {
        expect(dispatchNativeMenu({ id: `section:${view.id}:${section.id}` }, h)).toBe(true);
        expected.push(`tab:${view.id}:${section.id}`);
      }
    }
    expect(expected).toHaveLength(15);
    expect(h.calls).toEqual(expected);
  });

  it("rejects invalid section destinations instead of opening a fallback pane", () => {
    const h = handlers();
    for (const id of ["section:", "section:work", "section:work:", "section:work:diff",
      "section:unknown:graph", "section:history:diff:extra", "section:__proto__:graph",
      "section:history:Diff", `section:history:${"x".repeat(10000)}`]) {
      expect(dispatchNativeMenu({ id }, h)).toBe(false);
    }
    expect(h.calls).toEqual([]);
  });

  it("routes Help and zoom commands exactly once", () => {
    const h = handlers();
    for (const id of ["shortcuts", "diagnostics", "documentation", "release-notes",
      "report-issue", "setup-tools", "zoom-in", "zoom-out", "reset-zoom"]) {
      expect(dispatchNativeMenu({ id }, h)).toBe(true);
    }
    expect(h.calls).toEqual(["shortcuts", "diagnostics", "documentation", "releaseNotes",
      "reportIssue", "setupTools", "zoomIn", "zoomOut", "resetZoom"]);
  });

  it("routes file and repository commands", () => {
    const h = handlers();
    expect(dispatchNativeMenu({ id: "open" }, h)).toBe(true);
    expect(dispatchNativeMenu({ id: "clone" }, h)).toBe(true);
    expect(dispatchNativeMenu({ id: "fetch" }, h)).toBe(true);
    expect(dispatchNativeMenu({ id: "quick-commit" }, h)).toBe(true);
    expect(h.calls).toEqual(["open", "clone", "fetch", "quickCommit"]);
  });

  it("opens settings from the app menu", () => {
    const h = handlers();
    expect(dispatchNativeMenu({ id: "settings" }, h)).toBe(true);
    expect(h.calls).toEqual(["settings"]);
  });

  it("maps view tabs and appearance", () => {
    const h = handlers();
    dispatchNativeMenu({ id: "tab-history" }, h);
    dispatchNativeMenu({ id: "tab-work" }, h);
    dispatchNativeMenu({ id: "tab-insights" }, h);
    dispatchNativeMenu({ id: "tab-code" }, h);
    dispatchNativeMenu({ id: "theme-system" }, h);
    expect(h.calls).toEqual([
      "tab:history",
      "tab:work",
      "tab:insights",
      "tab:code",
      "themeSystem",
    ]);
  });

  it("routes every registered view tab", () => {
    const h = handlers();
    for (const view of REGISTERED_VIEWS) {
      expect(dispatchNativeMenu({ id: `tab-${view.id}` }, h)).toBe(true);
      expect(h.calls.at(-1)).toBe(`tab:${view.id}`);
    }
    // Guarded, so an empty registry could not pass this as a clean sweep.
    expect(h.calls).toHaveLength(REGISTERED_VIEWS.length);
    expect(REGISTERED_VIEWS.length).toBeGreaterThanOrEqual(4);
  });

  it("does not route the retired terminal view id", () => {
    // The terminal became a dock. `tab-terminal` must fall through as
    // unhandled rather than resolving to a pane that no longer exists —
    // an old menu build sending it would otherwise park the session on a
    // view id nothing renders.
    const h = handlers();
    expect(dispatchNativeMenu({ id: "tab-terminal" }, h)).toBe(false);
    expect(h.calls).toEqual([]);
  });

  it("opens a recent path and ignores empty recent", () => {
    const h = handlers();
    expect(
      dispatchNativeMenu({ id: "open-recent", path: "/tmp/repo" }, h),
    ).toBe(true);
    expect(dispatchNativeMenu({ id: "open-recent" }, h)).toBe(false);
    expect(h.calls).toEqual(["recent:/tmp/repo"]);
  });

  it("routes repository tab management commands", () => {
    const h = handlers();
    expect(dispatchNativeMenu({ id: "close-tab" }, h)).toBe(true);
    expect(dispatchNativeMenu({ id: "next-repo-tab" }, h)).toBe(true);
    expect(dispatchNativeMenu({ id: "prev-repo-tab" }, h)).toBe(true);
    expect(dispatchNativeMenu({ id: "reopen-repo-tab" }, h)).toBe(true);
    expect(h.calls).toEqual(["closeTab", "nextTab", "prevTab", "reopenTab"]);
  });

  it("returns false for unknown ids", () => {
    const h = handlers();
    expect(dispatchNativeMenu({ id: "not-a-command" }, h)).toBe(false);
    expect(h.calls).toEqual([]);
  });
});

describe("repoWindowTitle", () => {
  it("formats repo and branch for Mission Control / the Window menu", () => {
    expect(repoWindowTitle(null, null)).toBe("GitPulse");
    expect(repoWindowTitle("/Users/acme/gitpulse", "main")).toBe(
      "gitpulse — main",
    );
    expect(repoWindowTitle("/Users/acme/gitpulse", null)).toBe("gitpulse");
  });
});

describe("dispatchNativeMenu — every id reaches its own handler", () => {
  // Every no-argument handler has the identical type `() => void`, so wiring
  // "push" to handlers.pull() compiles cleanly and no type check can catch it.
  // Asserting the exact call for each id is the only thing that does.
  // [menu id, handler name on the interface, label the stub records]. The
  // last two differ for the tab handlers, and conflating them made this
  // completeness check report handlers that were in fact routed.
  const ROUTES: Array<[string, string, string]> = [
    ["clear-recents", "clearRecents", "clearRecents"],
    ["check-updates", "checkUpdates", "checkUpdates"],
    ["stage-all", "stageAll", "stageAll"],
    ["unstage-all", "unstageAll", "unstageAll"],
    ["create-branch", "createBranch", "createBranch"],
    ["rename-branch", "renameBranch", "renameBranch"],
    ["operation-continue", "operationContinue", "operationContinue"],
    ["operation-abort", "operationAbort", "operationAbort"],
    ["operation-skip", "operationSkip", "operationSkip"],
    ["copy-repo-path", "copyRepoPath", "copyRepoPath"],
    ["copy-branch", "copyBranch", "copyBranch"],
    ["copy-commit", "copyCommit", "copyCommit"],
    ["reveal-repo", "revealRepo", "revealRepo"],
    ["open-remote", "openRemote", "openRemote"],

    ["shortcuts", "shortcuts", "shortcuts"],
    ["diagnostics", "diagnostics", "diagnostics"],
    ["documentation", "documentation", "documentation"],
    ["release-notes", "releaseNotes", "releaseNotes"],
    ["report-issue", "reportIssue", "reportIssue"],
    ["setup-tools", "setupTools", "setupTools"],
    ["zoom-in", "zoomIn", "zoomIn"],
    ["zoom-out", "zoomOut", "zoomOut"],
    ["reset-zoom", "resetZoom", "resetZoom"],
    ["open", "open", "open"],
    ["clone", "clone", "clone"],
    ["settings", "settings", "settings"],
    ["refresh", "refresh", "refresh"],
    ["toggle-theme", "toggleTheme", "toggleTheme"],
    ["theme-system", "themeSystem", "themeSystem"],
    ["theme-light", "themeLight", "themeLight"],
    ["theme-dark", "themeDark", "themeDark"],
    ["fetch", "fetch", "fetch"],
    ["pull", "pull", "pull"],
    ["push", "push", "push"],
    ["stash", "stash", "stash"],
    ["stash-pop", "stashPop", "stashPop"],
    ["rebase", "rebase", "rebase"],
    ["quick-commit", "quickCommit", "quickCommit"],
    ["palette", "palette", "palette"],
    ["focus-filter", "focusFilter", "focusFilter"],
    // Workspace-scoped, so it is its own id rather than a `tab-*` one; this
    // route also proves it never reaches setTab.
    ["fleet", "fleet", "fleet"],
    // A dock rather than a view, so it likewise has its own id; this route
    // also proves it never reaches setTab.
    ["terminal-dock", "terminalDock", "terminalDock"],
    ["close-tab", "closeRepoTab", "closeTab"],
    ["next-repo-tab", "nextRepoTab", "nextTab"],
    ["prev-repo-tab", "prevRepoTab", "prevTab"],
    ["reopen-repo-tab", "reopenRepoTab", "reopenTab"],
  ];

  for (const [id, , label] of ROUTES) {
    it(`"${id}" calls ${label} and nothing else`, () => {
      const h = handlers();
      expect(dispatchNativeMenu({ id }, h)).toBe(true);
      expect(h.calls).toEqual([label]);
    });
  }

  it("covers every no-argument handler in the interface", () => {
    // A handler added without a route here would otherwise go unnoticed; the
    // path-taking and error handlers are routed separately below.
    const routed = new Set(ROUTES.map(([, handler]) => handler));
    const exempt = new Set(["setTab", "activateRepo", "openRecent", "openRepo", "openError", "setDropActive"]);
    const declared = Object.keys(handlers()).filter((k) => k !== "calls");
    const unrouted = declared.filter((k) => !routed.has(k) && !exempt.has(k));
    expect(unrouted).toEqual([]);
  });
});

describe("dispatchNativeMenu — path-carrying and unknown ids", () => {
  it("refuses an open without a path rather than opening nothing", () => {
    const h = handlers();
    // Returning true would tell the caller the menu action was handled.
    expect(dispatchNativeMenu({ id: "open-recent" }, h)).toBe(false);
    expect(dispatchNativeMenu({ id: "open-repo", path: null }, h)).toBe(false);
    expect(dispatchNativeMenu({ id: "open-recent", path: "" }, h)).toBe(false);
    expect(h.calls).toEqual([]);
  });

  it("passes the path through unchanged when one is given", () => {
    const h = handlers();
    expect(dispatchNativeMenu({ id: "open-recent", path: "/a/b c/repo" }, h)).toBe(true);
    expect(dispatchNativeMenu({ id: "open-repo", path: "/x/日本語" }, h)).toBe(true);
    expect(h.calls).toEqual(["recent:/a/b c/repo", "repo:/x/日本語"]);
  });

  it("reports an unknown id as unhandled without calling anything", () => {
    const h = handlers();
    for (const id of ["", "nope", "tab-", "tab-nonexistent", "OPEN", " open", "\u0000"]) {
      expect(dispatchNativeMenu({ id }, h), id).toBe(false);
    }
    expect(h.calls).toEqual([]);
  });
});

describe("native menu live-context gate", () => {
  it("rejects a stale or disabled action before its handler runs", () => {
    const h = handlers();
    h.canDispatch = () => false;
    expect(dispatchNativeMenu({ id: "stage-all" }, h)).toBe(false);
    expect(h.calls).toEqual([]);
  });
  it("switches open repositories with an exact path, including colons", () => {
    const h = handlers();
    expect(dispatchNativeMenu({ id: "activate-repo", path: "/r/a:b" }, h)).toBe(true);
    expect(h.calls).toEqual(["activate:/r/a:b"]);
    expect(dispatchNativeMenu({ id: "activate-repo" }, h)).toBe(false);
  });
});
