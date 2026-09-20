import { describe, expect, it } from "vitest";
import type { MonitorPhase } from "./phase";
import {
  barWidthPct,
  durationMs,
  formatDuration,
  GLANCE_NAME_CHARS,
  glanceName,
  glanceState,
  glanceTitle,
  longestDurationMs,
  medianSettledDurationMs,
  parseInstant,
  shortCommit,
  TIMELINE_PREVIEW_COUNT,
  type TimelineRow,
} from "./timeline";

const T0 = Date.parse("2026-09-17T08:00:00Z");

function makeRow(overrides: Partial<TimelineRow> = {}): TimelineRow {
  return {
    id: "1",
    label: "run",
    sublabel: "",
    phase: "settled_ok" as MonitorPhase,
    stateLabel: "Passed",
    startedAt: new Date(T0).toISOString(),
    endedAt: new Date(T0 + 60_000).toISOString(),
    commitSha: "0123456789abcdef0123456789abcdef01234567",
    branch: "main",
    trigger: "push",
    url: "https://example.invalid/run/1",
    ...overrides,
  };
}

describe("timeline preview length", () => {
  it("draws three recent rows so a twenty-run sample cannot bury the rail", () => {
    expect(TIMELINE_PREVIEW_COUNT).toBe(3);
  });
});

describe("glance identity", () => {
  it("prefers the shorter name, so a workflow wins over a commit subject", () => {
    expect(
      glanceName(
        makeRow({
          label: "chore(vendor): re-vendor dc-store from upstream",
          sublabel: "CI",
        }),
      ),
    ).toBe("CI");
    expect(
      glanceName(makeRow({ label: "rollout-7f3", sublabel: "fix the thing on main" })),
    ).toBe("rollout-7f3");
  });

  it("caps a leftover long identity without splitting a code point", () => {
    const out = glanceName(makeRow({ label: "👩‍👩‍👧‍👦".repeat(40), sublabel: "" }));
    expect([...out].length).toBeLessThanOrEqual(GLANCE_NAME_CHARS + 1);
    expect(out).not.toContain("�");
    expect(out.endsWith("…")).toBe(true);
  });

  it("never invents a name when both fields are empty", () => {
    expect(glanceName(makeRow({ label: "", sublabel: "" }))).toBe("");
  });
});

describe("glance state", () => {
  it("prints a four-word vocabulary, never the source's sentence", () => {
    expect(glanceState("settled_ok")).toBe("Pass");
    expect(glanceState("settled_bad")).toBe("Fail");
    expect(glanceState("in_flight")).toBe("Live");
    expect(glanceState("unknown")).toBe("—");
  });
});

describe("glance tooltip", () => {
  it("keeps the searchable full wording off the tile face", () => {
    const row = makeRow({
      label: "chore(vendor): re-vendor",
      stateLabel: "Completed (no conclusion reported)",
      branch: "main",
      trigger: "push",
    });
    expect(glanceTitle(row)).toContain("Completed (no conclusion reported)");
    expect(glanceTitle(row)).toContain("chore(vendor): re-vendor");
    expect(glanceState(row.phase)).not.toContain("Completed");
  });
});

describe("parsing an instant", () => {
  it("reads a real ISO timestamp", () => {
    expect(parseInstant("2026-09-17T08:00:00Z")).toBe(T0);
  });

  it("returns null for every shape of absence", () => {
    // `Date.parse` yields NaN for all of these, and NaN propagates silently
    // through every subtraction downstream — a NaN duration draws exactly like
    // a fast one.
    for (const value of ["", "   ", "not a date", "0000-13-45T99:99:99Z"]) {
      expect(parseInstant(value), JSON.stringify(value)).toBeNull();
    }
  });

  it("returns null for values that are not strings at all", () => {
    // The wire fields are typed as strings, but they arrive from serde and
    // from a CLI; a null slipping through must not become the epoch.
    for (const value of [null, undefined, 0, {}, []]) {
      expect(parseInstant(value as unknown as string), JSON.stringify(value)).toBeNull();
    }
  });
});

describe("duration", () => {
  it("measures a settled row from start to last change", () => {
    expect(durationMs(makeRow(), T0 + 999_999)).toBe(60_000);
  });

  it("is unknown, not zero, for a row that never started", () => {
    // A queued run has no start instant. Zero would render as an instant
    // success; the whole point is that these two must not look alike.
    expect(durationMs(makeRow({ startedAt: "", phase: "in_flight" }), T0)).toBeNull();
    expect(durationMs(makeRow({ startedAt: "" }), T0)).toBeNull();
  });

  it("is unknown for a settled row with no end instant", () => {
    expect(durationMs(makeRow({ endedAt: "" }), T0 + 60_000)).toBeNull();
  });

  it("grows against the clock while the row is in flight", () => {
    const row = makeRow({ phase: "in_flight", endedAt: "" });
    expect(durationMs(row, T0 + 30_000)).toBe(30_000);
    expect(durationMs(row, T0 + 90_000)).toBe(90_000);
  });

  it("is unknown rather than negative when the clock disagrees with itself", () => {
    // Clock skew, a suspended machine, or a timezone bug upstream. Clamping
    // to zero would publish "0s" for a run that took ten minutes.
    expect(durationMs(makeRow({ endedAt: new Date(T0 - 60_000).toISOString() }), T0)).toBeNull();
    expect(durationMs(makeRow({ phase: "in_flight", endedAt: "" }), T0 - 60_000)).toBeNull();
  });

  it("is unknown when the clock itself is unreadable", () => {
    const row = makeRow({ phase: "in_flight", endedAt: "" });
    expect(durationMs(row, Number.NaN)).toBeNull();
    expect(durationMs(row, Number.POSITIVE_INFINITY)).toBeNull();
  });

  it("reads a zero-length settled span as zero, not as unknown", () => {
    // A genuinely instantaneous run (a workflow that skipped every job) is a
    // real measurement and must stay distinguishable from an absent one.
    expect(durationMs(makeRow({ endedAt: makeRow().startedAt }), T0)).toBe(0);
  });
});

describe("the bar scale", () => {
  it("is null when no duration in the sample is known", () => {
    // Dividing by a zero maximum yields Infinity, and a bar of `Infinity%`
    // clamps to full width — drawing every unknown row as the slowest run.
    const rows = [makeRow({ startedAt: "" }), makeRow({ id: "2", startedAt: "" })];
    expect(longestDurationMs(rows, T0)).toBeNull();
    expect(barWidthPct(durationMs(rows[0], T0), longestDurationMs(rows, T0))).toBeNull();
  });

  it("takes the longest known duration and ignores the unknown ones", () => {
    const rows = [
      makeRow({ id: "1", endedAt: new Date(T0 + 10_000).toISOString() }),
      makeRow({ id: "2", startedAt: "" }),
      makeRow({ id: "3", endedAt: new Date(T0 + 90_000).toISOString() }),
    ];
    expect(longestDurationMs(rows, T0)).toBe(90_000);
  });

  it("clamps into the track even when the row outgrew the scale", () => {
    // An in-flight row keeps growing between the scale being computed and the
    // row being drawn, so `ms > longestMs` is a normal transient, not a bug.
    expect(barWidthPct(200, 100)).toBe(100);
  });

  it("floors a known-but-tiny duration at a visible sliver", () => {
    // A zero-width bar reads as missing data, which is the one thing a
    // measured three-second run is not.
    const width = barWidthPct(1, 10_000_000);
    expect(width).not.toBeNull();
    expect(width).toBeGreaterThan(0);
  });

  it("refuses a nonsensical scale instead of producing a width from it", () => {
    for (const [ms, longest] of [
      [null, 100],
      [100, null],
      [100, 0],
      [100, -5],
      [Number.NaN, 100],
      [100, Number.NaN],
      [Number.POSITIVE_INFINITY, 100],
    ] as const) {
      expect(barWidthPct(ms as number | null, longest as number | null), `${ms}/${longest}`).toBeNull();
    }
  });
});

describe("formatting a duration", () => {
  it("marks an unknown duration as visibly not a number", () => {
    expect(formatDuration(null)).toBe("—");
    expect(formatDuration(Number.NaN)).toBe("—");
    expect(formatDuration(-1)).toBe("—");
  });

  it("scales the unit to the magnitude", () => {
    expect(formatDuration(0)).toBe("0ms");
    expect(formatDuration(999)).toBe("999ms");
    expect(formatDuration(3_400)).toBe("3.4s");
    expect(formatDuration(59_900)).toBe("59.9s");
    expect(formatDuration(60_000)).toBe("1m 00s");
    expect(formatDuration(125_000)).toBe("2m 05s");
    expect(formatDuration(3_600_000)).toBe("1h 00m");
    expect(formatDuration(4_320_000)).toBe("1h 12m");
  });

  it("never produces an unpadded or malformed component", () => {
    // A "2m 5s" beside a "2m 45s" reads as the shorter of the two at a glance
    // in a right-aligned column.
    for (let seconds = 60; seconds < 7_200; seconds += 37) {
      const text = formatDuration(seconds * 1000);
      expect(text, `${seconds}s`).toMatch(/^(\d+m \d{2}s|\d+h \d{2}m)$/);
    }
  });
});

describe("median settled duration", () => {
  const settled = (id: string, ms: number, phase: MonitorPhase = "settled_ok") =>
    makeRow({ id, phase, endedAt: new Date(T0 + ms).toISOString() });

  it("returns null and an empty sample when nothing can be measured", () => {
    expect(medianSettledDurationMs([], T0)).toEqual({ medianMs: null, sample: 0 });
    expect(medianSettledDurationMs([makeRow({ startedAt: "" })], T0)).toEqual({
      medianMs: null,
      sample: 0,
    });
  });

  it("excludes in-flight rows, whose duration is still growing", () => {
    // Including them makes the median drift every time the poll fires.
    const rows = [
      settled("1", 10_000),
      makeRow({ id: "2", phase: "in_flight", endedAt: "" }),
      settled("3", 30_000),
    ];
    const result = medianSettledDurationMs(rows, T0 + 10_000_000);
    expect(result).toEqual({ medianMs: 20_000, sample: 2 });
  });

  it("excludes unjudgeable rows for the same reason a pass rate does", () => {
    const rows = [settled("1", 10_000), settled("2", 999_000, "unknown")];
    expect(medianSettledDurationMs(rows, T0)).toEqual({ medianMs: 10_000, sample: 1 });
  });

  it("takes the middle of an odd sample and the mean of the middle two", () => {
    expect(
      medianSettledDurationMs([settled("1", 10_000), settled("2", 50_000), settled("3", 30_000)], T0)
        .medianMs,
    ).toBe(30_000);
    expect(
      medianSettledDurationMs([settled("1", 10_000), settled("2", 40_000)], T0).medianMs,
    ).toBe(25_000);
  });

  it("does not mutate the caller's array", () => {
    // It sorts internally; sorting the input in place would silently reorder
    // the rows the component is rendering.
    const rows = [settled("1", 50_000), settled("2", 10_000)];
    const order = rows.map((r) => r.id);
    medianSettledDurationMs(rows, T0);
    expect(rows.map((r) => r.id)).toEqual(order);
  });

  it("counts a failed run's duration too", () => {
    // How long a run takes to fail is exactly as interesting as how long it
    // takes to pass — often more.
    expect(
      medianSettledDurationMs([settled("1", 20_000, "settled_bad")], T0),
    ).toEqual({ medianMs: 20_000, sample: 1 });
  });
});

describe("short commit", () => {
  it("abbreviates to git's own default", () => {
    expect(shortCommit("0123456789abcdef0123456789abcdef01234567")).toBe("0123456");
  });

  it("is safe on absence", () => {
    expect(shortCommit("")).toBe("");
    expect(shortCommit(null as unknown as string)).toBe("");
  });
});
