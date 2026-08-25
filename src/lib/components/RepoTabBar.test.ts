import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import { get } from "svelte/store";
import RepoTabBar from "./RepoTabBar.svelte";
import { repoStore } from "../stores/repoStore";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "RepoTabBar.svelte"),
  "utf8"
);

async function seedTwoTabs() {
  // Broken opens still create tabs (with an error), which is all the bar
  // needs to render. Two tabs exercise the roving tabindex split.
  await repoStore.openRepo("/r/tabbar-alpha", { allowBroken: true });
  await repoStore.openRepo("/r/tabbar-beta", { allowBroken: true });
}

describe("RepoTabBar a11y", () => {
  it("renders tabs with roving tabindex, keyboard pin docs, and labelled close buttons", async () => {
    await seedTwoTabs();
    expect(get(repoStore).openTabs.length).toBeGreaterThanOrEqual(2);

    const { body } = render(RepoTabBar);

    // Roving tabindex: exactly one tab (the active one) is tab-stop 0.
    const zeroStops = body.match(/tabindex="0"/g) ?? [];
    const minusOnes = body.match(/tabindex="-1"/g) ?? [];
    // One for the tablist container + one for the active tab.
    expect(zeroStops.length).toBe(2);
    expect(minusOnes.length).toBe(get(repoStore).openTabs.length - 1);

    // Keyboard pin affordance is documented on the element and in the title.
    expect(body).toContain('aria-keyshortcuts="Enter p"');
    expect(body).toContain("P to pin");

    // Close buttons speak their target, not just "Close".
    expect(body).toMatch(/aria-label="Close [^"]+"/);
  });

  it("routes arrow keys through the shared roving-focus helper", () => {
    // The tablist owns a keydown handler delegating focus math to
    // dom/rovingFocus (unit-tested there).
    expect(source).toContain('onkeydown={onTablistKeydown}');
    expect(source).toContain("nextRovingIndex(current, tabs.length, key)");
  });
});
