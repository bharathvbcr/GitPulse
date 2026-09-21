/**
 * The rule that decides whether the user can see a terminal.
 *
 * Three things consult it — the unread dot, the notification suppressor, and
 * the pane's own `hidden` class — and the first two used to answer it
 * separately and not quite identically. Its failure mode is a session judged
 * "watched" when it is behind a collapsed dock, which is silence for the one
 * event this whole feature exists to announce.
 */
import { describe, expect, it } from "vitest";
import { paneOnScreen } from "./tabs";

const base = { visible: true, mode: "shell", activeId: "tab-1", tabId: "tab-1" } as const;

describe("paneOnScreen", () => {
  it("the selected tab of an open dock is on screen", () => {
    expect(paneOnScreen({ ...base })).toBe(true);
  });

  it("an unselected tab is not", () => {
    expect(paneOnScreen({ ...base, tabId: "tab-2" })).toBe(false);
  });

  it("a collapsed dock hides even its selected tab", () => {
    // The case that matters: selected is not seen, and treating it as seen
    // would suppress the banner for an agent nobody is watching.
    expect(paneOnScreen({ ...base, visible: false })).toBe(false);
  });

  it("the console hides every terminal", () => {
    expect(paneOnScreen({ ...base, mode: "console" })).toBe(false);
  });

  it("a split shows both of its panes, selected or not", () => {
    const split = { ...base, splitIds: ["tab-1", "tab-2"] as const };
    expect(paneOnScreen(split)).toBe(true);
    expect(paneOnScreen({ ...split, tabId: "tab-2" })).toBe(true);
    expect(paneOnScreen({ ...split, tabId: "tab-3" })).toBe(false);
  });

  it("a split still obeys the dock", () => {
    expect(
      paneOnScreen({ ...base, splitIds: ["tab-1", "tab-2"] as const, visible: false }),
    ).toBe(false);
  });

  it("treats a null split as no split rather than as an empty one", () => {
    // `splitIds?.includes(id)` on a null split is undefined, which is falsy —
    // the shape that made the old unread guard depend on its caller having
    // already checked something else.
    expect(paneOnScreen({ ...base, splitIds: null })).toBe(true);
    expect(paneOnScreen({ ...base, splitIds: undefined })).toBe(true);
  });

  it("no tab selected means nothing is on screen", () => {
    expect(paneOnScreen({ ...base, activeId: null })).toBe(false);
  });
});
