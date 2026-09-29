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

describe("repoStore quit flush", () => {
  it("flushes on pagehide for the life of the store, not only while the poll runs", () => {
    const ensureIdx = repoStore.indexOf("function ensureStatusPoll()");
    const stopIdx = repoStore.indexOf("function stopStatusPoll()");
    const stopBody = repoStore.slice(stopIdx, repoStore.indexOf("async function runStatusPoll()"));
    expect(ensureIdx).toBeGreaterThan(-1);
    expect(stopIdx).toBeGreaterThan(ensureIdx);
    expect(stopBody).not.toContain("pagehide");
    expect(repoStore).toContain('document.addEventListener("pagehide"');
    expect(repoStore).toContain("flushPersist(true)");
  });

  it("installs the quit flush once", () => {
    expect(repoStore).toContain("let pagehideWired = false;");
    const adds = repoStore.match(/pagehideWired/g)?.length ?? 0;
    expect(adds).toBeGreaterThanOrEqual(2);
    expect(repoStore).not.toContain('document.removeEventListener("pagehide"');
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
