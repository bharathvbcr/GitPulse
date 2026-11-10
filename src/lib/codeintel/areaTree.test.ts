import { describe, expect, it } from "vitest";
import { eachKeysAreUnique } from "../ui/eachKeys";
import { buildAreaTree, normalizeArea, summaryWithoutArea } from "./areaTree";
import type { RepoMapSubsystem } from "./types";

function sub(area: string, summary = `${area}: 1 file, rust`): RepoMapSubsystem {
  return {
    area, summary, entry_points: [], critical_files: [], neighbors: [], handoff_paths: [],
    role_files: {}, role_file_counts: {},
  };
}

const outline = (subs: RepoMapSubsystem[]): string[] =>
  buildAreaTree(subs).map((r) => `${"  ".repeat(r.depth)}${r.label}${r.subsystemIndex === null ? " [group]" : ""}`);

// The 14 areas of GitPulse's own .devcouncil/repo_map.json, in producer order.
const REAL_AREAS = [
  "harness",
  "crates/gitpulse-worktree/src/bin",
  "crates/gitpulse-worktree/tests",
  "src-tauri/framework/tao/examples",
  "src-tauri/framework/muda/src",
  "src-tauri/benches",
  "src-tauri",
  "src-tauri/src",
  "scripts",
  "src-tauri/src/commands",
  "src-tauri/framework/tauri/mobile/android/src/main/java/app/tauri",
  "src-tauri/src/codeintel",
  "src-tauri/vendored/dc-store/src/workbench",
  "src-tauri/src/engine",
];

describe("buildAreaTree", () => {
  it("pins the tree for the real repo map", () => {
    const subs = REAL_AREAS.map((a) => sub(a));
    expect(outline(subs)).toEqual([
      "crates/gitpulse-worktree [group]",
      "  src/bin",
      "  tests",
      "harness",
      "scripts",
      "src-tauri",
      "  benches",
      "  framework [group]",
      "    muda/src",
      "    tao/examples",
      "    tauri/mobile/android/src/main/java/app/tauri",
      "  src",
      "    codeintel",
      "    commands",
      "    engine",
      "  vendored/dc-store/src/workbench",
    ]);
    const rows = buildAreaTree(subs);
    expect(eachKeysAreUnique(rows, (r) => r.key)).toBe(true);
    // Every subsystem is reachable exactly once, by its producer index.
    const indices = rows.flatMap((r) => (r.subsystemIndex === null ? [] : [r.subsystemIndex]));
    expect(indices.sort((a, b) => a - b)).toEqual(subs.map((_, i) => i));
  });

  it("links parents, group rows and hasChildren", () => {
    const rows = buildAreaTree(REAL_AREAS.map((a) => sub(a)));
    const byLabel = new Map(rows.map((r) => [r.area, r]));
    const tauri = byLabel.get("src-tauri");
    const src = byLabel.get("src-tauri/src");
    const group = byLabel.get("src-tauri/framework");
    expect(tauri?.parentKey).toBeNull();
    expect(tauri?.hasChildren).toBe(true);
    expect(src?.parentKey).toBe(tauri?.key);
    expect(group?.subsystemIndex).toBeNull();
    expect(group?.duplicateCount).toBe(0);
    expect(group?.parentKey).toBe(tauri?.key);
    expect(byLabel.get("src-tauri/src/commands")?.parentKey).toBe(src?.key);
    expect(byLabel.get("src-tauri/src/commands")?.hasChildren).toBe(false);
    expect(byLabel.get("harness")?.hasChildren).toBe(false);
  });

  it("nests children that precede their parent in the input", () => {
    expect(outline([sub("a/b/c"), sub("a/b"), sub("a")])).toEqual(["a", "  b", "    c"]);
  });

  it("keeps colliding basenames apart by path", () => {
    const rows = buildAreaTree([sub("x/src"), sub("y/src")]);
    expect(rows.map((r) => r.label)).toEqual(["x/src", "y/src"]);
    expect(eachKeysAreUnique(rows, (r) => r.key)).toBe(true);
  });

  it("merges exact duplicates: first index wins, count carried", () => {
    const rows = buildAreaTree([sub("a"), sub("b"), sub("./a/"), sub("a")]);
    expect(rows.map((r) => [r.label, r.subsystemIndex, r.duplicateCount])).toEqual([
      ["a", 0, 3],
      ["b", 1, 1],
    ]);
    expect(rows[0].area).toBe("a");
  });

  it("normalizes ./, trailing slash, backslashes and doubled slashes", () => {
    expect(normalizeArea("./src\\lib//")).toBe("src/lib");
    expect(normalizeArea("  ./ ")).toBe("");
    expect(outline([sub("src\\lib"), sub("./src/lib/ui/")])).toEqual(["src/lib", "  ui"]);
  });

  it("does not collapse a group with two children, but collapses single chains", () => {
    expect(outline([sub("a/b/c"), sub("a/b/d")])).toEqual(["a/b [group]", "  c", "  d"]);
    expect(outline([sub("a/b/c/d")])).toEqual(["a/b/c/d"]);
  });

  it("handles empty input and root areas without crashing", () => {
    expect(buildAreaTree([])).toEqual([]);
    const rows = buildAreaTree([sub("src"), sub("."), sub("")]);
    expect(rows.map((r) => [r.label, r.depth, r.subsystemIndex, r.duplicateCount, r.parentKey])).toEqual([
      [".", 0, 1, 2, null],
      ["src", 1, 0, 1, "area:"],
    ]);
    expect(rows[0].hasChildren).toBe(true);
    expect(eachKeysAreUnique(rows, (r) => r.key)).toBe(true);
  });

  it("is deterministic regardless of input order", () => {
    const forward = buildAreaTree(REAL_AREAS.map((a) => sub(a))).map((r) => r.area);
    const backward = buildAreaTree([...REAL_AREAS].reverse().map((a) => sub(a))).map((r) => r.area);
    expect(backward).toEqual(forward);
  });

  it("sorts siblings by code point, not locale", () => {
    expect(outline([sub("b"), sub("B"), sub("a"), sub("_x")])).toEqual(["B", "_x", "a", "b"]);
  });
});

describe("summaryWithoutArea", () => {
  it("strips a leading '<area>: ' prefix", () => {
    expect(summaryWithoutArea(sub("src-tauri/src", "src-tauri/src: 479 files, mostly rust, json, sql (3 entry, 2 models, 6 services, 15 tests)")))
      .toBe("479 files, mostly rust, json, sql (3 entry, 2 models, 6 services, 15 tests)");
    expect(summaryWithoutArea(sub("harness", "harness: 89 files, mostly html, svelte, typescript (1 models, 1 tests)")))
      .toBe("89 files, mostly html, svelte, typescript (1 models, 1 tests)");
  });

  it("strips an em-dash separator", () => {
    expect(summaryWithoutArea(sub("scripts", "scripts — 158 files"))).toBe("158 files");
  });

  it("leaves summaries without the prefix unchanged", () => {
    expect(summaryWithoutArea(sub("src-tauri", "src-tauri/src: 479 files"))).toBe("src-tauri/src: 479 files");
    expect(summaryWithoutArea(sub("src", "source tree"))).toBe("source tree");
    expect(summaryWithoutArea(sub("", ": odd"))).toBe(": odd");
  });

  it("strips the area as written or normalized, but not a different area", () => {
    expect(summaryWithoutArea(sub("./scripts/", "./scripts/: 3 files"))).toBe("3 files");
    expect(summaryWithoutArea(sub("./scripts/", "scripts: 3 files"))).toBe("3 files");
    expect(summaryWithoutArea(sub("./scripts/", "scripts-extra: 3 files"))).toBe("scripts-extra: 3 files");
  });
});
