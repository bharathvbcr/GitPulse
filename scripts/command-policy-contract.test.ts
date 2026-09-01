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

describe("native mutation policy contract", () => {
  it("routes every native Git mutation through the canonical command gate", () => {
    for (const name of [
      "cmd_stage_file",
      "cmd_unstage_file",
      "cmd_fetch",
      "cmd_stash_save",
      "cmd_stash_pop",
    ]) {
      expect(functionSource(name), `${name} must call guard before its writer`).toContain("guard(");
    }
  });
});
