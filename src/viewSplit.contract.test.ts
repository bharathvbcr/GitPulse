import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { REGISTERED_VIEWS, defaultSectionFor } from "./lib/views/viewRegistry";
import type { ViewTab } from "./lib/repos/persist";

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
 * Views allowed to ship their default pane in the entry chunk, and why.
 *
 * An explicit exemption list rather than a derived one, but note which way it
 * fails: a view that is *not* listed is checked. Register a fifth view next
 * month and it is covered the moment it exists — the old form keyed off
 * `menuGroup`, and a new view landing in the wrong group escaped the check
 * silently. Adding a name here is a deliberate act with a reason attached.
 *
 * Each of these opens on a pane a session actually starts in, so deferring it
 * would trade a smaller entry chunk for a round trip on every launch. Their
 * *other* sections are still asserted lazy below, and the bundle-budget
 * plugin is the backstop on the total either way.
 */
const EAGER_DEFAULT_PANE: readonly ViewTab[] = [
  "work", // Overview — the tab a session opens on.
  "code", // Explorer — the file tree and editor, opened from the sidebar.
  "history", // Graph — the commit table the repository loads into.
];

/**
 * A view whose branch mounts a section host rather than a LazyView directly.
 *
 * Consolidation gave every view its own shell, and the shell is a few dozen
 * lines that legitimately ship in the entry chunk. What must NOT ship with it
 * are the panes, so the check follows one level of delegation and asserts the
 * property where it actually lives, rather than being relaxed.
 */
function sectionHostSource(site: string): string | null {
  const match = /<([A-Z][A-Za-z0-9]*View)\b/.exec(site);
  if (!match) return null;
  const name = match[1];
  try {
    return readFileSync(
      join(dirname(fileURLToPath(import.meta.url)), "lib", "components", `${name}.svelte`),
      "utf8",
    );
  } catch {
    return null;
  }
}

describe("view code splitting follows the registry", () => {
  it("has views to check, and exempts fewer than it checks the panes of", () => {
    // Guards the cases where the checks below would pass by checking nothing:
    // an empty registry makes `it.each` generate zero tests, and an exemption
    // list covering everything would silence the second assertion entirely.
    expect(REGISTERED_VIEWS.length).toBeGreaterThanOrEqual(4);
    const exempt = REGISTERED_VIEWS.filter((view) => EAGER_DEFAULT_PANE.includes(view.id));
    expect(exempt.length).toBeLessThan(REGISTERED_VIEWS.length);
    // Every exemption names a registered view — a stale entry here would
    // quietly exempt nothing while reading as if it exempted something.
    expect(exempt.map((view) => view.id).sort()).toEqual([...EAGER_DEFAULT_PANE].sort());
  });

  it.each(REGISTERED_VIEWS.map((view) => [view.id, view.label]))(
    "renders %s (%s) through LazyView, not a static import",
    (id) => {
      const site = branchFor(id);
      expect(site, `no render site found for the "${id}" tab`).not.toBe("");
      if (site.includes("<LazyView")) return;

      // Delegated to a section host: its panes must be lazy inside it.
      const host = sectionHostSource(site);
      expect(
        host,
        `the "${id}" tab is rendered eagerly, so it ships in the entry chunk`,
      ).not.toBeNull();

      // Every host has at least one section it defers — a host with none has
      // pulled its whole view into the entry chunk.
      expect(
        host,
        `the "${id}" section host renders every pane eagerly`,
      ).toContain("<LazyView");

      // A view not on the exemption list may not statically import any pane;
      // an exempt one may import exactly the pane its default section renders.
      if (!EAGER_DEFAULT_PANE.includes(id as ViewTab)) {
        // Shared section CHROME is not a pane: it carries no view content and
        // is imported by every host by design. Narrowing this by name keeps
        // the check on what it is actually about — a heavy pane sneaking back
        // into the entry chunk — instead of on a spelling.
        const SECTION_CHROME = ["ViewSectionBar", "ViewSectionPanel"];
        const staticPanes = [...(host ?? "").matchAll(/import\s+(\w*(?:Panel|Viewer))\s+from/g)]
          .map(([, name]) => name)
          .filter((name) => !SECTION_CHROME.includes(name));
        expect(
          staticPanes,
          `the "${id}" section host statically imports a pane, putting it back in the entry chunk`,
        ).toEqual([]);
      }
    },
  );

  it("gives every view a default section, so the exemptions describe something real", () => {
    // The exemption is "this view's *default* pane may be eager". That only
    // means anything if the view has a default section to name.
    for (const id of EAGER_DEFAULT_PANE) {
      expect(defaultSectionFor(id), `${id} is exempt but has no default section`).not.toBeNull();
    }
  });
});
