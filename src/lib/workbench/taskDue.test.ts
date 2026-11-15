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

describe("DST and non-UTC timezone behaviour", () => {
  const activeZone = Intl.DateTimeFormat().resolvedOptions().timeZone;

  it("never silently shifts an hour when composeDue succeeds across DST transition dates", () => {
    for (const [year, month, day] of [
      [2026, 2, 8],   // US spring-forward (23h day in America/New_York)
      [2026, 3, 5],   // NZ fall-back (25h day in Pacific/Auckland)
      [2026, 8, 27],  // NZ spring-forward (23h day in Pacific/Auckland)
      [2026, 10, 1],  // US fall-back (25h day in America/New_York)
    ]) {
      for (let hour = 0; hour < 24; hour += 1) {
        const time = `${String(hour).padStart(2, "0")}:30`;
        const sec = composeDue({ year, month, day }, time);
        if (sec === null) continue;
        expect(duePartsOf(sec)).toEqual({ day: { year, month, day }, time });
      }
    }
  });

  it("keeps consecutive local days in monthGrid and relativeDue across 23h and 25h days", () => {
    for (const view of [
      { year: 2026, month: 2 },
      { year: 2026, month: 3 },
      { year: 2026, month: 8 },
      { year: 2026, month: 10 },
    ]) {
      const cells = monthGrid(view, null, NOW_MS);
      expect(cells).toHaveLength(42);
      expect(new Set(cells.map((c) => c.key)).size).toBe(42);
      for (let i = 1; i < cells.length; i += 1) {
        const prev = new Date(cells[i - 1].year, cells[i - 1].month, cells[i - 1].day);
        prev.setDate(prev.getDate() + 1);
        expect({ year: cells[i].year, month: cells[i].month, day: cells[i].day }).toEqual({
          year: prev.getFullYear(),
          month: prev.getMonth(),
          day: prev.getDate(),
        });
      }
    }

    const marNow = at(2026, 2, 8, 9, 0);
    expect(relativeDue(at(2026, 2, 7, 17, 0), marNow).text).toBe("yesterday");
    expect(relativeDue(at(2026, 2, 8, 17, 0), marNow).text).toBe("today");
    expect(relativeDue(at(2026, 2, 9, 17, 0), marNow).text).toBe("tomorrow");

    const novNow = at(2026, 10, 1, 9, 0);
    expect(relativeDue(at(2026, 9, 31, 17, 0), novNow).text).toBe("yesterday");
    expect(relativeDue(at(2026, 10, 1, 17, 0), novNow).text).toBe("today");
    expect(relativeDue(at(2026, 10, 2, 17, 0), novNow).text).toBe("tomorrow");
  });

  it.skipIf(activeZone !== "America/New_York")(
    "rejects the 2026-03-08 02:30 spring-forward gap with null and round-trips fall-back 01:30 in America/New_York",
    () => {
      // 2026-03-08 02:00 EST jumps directly to 03:00 EDT in America/New_York.
      expect(composeDue({ year: 2026, month: 2, day: 8 }, "02:30")).toBeNull();
      expect(readDuePhrase("2026-03-08 02:30", null, NOW_MS)).toBeNull();
      const validOnGapDay = composeDue({ year: 2026, month: 2, day: 8 }, "17:00");
      expect(validOnGapDay).not.toBeNull();
      expect(readDuePhrase("02:30", validOnGapDay, NOW_MS)).toBeNull();

      // Boundary minutes flanking the spring-forward gap remain valid and 60s apart.
      const beforeGap = composeDue({ year: 2026, month: 2, day: 8 }, "01:59");
      const afterGap = composeDue({ year: 2026, month: 2, day: 8 }, "03:00");
      expect(beforeGap).not.toBeNull();
      expect(afterGap).not.toBeNull();
      expect(afterGap! - beforeGap!).toBe(60);
      expect(duePartsOf(beforeGap)?.time).toBe("01:59");
      expect(duePartsOf(afterGap)?.time).toBe("03:00");

      // 2026-11-01 01:30 occurs twice (EDT then EST); composeDue accepts and round-trips it.
      const overlap = composeDue({ year: 2026, month: 10, day: 1 }, "01:30");
      expect(overlap).not.toBeNull();
      expect(duePartsOf(overlap)).toEqual({
        day: { year: 2026, month: 10, day: 1 },
        time: "01:30",
      });
    },
  );

  it.skipIf(activeZone !== "Pacific/Auckland")(
    "rejects the 2026-09-27 02:30 spring-forward gap with null and round-trips fall-back 02:30 in Pacific/Auckland",
    () => {
      // 2026-09-27 02:00 NZST jumps directly to 03:00 NZDT in Pacific/Auckland.
      expect(composeDue({ year: 2026, month: 8, day: 27 }, "02:30")).toBeNull();
      const beforeGap = composeDue({ year: 2026, month: 8, day: 27 }, "01:59");
      const afterGap = composeDue({ year: 2026, month: 8, day: 27 }, "03:00");
      expect(beforeGap).not.toBeNull();
      expect(afterGap).not.toBeNull();
      expect(afterGap! - beforeGap!).toBe(60);

      // 2026-04-05 02:30 occurs twice during NZDT->NZST fall-back and round-trips cleanly.
      const overlap = composeDue({ year: 2026, month: 3, day: 5 }, "02:30");
      expect(overlap).not.toBeNull();
      expect(duePartsOf(overlap)).toEqual({
        day: { year: 2026, month: 3, day: 5 },
        time: "02:30",
      });
    },
  );

  it.skipIf(activeZone !== "Asia/Kolkata" && activeZone !== "Asia/Calcutta")(
    "round-trips 02:30 and half-hour midnight boundaries without drift in Asia/Kolkata",
    () => {
      expect(new Date(2026, 2, 8, 2, 30).getTimezoneOffset()).toBe(-330);
      for (const time of ["00:15", "02:30", "23:45"]) {
        const sec = composeDue({ year: 2026, month: 2, day: 8 }, time);
        expect(sec).not.toBeNull();
        expect(duePartsOf(sec)).toEqual({
          day: { year: 2026, month: 2, day: 8 },
          time,
        });
      }
    },
  );
});
