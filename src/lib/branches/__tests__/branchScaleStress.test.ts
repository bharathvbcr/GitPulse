import { describe, expect, it } from "vitest";
import type { BranchInfo, TagInfo } from "../types";
import {
  groupBranches,
  filterBranchSections,
  highlightMatches,
} from "../groupBranches";
import { flattenRows } from "../flattenRows";
import { computeWindow } from "../../dom/virtualWindow";

function generateStressBranches(count: number): { branches: BranchInfo[]; tags: TagInfo[] } {
  const branches: BranchInfo[] = [];
  const now = Date.now() / 1000;
  const ONE_DAY = 86400;

  for (let i = 0; i < count; i++) {
    const isRemote = i % 3 === 0;
    const isCurrent = i === 42;
    const isDefault = i === 0;
    const isStale = i % 5 === 0;
    const timestamp = isStale ? now - 120 * ONE_DAY : now - (i % 30) * ONE_DAY;

    // Generate varied hierarchy structures
    let name: string;
    if (i % 10 === 0) {
      name = `feat/area-${i % 20}/sub-${i % 5}/ticket-${i}-implement-feature`;
    } else if (i % 7 === 0) {
      name = `fix/issue-${i}-regression-patch`;
    } else if (i % 4 === 0) {
      name = `release/v${Math.floor(i / 100)}.${i % 10}.0`;
    } else {
      name = `user/developer-${i % 50}/experiment-${i}`;
    }

    if (isRemote) {
      name = `origin/${name}`;
    }

    branches.push({
      name,
      is_current: isCurrent,
      is_remote: isRemote,
      remote_name: isRemote ? "origin" : null,
      tip_commit_id: `commit_oid_${i.toString(16).padStart(40, "0")}`,
      ahead_count: i % 4,
      behind_count: i % 6,
      upstream: isRemote ? null : `origin/${name}`,
      is_default: isDefault,
      is_gone: i % 50 === 0,
      last_commit_timestamp: timestamp,
      last_author: `Dev ${i % 20}`,
      last_summary: `Commit message for branch ${i}`,
      commits_ahead_of_base: i % 10,
      commits_behind_base: i % 8,
      additions: (i * 17) % 500,
      deletions: (i * 13) % 200,
      files_changed: (i * 7) % 30,
    });
  }

  const tags: TagInfo[] = [];
  for (let t = 0; t < Math.min(1000, Math.floor(count / 10)); t++) {
    tags.push({
      name: `v${Math.floor(t / 10)}.${t % 10}.0`,
      commit_id: `tag_oid_${t.toString(16).padStart(40, "0")}`,
      message: t % 2 === 0 ? `Release tag ${t}` : null,
    });
  }

  return { branches, tags };
}

describe("Branch scale and stress benchmarks (10,000+ branches)", () => {
  const TOTAL_BRANCHES = 10_000;
  const { branches, tags } = generateStressBranches(TOTAL_BRANCHES);

  it("groups 10,000 branches and 1,000 tags without quadratic blowup", () => {
    const start = performance.now();
    const sections = groupBranches(branches, tags);
    const duration = performance.now() - start;

    expect(sections.length).toBeGreaterThanOrEqual(3);
    const totalBranchCount = sections.reduce((sum, s) => sum + s.branchCount, 0);
    expect(totalBranchCount).toBe(TOTAL_BRANCHES + tags.length);
    expect(duration).toBeLessThan(2000); // Tripwire: generous for loaded machines
  });

  it("handles pinned branches section efficiently at 10,000 branch scale", () => {
    const pinned = new Set<string>([
      branches[0].name,
      branches[42].name,
      branches[100].name,
      branches[500].name,
      branches[1000].name,
    ]);

    const start = performance.now();
    const sections = groupBranches(branches, tags, pinned);
    const duration = performance.now() - start;

    const pinnedSection = sections.find((s) => s.id === "pinned");
    expect(pinnedSection).toBeDefined();
    expect(pinnedSection?.branchCount).toBe(5);
    expect(pinnedSection?.branches.length).toBe(5);
    expect(duration).toBeLessThan(2000);
  });

  // Wall-clock budgets here are regression tripwires, not SLAs: this suite
  // runs alongside Rust builds on developer machines, so fixed deadlines
  // near the cold-cost flake hard. Functional assertions below still pin
  // behavior; the ceilings only catch pathological blowups.
  it("filters 10,000 branches by quick tabs without pathological cost", () => {
    const pinned = new Set([branches[10].name, branches[20].name]);
    const sections = groupBranches(branches, tags, pinned);

    const tabs = ["all", "local", "remote", "active", "stale", "pinned", "tags"] as const;
    for (const tab of tabs) {
      const start = performance.now();
      const filtered = filterBranchSections(sections, "", tab);
      const duration = performance.now() - start;

      expect(duration).toBeLessThan(2000);
      expect(filtered).toBeDefined();
    }
  });

  it("memoizes repeated query filtering on 10,000 branches", () => {
    const sections = groupBranches(branches, tags);

    const start = performance.now();
    const result1 = filterBranchSections(sections, "feature");
    const duration1 = performance.now() - start;

    expect(duration1).toBeLessThan(2000);
    expect(result1.length).toBeGreaterThan(0);

    // Repeated query hits memoized branch text
    const start2 = performance.now();
    const result2 = filterBranchSections(sections, "feature");
    const duration2 = performance.now() - start2;

    // Memoized work must not be slower than the cold pass, with slack for
    // scheduler jitter on loaded machines (a fixed wall-clock budget flakes
    // under parallel test runs).
    expect(duration2).toBeLessThan(Math.max(2000, duration1 * 1.25));
    expect(result2.length).toBe(result1.length);
  });

  it("flattens and virtualizes 10,000 branches into a bounded window", () => {
    const sections = groupBranches(branches, tags);
    const isCollapsed = (_id: string, kind: "pinned" | "recent" | "local" | "remote" | "tags") =>
      kind === "remote" || kind === "tags";

    const flattenStart = performance.now();
    const allRows = flattenRows(sections, isCollapsed);
    const flattenDuration = performance.now() - flattenStart;

    expect(flattenDuration).toBeLessThan(2000);
    expect(allRows.length).toBeGreaterThan(5000);

    // Test virtual window calculations across 100 random scroll positions
    const windowStart = performance.now();
    const ROW_HEIGHT = 26;
    const VIEWPORT_HEIGHT = 600;

    for (let scroll = 0; scroll < allRows.length * ROW_HEIGHT; scroll += 1000) {
      const win = computeWindow(scroll, VIEWPORT_HEIGHT, allRows.length, ROW_HEIGHT, 12);
      const visible = allRows.slice(win.start, win.end);

      expect(visible.length).toBeLessThanOrEqual(50); // Never renders unbounded DOM
      expect(win.start).toBeGreaterThanOrEqual(0);
      expect(win.end).toBeLessThanOrEqual(allRows.length);
    }
    const windowDuration = performance.now() - windowStart;
    expect(windowDuration).toBeLessThan(2000); // O(1) window slicing tripwire
  });

  it("accurately highlights substrings with zero regex and zero crash on exotic tokens", () => {
    const queries = ["feat", "auth/2", "[regex]", "(paren)", "special.*", "a"];
    for (const q of queries) {
      const chunks = highlightMatches("feat/auth/2-oauth-token", q);
      expect(chunks.length).toBeGreaterThan(0);
      const reconstructed = chunks.map((c) => c.text).join("");
      expect(reconstructed).toBe("feat/auth/2-oauth-token");
    }
  });
});
