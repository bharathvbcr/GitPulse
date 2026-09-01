import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { CONTRACTS } from "./check-coverage-types.mjs";

/**
 * check:types verifies the payloads listed in its CONTRACTS table. This pins
 * the complement: every named type returned by a `#[tauri::command]` that the
 * table does not cover, with a reason.
 *
 * Without this, the gap is invisible. "OK: type contract holds" would read as
 * "every IPC payload is verified" while a newly added command quietly joined
 * the unchecked set — a check that never ran, reporting the same green as one
 * that ran and passed.
 */
const RUST_ROOT = fileURLToPath(new URL("../src-tauri/src/", import.meta.url));

/** Types that appear in command signatures but are not payload structs. */
const NOT_PAYLOADS = new Set(["Result", "Vec", "Option", "String", "HashMap", "Guarded", "Box"]);

/**
 * Unchecked IPC payload types, each with why it cannot be compared today.
 *
 * Every entry here shares one cause: the TypeScript side has no interface of
 * that name for the checker to read — the payload is consumed through an
 * inline or anonymous type at the `invoke` call. Giving each a named interface
 * is what would move it into CONTRACTS; until then the exposure is that a Rust
 * field rename surfaces as a silently `undefined` property.
 */
const UNCHECKED = new Map<string, string>([
  ["ConflictDocument", "conflict parse result; consumed inline by the merge editor"],
  ["ConventionalCommit", "commit-message parse result; consumed inline by the composer"],
  ["CubicBezierCurve", "graph connector geometry; consumed inline by the canvas renderer"],
  ["LanguageInfo", "single-file language detection; consumed inline by the viewers"],
  ["LineCounts", "line-count tally; consumed inline by the viewers"],
  ["StackHierarchyPayload", "stack hierarchy; consumed inline by the stack viewer"],
  ["TerminalSpawned", "terminal spawn ack; consumed inline at the invoke call"],
]);

/**
 * Every `.rs` file under a directory. A hand-rolled walk rather than a glob
 * library: fast-glob is only a transitive dependency here, and reaching past
 * package.json for it would break the day a parent stopped pulling it in.
 * @param dir absolute directory to walk
 */
function rustFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...rustFiles(full));
    else if (entry.isFile() && entry.name.endsWith(".rs")) out.push(full);
  }
  return out;
}

/** Named types returned by every `#[tauri::command]` in the Rust tree. */
function ipcReturnedTypes(): Map<string, string> {
  const found = new Map<string, string>();
  for (const file of rustFiles(RUST_ROOT)) {
    const source = readFileSync(file, "utf8");
    const commands = source.matchAll(
      /#\[tauri::command[^\]]*\][\s\S]{0,400}?fn\s+(\w+)\s*\([\s\S]*?\)\s*->\s*([^{]+)\{/g,
    );
    for (const command of commands) {
      for (const name of command[2].match(/\b[A-Z][A-Za-z0-9_]+\b/g) ?? []) {
        if (!NOT_PAYLOADS.has(name)) found.set(name, command[1]);
      }
    }
  }
  return found;
}

describe("IPC type coverage is accounted for, not assumed", () => {
  const returned = ipcReturnedTypes();
  const covered = new Set<string>(CONTRACTS.flatMap((contract) => [...contract.structs]));

  it("finds the commands to check at all", () => {
    // If the scan breaks, every other assertion here passes vacuously.
    expect(returned.size).toBeGreaterThan(30);
  });

  it("checks every IPC payload type except the ones listed with a reason", () => {
    const unchecked = [...returned.keys()].filter((name) => !covered.has(name)).sort();
    expect(unchecked).toEqual([...UNCHECKED.keys()].sort());
  });

  it("keeps the exemption list honest: nothing listed is secretly covered", () => {
    // An exemption that no longer applies should be deleted, not left to imply
    // the gap is bigger than it is.
    const stale = [...UNCHECKED.keys()].filter(
      (name) => covered.has(name) || !returned.has(name),
    );
    expect(stale, "these are no longer unchecked IPC types").toEqual([]);
  });

  it("gives every exemption a real reason", () => {
    for (const [name, reason] of UNCHECKED) {
      expect(reason.length, `${name} needs a reason, not a placeholder`).toBeGreaterThan(20);
    }
  });
});
