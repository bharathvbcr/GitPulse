import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * Serde enums cross the wire as strings, and `check:types` skips them by
 * design — it compares struct fields. So nothing checked that a Rust variant
 * and its TypeScript literal still spell the same thing.
 *
 * This is the quietest drift there is. Rename a variant and TypeScript keeps
 * compiling: the union still lists a valid string, the comparison just stops
 * matching anything, and a branch silently becomes dead. No type error, no
 * test failure, no crash.
 */
const RUST_ROOT = fileURLToPath(new URL("../src-tauri/src/", import.meta.url));
const TS_ROOT = fileURLToPath(new URL("../src/", import.meta.url));

/**
 * Rust enums with no same-named TypeScript union, and why.
 * An entry means "the frontend does not branch on this", not "unchecked is
 * fine" — if a UI starts comparing these strings, it needs the named type.
 */
const NO_TS_MIRROR = new Map<string, string>([
  ["CoverageFormat", "the frontend renders `format` as an opaque label and never branches on it"],
  ["ManviActionKind", "activity labels are produced by the frontend, not parsed from the backend"],
  [
    "RebaseActionKind",
    "modelled in the UI as PlannerAction plus a separate wire union, because Reword carries a payload and serializes as an object rather than a bare string",
  ],
]);

/** serde's rename_all, for the rules this repo uses. */
function serializedName(variant: string, rule: string | undefined): string {
  if (rule === "lowercase") return variant.toLowerCase();
  if (rule === "snake_case") return variant.replace(/(?<!^)(?=[A-Z])/g, "_").toLowerCase();
  if (rule === undefined) return variant;
  throw new Error(`unsupported rename_all on an enum: ${rule}`);
}

function walk(dir: string, exts: string[]): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...walk(full, exts));
    else if (entry.isFile() && exts.some((e) => entry.name.endsWith(e)) && !/\.(test|spec)\./.test(entry.name)) {
      out.push(full);
    }
  }
  return out;
}

function balanced(source: string, from: number): string {
  const open = source.indexOf("{", from);
  if (open === -1) return "";
  let depth = 0;
  for (let i = open; i < source.length; i += 1) {
    if (source[i] === "{") depth += 1;
    else if (source[i] === "}") {
      depth -= 1;
      if (depth === 0) return source.slice(open + 1, i);
    }
  }
  return "";
}

interface RustEnum {
  /** Unit variants, as they appear on the wire. */
  unit: string[];
  /** Variants carrying data — serialized as `{ Name: ... }`, not a string. */
  withData: string[];
}

function rustEnums(): Map<string, RustEnum> {
  const found = new Map<string, RustEnum>();
  for (const file of walk(RUST_ROOT, [".rs"])) {
    const source = readFileSync(file, "utf8");
    for (const match of source.matchAll(/((?:#\[[^\]]*\]\s*)*)pub enum (\w+)\s*\{/g)) {
      if (!match[1].includes("Serialize")) continue;
      const rule = /rename_all\s*=\s*"([^"]+)"/.exec(match[1])?.[1];
      const body = balanced(source, match.index ?? 0);
      const unit: string[] = [];
      const withData: string[] = [];
      for (const variant of body.matchAll(/^\s*([A-Z]\w*)(\s*[({])?/gm)) {
        (variant[2] ? withData : unit).push(serializedName(variant[1], rule));
      }
      found.set(match[2], { unit, withData });
    }
  }
  return found;
}

/** String literals in the TS union named `name`, if one exists. */
function tsUnion(name: string): { literals: Set<string>; file: string } | null {
  for (const file of walk(TS_ROOT, [".ts", ".svelte"])) {
    const source = readFileSync(file, "utf8");
    const match = new RegExp(`(?:export\\s+)?type\\s+${name}\\s*=\\s*([^;]+);`).exec(source);
    if (!match) continue;
    return {
      literals: new Set([...match[1].matchAll(/"([^"]+)"/g)].map((m) => m[1])),
      file: path.relative(TS_ROOT, file),
    };
  }
  return null;
}

describe("serde enum variants match their TypeScript unions", () => {
  const enums = rustEnums();

  it("finds the enums to check at all", () => {
    expect(enums.size).toBeGreaterThan(8);
  });

  it("spells every unit variant the same on both sides", () => {
    const drift: string[] = [];
    for (const [name, { unit }] of enums) {
      if (NO_TS_MIRROR.has(name)) continue;
      const ts = tsUnion(name);
      if (!ts) {
        drift.push(`${name}: no TypeScript union of this name, and no documented reason`);
        continue;
      }
      const missing = unit.filter((v) => !ts.literals.has(v));
      const extra = [...ts.literals].filter((v) => !unit.includes(v));
      if (missing.length > 0) drift.push(`${name}: Rust sends ${missing.join(", ")}; TS does not accept them`);
      if (extra.length > 0) drift.push(`${name}: TS accepts ${extra.join(", ")}; Rust never sends them`);
    }
    expect(drift).toEqual([]);
  });

  it("does not let a data-carrying variant masquerade as a bare string", () => {
    // `Reword(String)` serializes as `{ "Reword": "..." }`, never as
    // `"Reword"`. A TS union listing it as a plain literal would typecheck and
    // then fail to deserialize backend-side.
    const wrong: string[] = [];
    for (const [name, { withData }] of enums) {
      if (NO_TS_MIRROR.has(name)) continue;
      const ts = tsUnion(name);
      if (!ts) continue;
      for (const variant of withData) {
        if (ts.literals.has(variant)) {
          wrong.push(`${name}.${variant} carries data but ${ts.file} lists it as a bare string`);
        }
      }
    }
    expect(wrong).toEqual([]);
  });

  it("keeps the no-mirror list from outliving its enums", () => {
    const stale = [...NO_TS_MIRROR.keys()].filter((name) => !enums.has(name));
    expect(stale, "these enums no longer exist").toEqual([]);
  });

  it("gives every no-mirror entry a real reason", () => {
    for (const [name, reason] of NO_TS_MIRROR) {
      expect(reason.length, `${name} needs a reason`).toBeGreaterThan(20);
    }
  });
});
