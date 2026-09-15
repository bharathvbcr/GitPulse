import { describe, expect, it } from "vitest";
import {
  composeDue,
  DUE_SHORTCUTS,
  dueLabel,
  duePartsOf,
  monthGrid,
  readDuePhrase,
  relativeDue,
  shiftView,
  viewFor,
} from "./taskDue";
import { parseQuickAddDue, QUICK_ADD_DUE_HOUR } from "./taskQuickAdd";

/** Monday 14 September 2026, 09:00 local. */
const NOW_MS = new Date(2026, 8, 14, 9, 0, 0, 0).getTime();
const NOW_SEC = Math.floor(NOW_MS / 1000);
const at = (year: number, month: number, day: number, hour = 0, minute = 0) =>
  Math.floor(new Date(year, month, day, hour, minute, 0, 0).getTime() / 1000);

describe("duePartsOf", () => {
  it("splits a deadline into the day and clock the controls edit", () => {
    expect(duePartsOf(at(2026, 8, 18, 17, 30))).toEqual({
      day: { year: 2026, month: 8, day: 18 },
      time: "17:30",
    });
  });

  it("pads a single-digit clock so an <input type=time> accepts it", () => {
    expect(duePartsOf(at(2026, 8, 18, 9, 5))?.time).toBe("09:05");
  });

  it("is null for every shape of no-deadline", () => {
    for (const value of [null, undefined, 0, -1, Number.NaN, Number.POSITIVE_INFINITY]) {
      expect(duePartsOf(value)).toBeNull();
    }
  });
});

describe("composeDue", () => {
  it("joins a day and a clock", () => {
    expect(composeDue({ year: 2026, month: 8, day: 18 }, "17:30")).toBe(at(2026, 8, 18, 17, 30));
  });

  it("falls back to the hour a bare date word means, so picking a day alone matches typing it", () => {
    const picked = composeDue({ year: 2026, month: 8, day: 18 }, "");
    expect(picked).toBe(at(2026, 8, 18, QUICK_ADD_DUE_HOUR, 0));
    expect(picked).toBe(parseQuickAddDue("2026-09-18", NOW_MS));
  });

  it("refuses a clock out of range rather than rolling it over", () => {
    expect(composeDue({ year: 2026, month: 8, day: 18 }, "24:00")).toBe(at(2026, 8, 18, QUICK_ADD_DUE_HOUR, 0));
    expect(composeDue({ year: 2026, month: 8, day: 18 }, "10:75")).toBe(at(2026, 8, 18, QUICK_ADD_DUE_HOUR, 0));
  });

  it("refuses a day the Date constructor would roll into the next month", () => {
    expect(composeDue({ year: 2026, month: 1, day: 31 }, "09:00")).toBeNull();
  });

  it("round-trips against duePartsOf", () => {
    const original = at(2026, 11, 31, 23, 59);
    const parts = duePartsOf(original);
    expect(parts).not.toBeNull();
    expect(composeDue(parts!.day, parts!.time)).toBe(original);
  });
});

describe("monthGrid", () => {
  it("is six Sunday-first weeks, so the grid never changes height", () => {
    const cells = monthGrid({ year: 2026, month: 8 }, null, NOW_MS);
    expect(cells).toHaveLength(42);
    expect(new Date(cells[0].year, cells[0].month, cells[0].day).getDay()).toBe(0);
  });

  it("marks today, the selection, and the days borrowed from the neighbours", () => {
    const cells = monthGrid({ year: 2026, month: 8 }, { year: 2026, month: 8, day: 18 }, NOW_MS);
    expect(cells.filter((cell) => cell.isToday).map((cell) => cell.label)).toEqual([14]);
    expect(cells.filter((cell) => cell.isSelected).map((cell) => cell.label)).toEqual([18]);
    const borrowed = cells.filter((cell) => !cell.inMonth);
    expect(borrowed.length).toBeGreaterThan(0);
    expect(borrowed.every((cell) => cell.month !== 8)).toBe(true);
  });

  it("gives every cell a distinct key", () => {
    const cells = monthGrid({ year: 2026, month: 8 }, null, NOW_MS);
    expect(new Set(cells.map((cell) => cell.key)).size).toBe(cells.length);
  });

  it("keeps a selection in another month unmarked", () => {
    const cells = monthGrid({ year: 2026, month: 8 }, { year: 2027, month: 8, day: 18 }, NOW_MS);
    expect(cells.some((cell) => cell.isSelected)).toBe(false);
  });
});

describe("viewFor and shiftView", () => {
  it("opens on the deadline's month, else on today's", () => {
    expect(viewFor(at(2026, 11, 3, 17), NOW_MS)).toEqual({ year: 2026, month: 11 });
    expect(viewFor(null, NOW_MS)).toEqual({ year: 2026, month: 8 });
  });

  it("carries the year at both ends", () => {
    expect(shiftView({ year: 2026, month: 11 }, 1)).toEqual({ year: 2027, month: 0 });
    expect(shiftView({ year: 2026, month: 0 }, -1)).toEqual({ year: 2025, month: 11 });
  });
});

describe("relativeDue", () => {
  it("counts whole calendar days, not hours", () => {
    // 17:00 today is still "today" at 09:00, and midnight tomorrow is
    // "tomorrow" even though it is only 15 hours away.
    expect(relativeDue(at(2026, 8, 14, 17), NOW_SEC).text).toBe("today");
    expect(relativeDue(at(2026, 8, 15, 0, 0), NOW_SEC).text).toBe("tomorrow");
  });

  it("phrases both directions", () => {
    expect(relativeDue(at(2026, 8, 18, 17), NOW_SEC).text).toBe("in 4 days");
    expect(relativeDue(at(2026, 8, 13, 17), NOW_SEC).text).toBe("yesterday");
    expect(relativeDue(at(2026, 8, 4, 17), NOW_SEC).text).toBe("10 days ago");
  });

  it("reports the board's own urgency rather than a second opinion", () => {
    expect(relativeDue(at(2026, 8, 13, 17), NOW_SEC).state).toBe("overdue");
    expect(relativeDue(at(2026, 11, 1, 17), NOW_SEC).state).toBe("later");
    expect(relativeDue(null, NOW_SEC)).toEqual({ text: "", state: "none" });
  });
});

describe("dueLabel", () => {
  it("says so plainly when there is no deadline", () => {
    expect(dueLabel(null)).toBe("No due date");
    expect(dueLabel(0)).toBe("No due date");
  });

  it("writes a real deadline out", () => {
    const label = dueLabel(at(2026, 8, 18, 17, 0), "en-US");
    expect(label).toContain("18");
    expect(label).toContain("2026");
  });
});

describe("readDuePhrase", () => {
  it("accepts the words the due: marker already accepts", () => {
    for (const word of ["today", "tomorrow", "friday", "next-week", "+2w", "2026-12-03"]) {
      expect(readDuePhrase(word, null, NOW_MS)).toBe(parseQuickAddDue(word, NOW_MS));
    }
  });

  it("reads a day and a clock together", () => {
    expect(readDuePhrase("friday 5pm", null, NOW_MS)).toBe(at(2026, 8, 18, 17, 0));
    expect(readDuePhrase("tomorrow 09:30", null, NOW_MS)).toBe(at(2026, 8, 15, 9, 30));
  });

  it("keeps the chosen day when the phrase is only a clock", () => {
    const friday = at(2026, 8, 18, 17, 0);
    expect(readDuePhrase("9am", friday, NOW_MS)).toBe(at(2026, 8, 18, 9, 0));
  });

  it("has nothing to say about a bare clock with no day chosen", () => {
    expect(readDuePhrase("9am", null, NOW_MS)).toBeNull();
  });

  it("guesses at nothing", () => {
    for (const text of ["", "   ", "someday", "next", "the 5th of never"]) {
      expect(readDuePhrase(text, null, NOW_MS)).toBeNull();
    }
  });
});

describe("DUE_SHORTCUTS", () => {
  it("every shortcut is a word the shared grammar resolves", () => {
    expect(DUE_SHORTCUTS.length).toBeGreaterThan(0);
    for (const shortcut of DUE_SHORTCUTS) {
      expect(parseQuickAddDue(shortcut.word, NOW_MS), shortcut.id).not.toBeNull();
    }
  });

  it("has no duplicate ids", () => {
    expect(new Set(DUE_SHORTCUTS.map((item) => item.id)).size).toBe(DUE_SHORTCUTS.length);
  });
});
