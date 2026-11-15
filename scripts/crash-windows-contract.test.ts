import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { filesUnder } from "../src/lib/stores/sourceScan";
import { checkTable, summary, type Row, type Table } from "./crash-windows";

/**
 * The crash-window table (`scripts/crash-windows.json`) against the Rust source.
 *
 * A crash window is a point between two durable steps where a kill or a failed
 * step leaves state the next run has to recover from. The table lists them for
 * three write paths only: the ledger, the ops log in `engine/git_writer.rs`,
 * and worktree merge. It is not a list of every crash test in the codebase and
 * it does not claim to be; the scanned files are named in the table's `scope`.
 *
 * What this test enforces, all through `crash-windows.ts`:
 *   - a hook in a scanned file that no row names fails (so adding an injection
 *     point without listing it is a red build);
 *   - a row naming a hook that is gone from its file fails;
 *   - a row citing a test that does not exist, is not a `#[test]`, is
 *     `#[ignore]`d or is cfg-gated without the row saying so fails;
 *   - two rows with one id fail;
 *   - `covered` and `uncovered` are counted apart, and an uncovered row never
 *     counts as covered.
 *
 * It reads source text only. It does not run the cited Rust tests: that a
 * cited test exists and is wired is checked here, that it passes is `cargo
 * test`'s job.
 */

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const TABLE_PATH = path.join(ROOT, "scripts", "crash-windows.json");

function read(relative: string): string | undefined {
  const file = path.join(ROOT, relative);
  return existsSync(file) ? readFileSync(file, "utf8") : undefined;
}

const table = JSON.parse(readFileSync(TABLE_PATH, "utf8")) as Table;

/** Every `.rs` file the table's scope names, directories walked recursively. */
function scopeFiles(scope: Table["scope"]): string[] {
  const walked = scope.dirs.flatMap((dir) =>
    filesUnder(path.join(ROOT, dir), (name) => name.endsWith(".rs")).map((file) => path.relative(ROOT, file)),
  );
  return [...new Set([...walked, ...scope.files])].sort();
}

function cited(rows: Row[]): string[] {
  return rows.flatMap((row) => [
    ...row.injection.hooks.map((hook) => hook.file),
    ...row.tests.map((test) => test.file),
    ...(row.nearestTests ?? []).map((test) => test.file),
  ]);
}

function load(files: string[]): Record<string, string> {
  const sources: Record<string, string> = {};
  for (const file of files) {
    const text = read(file);
    if (text !== undefined) sources[file] = text;
  }
  return sources;
}

const SCOPE = scopeFiles(table.scope);
const SOURCES = load([...SCOPE, ...cited(table.rows), ...table.ignoredHooks.map((hook) => hook.file)]);
const REPORT = checkTable(table, SOURCES, SCOPE);

describe("crash-window table", () => {
  it("is consistent with the Rust source", () => {
    expect(REPORT.errors, REPORT.errors.join("\n")).toEqual([]);
  });

  it("reports covered and uncovered apart, and states what it scanned", () => {
    const text = summary(REPORT);
    // Printed so a run shows what was checked, not only that it passed.
    console.info(`\n${text}`);
    expect(text).toContain(`covered ${REPORT.covered}`);
    expect(text).toContain(`uncovered ${REPORT.uncovered}`);
    expect(REPORT.covered + REPORT.uncovered).toBe(table.rows.length);
    expect(table.rows.filter((row) => row.status === "covered")).toHaveLength(REPORT.covered);
    expect(table.rows.filter((row) => row.status === "uncovered")).toHaveLength(REPORT.uncovered);
  });

  it("scans every file it names, and names a scope that exists", () => {
    expect(SCOPE.length).toBeGreaterThan(0);
    const missing = SCOPE.filter((file) => SOURCES[file] === undefined);
    expect(missing, `scope files that do not exist: ${missing.join(", ")}`).toEqual([]);
  });

  it("lists at least one window for each of the three paths", () => {
    for (const name of table.paths) {
      expect(
        table.rows.some((row) => row.path === name),
        `path ${name} has no row, so its windows would go unlisted without a failure`,
      ).toBe(true);
    }
    expect(table.paths).toEqual(["ledger", "ops-log", "worktree-merge"]);
  });

  it("keeps a window uncovered rather than hiding it when it has no injection point", () => {
    const hidden = table.rows.filter((row) => row.status === "covered" && row.injection.kind === "none");
    expect(hidden.map((row) => row.id)).toEqual([]);
  });
});
