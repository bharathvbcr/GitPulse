import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { REGISTERED_VIEWS } from "./lib/views/viewRegistry";

const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "App.svelte"), "utf8");

/**
 * The branch body App.svelte renders for one tab: everything between that
 * tab's `activeTab === "<id>"` test and the next branch in the chain.
 */
function branchFor(tab: string): string {
  const start = source.indexOf(`$repoStore.activeTab === "${tab}"`);
  if (start === -1) return "";
  const rest = source.slice(start);
  const next = rest.slice(1).search(/\{:else if |\{\/if\}/);
  return next === -1 ? rest : rest.slice(0, next + 1);
}

/**
 * Derived from the registry rather than a hand-listed set of view names.
 *
 * A list has to be remembered: add an `inspect`/`more` view next month, forget
 * to add it to the list, and the suite still passes while the entry chunk
 * quietly regrows until the bundle-budget plugin fails on some unrelated
 * change. Deriving it means a new view is covered the moment it is registered.
 */
describe("view code splitting follows the registry", () => {
  const deferred = REGISTERED_VIEWS.filter((view) => view.menuGroup !== "work");

  /**
   * Tabs rendered outside the `activeTab` if-chain, with the guard that owns
   * their render site instead. Terminal is kept mounted behind `display:none`
   * so the PTY survives a tab switch, so it has no branch in the chain. This
   * is the one documented exception — everything else must be found by the
   * chain, so a new view cannot quietly opt out by landing here.
   */
  const RENDERED_OUTSIDE_CHAIN: Partial<Record<string, string>> = {
    terminal: "{#if terminalMounted}",
  };

  function renderSite(tab: string): string {
    const guard = RENDERED_OUTSIDE_CHAIN[tab];
    if (guard) {
      const at = source.indexOf(guard);
      return at === -1 ? "" : source.slice(at, at + 600);
    }
    return branchFor(tab);
  }

  /**
   * Guards the cases where the checks below would pass by checking nothing:
   * an empty `deferred` makes the `it.each` generate zero tests, and an empty
   * `work` group means the filter stopped discriminating. Either would report
   * a green suite for a split that is no longer verified at all.
   */
  it("splits the registry into two non-empty buckets", () => {
    const eager = REGISTERED_VIEWS.filter((view) => view.menuGroup === "work");
    expect(eager.length).toBeGreaterThan(0);
    expect(deferred.length).toBeGreaterThan(0);
    expect(eager.length + deferred.length).toBe(REGISTERED_VIEWS.length);
  });

  it.each(deferred.map((view) => [view.id, view.label]))(
    "renders %s (%s) through LazyView, not a static import",
    (id) => {
      const site = renderSite(id);
      expect(site, `no render site found for the "${id}" tab`).not.toBe("");
      expect(site, `the "${id}" tab is rendered eagerly, so it ships in the entry chunk`).toContain(
        "<LazyView",
      );
    },
  );
});
