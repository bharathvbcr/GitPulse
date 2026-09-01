import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * PolicyStatus is enumerated on both sides of the IPC boundary: a Rust enum
 * serialized lowercase, and a TypeScript string union the harness store
 * switches on exhaustively.
 *
 * A variant added in Rust and not in TypeScript arrives as a value outside the
 * union. The switch falls through, and the verdict is rendered as whatever the
 * fallback is — which for a security gate means a refusal could be presented
 * as something milder. Nothing catches that: the value crosses as a plain
 * string, so no type error occurs at either end.
 *
 * Coverage families are deliberately not checked this way. The frontend types
 * them as `string` and passes them through, so a new family degrades to
 * generic handling rather than breaking a union.
 */
const rust = readFileSync(new URL("../src-tauri/src/harness/policy.rs", import.meta.url), "utf8");
const store = readFileSync(new URL("../src/lib/stores/harnessStore.ts", import.meta.url), "utf8");

/** Variants of the Rust enum, lowercased the way serde renames them. */
function rustVariants(): string[] {
  const block = /pub enum PolicyStatus \{([\s\S]*?)\n\}/.exec(rust);
  expect(block, "PolicyStatus enum not found").not.toBeNull();
  // `#[serde(rename_all = "lowercase")]` is what makes this mapping correct;
  // if that attribute changes, the wire values change with it.
  expect(rust).toContain('#[serde(rename_all = "lowercase")]');
  return [...block![1].matchAll(/^\s{4}(\w+),/gm)].map((m) => m[1].toLowerCase()).sort();
}

/** Members of the TypeScript union. */
function tsUnion(): string[] {
  const line = /export type PolicyStatus =([^;]*);/.exec(store);
  expect(line, "PolicyStatus union not found").not.toBeNull();
  return [...line![1].matchAll(/"([a-z_]+)"/g)].map((m) => m[1]).sort();
}

describe("policy status contract", () => {
  const variants = rustVariants();
  const union = tsUnion();

  it("found both sides", () => {
    expect(variants.length).toBeGreaterThanOrEqual(5);
    expect(union.length).toBeGreaterThanOrEqual(5);
  });

  it("enumerates exactly the same statuses on both sides", () => {
    expect(union).toEqual(variants);
  });

  it("handles every status where the store switches on it", () => {
    // An unhandled status would fall to whatever the default is, which for a
    // gate verdict is the wrong direction to fail in.
    for (const status of variants) {
      expect(store, `no case for "${status}"`).toContain(`case "${status}":`);
    }
  });
});
