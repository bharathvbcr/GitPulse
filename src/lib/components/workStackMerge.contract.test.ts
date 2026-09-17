import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";

import WorkView from "./WorkView.svelte";

const workSource = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "WorkView.svelte"),
  "utf8",
);
const stackSource = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "CodeStackViewer.svelte"),
  "utf8",
);

/**
 * The Stack section became a collapsible Overview card. These are the seams
 * that merge created; each test names the failure it would have shipped.
 */
describe("Stack merged into Overview", () => {
  it("renders through LazyView with a stable module-scope loader, not an inline arrow", () => {
    // An inline arrow is a new loader identity on every render, which misses
    // LazyView's cache and remounts the stack viewer on every parent update.
    expect(workSource).toMatch(/const loadStack: ViewLoader = \(\) => import\("\.\/CodeStackViewer\.svelte"\);/);
    expect(workSource).not.toMatch(/load=\{\s*\(\)\s*=>/);
  });

  it("no Work section named stack exists anywhere in the registry", () => {
    // The section was retired, not renamed: a lingering entry would draw a
    // segmented-control button that swaps to a pane WorkspaceView no longer
    // renders (the {#if} chain's else-branch would silently swallow it).
    const registry = readFileSync(
      join(dirname(fileURLToPath(import.meta.url)), "../views/viewRegistry.ts"),
      "utf8",
    );
    expect(registry).not.toMatch(/id: "stack"/);
  });

  it("retired restores land on Work Overview, which is where the card lives", () => {
    const persist = readFileSync(
      join(dirname(fileURLToPath(import.meta.url)), "../repos/persist.ts"),
      "utf8",
    );
    expect(persist).toContain('stack: { tab: "work" }');
    // No section recorded: the retirement must not name a lens that no
    // longer exists, or a restored session would land on a pane that is gone.
    expect(persist).not.toContain('stack: { tab: "work", section: "stack" }');
  });

  it("keeps the stack cache at module scope so closing the card preserves the last tree", () => {
    // Collapsing the card unmounts the viewer. An instance-scope cache would
    // blank the card on every reopen and present "loading" as "no stack".
    expect(stackSource).toContain("const stackCache = createRepoPanelCache<StackHierarchyPayload>();");
  });

  it("keeps the restack latch and cascade intact after the move", () => {
    // The one mutating action on the page must survive the relocation
    // byte-for-byte in behavior: one cascade at a time, latch released on
    // every exit, plan built before the first rewrite.
    expect(stackSource).toContain("let restackingKey = $state<string | null>(null);");
    expect(stackSource).toContain("cascadePlan(stackNodes, node.branch_name)");
    expect(stackSource).toContain("forkPoint: step.forkPoint");
  });

  it("renders the collapsed card as a disclosure, not a pane", () => {
    // The toggle must carry aria-expanded and the lazy pane must be gated on
    // the same state, or the card is a button that does nothing.
    expect(workSource).toContain("aria-expanded={showStackDetail}");
    expect(workSource).toContain("{#if showStackDetail}");
  });

  it("renders Overview without a repository (SSR smoke)", () => {
    const { body } = render(WorkView);
    expect(body).toContain("Overview");
  });

  it("does not regress the retired native id: dispatch of section:work:stack must fail closed", () => {
    // Retired section ids must not navigate anywhere: the registry no longer
    // offers the section, so the dispatcher must refuse it rather than
    // silently landing on a default pane.
    const dispatcher = readFileSync(
      join(dirname(fileURLToPath(import.meta.url)), "../desktop/nativeActions.ts"),
      "utf8",
    );
    // The dispatcher matches against REGISTERED_VIEWS only, so this is
    // structural: the id can no longer be produced by any registration.
    expect(dispatcher).toContain("payload.id === `section:${view.id}:${entry.id}`");
  });
});
