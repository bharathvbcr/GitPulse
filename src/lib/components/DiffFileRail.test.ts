import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { createRawSnippet } from "svelte";
import { render } from "svelte/server";
import DiffFileRail from "./DiffFileRail.svelte";
import DiffHistoryGraph from "./DiffHistoryGraph.svelte";
import { buildFileRail, type RailInput } from "../diff/fileRail";
import type { VisualCommitRow } from "../canvas/GraphRenderer";

function rail(over: Partial<RailInput> = {}) {
  return buildFileRail({
    selectionKind: "commit",
    commitFiles: [
      { path: "src/a.ts", status_code: "M", additions: 10, deletions: 2 },
      { path: "src/b.ts", status_code: "A", additions: 40, deletions: 0 },
    ],
    commitFilesTruncated: false,
    commitFilesTotal: 0,
    statuses: [],
    ...over,
  });
}

const props = (over = {}) => ({
  rail: rail(),
  currentPath: "src/a.ts",
  currentIsStaged: false,
  changeLabel: "Add the parser",
  onOpen: () => {},
  onCollapse: () => {},
  ...over,
});

/** A stand-in History body, so the rail's tab plumbing is tested on its own. */
const historyBody = createRawSnippet(() => ({
  render: () => `<div data-test-history>graph body</div>`,
}));

describe("DiffFileRail", () => {
  it("lists every file with its churn", () => {
    const { body } = render(DiffFileRail, { props: props() });
    expect(body).toContain("a.ts");
    expect(body).toContain("b.ts");
    expect(body).toContain("+10 −2");
  });

  it("renders shared preview markers without a second query path", () => {
    const markers = new Map([
      [
        "src/a.ts",
        {
          kind: "breaks" as const,
          label: "3",
          title: "3 broken callers",
          brokenTotal: 3,
        },
      ],
    ]);
    const { body } = render(DiffFileRail, {
      props: props({ previewMarkers: markers }),
    });
    expect(body).toContain('data-preview-marker="breaks"');
    expect(body).toContain(">3<");
  });

  it("marks the file currently on screen", () => {
    const { body } = render(DiffFileRail, { props: props() });
    expect(body).toContain('aria-current="true"');
  });

  it("says when the list was cut short, rather than passing it off as whole", () => {
    const { body } = render(DiffFileRail, {
      props: props({ rail: rail({ commitFilesTruncated: true, commitFilesTotal: 312 }) }),
    });
    expect(body).toContain("showing 2 of 312 files");
  });

  it("adds no truncation chrome when the list is complete", () => {
    expect(render(DiffFileRail, { props: props() }).body).not.toContain("showing");
  });

  it("distinguishes the staged side of a worktree file", () => {
    const worktreeRail = buildFileRail({
      selectionKind: "file",
      commitFiles: null,
      commitFilesTruncated: false,
      commitFilesTotal: 0,
      statuses: [
        { path: "a.ts", status_code: "M", is_staged: true, additions: 1, deletions: 0 },
        { path: "a.ts", status_code: "M", is_staged: false, additions: 2, deletions: 0 },
      ],
    });
    const { body } = render(DiffFileRail, { props: props({ rail: worktreeRail }) });
    expect(body).toContain("staged");
  });

  it("escapes a hostile path rather than emitting it as markup", () => {
    const nasty = buildFileRail({
      selectionKind: "commit",
      commitFiles: [
        { path: "<script>alert(1)</script>.ts", status_code: "M", additions: 0, deletions: 0 },
      ],
      commitFilesTruncated: false,
      commitFilesTotal: 0,
      statuses: [],
    });
    const { body } = render(DiffFileRail, { props: props({ rail: nasty }) });
    expect(body).not.toContain("<script>alert(1)</script>");
  });
});

describe("DiffFileRail tells its files apart", () => {
  const sameName = buildFileRail({
    selectionKind: "commit",
    commitFiles: [
      { path: "src/analyzer/mod.rs", status_code: "M", additions: 1, deletions: 0 },
      { path: "src/codeintel/mod.rs", status_code: "M", additions: 2, deletions: 0 },
      { path: "docs/README.md", status_code: "M", additions: 3, deletions: 0 },
    ],
    commitFilesTruncated: false,
    commitFilesTotal: 0,
    statuses: [],
  });

  it("qualifies rows whose basenames collide", () => {
    // The regression: this repository's own head commit listed `mod.rs` eight
    // times and `plugin.json` three times, all identical, in a 224px column.
    const { body } = render(DiffFileRail, {
      props: props({ rail: sameName, currentPath: "src/analyzer/mod.rs" }),
    });
    expect(body).toContain("analyzer/");
    expect(body).toContain("codeintel/");
  });

  it("leaves a unique basename alone rather than qualifying everything", () => {
    const { body } = render(DiffFileRail, { props: props({ rail: sameName }) });
    // Scoped to the dimmed prefix span: the full path is still in the row's
    // title attribute, which is where it belongs.
    const prefixes = [...body.matchAll(/opacity-55[^>]*>([^<]*)</g)].map((m) => m[1]);
    expect(prefixes).toContain("analyzer/");
    expect(prefixes).toContain("codeintel/");
    expect(prefixes).not.toContain("docs/");
  });

  it("keeps the full path available as the row's tooltip", () => {
    const { body } = render(DiffFileRail, { props: props({ rail: sameName }) });
    expect(body).toContain('title="src/analyzer/mod.rs"');
  });

  it("offers a filter and a tree, because a two-hundred-file list needs both", () => {
    const { body } = render(DiffFileRail, { props: props({ rail: sameName }) });
    expect(body).toContain('aria-label="Filter files in this diff"');
    expect(body).toContain('aria-label="File list layout"');
  });

  it("can be resized, so a fixed column is not a fixed cost", () => {
    const { body } = render(DiffFileRail, {
      props: props({ rail: sameName, onResize: () => {} }),
    });
    expect(body).toContain('aria-label="Resize the file list"');
    expect(body).toContain('aria-valuenow');
  });

  it("says so when the change has no files at all", () => {
    const empty = buildFileRail({
      selectionKind: "commit",
      commitFiles: [],
      commitFilesTruncated: false,
      commitFilesTotal: 0,
      statuses: [],
    });
    const { body } = render(DiffFileRail, { props: props({ rail: empty }) });
    expect(body).toContain("No files in this change.");
  });
});

describe("DiffViewer wiring", () => {
  const source = readFileSync(new URL("./DiffViewer.svelte", import.meta.url), "utf8");

  it("carries its own file list so reading a second file is not a round trip", () => {
    // The regression: a commit's files are listed by CommitDetails, which
    // lives only in the Graph view, so opening one switched views and the
    // list vanished with the view that owned it.
    expect(source).toContain("DiffFileRail");
    expect(source).toContain("buildFileRail");
  });

  it("does not depend on the Graph view having run first", () => {
    // A restored session opens straight onto a persisted commit selection
    // with the graph store empty; the rail must fetch the list itself.
    expect(source).toContain("cmd_get_commit_files");
  });

  it("owns the rail's tab so it survives a file switch", () => {
    expect(source).toContain("bind:tab={railTab}");
  });

  it("reads a range selection's kind from the store, not from which ids are set", () => {
    // The regression: `commitId ? "commit" : filePath ? "file" : "range"` read
    // a range selection (filePath `from...to`, no commit) as a worktree file,
    // and offered the working tree's files beside a branch comparison.
    expect(source).toContain("selectionKind: $repoStore.selectionKind,");
    expect(source).not.toMatch(/selectedFilePath\s*\?\s*"file"\s*:\s*"range"/);
  });

  it("lists a comparison's files from the comparison, not from either commit", () => {
    expect(source).toContain('"cmd_get_compare_files"');
    expect(source).toContain("selectCompareFileDiff(compareBase, commitId, entry.path)");
  });

  it("toggles the History tab with ⌥H by physical key", () => {
    // Option+H types "˙" on macOS; matching `event.key` would never fire there.
    expect(source).toContain('event.code === "KeyH"');
  });
});

describe("DiffFileRail tabs", () => {
  it("offers Files and History when a history body is given", () => {
    const { body } = render(DiffFileRail, { props: props({ history: historyBody }) });
    expect(body).toContain('role="tablist"');
    expect(body).toContain("Files");
    expect(body).toContain("History");
    // Files is the default: the file list shows, the history body does not.
    expect(body).toContain('aria-label="Filter files in this diff"');
    expect(body).not.toContain("data-test-history");
  });

  it("gives the History tab the whole column, with no file list beside it", () => {
    // Two stacked scrollers in one narrow column fought over every wheel
    // event; one tab owns the height at a time.
    const { body } = render(DiffFileRail, { props: props({ history: historyBody, tab: "history" }) });
    expect(body).toContain("data-test-history");
    expect(body).not.toContain('aria-label="Filter files in this diff"');
  });

  it("offers no tabs when there is no history to show", () => {
    const { body } = render(DiffFileRail, { props: props({ tab: "history" }) });
    expect(body).not.toContain('role="tablist"');
    expect(body).toContain('aria-label="Filter files in this diff"');
  });

  it("names the change on screen from either tab, so neither has to be opened", () => {
    for (const tab of ["files", "history"] as const) {
      const { body } = render(DiffFileRail, { props: props({ history: historyBody, tab }) });
      expect(body, tab).toContain("Add the parser");
    }
  });

  it("names a comparison and offers the way out of it", () => {
    const { body } = render(DiffFileRail, {
      props: props({ comparing: "abc1234..def5678", onExitCompare: () => {} }),
    });
    expect(body).toContain("abc1234..def5678");
    expect(body).toContain("data-exit-compare");
  });

  it("heads a comparison's file list as compared files", () => {
    const compared = buildFileRail({
      selectionKind: "compare",
      commitFiles: [{ path: "src/a.ts", status_code: "M", additions: 1, deletions: 1 }],
      commitFilesTruncated: false,
      commitFilesTotal: 0,
      statuses: [],
    });
    expect(render(DiffFileRail, { props: props({ rail: compared }) }).body).toContain("Compared files");
  });
});

const graphRow = (id: string, over: Partial<VisualCommitRow> = {}): VisualCommitRow => ({
  id,
  parent_ids: [],
  summary: `summary ${id}`,
  author_name: "Ada",
  author_email: "ada@example.com",
  timestamp: 1_700_000_000,
  lane: 0,
  color_index: 0,
  active_lanes: [0],
  active_lane_colors: [0],
  connections: [],
  is_merge: false,
  is_root: false,
  ...over,
});

const historyRows = [
  graphRow("a1b2c3d4e5f6", { summary: "Add the parser" }),
  graphRow("b2c3d4e5f6a1", { summary: "", author_name: "Bob", is_merge: true }),
];

const historyProps = (over = {}) => ({
  rows: historyRows,
  refsByCommit: new Map([["a1b2c3d4e5f6", [{ name: "main", kind: "current-branch" as const }]]]),
  closeTargets: new Map(),
  authorCounts: new Map(),
  selectedCommitId: "a1b2c3d4e5f6",
  workingTreeCount: 3,
  width: 248,
  hover: null,
  freshness: {},
  onHover: () => {},
  onVisible: () => {},
  onLoadMore: () => {},
  onOpenCommit: () => {},
  onCompare: () => {},
  onExitCompare: () => {},
  onPickWorkingTree: () => {},
  ...over,
});

describe("DiffHistoryGraph", () => {
  it("lists the graph's commits so switching change needs no trip to Graph", () => {
    const { body } = render(DiffHistoryGraph, { props: historyProps() });
    expect(body).toContain("Add the parser");
    expect(body).toContain("a1b2c3d");
    expect(body).toContain("Ada");
    // The branch that points at a commit is on its row.
    expect(body).toContain(">main<");
  });

  it("marks the commit currently on screen", () => {
    const { body } = render(DiffHistoryGraph, { props: historyProps() });
    expect(body).toMatch(/data-commit-id="a1b2c3d4e5f6"[^>]*aria-selected="true"|aria-selected="true"[^>]*data-commit-id="a1b2c3d4e5f6"/);
  });

  it("offers uncommitted work as a first-class entry", () => {
    // It is the one thing a reader returns to most, and the only entry that
    // is not in the graph at all.
    const { body } = render(DiffHistoryGraph, { props: historyProps({ selectedCommitId: null }) });
    expect(body).toContain("Uncommitted changes");
    expect(body).toContain('aria-current="true"');
  });

  it("says a clean tree is clean rather than showing a zero", () => {
    expect(render(DiffHistoryGraph, { props: historyProps({ workingTreeCount: 0 }) }).body).toContain("clean");
  });

  it("shows neither count nor 'clean' when the tree was not counted", () => {
    // -1 is "not counted"; rendering it as clean would assert something the
    // app does not know.
    const { body } = render(DiffHistoryGraph, { props: historyProps({ workingTreeCount: -1 }) });
    const entry = body.slice(body.indexOf("Uncommitted changes"), body.indexOf("Uncommitted changes") + 200);
    expect(entry).not.toContain("clean");
  });

  it("names an empty commit message instead of rendering a blank row", () => {
    expect(render(DiffHistoryGraph, { props: historyProps() }).body).toContain("(no commit message)");
  });

  it("says a merge is a merge in words, not only as a canvas shape", () => {
    expect(render(DiffHistoryGraph, { props: historyProps() }).body).toContain("merge commit");
  });

  it("marks the comparison's base on its row", () => {
    const { body } = render(DiffHistoryGraph, { props: historyProps({ compareBase: "b2c3d4e5f6a1" }) });
    expect(body).toContain(">base<");
  });

  it("says where the loaded history ends, and offers Retry when older history failed", () => {
    expect(render(DiffHistoryGraph, { props: historyProps() }).body).toContain("Start of the loaded history");
    const failed = render(DiffHistoryGraph, {
      props: historyProps({ hasMore: true, loadError: "git exploded" }),
    }).body;
    expect(failed).toContain("Could not load older history: git exploded");
    expect(failed).toContain("Retry");
  });

  it("is one listbox the keyboard can drive", () => {
    const { body } = render(DiffHistoryGraph, { props: historyProps() });
    expect(body).toContain('role="listbox"');
    expect(body).toContain('role="option"');
  });
});

describe("DiffFileRail resize handle", () => {
  const source = readFileSync(new URL("./DiffFileRail.svelte", import.meta.url), "utf8");

  it("is a real window splitter, not a pointer-only grab strip", () => {
    // A drag handle with no keyboard path is unreachable without a mouse, and
    // one that does not report its bounds cannot be read by a screen reader.
    const { body } = render(DiffFileRail, { props: props({ onResize: () => {} }) });
    expect(body).toContain('role="separator"');
    expect(body).toContain('aria-orientation="vertical"');
    expect(body).toContain('tabindex="0"');
    expect(body).toMatch(/aria-valuenow="\d+"/);
    expect(body).toContain('aria-valuemin="180"');
    expect(body).toContain('aria-valuemax="520"');
  });

  it("draws no handle at all when the caller cannot resize", () => {
    expect(render(DiffFileRail, { props: props() }).body).not.toContain('role="separator"');
  });

  it("answers to every key the splitter pattern defines", () => {
    // Arrows step; Home/End jump to narrowest and widest. Stepping alone
    // makes "as narrow as it goes" a dozen keypresses.
    for (const key of ["ArrowLeft", "ArrowRight", "Home", "End"]) {
      expect(source, key).toContain(`event.key === "${key}"`);
    }
  });

  it("clamps every keyboard move to the handle's own bounds", () => {
    // The reported aria-valuemin/max and the values the keys produce have to
    // be the same two numbers, or the control lies about its range.
    expect(source).toContain("Math.max(MIN_WIDTH, width - step)");
    expect(source).toContain("Math.min(MAX_WIDTH, width + step)");
    expect(source).toContain("onResize(MIN_WIDTH)");
    expect(source).toContain("onResize(MAX_WIDTH)");
    expect(source).toContain("Math.max(MIN_WIDTH, Math.min(MAX_WIDTH, next))");
  });
});
