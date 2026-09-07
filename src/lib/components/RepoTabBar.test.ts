import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import { compile } from "svelte/compiler";
import RepoTabBar from "./RepoTabBar.svelte";

import { repoStore } from "../stores/repoStore";

const source = readFileSync(new URL("./RepoTabBar.svelte", import.meta.url), "utf8");

describe("RepoTabBar", () => {
  it("renders nothing when no tabs are open", () => {
    const { body } = render(RepoTabBar);
    expect(body).not.toContain('title="Open repository"');
  });

  it("renders tab bar when tabs are open", async () => {
    await repoStore.openRepo("/repo/my-project", { allowBroken: true, activate: true });

    const { body } = render(RepoTabBar);
    expect(body).toContain("my-project");
    expect(body).toContain('title="Open repository"');
    expect(body).toContain('title="Recent repositories"');

    await repoStore.closeActiveTab();
  });

  it("reports copy-path failure through the shared clipboard seam", () => {
    expect(source).toContain('from "../desktop/clipboard"');
    expect(source).toContain("if (!(await copyText(path)))");
    expect(source).toContain('repoStore.setError("Could not copy path to clipboard")');
  });

  it("gives both popup menus complete keyboard and assistive semantics", () => {
    expect(source).toContain('role="menu"');
    expect((source.match(/role="menuitem"/g) ?? []).length).toBeGreaterThanOrEqual(12);
    expect(source).toContain('aria-haspopup="menu"');
    expect(source).toContain("aria-expanded={recentsOpen}");
    expect(source).toContain("function handlePopupKeydown");
    for (const key of ['"Escape"', '"Tab"', '"ArrowDown"', '"ArrowUp"', '"Home"', '"End"']) {
      expect(source).toContain(key);
    }
  });

  it("moves focus into an opened menu and restores its opener on Escape dismissal", () => {
    expect(source).toContain("menuOpener = e.currentTarget instanceof HTMLElement");
    expect(source).toContain("recentsTriggerEl");
    expect(source).toContain("focusPopup(menuEl)");
    expect(source).toContain("focusPopup(recentsEl)");
    expect(source).toContain("closeMenu({ restoreFocus: true })");
  });

  it("closes on Tab and continues from the opener in document tab order", () => {
    const handler = source.slice(
      source.indexOf("function handlePopupKeydown"),
      source.indexOf("function isTypingTarget"),
    );
    const tabBranch = handler.slice(
      handler.indexOf('e.key === "Tab"'),
      handler.indexOf('e.key === "Escape"'),
    );

    expect(tabBranch).toContain("focusAdjacentToMenuOpener(");
    expect(tabBranch).toContain("e.shiftKey");
    expect(tabBranch).not.toContain("restoreFocus");
    expect(source).toContain("candidate.tabIndex >= 0");
  });

  it("keeps close controls outside tab elements and exposes one roving tab stop", () => {
    expect(source).toContain('<button\n            type="button"\n            role="tab"');
    expect(source).not.toContain('<div\n          role="tab"');
    const tablist = source.slice(source.indexOf('role="tablist"'), source.indexOf("{#each $repoStore.openTabs"));
    expect(tablist).not.toContain('tabindex="0"');
    expect(source).toContain("[data-tab-shell-index]");
    expect(source).toContain('aria-keyshortcuts="Enter p Delete Control+Shift+ArrowLeft Control+Shift+ArrowRight"');
    expect(source).toContain('if (e.key === "Delete")');

    const closeButton = source.slice(
      source.indexOf('title="Close"') - 100,
      source.indexOf('title="Close"') + 250,
    );
    expect(closeButton).toContain('tabindex="-1"');
    expect(closeButton).not.toContain("tab.isActive ? 0 : -1");
  });

  it("has no accessibility compiler warnings", () => {
    const { warnings } = compile(source, { generate: "client" });
    expect(warnings.filter(({ code }) => code.startsWith("a11y_"))).toEqual([]);
  });

  it("elevates the tab bar stacking context so dropdowns are not clipped beneath workspace panes", () => {
    expect(source).toMatch(/class="[^"]*gp-repo-tabs[^"]*relative[^"]*z-20/);
  });

  it("lets the repo label and current branch render in full instead of clipping both inside 14rem", () => {
    const each = source.slice(
      source.indexOf("{#each $repoStore.openTabs"),
      source.indexOf("{#if tab.pinned}"),
    );
    expect(each).not.toContain("max-w-56");
    expect(each).not.toContain("min-w-0 flex-1");

    const label = source.split("\n").find((line) => line.includes("{tab.label}") && line.includes("<span"));
    expect(label).toBeDefined();
    expect(label).not.toContain("truncate");
    expect(label).toContain("whitespace-nowrap");

    const branch = source
      .split("\n")
      .find((line) => line.includes("{tab.currentBranch}") && line.includes("<span"));
    expect(branch).toBeDefined();
    expect(branch).not.toContain("truncate");
    expect(branch).toContain("whitespace-nowrap");
  });

  it("reorders tabs by drag, keyboard, and the context menu", () => {
    expect(source).toContain("dropReorderIndex");
    expect(source).toContain('e.dataTransfer.setData("text/plain", id)');
    expect(source).toContain("application/x-gitpulse-repo-tab");
    expect(source).toContain("Control+Shift+ArrowLeft");
    expect(source).toContain("Move left");
    expect(source).toContain("Move right");
    expect(source).toContain("Move to start");
    expect(source).toContain("Move to end");
    expect(source).toContain("repoStore.moveTab");
    expect(source).toContain("repoStore.moveTabBy");
    expect(source).toContain('aria-live="polite"');
    expect(source).toContain("Drag to reorder");
  });

  it("bounds recent repositories dropdown height and enables scrolling to prevent viewport clipping", () => {
    const recentsMenu = source.slice(
      source.indexOf('id="recent-repositories-menu"'),
      source.indexOf("<!-- Workspace-wide actions"),
    );
    expect(recentsMenu).toContain("max-h-");
    expect(recentsMenu).toContain("overflow-y-auto");
    expect(recentsMenu).toContain("shrink-0");
  });
});

