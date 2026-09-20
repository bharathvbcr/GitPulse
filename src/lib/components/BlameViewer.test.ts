import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));

/**
 * Component behavior is pinned at the source level (house convention: the
 * vitest environment is node, so nothing mounts here). These assertions lock
 * the blame page's audit fixes in place: the explorer wiring, single-load-site
 * selection write-back, the zero-OID guard, coverage-failure surfacing, and
 * the freshness fingerprint.
 */
const source = readFileSync(join(here, "BlameViewer.svelte"), "utf8");

describe("BlameViewer file explorer integration", () => {
  it("mounts the explorer beside the blame pane with a toggle and unified w-72 styling", () => {
    expect(source).toContain('import FileTreePanel from "./files/FileTreePanel.svelte"');
    expect(source).toContain("{#if explorerOpen}");
    expect(source).toContain("<FileTreePanel />");
    expect(source).toContain("explorerOpen = !explorerOpen");
    expect(source).toContain('class="w-72 shrink-0 h-full overflow-hidden"');
    expect(source).toContain('key.toLowerCase() === "b"');
  });

  it("reads the selection instead of offering a second way to name a file", () => {
    // The path box is gone. It existed because Blame was a destination you
    // could arrive at with nothing selected; Blame is a section of Code now,
    // and Explorer — one click away, sharing `selectedFilePath` — is where a
    // file is chosen. A second picker here would be a second source of truth
    // for one subject, and the one that could disagree.
    expect(source).not.toContain("bind:value={filePath}");
    expect(source).not.toContain('placeholder="Path to file in repo..."');
    expect(source).not.toContain("repoStore.selectFilePath(");
    // The selection is still what drives the load, and it is still the only
    // thing that does.
    expect(source).toContain("const selected = $repoStore.selectedFilePath;");
  });

  it("keeps a way to ask for the same file again after a failure", () => {
    // Retry used to mean re-typing the path and pressing Enter — deleting the
    // box without replacing that would have removed the only recovery from a
    // failed blame. The store does not notify for an unchanged value, so the
    // retry calls the loader directly.
    expect(source).toContain("function retryBlame()");
    expect(source).toContain("void loadBlameFor(repo, path);");
    expect(source).toContain("onclick={retryBlame}");
  });
});

describe("BlameViewer audit fixes", () => {
  it("renders uncommitted (zero-OID) lines as plain text, not commit links", () => {
    // The zero-OID predicate moved to the timeline module, which has to make
    // the same judgement to keep worktree lines off a commit-date axis. Two
    // copies of that regex is how one of them comes to miss the 64-character
    // SHA-256 form; `blameTimeline.test.ts` pins both lengths at the owner.
    expect(source).toContain("isUncommittedLine(line)");
    expect(source).toContain('from "../metrics/blameTimeline"');
    expect(source).not.toContain("const ZERO_OID_RE");
    // The guarded branch renders a span, not a button that would inspect a
    // nonexistent commit.
    expect(source).toContain('title="Not committed yet"');
  });

  it("surfaces coverage fetch failure instead of swallowing it", () => {
    expect(source).toContain("coverageFailed");
    expect(source).not.toContain(".catch(() => new Map<number, number>())");
    expect(source).toContain("Coverage unavailable");
  });

  it("re-blames when blame inputs move, memoized on a fingerprint", () => {
    // Status of the blamed file + current branch tip participate in the key,
    // so watcher refreshes after external edits/commits refresh stale blame.
    expect(source).toContain("s.path === selected");
    expect(source).toContain("tip_commit_id");
    expect(source).toContain("if (key === prevKey) return;");
  });

  it("sizes its rows from the density owner instead of a fixed class", () => {
    // The rows were `h-6` while VirtualList positioned them from
    // rowHeight("blame", density): at Compact the 24px row overhung its 20px
    // slot and drew over its neighbour. The row's height and the windowing
    // math have to come from the same number.
    expect(source).toContain('const ROW = $derived(rowHeight("blame", $densityStore));');
    expect(source).toContain('style:height={`${ROW}px`}');
    expect(source).toContain("rowHeight={ROW}");
    expect(source).not.toMatch(/class="flex items-center h-6/);
  });

  it("gives long source lines somewhere to go", () => {
    // Blame rows are code. `overflow-hidden` per row clipped a long line at
    // the pane edge with no scrollbar to reach the rest of it.
    expect(source).toContain("contentWidth");
    expect(source).not.toContain("whitespace-pre overflow-hidden");
    // ...but not the overflow arrow: this pane has a real scrollbar, which is
    // the rule ScrollCue.surfaces.test.ts states for source panes.
    expect(source).not.toContain("scrollCue");
  });

  it("keeps every rejection routed through the diagnostics reporting seam", () => {
    // reportPanelError formats via formatError AND lands the failure in the
    // persistent diagnostics ring; the banner text contract is unchanged.
    expect(source).toContain('from "../diagnostics/report"');
    expect(source).toContain('reportPanelError("blame", err)');
    expect(source).not.toMatch(/String\(\s*(err|reason|error|e)\s*\)/);
  });
});

describe("BlameViewer code-age timeline", () => {
  it("derives the axis, the row tint and the filter from one owner", () => {
    // The page used to carry its own `getHeatmapColor` beside a hand-listed
    // legend. A colour function and a legend spelled separately is how a
    // swatch comes to name a band the rows no longer use.
    expect(source).toContain("buildBlameTimeline(blameLines, blameNow)");
    expect(source).toContain("blameRowTint(line, timeline.nowMs)");
    // The legend is the timeline's own band shares, not a second hand-written
    // list of the scale beside it.
    expect(source).toContain("{#each timeline.bands as share (share.band.id)}");
    expect(source).not.toContain("function getHeatmapColor");
    expect(source).not.toContain("rgba(239, 68, 68");
  });

  it("measures the whole file against one captured instant", () => {
    // Reading the clock inside the derived timeline would let a line drift
    // across a band boundary between the column that counted it and the
    // filter that selects it.
    expect(source).toContain("blameNow = Date.now();");
    expect(source).not.toMatch(/buildBlameTimeline\([^)]*Date\.now\(\)/);
  });

  it("filters the rows through the same classifier the columns counted with", () => {
    // One classifier for all three ways in — a column, a chip, a legend band.
    expect(source).toContain("selectionMatches(line, active, timeline)");
    // A selection this file does not have shows the whole file rather than an
    // empty pane, which would read as "this file has no lines".
    expect(source).toContain("if (active === null || activeLabel === null) return blameLines;");
  });

  it("drops a period selection when the subject changes", () => {
    // A period picked in the file being replaced says nothing about the one
    // arriving; carrying it over would open the new file pre-filtered. Load,
    // failure and cleared-selection all have to reset it.
    const resets = source.match(/selection = null;/g) ?? [];
    expect(resets.length).toBeGreaterThanOrEqual(3);
  });

  it("refuses to select a period that holds no lines", () => {
    expect(source).toContain("if (period.lines === 0) return;");
    expect(source).toContain("aria-disabled={period.lines === 0}");
  });

  it("announces the timeline as a pressed-button group", () => {
    expect(source).toContain('role="group"');
    expect(source).toContain('aria-pressed={isSelected({ kind: "bucket", key: period.key })}');
    expect(source).toContain("aria-label={describePeriod(period)}");
    expect(source).toContain('aria-pressed={isSelected({ kind: "bucket", key: extra.key })}');
    expect(source).toContain('aria-pressed={isSelected({ kind: "band", id: share.band.id })}');
    expect(source).toContain('aria-label="Filter by code age"');
  });

  it("keeps the off-axis lines reachable rather than silently excluded", () => {
    // Worktree-only, clock-skewed and undated lines are counted in the same
    // denominator as the columns, so they have to be visible and selectable.
    expect(source).toContain("{#each timeline.extras as extra (extra.key)}");
    expect(source).toContain("title={extra.explanation}");
    expect(source).toContain("No dated lines to place on a timeline.");
  });

  it("states the filtered count against the file total", () => {
    expect(source).toContain(
      'Showing {plural(visibleLines.length, "line")} of {timeline.totalLines}',
    );
  });

  it("groups a commit's consecutive lines without hiding the way back to it", () => {
    expect(source).toContain("visibleLines[index - 1]?.commit_id === line.commit_id");
    // The hash is still rendered for a grouped line; it is only quiet until
    // hovered or focused.
    expect(source).toContain("opacity-0 hover:opacity-100 focus-visible:opacity-100");
    expect(source).toContain("hoverCommit === line.commit_id");
  });

  it("maps the file's age onto a rail, projected from the list on screen", () => {
    // Under a filter the drawn list and the file are different lists, and a
    // map of the other one points every mark at a row that is not there.
    expect(source).toContain("buildAgeTicks(visibleLines, timeline.nowMs)");
    expect(source).toContain("viewportBand(listScroll, railHeight, visibleLines.length, ROW)");
    expect(source).toContain("data-blame-rail");
  });

  it("borrows the diff map's geometry rather than deriving a second copy", () => {
    // scrollForRatio CENTRES the target: a top-aligned jump clamps the last
    // mark off screen, which is the mistake documented at that owner.
    expect(source).toContain('from "../diff/minimap"');
    expect(source).toContain("scrollForRatio(ratio, visibleLines.length, ROW, railHeight)");
    expect(source).toContain("ratioFromPointer(event.clientY, rect.top, rect.height)");
  });

  it("keeps the rail's idea of the viewport equal to the list's", () => {
    // The rail measures its own height and calls it the list's viewport.
    // Vertical padding inside the scroller would make the two disagree by
    // exactly that much, and the band would drift down the rail.
    expect(source).toContain("bind:clientHeight={railHeight}");
    expect(source).toContain("bind:scrollTop={listScroll}");
    expect(source).not.toMatch(/class="h-full px-1\.5 py-1"/);
  });

  it("makes the legend carry the distribution and select on it", () => {
    // Four static swatches restating a scale already visible in the rows were
    // paying no rent.
    expect(source).toContain("selectBand(share)");
    expect(source).toContain("formatShare(share.percent)");
    expect(source).toContain("if (share.lines === 0) return;");
  });

  it("shows each line's age in the reader's chosen timestamp style", () => {
    // FEATURES has claimed a relative timestamp in this gutter for as long as
    // the page has existed; it was never drawn. Honouring the shared
    // preference is what keeps it from becoming a third timestamp dialect.
    expect(source).toContain('from "../ui/timestampFormat"');
    expect(source).toContain("$timestampFormat.text(line.timestamp)");
    expect(source).toContain("$timestampFormat.title(line.timestamp)");
  });
});
