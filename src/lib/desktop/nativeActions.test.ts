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
