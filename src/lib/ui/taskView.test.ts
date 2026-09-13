import { describe, expect, it } from "vitest";
import { STATUSES, type TaskStatus } from "../workbench/vocabulary";
import {
  DEFAULT_TASK_CARD_FIELDS,
  TASK_CARD_FIELDS,
  hiddenColumnReport,
  isBoardLayout,
  isTaskCardField,
  isTaskDensity,
  sanitizeCardFields,
  sanitizeHiddenStatuses,
  toggleHiddenStatus,
  visibleBoardStatuses, canHideStatus} from "./taskView";

describe("preference guards", () => {
  it("accepts only the layouts and densities the board can render", () => {
    expect(isBoardLayout("board")).toBe(true);
    expect(isBoardLayout("list")).toBe(true);
    expect(isBoardLayout("timeline")).toBe(false);
    expect(isBoardLayout(undefined)).toBe(false);
    expect(isTaskDensity("compact")).toBe(true);
    expect(isTaskDensity("comfortable")).toBe(true);
    expect(isTaskDensity("cozy")).toBe(false);
    expect(isTaskCardField("owner")).toBe(true);
    expect(isTaskCardField("title")).toBe(false);
  });
});

describe("sanitizeCardFields", () => {
  it("falls back to the default set when nothing usable was stored", () => {
    expect(sanitizeCardFields(undefined)).toEqual([...DEFAULT_TASK_CARD_FIELDS]);
    expect(sanitizeCardFields(null)).toEqual([...DEFAULT_TASK_CARD_FIELDS]);
    expect(sanitizeCardFields("repo")).toEqual([...DEFAULT_TASK_CARD_FIELDS]);
    expect(sanitizeCardFields({ repo: true })).toEqual([...DEFAULT_TASK_CARD_FIELDS]);
  });

  it("keeps an explicitly empty choice instead of restoring the defaults", () => {
    expect(sanitizeCardFields([])).toEqual([]);
  });

  it("normalizes order and duplicates so a stored list cannot reshuffle a card", () => {
    expect(sanitizeCardFields(["labels", "repo", "labels", "owner"])).toEqual(["repo", "owner", "labels"]);
    expect(sanitizeCardFields([...TASK_CARD_FIELDS].reverse())).toEqual([...TASK_CARD_FIELDS]);
  });

  it("drops unknown keys written by another build without losing the known ones", () => {
    expect(sanitizeCardFields(["repo", "estimate", 7, null, "due"])).toEqual(["repo", "due"]);
  });

  it("treats a list this build recognizes none of as drift, not as a blank card", () => {
    // An empty array is a choice; a non-empty array of names this build does
    // not have is another build's choice, and honouring it would leave every
    // card carrying nothing but a title with no way to tell why.
    expect(sanitizeCardFields(["estimate", "assignee"])).toEqual([...DEFAULT_TASK_CARD_FIELDS]);
    expect(sanitizeCardFields([7, null, {}])).toEqual([...DEFAULT_TASK_CARD_FIELDS]);
  });

  it("stays bounded on a hostile array", () => {
    const hostile = Array.from({ length: 100_000 }, (_, i) => (i % 2 ? "owner" : `junk-${i}`));
    expect(sanitizeCardFields(hostile)).toEqual(["owner"]);
  });
});

describe("sanitizeHiddenStatuses", () => {
  it("returns nothing hidden for a missing or malformed value", () => {
    expect(sanitizeHiddenStatuses(undefined)).toEqual([]);
    expect(sanitizeHiddenStatuses("done")).toEqual([]);
    expect(sanitizeHiddenStatuses({ done: true })).toEqual([]);
  });

  it("keeps status order and removes duplicates and unknown names", () => {
    expect(sanitizeHiddenStatuses(["done", "inbox", "done", "archived"])).toEqual(["inbox", "done"]);
  });

  it("refuses a stored value that would leave the board with no columns", () => {
    expect(sanitizeHiddenStatuses([...STATUSES])).toEqual([]);
    expect(sanitizeHiddenStatuses([...STATUSES, "nonsense"])).toEqual([]);
  });

  it("allows hiding all but one column", () => {
    const allButReady = STATUSES.filter((status) => status !== "ready");
    expect(sanitizeHiddenStatuses(allButReady)).toEqual(allButReady);
  });
});

describe("toggleHiddenStatus", () => {
  it("adds and removes one column at a time", () => {
    expect(toggleHiddenStatus([], "done")).toEqual(["done"]);
    expect(toggleHiddenStatus(["done"], "done")).toEqual([]);
    expect(toggleHiddenStatus(["done"], "inbox")).toEqual(["inbox", "done"]);
  });

  it("refuses the toggle that would hide the last visible column, without discarding the rest", () => {
    const allButDone = STATUSES.filter((status) => status !== "done");
    // Refused means unchanged. It used to return [] here — deferring to the
    // storage repair — so unchecking the last column turned all six back on
    // and silently threw away five deliberate choices.
    expect(toggleHiddenStatus(allButDone, "done")).toEqual(allButDone);
    expect(canHideStatus(allButDone, "done")).toBe(false);
    // Every other row in that state is still live, because un-hiding is free.
    for (const status of allButDone) {
      expect(canHideStatus(allButDone, status), status).toBe(true);
      expect(toggleHiddenStatus(allButDone, status)).not.toEqual(allButDone);
    }
  });

  it("allows hiding up to one short of every column", () => {
    let hidden: TaskStatus[] = [];
    for (const status of STATUSES.slice(0, STATUSES.length - 1)) {
      expect(canHideStatus(hidden, status), status).toBe(true);
      hidden = toggleHiddenStatus(hidden, status);
    }
    expect(hidden).toHaveLength(STATUSES.length - 1);
    expect(visibleBoardStatuses(hidden, {}, false)).toEqual([STATUSES[STATUSES.length - 1]]);
  });

  it("still repairs an all-hidden value that came from storage", () => {
    // The refusal above governs a click. A stored set that hides everything is
    // unrecoverable from the board itself — no column, so no menu to fix it
    // from — so that one falls open to showing all columns, as before.
    expect(sanitizeHiddenStatuses([...STATUSES])).toEqual([]);
    expect(toggleHiddenStatus([...STATUSES], "done")).toEqual(["done"]);
  });
});

describe("visibleBoardStatuses", () => {
  it("collapses empty columns while idle and re-expands them while dragging", () => {
    const counts = { ready: 2, done: 1 };
    expect(visibleBoardStatuses([], counts, false)).toEqual(["ready", "done"]);
    expect(visibleBoardStatuses([], counts, true)).toEqual([...STATUSES]);
  });

  it("keeps every column when the board is empty so drop targets still exist", () => {
    expect(visibleBoardStatuses([], {}, false)).toEqual([...STATUSES]);
  });

  it("never draws a hidden column, not even as a drop target mid-drag", () => {
    const counts = { ready: 2, done: 1 };
    expect(visibleBoardStatuses(["done"], counts, false)).toEqual(["ready"]);
    expect(visibleBoardStatuses(["done"], counts, true)).toEqual(
      STATUSES.filter((status) => status !== "done"),
    );
  });

  it("shows the remaining columns when hiding leaves only empty ones", () => {
    expect(visibleBoardStatuses(["ready"], { ready: 4 }, false)).toEqual(
      STATUSES.filter((status) => status !== "ready"),
    );
  });

  it("ignores a stored hidden set that would hide everything", () => {
    expect(visibleBoardStatuses([...STATUSES], { ready: 1 }, false)).toEqual(["ready"]);
  });
});

describe("hiddenColumnReport", () => {
  it("says nothing when no column is hidden", () => {
    expect(hiddenColumnReport([], { ready: 5 })).toBeNull();
  });

  it("says nothing when the hidden columns are empty", () => {
    expect(hiddenColumnReport(["done"], { ready: 5 })).toBeNull();
    expect(hiddenColumnReport(["done"], { ready: 5, done: 0 })).toBeNull();
  });

  it("reports work the reader cannot see, with the count and the column names", () => {
    expect(hiddenColumnReport(["done"], { ready: 5, done: 1 })).toEqual({
      statuses: ["done"],
      tasks: 1,
      summary: "1 task in Done",
    });
    expect(hiddenColumnReport(["review", "done"], { review: 4, done: 8 })).toEqual({
      statuses: ["review", "done"],
      tasks: 12,
      summary: "12 tasks in Review and Done",
    });
  });

  it("lists three or more hidden columns as a readable sentence", () => {
    expect(hiddenColumnReport(["inbox", "review", "done"], { inbox: 1, review: 1, done: 1 })?.summary)
      .toBe("3 tasks in Inbox, Review and Done");
  });

  it("treats a negative or sub-task count as no work rather than inventing some", () => {
    expect(hiddenColumnReport(["done"], { done: -4 } as Partial<Record<TaskStatus, number>>)).toBeNull();
    // 0.4 is not a task. Truncating to 0 must report nothing, not "0 tasks".
    expect(hiddenColumnReport(["done"], { done: 0.4 })).toBeNull();
    expect(hiddenColumnReport(["done"], { done: 1.9 })).toEqual({
      statuses: ["done"],
      tasks: 1,
      summary: "1 task in Done",
    });
  });

  it("cannot report a column the sanitizer refused to hide", () => {
    const counts = Object.fromEntries(STATUSES.map((status) => [status, 3]));
    expect(hiddenColumnReport([...STATUSES], counts)).toBeNull();
  });
});
