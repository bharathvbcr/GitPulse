import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { AUDIT_FAILURE_LABELS } from "../src/lib/health/report";

const REPO_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const DEPS_RS = join(REPO_ROOT, "src-tauri", "src", "analyzer", "deps.rs");

/**
 * The `failure_codes` array inside `audit_is_complete`, read from the Rust
 * source rather than restated here.
 *
 * Parsing the source is the point: these codes are what makes
 * `audit_complete` go false, and the frontend uses the same set to say WHICH
 * audit failed. Two hand-maintained copies of that list would drift the first
 * time a scanner is added, and the drift would be invisible — the UI would
 * keep rendering "Local audit incomplete" with no cause named, which is the
 * exact failure this pair of lists exists to prevent.
 */
function rustFailureCodes(): string[] {
  const source = readFileSync(DEPS_RS, "utf8");
  const marker = "let failure_codes = [";
  const start = source.indexOf(marker);
  expect(start, `${DEPS_RS} no longer declares failure_codes`).toBeGreaterThan(-1);
  const end = source.indexOf("];", start);
  expect(end, "failure_codes array is unterminated").toBeGreaterThan(start);
  const body = source.slice(start + marker.length, end);
  const codes = [...body.matchAll(/"([a-z_]+)"/g)].map((match) => match[1]);
  expect(codes.length, "failure_codes parsed as empty — the parse is broken").toBeGreaterThan(0);
  return codes;
}

describe("audit failure codes stay in step with the Rust scanner", () => {
  it("labels exactly the codes that disqualify audit_complete", () => {
    expect([...Object.keys(AUDIT_FAILURE_LABELS)].sort()).toEqual([...rustFailureCodes()].sort());
  });

  it("gives every code a label a reader would recognise", () => {
    for (const [code, label] of Object.entries(AUDIT_FAILURE_LABELS)) {
      expect(label.trim(), `${code} has an empty label`).not.toBe("");
      // A label that is just the raw code teaches the reader nothing; the
      // point of the map is to turn `govulncheck_failed` into something a
      // person can act on.
      expect(label, `${code} is labelled with its own code`).not.toBe(code);
    }
  });

  it("parses the real Rust source, not an empty string", () => {
    // Guards the guard: a moved or renamed array would otherwise make the
    // comparison above trivially pass against two empty lists.
    const codes = rustFailureCodes();
    expect(codes).toContain("cargo_audit_failed");
    expect(codes.length).toBeGreaterThanOrEqual(5);
  });
});
