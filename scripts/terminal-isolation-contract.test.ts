import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * SECURITY.md: "AI models and the MANVI sidecar have zero access to the
 * terminal PTY, its file descriptors, or keystrokes."
 *
 * That holds today because the dependency runs one way: `terminal/` calls into
 * `harness/` for the command gate, and neither `ai/` nor `harness/` references
 * the terminal at all. A single import would reverse that quietly, so the
 * direction is asserted rather than assumed.
 */
const SRC = fileURLToPath(new URL("../src-tauri/src", import.meta.url));

function rustFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((entry) => {
    const full = path.join(dir, entry);
    if (statSync(full).isDirectory()) return rustFiles(full);
    return full.endsWith(".rs") ? [full] : [];
  });
}

/** Source with `#[cfg(test)]` modules dropped, so test fixtures do not count. */
function productionSource(file: string): string {
  const text = readFileSync(file, "utf8");
  const marker = text.indexOf("#[cfg(test)]");
  return marker === -1 ? text : text.slice(0, marker);
}

describe("terminal isolation contract", () => {
  for (const module of ["ai", "harness"]) {
    it(`${module}/ never reaches the terminal or a PTY`, () => {
      const offenders: string[] = [];
      for (const file of rustFiles(path.join(SRC, module))) {
        const source = productionSource(file);
        // Word-bounded: "is_empty()" contains "pty" and is not a PTY.
        for (const pattern of [
          /\bTerminalSessions\b/,
          /\bportable_pty\b/,
          /\bcrate::terminal\b/,
          /\bMasterPty\b/,
          /\bPtyPair\b/,
        ]) {
          if (pattern.test(source)) {
            offenders.push(`${path.relative(SRC, file)} matches ${pattern}`);
          }
        }
      }
      expect(offenders).toEqual([]);
    });
  }

  it("keeps the dependency pointing from terminal to harness, not back", () => {
    // The gate judges terminal commands, which is why terminal/ imports it.
    const terminal = productionSource(path.join(SRC, "terminal", "mod.rs"));
    expect(terminal).toContain("crate::harness");
    // The reverse would give the gate — and the sidecar behind it — a route to
    // the PTY.
    for (const file of rustFiles(path.join(SRC, "harness"))) {
      expect(productionSource(file), file).not.toContain("crate::terminal");
    }
  });
});
