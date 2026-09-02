import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { render } from "svelte/server";
import DiffFileRail from "./DiffFileRail.svelte";
import { buildFileRail, type RailInput } from "../diff/fileRail";

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
  onOpen: () => {},
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
