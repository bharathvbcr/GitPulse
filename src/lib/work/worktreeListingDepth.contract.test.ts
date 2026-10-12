import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * `cmd_list_worktrees` measures each worktree to the depth it is asked for,
 * and a listing that does not ask is the cheap one (paths, branches, heads).
 * The full scan costs git children per worktree, so every caller names what
 * it reads; one that forgot used to be handed — and pay for — the full scan.
 */
const srcRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

function productionSources(directory: string): string[] {
  const files: string[] = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...productionSources(path));
    } else if (/\.(?:ts|svelte)$/.test(entry.name) && !/\.(?:test|spec|contract)\.ts$/.test(entry.name)) {
      files.push(path);
    }
  }
  return files;
}

/** Every production call: the command name as an argument, then its args. */
function listingCalls(): Array<{ location: string; args: string | null }> {
  const calls: Array<{ location: string; args: string | null }> = [];
  for (const path of productionSources(srcRoot)) {
    const source = readFileSync(path, "utf8");
    for (const match of source.matchAll(/"cmd_list_worktrees"\s*,/g)) {
      const line = source.slice(0, match.index).split("\n").length;
      const literal = /^\s*(\{[^}]*\})/.exec(source.slice((match.index ?? 0) + match[0].length));
      calls.push({ location: `${relative(srcRoot, path)}:${line}`, args: literal ? literal[1] : null });
    }
  }
  return calls;
}

const depthOf = (args: string | null) => (args ? /\bdepth:\s*"(listing|dirty|full)"/.exec(args)?.[1] ?? null : null);

describe("worktree listing depth", () => {
  it("names a depth at every call, in an argument literal", () => {
    const calls = listingCalls();
    expect(calls.length).toBeGreaterThanOrEqual(4);
    const unnamed = calls.filter((call) => depthOf(call.args) === null).map((call) => call.location);
    expect(unnamed).toEqual([]);
  });

  it("asks for what each caller reads", () => {
    const byFile = new Map<string, string[]>();
    for (const call of listingCalls()) {
      const file = call.location.replace(/:\d+$/, "");
      byFile.set(file, [...(byFile.get(file) ?? []), depthOf(call.args) ?? "unnamed"]);
    }
    expect(Object.fromEntries(byFile)).toEqual({
      // Renders diff stats, divergence and dev-server routes per row.
      "lib/components/WorktreesPanel.svelte": ["full"],
      // Dirty counts only.
      "lib/work/load.ts": ["dirty"],
      // Branch, head and the dirty counts a merge or removal is judged on.
      "lib/workbench/attemptWorktree.ts": ["dirty"],
      // The attempt's head only.
      "lib/workbench/waveHost.ts": ["listing"],
    });
  });
});
