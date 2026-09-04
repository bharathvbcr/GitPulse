import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import HarnessBadge from "./HarnessBadge.svelte";
import { MANVI_FOCUS_IDS } from "../ui/manviFocus";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "HarnessBadge.svelte"),
  "utf8",
);

/** Every destination the header chips route to, in markup order. */
const chipTargets = [...source.matchAll(/openManvi\("([a-z]+)"\)/g)].map(
  (match) => match[1],
);

describe("HarnessBadge destinations", () => {
  it("sends each chip to its own MANVI section", () => {
    // The gap: shield, model and verdict all called setActiveTab("manvi"),
    // which lands on the Ops pane — a different pane from two of the three
    // subjects, with the third several cards down the page.
    expect(chipTargets.length).toBeGreaterThanOrEqual(3);
    expect(new Set(chipTargets).size).toBe(chipTargets.length);
    for (const target of chipTargets) {
      expect(MANVI_FOCUS_IDS).toContain(target);
    }
  });

  it("routes every chip through the focus request, never straight to the tab", () => {
    expect(source.match(/setActiveTab\("work", "policy"\)/g)).toHaveLength(1);
    expect(source).toContain("requestManviFocus(target);");
  });

  it("tells each chip's tooltip where the click lands", () => {
    // Guarded, so an empty match list cannot pass this as if it had checked.
    expect(chipTargets.length).toBeGreaterThanOrEqual(3);
    for (const target of chipTargets) {
      expect(source).toContain(`manviFocusHint("${target}")`);
    }
  });
});

describe("HarnessBadge reachability", () => {
  it("does not offer a link to a view that cannot open", () => {
    // MANVI is a repository view: with no repository open there is no session
    // to switch tabs on, so the click did nothing at all. The chip keeps
    // reporting status; it just stops claiming to be a link.
    expect(source).toContain("let reachable = $derived(Boolean($repoStore.currentPath));");
    expect(source.match(/disabled=\{!reachable\}/g)).toHaveLength(
      chipTargets.length,
    );
    expect(source).toContain("Open a repository to reach the MANVI view.");
    // Hover styling is gated on the same condition, so a dead chip does not
    // light up under the pointer.
    expect(source).not.toMatch(/[^:]hover:/);
  });
});

describe("HarnessBadge rendering", () => {
  it("renders the harness and model chips with distinct tooltips", () => {
    const { body } = render(HarnessBadge);
    expect(body).toContain("MANVI");
    const titles = [...body.matchAll(/title="([^"]*)"/g)].map(
      (match) => match[1],
    );
    expect(titles.length).toBeGreaterThanOrEqual(2);
    expect(new Set(titles).size).toBe(titles.length);
    // Server render carries no open repository, so every chip says so rather
    // than offering a click that would do nothing.
    for (const title of titles) {
      expect(title).toContain("Open a repository to reach the MANVI view.");
    }
    expect(body).toContain("disabled");
  });
});
