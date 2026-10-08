import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const source = readFileSync(new URL("../src-tauri/src/commands/mod.rs", import.meta.url), "utf8");

function functionSource(name: string): string {
  const start = source.indexOf(`pub async fn ${name}`);
  expect(start, `${name} must exist`).toBeGreaterThanOrEqual(0);
  const end = source.indexOf("\n}\n", start);
  expect(end, `${name} must have a closed body`).toBeGreaterThan(start);
  return source.slice(start, end);
}

/**
 * Every command whose return type is `Guarded<T>`.
 *
 * DERIVED, not listed. The previous version of this contract named five
 * commands by hand, so the eighteen added since were covered by nothing and a
 * new mutation shipped without a gate would have passed. `Guarded<T>` exists
 * precisely to carry a policy verdict to the UI, so returning it IS the
 * declaration that this command mutates and must be judged — which makes it
 * the right thing to enumerate from.
 */
function guardedCommands(): string[] {
  const found = [
    ...source.matchAll(/pub async fn (cmd_\w+)\([^)]*\)\s*->\s*Result<Guarded</gs),
  ].map((match) => match[1]);
  return [...new Set(found)];
}

describe("native mutation policy contract", () => {
  it("finds the guarded command set at all", () => {
    // A regex that silently matches nothing would make every assertion below
    // vacuously true — the exact way a contract test rots into decoration.
    const commands = guardedCommands();
    expect(commands.length).toBeGreaterThan(30);
    for (const anchor of ["cmd_commit", "cmd_push", "cmd_reset", "cmd_stash_action"]) {
      expect(commands, `${anchor} must be among the guarded commands`).toContain(anchor);
    }
  });

  it("routes every policy-carrying command through the write gate", () => {
    for (const name of guardedCommands()) {
      const body = functionSource(name);
      // `guard` judges a rendered command line; `guard_file` judges a path.
      // Both are the canonical harness entry points; anything else is a
      // command that reports a verdict it never actually obtained.
      const judged = body.includes("guard(") || body.includes("guard_file(");
      expect(judged, `${name} returns Guarded<..> but never calls the write gate`).toBe(true);
    }
  });

  it("keeps the historically hand-listed commands covered", () => {
    // Regression guard for the derivation itself: these five were the whole
    // contract before it was derived, so losing them means the regex broke.
    const commands = guardedCommands();
    for (const name of [
      "cmd_stage_file",
      "cmd_unstage_file",
      "cmd_fetch",
      "cmd_stash_save",
      "cmd_stash_pop",
    ]) {
      expect(commands).toContain(name);
      expect(functionSource(name)).toContain("guard(");
    }
  });
});
