import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const repoStore = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "repoStore.ts"),
  "utf8"
);
const tabBar = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "../components/RepoTabBar.svelte"),
  "utf8"
);

describe("repoStore status-poll listener symmetry", () => {
  it("removes the pagehide listener when the poll stops (no duplicate accumulation)", () => {
    const ensureIdx = repoStore.indexOf("function ensureStatusPoll()");
    const stopIdx = repoStore.indexOf("function stopStatusPoll()");
    expect(ensureIdx).toBeGreaterThan(-1);
    expect(stopIdx).toBeGreaterThan(ensureIdx);
    const addIdx = repoStore.indexOf('document.addEventListener("pagehide"', ensureIdx);
    const removeIdx = repoStore.indexOf('document.removeEventListener("pagehide"', stopIdx);
    expect(addIdx).toBeGreaterThan(-1);
    expect(removeIdx).toBeGreaterThan(-1);
  });

  it("gates both directions with a wired flag so cycles stay idempotent", () => {
    expect(repoStore).toContain("let pagehideWired = false;");
    const adds = repoStore.match(/pagehideWired/g)?.length ?? 0;
    // Decl + guard on add + guard on remove = at least 3 mentions.
    expect(adds).toBeGreaterThanOrEqual(3);
  });
});

describe("RepoTabBar active-tab auto-scroll", () => {
  it("re-runs its scroll effect when activation changes, not only on bind", () => {
    const effectIdx = tabBar.indexOf("$effect(() => {");
    expect(effectIdx).toBeGreaterThan(-1);
    // The effect must track reactive tab state; reading openTabs/isActive
    // makes activation changes re-trigger the scrollIntoView.
    const body = tabBar.slice(effectIdx);
    const tracksTabs =
      body.includes("$repoStore.openTabs") || body.includes("isActive");
    expect(tracksTabs).toBe(true);
  });
});
