import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * Rust emits events by name; the frontend listens by name. Nothing checks that
 * the two agree, and a mismatch is silent in both directions: an emit nobody
 * hears looks like a working feature that never fires — the watcher would
 * announce every filesystem change into the void and the app would simply
 * never refresh — and a listener for an event nobody sends waits forever.
 *
 * Neither produces an error, a warning, or a failing type check.
 */
const ROOT = fileURLToPath(new URL("..", import.meta.url));

function filesUnder(dir: string, exts: string[]): string[] {
  return readdirSync(dir).flatMap((entry) => {
    const full = path.join(dir, entry);
    if (statSync(full).isDirectory()) return filesUnder(full, exts);
    return exts.some((e) => entry.endsWith(e)) ? [full] : [];
  });
}

const rustSource = filesUnder(path.join(ROOT, "src-tauri", "src"), [".rs"])
  .map((f) => readFileSync(f, "utf8"))
  .join("\n");

// The whole frontend, not just src/lib: the watcher listener lives in App.svelte.
const frontendSource = filesUnder(path.join(ROOT, "src"), [".ts", ".svelte"])
  .map((f) => readFileSync(f, "utf8"))
  .join("\n");

/** Event names Rust emits, from `.emit("name"` and from EVENT constants. */
function emittedEvents(): string[] {
  const names = new Set<string>();
  for (const m of rustSource.matchAll(/\.emit(?:_to)?\(\s*"([a-z][a-z0-9-]*)"/g)) {
    names.add(m[1]);
  }
  for (const m of rustSource.matchAll(
    /const [A-Z_]*EVENT[A-Z_]*: &str = "([a-z][a-z0-9-]*)"/g,
  )) {
    names.add(m[1]);
  }
  return [...names].sort();
}

/**
 * Events Rust emits for a listener in the same process, never the frontend.
 * Each names the one file allowed to emit it, so an allowance cannot quietly
 * become a production emit nobody hears.
 */
const PROCESS_LOCAL: Record<string, string> = {
  // Session output travels on a per-session Channel. `OutputSink::events` re-emits
  // it as this event so the tests and benches that count messages can listen.
  "terminal-output": "src-tauri/src/terminal/sink.rs",
};

describe("cross-language event contract", () => {
  const emitted = emittedEvents();

  for (const [event, owner] of Object.entries(PROCESS_LOCAL)) {
    it(`"${event}" is emitted only by ${owner}`, () => {
      const emitters = filesUnder(path.join(ROOT, "src-tauri", "src"), [".rs"])
        .filter((f) => new RegExp(`\\.emit(?:_to)?\\(\\s*"${event}"`).test(readFileSync(f, "utf8")))
        .map((f) => path.relative(ROOT, f).split(path.sep).join("/"));
      expect(emitters).toEqual([owner]);
    });
  }

  it("found the events to check", () => {
    // A regex that silently matched nothing would make this suite vacuous.
    expect(emitted.length).toBeGreaterThanOrEqual(5);
    expect(emitted).toContain("repo-changed");
  });

  for (const event of emitted.filter((name) => !(name in PROCESS_LOCAL))) {
    it(`"${event}" is listened for somewhere in the frontend`, () => {
      expect(
        frontendSource.includes(`"${event}"`),
        `Rust emits "${event}" and nothing in the frontend names it`,
      ).toBe(true);
    });
  }

  it("every event emitted through a named constant is also emitted somewhere", () => {
    // A constant defined and never emitted is a listener contract with no
    // producer — the mirror of the case above.
    for (const m of rustSource.matchAll(
      /const ([A-Z_]*EVENT[A-Z_]*): &str = "([a-z][a-z0-9-]*)"/g,
    )) {
      const [, constName, value] = m;
      const usedByName = new RegExp(`\\b${constName}\\b`, "g");
      const uses = [...rustSource.matchAll(usedByName)].length;
      expect(uses, `${constName} ("${value}") is defined but never used`).toBeGreaterThan(1);
    }
  });
});
