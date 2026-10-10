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
  it("flushes on page hide for the life of the store, not only while the poll runs", () => {
    const ensureIdx = repoStore.indexOf("function ensureStatusPoll()");
    const stopIdx = repoStore.indexOf("function stopStatusPoll()");
    const stopBody = repoStore.slice(stopIdx, repoStore.indexOf("async function runStatusPoll()"));
    expect(ensureIdx).toBeGreaterThan(-1);
    expect(stopIdx).toBeGreaterThan(ensureIdx);
    expect(stopBody).not.toContain("flushOnPageHide");
    // Bound through the shared wiring, which listens on window — `pagehide`
    // is fired there — and joins the registry the native exit flushes.
    // repoStore.test.ts drives it ("writes a pending workspace change...").
    expect(repoStore).toContain("flushOnPageHide(() => flushPersist(true));");
    expect(repoStore).not.toContain('document.addEventListener("pagehide"');
  });

  it("installs the quit flush once", () => {
    // One registration at store creation; nothing re-binds or removes it.
    expect(repoStore.match(/flushOnPageHide\(/g)?.length).toBe(1);
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
