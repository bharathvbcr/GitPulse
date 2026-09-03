import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { FAIL_VERDICTS, PASS_VERDICTS } from "../src/lib/provenance/badge";

/**
 * Every verdict GitPulse writes into a git note must be one the badge
 * classifier recognises.
 *
 * The classifier fails closed: a verdict it has never heard of is rendered
 * `unrecognised`, never as a pass. That is the safe behaviour, and it is also
 * a silent one — a writer that started emitting `"green-ish"` would put a
 * permanent amber badge on every verified commit and nothing would say why.
 * This is the check that says why, at the moment the writer changes.
 *
 * Derived rather than hand-listed: the writers are found by looking for
 * `VerificationNote` constructions in the Rust tree, so a second writer added
 * later is covered without anyone remembering to add it here. A construction
 * whose verdict this check cannot resolve is a failure, not a skip — an
 * unreadable writer is exactly the case where a hand-maintained list would
 * quietly stop covering anything.
 */
// `fileURLToPath`, never `URL.pathname`: on Windows the latter yields
// "/D:/a/..." and joining it back onto a path produces "D:\D:\a\...".
const RUST_ROOT = fileURLToPath(new URL("../src-tauri/src/", import.meta.url));
const KNOWN = new Set([...PASS_VERDICTS, ...FAIL_VERDICTS]);

function rustFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((entry) => {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) return rustFiles(path);
    return path.endsWith(".rs") ? [path] : [];
  });
}

/**
 * Production source only: everything from the first `#[cfg(test)]` onwards is
 * dropped. Test fixtures deliberately construct unrecognised verdicts — that
 * is what `badge.test.ts` asserts about — and holding them to this contract
 * would make it impossible to test the failing case.
 */
function productionSource(path: string): string {
  const text = readFileSync(path, "utf8");
  const testMod = text.indexOf("#[cfg(test)]");
  return testMod === -1 ? text : text.slice(0, testMod);
}

interface Writer {
  file: string;
  expression: string;
}

function findWriters(): Writer[] {
  const writers: Writer[] = [];
  for (const file of rustFiles(RUST_ROOT)) {
    const source = productionSource(file);
    // The struct's own definition is not a construction of it.
    for (const match of source.matchAll(/VerificationNote\s*\{([\s\S]*?)\}/g)) {
      const body = match[1];
      const verdict = /(?:^|\n)\s*verdict:\s*([^,\n]+)/.exec(body);
      if (!verdict) continue;
      // Slash-separated so writer identity — which names test cases and is
      // asserted on below — does not change shape between platforms.
      writers.push({
        file: relative(RUST_ROOT, file).split(sep).join("/"),
        expression: verdict[1].trim(),
      });
    }
  }
  return writers;
}

/**
 * Resolves a `verdict:` expression to the set of strings it can produce.
 *
 * Handles the two shapes that exist: a literal, and a call to a local function
 * that returns one of a fixed set of literals. Anything else returns null,
 * which fails the test rather than passing it.
 */
function resolveVerdicts(writer: Writer): string[] | null {
  const literal = /^"([^"]*)"(?:\.to_string\(\)|\.into\(\))?$/.exec(writer.expression);
  if (literal) return [literal[1]];

  const call = /^([a-z_][a-z0-9_]*)\s*\(/.exec(writer.expression);
  if (!call) return null;

  const source = productionSource(join(RUST_ROOT, writer.file));
  const fn = new RegExp(`fn ${call[1]}\\b[^{]*\\{([\\s\\S]*?)\\n\\}`).exec(source);
  if (!fn) return null;

  const literals = [...fn[1].matchAll(/"([^"]*)"/g)].map((m) => m[1]);
  return literals.length > 0 ? literals : null;
}

describe("every verdict GitPulse writes is one the badge understands", () => {
  const writers = findWriters();

  it("finds the writers at all", () => {
    // A rename that made this scan match nothing would turn every assertion
    // below into a vacuous pass.
    expect(writers.length).toBeGreaterThan(0);
    expect(writers.map((w) => w.file)).toContain("ci_local.rs");
  });

  for (const writer of writers) {
    it(`${writer.file}: ${writer.expression}`, () => {
      const verdicts = resolveVerdicts(writer);
      expect(
        verdicts,
        `this contract could not work out which verdict ${writer.file} writes from ` +
          `${JSON.stringify(writer.expression)}. Resolve it here rather than leaving the ` +
          `writer unchecked.`,
      ).not.toBeNull();

      for (const verdict of verdicts!) {
        expect(
          KNOWN.has(verdict.toLowerCase()),
          `${writer.file} writes verdict ${JSON.stringify(verdict)}, which ` +
            `freshnessBadge does not recognise — it would render "unrecognised" ` +
            `forever. Add it to PASS_VERDICTS or FAIL_VERDICTS in ` +
            `src/lib/provenance/badge.ts.`,
        ).toBe(true);
      }
    });
  }
});
