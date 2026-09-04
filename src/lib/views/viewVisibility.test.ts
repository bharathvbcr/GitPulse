import { describe, expect, it } from "vitest";
import { VIEW_TABS, type ViewTab } from "../repos/persist";
import { viewNavTabs } from "./viewNav";
import {
  isViewVisible,
  pinnedVisibleReason,
  sanitizeHiddenViews,
  visibleViewNav,
} from "./viewVisibility";

const shownTabs = (hidden: readonly ViewTab[], activeTab: ViewTab, conflictedCount = 0) =>
  viewNavTabs(visibleViewNav(hidden, { activeTab, conflictedCount }));

describe("sanitizeHiddenViews", () => {
  it("keeps only real view ids and de-duplicates them", () => {
    expect(sanitizeHiddenViews(["code", "code", "insights"])).toEqual(["code", "insights"]);
  });

  it("survives every shape a corrupt preferences blob can hold", () => {
    for (const value of [undefined, null, "code", 7, {}, { code: true }]) {
      expect(sanitizeHiddenViews(value), `value: ${JSON.stringify(value)}`).toEqual([]);
    }
    expect(sanitizeHiddenViews(["code", "not-a-view", null, 3, {}])).toEqual(["code"]);
  });

  it("drops retired ids rather than restoring a view this build has no pane for", () => {
    // Hidden lists are persisted user data and outlive the build that wrote
    // them. `files` and `blame` were views once; keeping them would leave the
    // preference naming something the header can never list.
    expect(sanitizeHiddenViews(["files", "blame", "code"])).toEqual(["code"]);
  });
});

describe("visibleViewNav", () => {
  it("lists every view when nothing is hidden", () => {
    expect(shownTabs([], "history").sort()).toEqual([...VIEW_TABS].sort());
  });

  it("drops the hidden ones, keeping registry order for the rest", () => {
    const shown = shownTabs(["code", "insights"], "history");
    expect(shown).toEqual(["work", "history"]);
  });

  it("keeps showing the active view even when it is hidden", () => {
    // Otherwise the header would say nothing about where the user actually
    // is, and the pane on screen would have no entry in the nav at all.
    expect(shownTabs(["insights"], "insights")).toContain("insights");
    expect(pinnedVisibleReason("insights", { activeTab: "insights" })).toBe("active");
  });

  it("brings Work back while conflicts are unresolved", () => {
    // A tidier header must never be the reason a parked merge goes unseen.
    // The pin followed Resolve into Work when Resolve stopped being a view:
    // hiding Work with markers outstanding would remove the door to the
    // editor that fixes them.
    expect(shownTabs(["work"], "history", 0)).not.toContain("work");
    expect(shownTabs(["work"], "history", 2)).toContain("work");
    expect(pinnedVisibleReason("work", { activeTab: "history", conflictedCount: 2 })).toBe(
      "conflicts",
    );
  });

  it("pins nothing else on a conflict count", () => {
    const shown = shownTabs(["code", "work"], "history", 2);
    expect(shown).toContain("work");
    expect(shown).not.toContain("code");
  });

  it("can empty the header down to the pinned views alone", () => {
    // Hiding everything is allowed; what must never happen is a header with
    // no entry for the pane on screen.
    const shown = shownTabs([...VIEW_TABS], "insights", 0);
    expect(shown).toEqual(["insights"]);
  });

  it("agrees with isViewVisible on every registered view", () => {
    const hidden: ViewTab[] = ["code", "work", "insights"];
    const context = { activeTab: "insights" as ViewTab, conflictedCount: 1 };
    const shown = new Set(viewNavTabs(visibleViewNav(hidden, context)));
    for (const tab of VIEW_TABS) {
      expect(isViewVisible(tab, hidden, context), `tab: ${tab}`).toBe(shown.has(tab));
    }
  });
});
