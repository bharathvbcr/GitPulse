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

  it("offers keyboard stepping that does not fight the diff's own scrolling", () => {
    expect(source).toContain("event.altKey");
    expect(source).toContain("ArrowDown");
    // Bare arrows scroll the diff, so they must not be captured.
    expect(source).not.toContain('event.key === "ArrowDown" && !event.altKey');
  });

  it("leaves typing targets alone", () => {
    expect(source).toContain('tag === "INPUT"');
    expect(source).toContain("isContentEditable");
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
});
