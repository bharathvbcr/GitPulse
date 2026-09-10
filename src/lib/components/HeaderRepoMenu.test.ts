import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import { compile } from "svelte/compiler";
import HeaderRepoMenu from "./HeaderRepoMenu.svelte";

const source = readFileSync(new URL("./HeaderRepoMenu.svelte", import.meta.url), "utf8");

describe("HeaderRepoMenu", () => {
  it("renders a single menu trigger instead of neighbour Open and Clone pills", () => {
    const { body } = render(HeaderRepoMenu, {
      props: { onOpen: () => {}, onClone: () => {} },
    });
    expect(body).toContain('data-testid="header-repo-menu"');
    expect(body).toContain('aria-label="Open or clone a repository"');
    expect(body).toContain('aria-haspopup="menu"');
    expect(body).toContain("Open");
    expect(body).not.toContain("Clone...");
    expect(body).not.toContain("Open...");
  });

  it("keeps Open and Clone as labelled menu items with the native-menu wording", () => {
    expect(source).toContain('role="menu"');
    expect(source).toContain('role="menuitem"');
    expect(source).toContain("Open Repository…");
    expect(source).toContain("Clone Repository…");
    expect(source).toContain("choose(onOpen)");
    expect(source).toContain("choose(onClone)");
  });

  it("drops the trigger word without dropping the accessible name", () => {
    expect(source).toContain('aria-label="Open or clone a repository"');
    expect(source).toContain(
      "{#if $interfaceStore.showHeaderActionLabels}<span>Open</span>{/if}",
    );
    expect(source).not.toContain("<span>Clone</span>");
  });

  it("portals the menu, clamps it to the viewport, and dismisses like other overlays", () => {
    expect(source).toContain("use:portal");
    expect(source).toContain("clampMenuPosition");
    expect(source).toContain("shouldDismissOverlay");
    expect(source).toContain("[data-header-repo-menu], [data-header-repo-menu-popup]");
    expect(source).toContain("LAYERS.MENU");
  });

  it("gives the menu complete keyboard and assistive semantics", () => {
    expect(source).toContain("aria-expanded={open}");
    expect(source).toContain('aria-controls="header-repo-menu"');
    expect(source).toContain("function onMenuKey");
    expect(source).toContain("cycleFocus");
    expect(source).toContain("focusAdjacentToTrigger");
    expect(source).toContain("close({ restoreFocus: true })");
    for (const key of ['"Escape"', '"Tab"', '"ArrowDown"', '"ArrowUp"', '"Home"', '"End"']) {
      expect(source).toContain(key);
    }
  });

  it("compiles without leftover warnings", () => {
    const { warnings } = compile(source, { generate: "client", filename: "HeaderRepoMenu.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });
});
