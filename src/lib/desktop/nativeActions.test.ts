import { describe, it, expect } from "vitest";
import { dispatchNativeMenu, type NativeMenuHandlers } from "./nativeActions";
import { repoWindowTitle } from "./windowChrome";
import { REGISTERED_VIEWS } from "../views/viewRegistry";

function handlers(): NativeMenuHandlers & { calls: string[] } {
  const calls: string[] = [];
  return {
    calls,
    open: () => calls.push("open"),
    clone: () => calls.push("clone"),
    settings: () => calls.push("settings"),
    refresh: () => calls.push("refresh"),
    toggleTheme: () => calls.push("toggleTheme"),
    themeSystem: () => calls.push("themeSystem"),
    themeLight: () => calls.push("themeLight"),
    themeDark: () => calls.push("themeDark"),
    setTab: (tab) => calls.push(`tab:${tab}`),
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
    dispatchNativeMenu({ id: "tab-diff" }, h);
    dispatchNativeMenu({ id: "tab-github" }, h);
    dispatchNativeMenu({ id: "tab-coverage" }, h);
    dispatchNativeMenu({ id: "tab-health" }, h);
    dispatchNativeMenu({ id: "theme-system" }, h);
    expect(h.calls).toEqual([
      "tab:diff",
      "tab:github",
      "tab:coverage",
      "tab:health",
      "themeSystem",
    ]);
  });

  it("routes every registered view tab, including manvi (regression)", () => {
    const h = handlers();
    for (const view of REGISTERED_VIEWS) {
      expect(dispatchNativeMenu({ id: `tab-${view.id}` }, h)).toBe(true);
      expect(h.calls.at(-1)).toBe(`tab:${view.id}`);
    }
    expect(h.calls.some((call) => call === "tab:manvi")).toBe(true);
    expect(h.calls.some((call) => call === "tab:terminal")).toBe(true);
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
    const exempt = new Set(["setTab", "openRecent", "openRepo", "openError", "setDropActive"]);
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
