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
import { destinationGuide, tipGuideKey } from "../views/viewGuide";

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
  });

  it("dismisses its hover flyout through the shared popover owner, not by hand", () => {
    // The flyout is portaled for the reason the dropdown was: the title bar
    // clips overflow. Its four dismissal paths (outside pointer, scroll,
    // resize, Escape) are the popover owner's, which is where the dropdown's
    // four regressions were fixed once for every popover in the app.
    expect(source).toContain("use:portal");
    expect(source).toContain("use:popover={flyoutPlacement}");
    expect(source).toContain('inside: "[data-view-group], [data-view-flyout]"');
  });

  it("opens nothing on click but a view or a section", () => {
    expect(source).not.toContain('role="menu"');
    expect(source).not.toContain('role="menuitem"');
    // A view pill's click navigates; only hover opens the flyout.
    expect(source).toContain("onclick={() => selectTab(item.id)}");
    expect(source).not.toMatch(/onclick=\{\(\) => \(?flyoutView =/);
  });

  it("puts the active view's sections beside it as real tabs", () => {
    const body = header();
    expect(body).toContain('role="tablist"');
    expect(body).toContain('aria-label="Work sections"');
    expect(body.match(/aria-current="page"/g)).toHaveLength(1);
    // Only the active view expands inline; collapsed views carry no tabs.
    expect(body).not.toContain('aria-label="Code sections"');
  });
});

const header = (conflictedCount = 0) =>
  render(ViewTabBar, { props: { conflictedCount } }).body;

describe("ViewTabBar view visibility", () => {
  afterEach(() => {
    interfaceStore.showAllViews();
    repoStore.setActiveTab("work");
  });

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
    // Scoped to the LABEL. The glanceable tooltip lives in a visually-hidden
    // description and on `data-tip-guide`, not in a `title` that used to
    // repeat "Work (F10)" — a bare substring search over the markup would
    // still mix the visible suffix with that copy.
    const labels = (html: string) =>
      [...html.matchAll(/<span>([^<]*)<\/span>/g)].map(([, text]) => text);
    expect(labels(header(0))).toContain("Work");
    expect(labels(header(0)).some((l) => l.startsWith("Work ("))).toBe(false);
    expect(labels(header(2))).toContain("Work (2)");
    expect(labels(header(2)).some((l) => l.startsWith("Code ("))).toBe(false);
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

  it("ships a destination guide instead of a label-only title", () => {
    const body = header();
    for (const item of VIEW_NAV) {
      const key = tipGuideKey(item.id);
      const guide = destinationGuide(key);
      expect(guide, item.id).not.toBeNull();
      // Every view's summary is in the markup for assistive tech
      // (aria-describedby), whatever the hover does.
      expect(body, `summary: ${item.id}`).toContain(guide!.summary);
    }
    // Hover has one meaning per pill: the active view (which opens no flyout)
    // shows its guide card; a collapsed view's flyout already says what the
    // view is, so it carries no card to open over its own flyout.
    expect(body).toContain(`data-tip-guide="${tipGuideKey("work")}"`);
    expect(body).not.toContain(`data-tip-guide="${tipGuideKey("code")}"`);
    expect(source).toContain("data-tip-guide={active ? guideKey : undefined}");
    expect(source).not.toContain("title={accelerator");
    expect(source).not.toContain("title={item.label");
  });

  it("reveals the repository surface when selecting a tab so Fleet or Tasks are not stranded", () => {
    expect(source).toContain('interfaceStore.setGlobalSurface("repository")');
  });
});

