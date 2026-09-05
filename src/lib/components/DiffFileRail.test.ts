import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { render } from "svelte/server";
import DiffFileRail from "./DiffFileRail.svelte";
import { buildFileRail, type RailInput } from "../diff/fileRail";
import { buildCommitRail } from "../diff/commitRail";

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

const commitRows = [
  { id: "a1b2c3d4e5f6", summary: "Add the parser", author_name: "Ada", timestamp: 1_700_000_000, is_merge: false },
  { id: "b2c3d4e5f6a1", summary: "", author_name: "Bob", timestamp: 1_699_900_000, is_merge: true },
];

const props = (over = {}) => ({
  rail: rail(),
  commits: buildCommitRail(commitRows),
  currentPath: "src/a.ts",
  currentIsStaged: false,
  selectedCommitId: "a1b2c3d4e5f6",
  workingTreeCount: 3,
  onOpen: () => {},
  onPickCommit: () => {},
  onPickWorkingTree: () => {},
  onCollapse: () => {},
  // The picker folds by default; these render it to assert what it says.
  commitsOpen: true,
  ...over,
});

describe("DiffFileRail", () => {
  it("lists every file with its churn", () => {
    const { body } = render(DiffFileRail, { props: props() });
    expect(body).toContain("a.ts");
    expect(body).toContain("b.ts");
    expect(body).toContain("+10 −2");
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

  it("owns the picker's fold so it survives a file switch", () => {
    expect(source).toContain("bind:commitsOpen");
  });
});

describe("DiffFileRail commit picker", () => {
  it("lists recent commits so switching change needs no trip to Graph", () => {
    const { body } = render(DiffFileRail, { props: props() });
    expect(body).toContain("Add the parser");
    expect(body).toContain("a1b2c3d");
    expect(body).toContain("Ada");
  });

  it("marks the commit currently on screen", () => {
    const { body } = render(DiffFileRail, { props: props() });
    // Both the open commit and the open file are marked.
    expect((body.match(/aria-current="true"/g) ?? []).length).toBeGreaterThanOrEqual(2);
  });

  it("offers uncommitted work as a first-class entry", () => {
    // It is the one thing a reader returns to most, and the only entry that
    // is not in the graph at all.
    const { body } = render(DiffFileRail, { props: props() });
    expect(body).toContain("Uncommitted changes");
  });

  it("says a clean tree is clean rather than showing a zero", () => {
    expect(render(DiffFileRail, { props: props({ workingTreeCount: 0 }) }).body).toContain("clean");
  });

  it("shows neither count nor 'clean' when the tree was not counted", () => {
    // -1 is "not counted"; rendering it as clean would assert something the
    // app does not know.
    const { body } = render(DiffFileRail, { props: props({ workingTreeCount: -1 }) });
    const picker = body.slice(body.indexOf("Uncommitted changes"), body.indexOf("Uncommitted changes") + 200);
    expect(picker).not.toContain("clean");
  });

  it("names an empty commit message instead of rendering a blank row", () => {
    expect(render(DiffFileRail, { props: props() }).body).toContain("(no commit message)");
  });

  it("marks a merge commit as one", () => {
    expect(render(DiffFileRail, { props: props() }).body).toContain("merge");
  });

  it("names the open change in its header, so folding it loses nothing", () => {
    // Folded is the default; the header has to answer "which change" without
    // being opened.
    const { body } = render(DiffFileRail, { props: props({ commitsOpen: false }) });
    expect(body).toContain("Add the parser");
    expect(body).not.toContain("(no commit message)");
  });

  it("names uncommitted work in that header when no commit is selected", () => {
    const { body } = render(DiffFileRail, {
      props: props({ commitsOpen: false, selectedCommitId: null }),
    });
    expect(body).toContain("Uncommitted changes");
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
