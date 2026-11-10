import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { compileModule } from "svelte/compiler";
import { afterAll, describe, expect, it } from "vitest";

/**
 * Runs Svelte's real client reactivity runtime, which vitest cannot (it
 * resolves the server build, where `$derived` is plain and `$effect` is a
 * no-op; see the verification-techniques memory). The fixture is compiled with
 * `compileModule(..., { generate: "client" })`, written inside the repo tree so
 * `svelte` resolves, and run under `node --conditions=browser`. The helper it
 * imports is the real `recomputeWhileActive.ts`, loaded by Node's own type
 * stripping.
 *
 * What this reaches: the derived chain Fleet is built from, driven by real
 * store publishes. What it cannot: `FleetView.svelte` itself, or any component.
 * Mounting needs a DOM and the component imports the Tauri bridge, so the
 * component's wiring is guarded by the structure scan at the bottom.
 */
const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const scratch = join(ROOT, "tmp");
mkdirSync(scratch, { recursive: true });
const work = mkdtempSync(join(scratch, "reactivity-"));
afterAll(() => rmSync(work, { recursive: true, force: true }));

function run(name: string, source: string): Record<string, unknown> {
  const compiled = compileModule(source, { generate: "client", filename: `${name}.svelte.js` });
  const file = join(work, `${name}.mjs`);
  writeFileSync(file, compiled.js.code);
  const out = execFileSync(process.execPath, ["--conditions=browser", file], { cwd: ROOT, encoding: "utf8", timeout: 60_000 });
  return JSON.parse(out.trim().split("\n").at(-1) ?? "{}");
}

const HELPER = "../../src/lib/runtime/recomputeWhileActive.ts";

/** A source store whose every publish is a new object, like repoStore. */
const STORES = `
import { writable, fromStore } from "svelte/store";
import { flushSync } from "svelte";
const repo = writable({ tabs: 1 });
const fleet = writable({ snapshot: 1 });
const publishRepo = () => repo.update((s) => ({ tabs: s.tabs + 1 }));
const publishFleet = () => fleet.update((s) => ({ snapshot: s.snapshot + 1 }));
const repoNow = fromStore(repo);
const fleetNow = fromStore(fleet);
`;

describe("derived work under Svelte's reactivity runtime", () => {
  it("control: a plain derived recomputes on every publish, so the harness can see work", () => {
    const result = run("control", `${STORES}
      const counts = { facts: 0 };
      const seen = [];
      const stop = $effect.root(() => {
        const facts = $derived.by(() => { counts.facts += 1; return repoNow.current.tabs; });
        $effect(() => { seen.push(facts); });
      });
      flushSync();
      for (let i = 0; i < 5; i++) { publishRepo(); flushSync(); }
      console.log(JSON.stringify({ counts, seen }));
      stop();
    `);
    expect(result).toEqual({ counts: { facts: 6 }, seen: [1, 2, 3, 4, 5, 6] });
  });

  it("a hidden Fleet-shaped chain does no work on publishes, and is current the moment it is shown", () => {
    const result = run("fleet", `${STORES}
      import { recomputeWhileActive } from "${HELPER}";
      const view = $state({ active: false });
      const counts = { facts: 0, recents: 0, rows: 0 };
      const seen = [];
      const stop = $effect.root(() => {
        const facts = $derived.by(recomputeWhileActive(() => view.active, () => { counts.facts += 1; return repoNow.current.tabs; }));
        const recents = $derived.by(recomputeWhileActive(() => view.active, () => { counts.recents += 1; return facts * 10 + repoNow.current.tabs; }));
        const rows = $derived.by(recomputeWhileActive(() => view.active, () => { counts.rows += 1; return [facts, recents, fleetNow.current.snapshot]; }));
        $effect(() => { seen.push(rows); });
      });
      flushSync();
      const mounted = { ...counts };
      for (let i = 0; i < 5; i++) { publishRepo(); publishFleet(); flushSync(); }
      const hidden = { ...counts };
      view.active = true;
      flushSync();
      const shown = { ...counts };
      publishRepo(); flushSync();
      const live = { ...counts };
      view.active = false;
      flushSync();
      publishRepo(); publishFleet(); flushSync();
      const hiddenAgain = { ...counts };
      console.log(JSON.stringify({ mounted, hidden, shown, live, hiddenAgain, seen }));
      stop();
    `);
    // Mounted hidden: each derived computed once, so the view is not empty.
    expect(result.mounted).toEqual({ facts: 1, recents: 1, rows: 1 });
    // Ten publishes while hidden: nothing recomputed.
    expect(result.hidden).toEqual({ facts: 1, recents: 1, rows: 1 });
    // Shown: each recomputed once, from the latest data (tabs 6, snapshot 6).
    expect(result.shown).toEqual({ facts: 2, recents: 2, rows: 2 });
    // What the view showed: the mount, the moment it was shown (latest data), the live publish; nothing while hidden.
    expect(result.seen).toEqual([[1, 11, 1], [6, 66, 6], [7, 77, 6]]);
    // Live while shown: a publish recomputes the chain that reads it.
    expect((result.live as Record<string, number>).facts).toBe(3);
    // Hidden again: quiet again.
    expect(result.hiddenAgain).toEqual(result.live);
  });
});

describe("what the technique cannot reach", () => {
  it("FleetView and the GitHub panels are wired as the behaviour assumes (structure, not exact text)", async () => {
    const { readFileSync } = await import("node:fs");
    const fleet = readFileSync(join(ROOT, "src/lib/components/FleetView.svelte"), "utf8");
    // Three store-fed deriveds gated on the `active` prop.
    expect(fleet.match(/recomputeWhileActive\(\s*\(\)\s*=>\s*active\b/g)).toHaveLength(3);
    expect(fleet).toMatch(/let\s*\{\s*active\s*=\s*true\s*\}/);
    const panel = readFileSync(join(ROOT, "src/lib/components/GitHubPanel.svelte"), "utf8");
    // The mount path is the only reader that may reuse a fresh read.
    expect(panel.match(/loadFor\(\s*repo\s*,\s*false\s*\)/g)).toHaveLength(1);
  });
});
