import { readdirSync, readFileSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * A bare `setInterval` keeps waking the renderer while the window is hidden:
 * on the long-lived WebContent process that is a timer firing (and, for a
 * "now" ticker, invalidating a derived row list) once a second for as long as
 * the app is open, for a label nobody can see. Every recurring UI timer goes
 * through `createAdaptiveTimer`, which stops while hidden and rejoins with
 * one immediate tick.
 *
 * The only bare callers allowed are ones that guard themselves with
 * `readBackgroundDocument`, listed here with the reason.
 */
const SELF_GUARDED: ReadonlyMap<string, string> = new Map([
  ["lib/components/TaskAgentPanel.svelte", "starts and stops its own interval from bindForegroundChanges"],
]);

const SRC = fileURLToPath(new URL("../../", import.meta.url));

function sources(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...sources(path));
    else if (/\.(svelte|ts)$/.test(entry.name) && !/\.(test|spec)\.ts$/.test(entry.name)) out.push(path);
  }
  return out;
}

/** Calls to the global `setInterval`, excluding members, declarations and comments. */
function bareIntervalCalls(source: string): number[] {
  const lines = source.split("\n");
  const hits: number[] = [];
  lines.forEach((line, index) => {
    const code = line.trim();
    if (code.startsWith("*") || code.startsWith("//") || code.startsWith("/*")) return;
    const declaration = /setInterval\(\s*\w+\s*:/.test(line);
    if (/(^|[^.\w])setInterval\s*\(/.test(line) && !declaration) hits.push(index + 1);
  });
  return hits;
}

describe("recurring UI timers stop while the window is hidden", () => {
  it("no component or module calls bare setInterval unless it guards itself", () => {
    const offenders: string[] = [];
    for (const file of sources(SRC)) {
      const name = relative(SRC, file).split(sep).join("/");
      if (SELF_GUARDED.has(name)) continue;
      const lines = bareIntervalCalls(readFileSync(file, "utf8"));
      if (lines.length > 0) offenders.push(`${name}:${lines.join(",")}`);
    }
    expect(offenders).toEqual([]);
  });

  it("each self-guarded exception still guards itself", () => {
    for (const name of SELF_GUARDED.keys()) {
      const source = readFileSync(join(SRC, name), "utf8");
      expect(source, name).toContain("readBackgroundDocument");
      expect(bareIntervalCalls(source).length, name).toBeGreaterThan(0);
    }
  });
});
