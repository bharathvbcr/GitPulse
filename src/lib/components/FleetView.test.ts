import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import FleetView, { applyFilter, scanTargets, SEVERITY_STRIPE } from "./FleetView.svelte";
import FleetCell from "./FleetCell.svelte";
import FleetLanguageBar from "./FleetLanguageBar.svelte";
import FleetPulsePanel from "./FleetPulse.svelte";
import FleetSparkline from "./FleetSparkline.svelte";
import { fleetLanguageMix, fleetPulse } from "../fleet/pulse";
import { HIDEABLE_COLUMNS } from "../fleet/sort";
import FleetTotals from "./FleetTotals.svelte";
import type { FleetTally } from "../fleet/aggregate";
import { UNSCANNED, deltaFrom, failedCell, readCell, type FleetRow } from "../fleet/types";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "FleetView.svelte"), "utf8");
const cellSource = readFileSync(join(here, "FleetCell.svelte"), "utf8");
const totalsSource = readFileSync(join(here, "FleetTotals.svelte"), "utf8");

function row(overrides: Partial<FleetRow> = {}): FleetRow {
  return {
    path: "/repo/a",
    label: "a",
    presence: "open",
    branch: "main",
    severity: "clean",
    headline: "clean",
    changes: UNSCANNED,
    sync: UNSCANNED,
    watchWarning: null,
    work: UNSCANNED,
    activity: UNSCANNED,
    commits: UNSCANNED,
    loc: UNSCANNED,
    storage: UNSCANNED,
    health: UNSCANNED,
    coverage: UNSCANNED,
    ...overrides,
  };
}

describe("FleetView", () => {
  it("renders with nothing open and says so", () => {
    const { body } = render(FleetView);
    expect(body).toContain("Fleet");
    expect(body).toContain("No repositories are open.");
    expect(body).toContain("No repositories yet");
  });

  it("takes the attention clause from the headline rather than re-spelling it", () => {
    // The tile restates a count fleetHeadline already owns. Spelling the
    // clause here too is how it came to read "1 need attention" beside a
    // sentence that read correctly.
    //
    // The strip carries the reading as data now rather than as markup, so the
    // assertion follows it there. The property is unchanged and the negative
    // is wider than it was: it catches the count next to a hard-coded verb in
    // either spelling, the interpolation and the template literal alike.
    expect(source).toContain("note: headline.attentionClause");
    expect(source).not.toMatch(/headline\.attention\}?\s*need/);
    // And the field is populated on the empty path this render exercises.
    expect(render(FleetView).body).toContain("0 need attention");
  });
});

describe("applyFilter", () => {
  it("shows everything on All, including recents", () => {
    const rows = [row(), row({ path: "/old", presence: "recent", severity: "unknown" })];
    expect(applyFilter(rows, "all")).toHaveLength(2);
  });

  it("keeps only open repositories that need something on Attention", () => {
    const rows = [
      row({ severity: "clean" }),
      row({ path: "/b", severity: "conflicts" }),
      // A recents row is unknown by construction; listing it under "needs
      // attention" would fill the list with rows nobody can act on.
      row({ path: "/old", presence: "recent", severity: "unknown" }),
    ];
    expect(applyFilter(rows, "attention").map((r) => r.path)).toEqual(["/b"]);
  });

  it("does not mutate the array it was handed", () => {
    const rows = [row()];
    applyFilter(rows, "all").push(row({ path: "/x" }));
    expect(rows).toHaveLength(1);
  });
});

describe("scanTargets", () => {
  it("never sends a scan at a repository that is not open", () => {
    // A recents path may not exist any more, and scanning it would spawn work
    // against a repository the user has not opened.
    const rows = [row(), row({ path: "/old", presence: "recent" })];
    expect(scanTargets(rows)).toEqual([{ path: "/repo/a", label: "a" }]);
  });
});

describe("SEVERITY_STRIPE", () => {
  it("gives every severity its own tone", () => {
    const severities: FleetRow["severity"][] = [
      "conflicts",
      "operation",
      "unknown",
      "uncommitted",
      "unpushed",
      "stash",
      "clean",
    ];
    for (const severity of severities) {
      expect(SEVERITY_STRIPE[severity], severity).toBeTruthy();
    }
  });
});

describe("FleetCell", () => {
  it("renders a measured value with its own content", () => {
    const { body } = render(FleetCell, {
      props: { cell: readCell(42, Date.now()), label: "Lines of code" },
    });
    expect(body).toContain('data-state="read"');
    expect(body).not.toContain("not scanned");
  });

  it("says not scanned rather than showing a zero or a dash", () => {
    const { body } = render(FleetCell, { props: { cell: UNSCANNED, label: "Storage" } });
    expect(body).toContain('data-state="unscanned"');
    expect(body).toContain("not scanned");
    // An em dash or a 0 here is the whole failure this component exists to
    // prevent: it is indistinguishable from a measurement.
    expect(body).not.toContain(">0<");
  });

  it("says could not read, and carries the reason, when a scan failed", () => {
    const { body } = render(FleetCell, {
      props: { cell: failedCell("npm is not installed"), label: "Vulnerabilities" },
    });
    expect(body).toContain('data-state="failed"');
    expect(body).toContain("could not read");
    expect(body).toContain("npm is not installed");
  });

  it("marks a partial value so a floor cannot read as a total", () => {
    const { body } = render(FleetCell, {
      props: { cell: readCell(42, Date.now(), true), label: "Storage" },
    });
    expect(body).toContain('data-partial="true"');
  });

  it("turns an absence into a scan when the column has one behind it", () => {
    // The place a reader notices a measurement is missing is the cell where it
    // is missing. Without this they have to find the toolbar sweep, which
    // rescans all two dozen repositories to fill in one gap.
    const { body } = render(FleetCell, {
      props: { cell: UNSCANNED, label: "Storage", onScan: () => {} },
    });
    expect(body).toContain("<button");
    expect(body).toContain("Click to scan just this one");
    // The words never change with the affordance: an absence reads the same
    // whether or not there is something to click.
    expect(body).toContain('data-state="unscanned"');
    expect(body).toContain("not scanned");
  });

  it("offers a retry on a failed cell without softening what it says", () => {
    const { body } = render(FleetCell, {
      props: { cell: failedCell("npm is not installed"), label: "Vulns", onScan: () => {} },
    });
    expect(body).toContain('data-state="failed"');
    expect(body).toContain("could not read");
    expect(body).toContain("npm is not installed");
    expect(body).toContain("Click to try this repository again");
  });

  it("stays inert for a column with no per-repository scan", () => {
    const { body } = render(FleetCell, { props: { cell: UNSCANNED, label: "Changes" } });
    expect(body).not.toContain("<button");
  });

  it("distinguishes unscanned from failed in the markup, not only in words", () => {
    const unscanned = render(FleetCell, { props: { cell: UNSCANNED, label: "x" } }).body;
    const failed = render(FleetCell, { props: { cell: failedCell("boom"), label: "x" } }).body;
    expect(unscanned).not.toBe(failed);
  });
});

describe("FleetCell while a scan is running", () => {
  it("keeps last week's measurement on screen while it is being replaced", () => {
    // The marker is strictly additive. Blanking a value the moment a rescan
    // starts would turn a known number into "nothing" for the length of the
    // scan — the one transition this component exists to make impossible.
    const { body } = render(FleetCell, {
      props: { cell: readCell(42, Date.now()), label: "Storage", scanning: true },
    });
    expect(body).toContain('data-state="read"');
    expect(body).toContain('data-scanning="true"');
    expect(body).toContain("fleet-cell-scanning");
    expect(body).not.toContain('data-state="scanning"');
  });

  it("replaces an absence, which has nothing to keep", () => {
    const { body } = render(FleetCell, {
      props: { cell: UNSCANNED, label: "Storage", scanning: true },
    });
    expect(body).toContain('data-state="scanning"');
    expect(body).toContain("scanning");
    // "scanning" must not be mistaken for a result, so the absence wording is
    // gone rather than shown alongside it.
    expect(body).not.toContain("not scanned");
  });

  it("says a failed cell is being retried, not that it failed again", () => {
    const { body } = render(FleetCell, {
      props: { cell: failedCell("npm is missing"), label: "Vulns", scanning: true },
    });
    expect(body).toContain('data-state="scanning"');
    expect(body).not.toContain("could not read");
  });

  it("is absent by default, so a still cell never looks busy", () => {
    const read = render(FleetCell, { props: { cell: readCell(1, Date.now()), label: "x" } }).body;
    expect(read).toContain('data-scanning="false"');
    expect(read).not.toContain("fleet-cell-scanning");
    // An absent cell says nothing about scanning either way — it is still just
    // "not scanned", which is the whole claim it is allowed to make.
    const absent = render(FleetCell, { props: { cell: UNSCANNED, label: "x" } }).body;
    expect(absent).toContain('data-state="unscanned"');
    expect(absent).not.toContain("scanning");
  });
});

describe("FleetCell change chips", () => {
  const withDelta = (change: number, goal: "lower" | "higher" | "neutral") => ({
    cell: readCell(100, Date.now(), false, deltaFrom(100, 100 - change, "2026-08-01")),
    label: "Vulnerabilities",
    deltaGoal: goal,
  });

  it("renders nothing at all when there is no baseline to compare against", () => {
    // A first scan has no direction. "+0" or "→" would be a claim about a past
    // nobody measured.
    const { body } = render(FleetCell, {
      props: { cell: readCell(100, Date.now()), label: "Vulnerabilities" },
    });
    expect(body).not.toContain("fleet-cell-delta");
  });

  it("shows the change, and names the day it is measured from", () => {
    const { body } = render(FleetCell, { props: withDelta(12, "lower") });
    expect(body).toContain("fleet-cell-delta");
    expect(body).toContain("+12");
    // The baseline day rides in the tooltip, because families are scanned
    // independently and "since yesterday" is often months wrong.
    expect(body).toContain("2026-08-01");
  });

  it("colours by the column's own goal, not by the sign", () => {
    const better = render(FleetCell, { props: withDelta(-6, "lower") }).body;
    const worse = render(FleetCell, { props: withDelta(-6, "higher") }).body;
    expect(better).toContain("emerald");
    expect(worse).toContain("amber");
  });

  it("stays quiet for an unchanged measurement", () => {
    const { body } = render(FleetCell, { props: withDelta(0, "lower") });
    expect(body).not.toContain("fleet-cell-delta");
  });

  it("never attaches a change to a cell that has no value", () => {
    for (const cell of [UNSCANNED, failedCell("boom")]) {
      const { body } = render(FleetCell, { props: { cell, label: "Vulns" } });
      expect(body).not.toContain("fleet-cell-delta");
    }
  });
});

describe("FleetTotals", () => {
  const tallyOf = (over: Partial<FleetTally> = {}): FleetTally => ({
    value: 10,
    counted: 4,
    eligible: 4,
    failed: 0,
    unscanned: 0,
    partial: false,
    ...over,
  });

  it("prints a complete total as a bare number", () => {
    // The clause is not decoration; printing it on a total that really does
    // cover everything is what taught readers to ignore it.
    const { body } = render(FleetTotals, {
      props: { totals: [{ key: "loc", label: "Lines", text: "1,000", tally: tallyOf() }] },
    });
    expect(body).toContain("1,000");
    expect(body).not.toContain("fleet-total-shortfall");
  });

  it("never lets a short total stand alone", () => {
    const { body } = render(FleetTotals, {
      props: {
        totals: [
          {
            key: "loc",
            label: "Lines",
            text: "1,000",
            tally: tallyOf({ counted: 2, failed: 1, unscanned: 1 }),
          },
        ],
      },
    });
    expect(body).toContain("fleet-total-shortfall");
    expect(body).toContain("2 of 4");
    expect(body).toContain("1 failed");
    expect(body).toContain("1 not scanned");
  });

  it("flags a total built from floors, even with every repository counted", () => {
    const { body } = render(FleetTotals, {
      props: {
        totals: [{ key: "loc", label: "Lines", text: "1,000", tally: tallyOf({ partial: true }) }],
      },
    });
    expect(body).toContain("fleet-total-shortfall");
    expect(body).toContain("partial");
  });

  it("carries a note for a reading that is a row count rather than a tally", () => {
    // "Repositories" is a count of rows, not a measurement of them, so it has
    // no tally and no shortfall — but it still gets to say something.
    const { body } = render(FleetTotals, {
      props: {
        totals: [{ key: "repos", label: "Repos", text: "4", tally: null, note: "3 need attention" }],
      },
    });
    expect(body).toContain("4");
    expect(body).toContain("3 need attention");
    expect(body).not.toContain("fleet-total-shortfall");
  });
});

describe("Fleet keyboard shortcuts", () => {
  it("listens on the view, never on the window", () => {
    // Fleet is *hidden* rather than unmounted while the repository pane shows,
    // so a window listener would keep firing "/" and "s" at someone typing
    // into the commit box behind it.
    expect(source).toContain("onkeydown={onGridKeydown}");
    expect(source).not.toMatch(/window\.addEventListener\(\s*["'`]keydown/);
    expect(source).not.toContain("<svelte:window");
  });

  it("hands every key back to a text field", () => {
    const handler = source.slice(source.indexOf("function onGridKeydown"));
    expect(handler).toContain('tag === "INPUT"');
    expect(handler).toContain('tag === "TEXTAREA"');
    expect(handler).toContain("isContentEditable");
    // Modified keys belong to the OS and the app's own accelerators.
    expect(handler).toContain("event.metaKey || event.ctrlKey || event.altKey");
    // A key pressed to compose a character is not a shortcut.
    expect(handler).toContain("isImeComposition");
  });

  it("cycles the sort through the columns actually on screen", () => {
    // Sorting by a hidden column would leave an order the reader can see the
    // effect of but not the cause of, and no way to undo it.
    const handler = source.slice(source.indexOf("function onGridKeydown"));
    expect(handler).toContain("shownColumns.map");
  });

  it("offers no shortcut that is the only way to reach its action", () => {
    // Each accelerator has a real focusable control behind it, which is what
    // makes the non-interactive container acceptable.
    expect(source).toContain("gitpulse-fleet-filter");
    expect(source).toContain("toggleFleetPulse");
    expect(source).toContain("fleetStore.refresh");
  });
});

describe("hiding a column cannot hide a failure", () => {
  it("reports what hiding cost, by column and by count", () => {
    expect(source).toContain("hiddenFailures");
    expect(source).toContain("fleet-hidden-failures");
  });

  it("keeps the two structural columns out of the menu", () => {
    expect(source).toContain("HIDEABLE_COLUMNS");
    // Repository and Severity are not offered, so the menu cannot produce a
    // grid of measurements with nothing to attribute them to.
    expect(HIDEABLE_COLUMNS).not.toContain("repository");
  });

  it("stores hidden columns in the interface store, so they survive a reload", () => {
    expect(source).toContain("fleetHiddenColumns");
    expect(source).toContain("toggleFleetColumn");
    expect(source).toContain("showAllFleetColumns");
  });
});

describe("the grid cannot invent a measurement", () => {
  it("routes every measurable column through FleetCell", () => {
    // Nine columns, six of them measurable. A column rendered inline would be
    // one where "no value" gets to pick its own spelling.
    const cells = source.match(/<FleetCell/g) ?? [];
    expect(cells.length).toBeGreaterThanOrEqual(6);
  });

  it("only ever reads a value inside a read branch", () => {
    // Every `.value` access in the template is guarded by its cell's kind, so
    // there is no path where an unscanned cell's absent value is coerced.
    for (const match of source.matchAll(/row\.(\w+)\.value/g)) {
      const field = match[1];
      expect(source, `${field}.value is read without a kind guard`).toContain(
        `row.${field}.kind === "read"`,
      );
    }
  });

  it("never renders a bare total when nothing was counted", () => {
    // The strip falls back to an em dash rather than printing 0 for a column
    // no repository has been scanned for...
    expect(source).toContain("locTotal.counted > 0");
    expect(source).toContain("storageTotal.counted > 0");
    expect(source).toContain("vulnTotal.counted > 0");
    expect(source).toContain("commitTotal.counted > 0");
    // ...and the strip itself, not the view, owns the coverage clause. Each
    // reading is handed its own tally rather than a finished string, so the
    // qualification cannot be dropped on the way in.
    expect(totalsSource).toContain("describeTally");
    expect(source).toContain("tally: commitTotal");
    expect(source).toContain("tally: storageTotal");
  });

  it("keeps the three cell states in one component, not spelled per column", () => {
    expect(cellSource).toContain('data-state="read"');
    expect(cellSource).toContain('data-state="unscanned"');
    expect(cellSource).toContain('data-state="failed"');
  });
});

describe("expensive scans stay opt-in", () => {
  it("never starts a family sweep from an effect", () => {
    // Storage walks 250,000 files and the audit spawns a package manager;
    // both run only from the toolbar, like autoRunCoverage elsewhere.
    const effects = source.match(/\$effect\(\(\) => \{[\s\S]*?\n  \}\);/g) ?? [];
    expect(effects.length).toBeGreaterThan(0);
    for (const effect of effects) {
      expect(effect).not.toContain("scanAll");
      expect(effect).not.toContain("scanOne");
    }
  });

  it("refreshes the cheap sweep only when the repository set changes", () => {
    expect(source).toContain("if (key === lastSwept) return;");
  });
});

describe("FleetView repository removal", () => {
  it("provides a remove button in each row and wires it to repoStore.removeRepo", () => {
    expect(source).toContain('data-testid="fleet-remove-repo"');
    expect(source).toContain("removeRow(row)");
    expect(source).toContain("repoStore.removeRepo(row.path)");
  });

  it("supports keyboard removal via Delete and Backspace keys on focused rows", () => {
    expect(source).toContain('event.key === "Delete" || event.key === "Backspace"');
    expect(source).toContain("removeRow(visibleRows[index])");
  });

  it("labels the remove button according to whether the repository is open or recent", () => {
    expect(source).toContain('title={row.presence === "open"');
    expect(source).toContain("Remove ${row.label} from Fleet (closes repository)");
    expect(source).toContain("Remove ${row.label} from Fleet");
    expect(source).toContain("aria-label={`Remove ${row.label} from Fleet`}");
  });

  it("includes an Actions column header with accessible screen reader text", () => {
    expect(source).toContain('<span class="sr-only">Actions</span>');
  });
});

describe("FleetSparkline", () => {
  const flat = new Array<number>(30).fill(0);

  it("draws nothing at all for an absent series", () => {
    // A flat row of baseline ticks is indistinguishable from a measured quiet
    // stretch; the caller renders "not scanned" through FleetCell instead.
    const { body } = render(FleetSparkline, {
      props: { counts: [], windowDays: 0, total: 0 },
    });
    expect(body).not.toContain('data-testid="fleet-sparkline"');
  });

  it("draws a measured silence, and names it as one", () => {
    const { body } = render(FleetSparkline, {
      props: { counts: flat, windowDays: 30, total: 0 },
    });
    expect(body).toContain('data-testid="fleet-sparkline"');
    expect(body).toContain("none in the last 30 days");
  });

  it("scales to its own peak and says what the peak was", () => {
    const counts = [...flat];
    counts[29] = 9;
    counts[0] = 3;
    const { body } = render(FleetSparkline, {
      props: { counts, windowDays: 30, total: 12 },
    });
    expect(body).toContain('data-peak="9"');
    expect(body).toContain("12 across 2 of the last 30 days");
  });

  it("says the counts are floors when the walk was capped", () => {
    const { body } = render(FleetSparkline, {
      props: { counts: [1, 2, 3], windowDays: 3, total: 6, partial: true },
    });
    expect(body).toContain("the history was capped");
  });
});

describe("FleetLanguageBar", () => {
  const stat = (language: string, percentage: number) => ({
    language,
    color_hex: "#123456",
    percentage,
  });

  it("draws nothing for a breakdown that is not on file", () => {
    const { body } = render(FleetLanguageBar, { props: { stats: [] } });
    expect(body).not.toContain('data-testid="fleet-language-bar"');
  });

  it("leaves the unaccounted remainder as visible track", () => {
    // Segments must never fill a bar they do not account for: a partial
    // reading has to look partial.
    const { body } = render(FleetLanguageBar, {
      props: { stats: [stat("Rust", 40), stat("Go", 20)] },
    });
    expect(body).toContain('data-accounted="60.0"');
    expect(body).toContain("40.0% unaccounted for");
  });

  it("says nothing about a remainder when the mix is complete", () => {
    const { body } = render(FleetLanguageBar, {
      props: { stats: [stat("Rust", 60), stat("Go", 40)] },
    });
    expect(body).not.toContain("unaccounted for");
  });
});

describe("FleetPulse", () => {
  const withCommits = (label: string, path: string, daily: number[]) =>
    row({
      path,
      label,
      commits: readCell(
        {
          windowDays: daily.length,
          commits: daily.reduce((sum, n) => sum + n, 0),
          authors: 1,
          activeDays: daily.filter((n) => n > 0).length,
          recent: daily.slice(-7).reduce((sum, n) => sum + n, 0),
          prior: 0,
          daily,
        },
        null,
      ),
    });

  const series = (last: number, days = 30) => {
    const daily = new Array<number>(days).fill(0);
    daily[days - 1] = last;
    return daily;
  };

  it("says why it is empty rather than drawing a flat line", () => {
    // A baseline over nothing reads as a calm workspace.
    const rows = [row({ commits: failedCell("git is not on PATH") })];
    const { body } = render(FleetPulsePanel, {
      props: { pulse: fleetPulse(rows), languages: fleetLanguageMix(rows) },
    });
    expect(body).toContain('data-testid="fleet-pulse-empty"');
    expect(body).toContain("no commit history could be read");
    expect(body).not.toContain('data-testid="fleet-sparkline"');
  });

  it("renders the summed series with its coverage clause", () => {
    const rows = [
      withCommits("a", "/a", series(4)),
      withCommits("b", "/b", series(1)),
      row({ path: "/c", label: "c", commits: UNSCANNED }),
    ];
    const { body } = render(FleetPulsePanel, {
      props: { pulse: fleetPulse(rows), languages: fleetLanguageMix(rows) },
    });
    expect(body).toContain('data-testid="fleet-sparkline"');
    expect(body).toContain("counted across 2 of 3");
    expect(body).toContain("1 not swept");
  });

  it("marks a capped history as a floor", () => {
    const rows = [
      row({
        commits: readCell(
          {
            windowDays: 30,
            commits: 99,
            authors: 1,
            activeDays: 1,
            recent: 99,
            prior: 0,
            daily: series(99),
          },
          null,
          true,
        ),
      }),
    ];
    const { body } = render(FleetPulsePanel, {
      props: { pulse: fleetPulse(rows), languages: fleetLanguageMix(rows) },
    });
    expect(body).toContain('data-testid="fleet-pulse-partial"');
  });

  it("never states a percentage against an empty prior period", () => {
    const rows = [withCommits("a", "/a", series(5))];
    const { body } = render(FleetPulsePanel, {
      props: { pulse: fleetPulse(rows), languages: fleetLanguageMix(rows) },
    });
    expect(body).toContain("after none in the 7 before");
    expect(body).not.toContain("+100%");
    expect(body).not.toContain("Infinity");
  });

  it("collapses to its header without losing the totals", () => {
    const rows = [withCommits("a", "/a", series(3))];
    const { body } = render(FleetPulsePanel, {
      props: { pulse: fleetPulse(rows), languages: fleetLanguageMix(rows), open: false },
    });
    expect(body).toContain("Fleet Pulse");
    // The header keeps the headline count even collapsed, so folding the panel
    // away never hides what it measured.
    expect(body).toContain("commits · last 30d");
    expect(body).not.toContain('data-testid="fleet-sparkline"');
  });

  it("points at the language scan rather than drawing a mix nobody measured", () => {
    const rows = [withCommits("a", "/a", series(3))];
    const { body } = render(FleetPulsePanel, {
      props: { pulse: fleetPulse(rows), languages: fleetLanguageMix(rows) },
    });
    expect(body).toContain('data-testid="fleet-languages-empty"');
    expect(body).not.toContain('data-testid="fleet-language-bar"');
  });

  it("labels summed author counts as slots, not as people", () => {
    // The sweep reports a per-repository count and never the identities, so
    // one person in three repositories counts three times. The label has to
    // say that rather than implying a headcount.
    const rows = [withCommits("a", "/a", series(3))];
    const { body } = render(FleetPulsePanel, {
      props: { pulse: fleetPulse(rows), languages: fleetLanguageMix(rows) },
    });
    expect(body).toContain("author slot");
  });
});

describe("the Fleet grid orders and filters honestly", () => {
  it("routes ordering through the module that keeps absences out of it", () => {
    // The comparator lives in `fleet/sort` precisely so `value ?? 0` — which
    // files every unaudited repository next to the clean ones — cannot be
    // reintroduced inline here.
    expect(source).toContain('from "../fleet/sort"');
    expect(source).toContain("sortRows(matched, sort)");
    expect(source).not.toMatch(/\.value\s*\?\?\s*0/);
  });

  it("keeps the default order identical to byUrgency", () => {
    expect(source).toContain("byUrgency(matched)");
  });

  it("filters with the substring search, never a user-supplied pattern", () => {
    expect(source).toContain("searchRows(applyFilter(rows, filter), query)");
    expect(source).not.toContain("new RegExp(query");
  });

  it("marks the sorted column for assistive technology", () => {
    expect(source).toContain("aria-sort={ariaSort(column.key)}");
  });
});

describe("the Fleet Pulse panel is wired to the grid", () => {
  it("renders above the toolbar and reads the same rows the grid does", () => {
    expect(source).toContain("<FleetPulsePanel");
    expect(source).toContain("fleetPulse(rows)");
    expect(source).toContain("fleetLanguageMix(rows)");
  });

  it("remembers whether it is open, like the Fleet surface itself", () => {
    expect(source).toContain("$interfaceStore.fleetPulseOpen");
    expect(source).toContain("interfaceStore.toggleFleetPulse()");
  });

  it("never computes a commit total without its coverage clause", () => {
    expect(source).toContain("commitTotal.counted > 0");
  });
});

describe("per-repository scans stay opt-in and open-only", () => {
  it("wires the absent cells to scanOne rather than to a sweep", () => {
    expect(source).toContain("fleetStore.scanOne(family, row.path)");
    expect(source).toContain('onScan={scanCell(row, "loc")}');
    expect(source).toContain('onScan={scanCell(row, "storage")}');
    expect(source).toContain('onScan={scanCell(row, "health")}');
    expect(source).toContain('onScan={scanCell(row, "coverage")}');
  });

  it("refuses to scan a repository that is not open", () => {
    // Same rule `scanTargets` enforces for the sweeps: a recents path may not
    // resolve any more, and scanning it spawns work in a repository the user
    // has not opened.
    expect(source).toContain('if (row.presence !== "open") return undefined;');
  });

  it("refuses to start a second scan while a sweep is running", () => {
    expect(source).toContain("if ($fleetStore.scanning !== null) return undefined;");
  });
});
