import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { render } from "svelte/server";
import { get } from "svelte/store";
import ViewTabBar from "./ViewTabBar.svelte";
import { interfaceStore } from "../stores/interfaceStore";
import { repoStore } from "../stores/repoStore";
import { VIEW_NAV } from "../views/viewNav";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "ViewTabBar.svelte"),
  "utf8",
);

describe("ViewTabBar has no menu left to dismiss", () => {
  /**
   * The header carried a portalled dropdown for the views that would not fit
   * a title bar, and four regressions were fixed in its dismissal alone
   * (mousedown vs capture-phase pointerdown, the tablist counting as
   * "inside", nested scroll dismissing it on open, header overflow clipping
   * the panel). Consolidation removed the reason for it: four views all fit
   * as tabs. The code is deleted rather than left standing, so what is
   * asserted here is that it stayed deleted — a reintroduced menu would
   * bring those four regressions back with it.
   */
  it("keeps the dropdown, and every listener it needed, gone", () => {
    expect(source).not.toContain("data-view-nav-menu");
    expect(source).not.toContain("data-view-nav-trigger");
    expect(source).not.toContain("shouldDismissOverlay");
    expect(source).not.toContain("addEventListener");
    expect(source).not.toContain("use:portal");
  });

  it("renders one tablist and nothing that opens on click but a view", () => {
    expect(source).toContain('role="tablist"');
    expect(source).toContain('role="tab"');
    expect(source).not.toContain('role="menu"');
    expect(source).not.toContain('role="menuitem"');
  });
});

describe("ViewTabBar view visibility", () => {
  afterEach(() => {
    interfaceStore.showAllViews();
    repoStore.setActiveTab("work");
  });

  const header = (conflictedCount = 0) =>
    render(ViewTabBar, { props: { conflictedCount } }).body;

  it("lists every view by default", () => {
    const body = header();
    // Derived from VIEW_NAV, so a view added later cannot be quietly absent
    // from the header without failing here.
    for (const item of VIEW_NAV) {
      expect(body, `view: ${item.id}`).toContain(`>${item.label}<`);
    }
    expect(VIEW_NAV.length).toBeGreaterThanOrEqual(4);
  });

  it("drops a hidden tab from the header", () => {
    interfaceStore.setViewHidden("code", true);
    const body = header();
    expect(body).not.toContain(">Code<");
    // The control: hiding one view leaves the others alone.
    expect(body).toContain(">History<");
  });

  it("keeps the active view listed even when it is hidden", () => {
    // repoStore starts on Work; hiding it must not erase the only marker of
    // where the user currently is.
    interfaceStore.setViewHidden("work", true);
    expect(header()).toContain(">Work<");
  });

  it("carries the unresolved-conflict count into the header", () => {
    // Resolve is a section of Work now, and the count came with it: a merge
    // parked mid-conflict has to be visible without opening anything. Only
    // Work is suffixed — a count on every tab would say nothing.
    expect(header(0)).toContain(">Work<");
    expect(header(0)).not.toContain("Work (");
    expect(header(2)).toContain("Work (2)");
    expect(header(2)).not.toContain("Code (");
  });

  it("cannot show the conflict pin here, and does not pretend to", () => {
    // The other half of the guarantee — Work stays listed while conflicts
    // stand, even when hidden — is not decidable from this render: with no
    // repository open, `setActiveTab` is a no-op, so the store's active tab
    // is always Work and the *active* pin keeps it listed for a different
    // reason. A test asserting it here would pass without exercising it.
    // `pinnedVisibleReason` covers it in views/viewVisibility.test.ts.
    interfaceStore.setViewHidden("work", true);
    repoStore.setActiveTab("history");
    expect(get(repoStore).activeTab).toBe("work");
    expect(header(0)).toContain(">Work<");
  });
});
