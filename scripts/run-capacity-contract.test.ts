import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import { MAX_LIVE_RUNS } from "../src/lib/workbench/vocabulary";

/**
 * How many task attempts may be live at once is decided by the store, in the
 * vendored `dc-store`, and repeated by the renderer, which uses it to size the
 * queue of terminals waiting to open. When the two disagree the failure is
 * silent: a renderer bound below the store's refused attempts the store had
 * already admitted — exactly what a hard-coded "two" did once the store
 * allowed more — and one above it promises capacity that does not exist.
 *
 * Read from the vendored source, not from a constant someone copied: the
 * vendored copy is what this build actually links.
 */
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const RUNS = path.join(ROOT, "src-tauri/vendored/dc-store/src/workbench/runs.rs");

function storeCapacity(source: string): number {
  const match = /^const MAX_ACTIVE_RUNS: i64 = (\d+);$/m.exec(source);
  if (!match) throw new Error("dc-store no longer declares `const MAX_ACTIVE_RUNS: i64 = N;` in runs.rs");
  return Number(match[1]);
}

describe("the renderer's run capacity is the store's", () => {
  const source = readFileSync(RUNS, "utf8");

  it("MAX_LIVE_RUNS equals dc-store's MAX_ACTIVE_RUNS", () => {
    expect(MAX_LIVE_RUNS).toBe(storeCapacity(source));
  });

  it("the store keys its busy check on the checkout, not the repository", () => {
    // The whole multi-task design rests on this predicate. A re-vendor that
    // went back to `repository_id` alone would make every worktree launch
    // fail again with no renderer-side signal.
    expect(source).toMatch(/repository_id=\?2 AND json_extract\(body,'\$\.git_dir'\)=\?3/);
    expect(source).toContain('"checkout_busy"');
    expect(source).not.toContain('"repository_busy"');
  });

  it("refuses a parse it cannot make rather than passing", () => {
    expect(() => storeCapacity("const MAX_ACTIVE_RUNS: usize = 8;")).toThrow();
  });
});
