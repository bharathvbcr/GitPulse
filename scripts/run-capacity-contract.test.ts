import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import { DEFAULT_LIVE_RUNS, MAX_LIVE_RUNS } from "../src/lib/workbench/vocabulary";

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

// The store's enforcement is not asserted from its source here: the limit the
// host names (`capacity_reached`, src-tauri/src/workbench/terminal_launch.rs
// tests) and the per-checkout busy refusal (`checkout_busy`, the same file and
// reconcile_tests.rs) are driven through `runs.prepare_terminal` in Rust.
function storeConstant(source: string, name: string): number {
  const match = new RegExp(`^pub const ${name}: i64 = (\\d+);$`, "m").exec(source);
  if (!match) throw new Error(`dc-store no longer declares \`pub const ${name}: i64 = N;\` in runs.rs`);
  return Number(match[1]);
}

describe("the renderer's run capacity is the store's", () => {
  const source = readFileSync(RUNS, "utf8");

  it("DEFAULT_LIVE_RUNS equals dc-store's DEFAULT_ACTIVE_RUNS", () => {
    expect(DEFAULT_LIVE_RUNS).toBe(storeConstant(source, "DEFAULT_ACTIVE_RUNS"));
  });

  it("MAX_LIVE_RUNS equals dc-store's MAX_ACTIVE_RUNS_CEILING", () => {
    expect(MAX_LIVE_RUNS).toBe(storeConstant(source, "MAX_ACTIVE_RUNS_CEILING"));
  });

  it("refuses a parse it cannot make rather than passing", () => {
    expect(() => storeConstant("pub const MAX_ACTIVE_RUNS_CEILING: usize = 64;", "MAX_ACTIVE_RUNS_CEILING")).toThrow();
  });
});
