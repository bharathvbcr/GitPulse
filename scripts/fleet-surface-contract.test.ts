import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { FLEET_ACTION_ID } from "../src/lib/desktop/nativeActions";

/**
 * Fleet is the app's one workspace-scoped surface, and that is exactly what
 * makes it easy to strand.
 *
 * Every repository view is reachable because `viewRegistry` forces it to be:
 * `view-menu-contract` walks `REGISTERED_VIEWS` and fails if any of them lacks
 * a native menu id, a parse arm and a menu item. Fleet is deliberately not in
 * that registry — it is not a `ViewTab`, because a ViewTab is stored on the
 * active repository's session and would be destroyed and rebuilt on every tab
 * switch — so none of that machinery covers it. This is the equivalent.
 *
 * The three entry points asserted here are the three places a user looks: the
 * repository tab strip, the command palette, and the native View menu. Losing
 * any one of them during a refactor is silent otherwise.
 */
const repo = (relative: string): string =>
  readFileSync(new URL(`../${relative}`, import.meta.url), "utf8");

const actions = repo("src-tauri/src/desktop/actions.rs");
const menu = repo("src-tauri/src/desktop/menu.rs");
const nativeActions = repo("src/lib/desktop/nativeActions.ts");
const tabBar = repo("src/lib/components/RepoTabBar.svelte");
const palette = repo("src/lib/components/CommandPalette.svelte");
const app = repo("src/App.svelte");
const persist = repo("src/lib/repos/persist.ts");

describe("the Fleet surface stays reachable", () => {
  it("has a native action id, a parse arm and an event id", () => {
    expect(actions, "FLEET id constant missing").toContain(
      `pub const FLEET: &str = "${FLEET_ACTION_ID}"`,
    );
    expect(actions, "FLEET has no parse arm").toContain("FLEET => Self::Fleet");
    // A constant with no reverse mapping is an action the frontend can never
    // be told about — the exact state Reflog was once in.
    expect(actions, "Fleet has no event id").toContain("Self::Fleet => FLEET");
  });

  it("has a native menu item, so it is not palette-only", () => {
    expect(menu, "Fleet has no menu item").toContain("actions::FLEET");
  });

  it("is dispatched by the frontend to its own handler", () => {
    // The case label is a literal on purpose: `view-menu-contract` scans this
    // file for `case "<id>":` to prove no clickable native id lacks a handler,
    // and a constant there would be invisible to it. This is the assertion
    // that keeps the literal and the exported constant from drifting.
    expect(nativeActions).toContain(`case "${FLEET_ACTION_ID}":`);
    expect(nativeActions).toContain("handlers.fleet()");
  });

  it("is reachable from the repository tab strip and the command palette", () => {
    expect(tabBar, "no Fleet control in the repo tab bar").toContain("interfaceStore.toggleFleet()");
    // The palette is the only entry point that works with nothing open, since
    // the tab strip is not rendered then.
    expect(palette, "no Fleet command in the palette").toContain("interfaceStore.setFleetOpen(true)");
  });
});

describe("Fleet is not a repository view", () => {
  it("is absent from the ViewTab union", () => {
    // A ViewTab is persisted per repository session and its pane lives inside
    // `{#key currentPath}`. Fleet answers a question about the workspace, so
    // making it one would both scope it wrongly and rebuild it on every
    // repository switch.
    expect(persist).not.toMatch(/\|\s*"fleet"/);
  });

  it("uses an id the tab-menu parser cannot claim", () => {
    // `viewTabForMenuId` resolves anything starting with `tab-`; a Fleet id in
    // that namespace would be routed into `setTab` and silently dropped.
    expect(FLEET_ACTION_ID.startsWith("tab-")).toBe(false);
  });
});

describe("Fleet is swapped by hiding, never by unmounting", () => {
  it("keeps the repository pane mounted behind a hidden class", () => {
    // The repo block is keyed on `currentPath` and holds the live terminal.
    // An `{#if fleetOpen}` / `{:else}` swap would destroy that subtree on
    // every toggle: the PTY dies with its pane, and coming back re-hydrates
    // every open tab from scratch.
    expect(app).toContain('class:hidden={fleetOpen}');
    expect(app).toContain('class:hidden={!fleetOpen}');
  });

  it("never gates the repository pane on the fleet flag with an if/else", () => {
    expect(app).not.toMatch(/\{#if\s+fleetOpen\}[\s\S]{0,400}\{:else\}/);
  });
});
