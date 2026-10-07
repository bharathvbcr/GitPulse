import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { AGENTS_ACTION_ID } from "../src/lib/desktop/nativeActions";

/**
 * Agents is a workspace surface, like Fleet and Tasks, and it is not a
 * repository view. The view registry therefore does not force a menu id,
 * a parse arm and a handler into existence. This is that check.
 */
const repo = (relative: string): string =>
  readFileSync(new URL(`../${relative}`, import.meta.url), "utf8");

const actions = repo("src-tauri/src/desktop/actions.rs");
const menu = repo("src-tauri/src/desktop/menu.rs");
const nativeActions = repo("src/lib/desktop/nativeActions.ts");
const tabBar = repo("src/lib/components/RepoTabBar.svelte");
const palette = repo("src/lib/palette/catalog.ts");
const app = repo("src/App.svelte");
const persist = repo("src/lib/repos/persist.ts");

describe("the Agents surface stays reachable", () => {
  it("has a native action id, a parse arm and an event id", () => {
    expect(actions).toContain(`pub const AGENTS: &str = "${AGENTS_ACTION_ID}"`);
    expect(actions).toContain("AGENTS => Self::Agents");
    expect(actions).toContain("Self::Agents => AGENTS");
  });

  it("has a native menu item and an accelerator that is not the context-menu key", () => {
    expect(menu).toContain("actions::AGENTS");
    expect(menu).toContain('const AGENTS_ACCEL: &str = "CmdOrCtrl+Shift+A"');
    expect(menu).not.toContain('Some("Shift+F10")');
  });

  it("is dispatched by the frontend to its own handler", () => {
    expect(nativeActions).toContain(`case "${AGENTS_ACTION_ID}":`);
    expect(nativeActions).toContain("handlers.agents()");
  });

  it("is reachable from the repository tab strip and the command palette", () => {
    expect(tabBar).toContain("interfaceStore.setAgentsOpen(true)");
    expect(tabBar).toContain("interfaceStore.setAgentsOpen(false)");
    expect(palette).toContain("interfaceStore.setAgentsOpen(true)");
  });
});

describe("Agents is not a repository view", () => {
  it("is absent from the ViewTab union", () => {
    expect(persist).not.toMatch(/\|\s*"agents"/);
  });

  it("uses an id the tab-menu parser cannot claim", () => {
    expect(AGENTS_ACTION_ID.startsWith("tab-")).toBe(false);
  });
});

describe("Agents is swapped by hiding, never by unmounting", () => {
  it("keeps its pane mounted behind a hidden class, after Fleet", () => {
    const fleet = app.indexOf('paneCrashes.report("fleet"');
    const agents = app.indexOf('paneCrashes.report("agents"');
    expect(fleet).toBeGreaterThan(-1);
    expect(agents).toBeGreaterThan(fleet);
    const pane = app.slice(agents, agents + 500);
    expect(pane).toContain("gp-workspace");
    expect(pane).toContain("bg-background");
    expect(pane).toContain('class:hidden={$interfaceStore.globalSurface !== "agents"}');
    expect(app).not.toMatch(/\{#if\s+agentsOpen\}[\s\S]{0,400}\{:(else|else if)/);
  });
});
