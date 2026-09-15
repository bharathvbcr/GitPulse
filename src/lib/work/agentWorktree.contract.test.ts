import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { agentKind, agentLayout, agentSessionSlug, isAgentWorktree } from "./agentWorktree";
import cases from "./agentWorktree.cases.json" with { type: "json" };

/**
 * The agent-worktree layout rule has two implementations — this module, and
 * `agent_layout` in `src-tauri/src/engine/worktree.rs`. That duplication is
 * deliberate: the backend labels worktrees while assembling a snapshot, and
 * the Work view labels paths it already holds without a second IPC round
 * trip. What is not acceptable is the two drifting, because then the chip on
 * a row and the count in the snapshot describe different worlds.
 *
 * `agentWorktree.cases.json` is the one corpus both are held to. This file
 * checks the TypeScript half and that the Rust half still reads the same
 * file; `agent_layout_matches_the_shared_corpus` in `worktree.rs` checks the
 * Rust half. Neither side can be changed alone without one of them going red.
 */

const CORPUS_PATH = "src/lib/work/agentWorktree.cases.json";

function repoFile(relative: string): string {
  return readFileSync(fileURLToPath(new URL(`../../../${relative}`, import.meta.url)), "utf8");
}

describe("agent worktree layout contract", () => {
  it("matches the shared corpus case for case", () => {
    for (const item of cases.cases) {
      const layout = agentLayout(item.path);
      expect({ path: item.path, kind: agentKind(item.path), slug: agentSessionSlug(item.path) }).toEqual({
        path: item.path,
        kind: item.kind,
        slug: item.slug,
      });
      // `isAgentWorktree` and the accessors must agree about which side of
      // the line a path falls on; a path with no kind that still reported a
      // layout would put an agent chip on a human's checkout.
      expect(isAgentWorktree(item.path), item.path).toBe(item.kind !== "");
      expect(layout === null, item.path).toBe(item.kind === "");
    }
  });

  it("is a corpus, not a handful of happy paths", () => {
    // Derived rather than hand-asserted: the point is that the corpus keeps
    // covering the space as cases are added, not that it has some exact size.
    const matched = cases.cases.filter((c) => c.kind !== "");
    const rejected = cases.cases.filter((c) => c.kind === "");
    expect(matched.length).toBeGreaterThanOrEqual(15);
    expect(rejected.length).toBeGreaterThanOrEqual(20);
    // Windows spellings are the half that silently rots, because nobody
    // developing on macOS ever types one.
    expect(cases.cases.filter((c) => c.path.includes("\\")).length).toBeGreaterThanOrEqual(4);
    // A container path whose slug is empty is a distinct outcome from a
    // rejected path, and the corpus must keep exercising it.
    expect(matched.some((c) => c.slug === "")).toBe(true);
  });

  it("keeps a regression case for every defect this rule has had", () => {
    const regressions = cases.cases.filter((c) => c.why.startsWith("REGRESSION:"));
    // Three classes were found and fixed together: an ancestor directory
    // named `worktrees` capturing the slug, `.GIT/worktrees` in its container
    // spelling escaping the git guard, and `..`-prefixed segments being
    // accepted as agent names. Each must keep a case.
    expect(regressions.some((c) => c.path.includes("/worktrees/") && c.kind !== "")).toBe(true);
    expect(regressions.some((c) => c.path.includes(".GIT"))).toBe(true);
    expect(regressions.some((c) => c.path.includes(".."))).toBe(true);
    expect(regressions.length).toBeGreaterThanOrEqual(6);
  });

  it("describes every case, and never the same path twice", () => {
    const paths = cases.cases.map((c) => c.path);
    expect(new Set(paths).size).toBe(paths.length);
    for (const item of cases.cases) {
      expect(item.why.length, item.path).toBeGreaterThan(20);
      // A rejected path must not carry a slug: that combination has no
      // meaning and would quietly pass the case-for-case check above.
      if (item.kind === "") expect(item.slug, item.path).toBe("");
    }
  });

  it("is still the corpus the Rust implementation reads", () => {
    // Guards the path, not the symbol name: a corpus moved or renamed on
    // this side while Rust keeps reading the old location would leave the
    // Rust test passing against a file nobody maintains any more.
    const rust = repoFile("src-tauri/src/engine/worktree.rs");
    expect(rust).toContain(CORPUS_PATH);
    expect(rust).toContain("agent_layout");
    // And the corpus this test read really is that file, not a stale copy.
    expect(JSON.parse(repoFile(CORPUS_PATH)).cases.length).toBe(cases.cases.length);
  });
});
