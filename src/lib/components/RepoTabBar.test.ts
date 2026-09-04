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
    expect((source.match(/role="menuitem"/g) ?? []).length).toBeGreaterThanOrEqual(8);
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
    expect(source).toContain('aria-keyshortcuts="Enter p Delete"');
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
});
