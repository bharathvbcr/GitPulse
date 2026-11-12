import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";
import {
  WARMED_LOADERS,
  loadMergeBranchDialog,
  loadProductTour,
  loadViewGuideArt,
  warmLazyChunks,
} from "./lazyLoaders";

const here = dirname(fileURLToPath(import.meta.url));
const read = (name: string) => readFileSync(join(here, name), "utf8");

describe("warmLazyChunks", () => {
  it("warms exactly the loaders LazyMount keys its cache on, once the schedule fires", async () => {
    expect(WARMED_LOADERS).toEqual([loadViewGuideArt, loadMergeBranchDialog]);
    const calls: string[] = [];
    const a = () => (calls.push("a"), Promise.resolve({ default: {} as never }));
    const b = () => (calls.push("b"), Promise.reject(new Error("offline")));
    let fire: () => void = () => {};
    warmLazyChunks((task) => (fire = task), [a, b]);
    expect(calls).toEqual([]);
    fire();
    expect(calls).toEqual(["a", "b"]);
    // A failed warm-up is quiet: no unhandled rejection reaches the test runner.
    await Promise.resolve();
  });

  it("schedules through requestIdleCallback when the webview has it, a timer when it does not", () => {
    vi.useFakeTimers();
    try {
      const idle = vi.fn();
      vi.stubGlobal("requestIdleCallback", idle);
      warmLazyChunks(undefined, []);
      expect(idle).toHaveBeenCalledTimes(1);
      vi.stubGlobal("requestIdleCallback", undefined);
      const timer = vi.spyOn(globalThis, "setTimeout");
      warmLazyChunks(undefined, []);
      expect(timer).toHaveBeenCalled();
    } finally {
      vi.unstubAllGlobals();
      vi.useRealTimers();
    }
  });
});

describe("one loader per deferred component", () => {
  it("has every call site import the shared loader instead of declaring its own", () => {
    // A private `() => import(...)` would be a different cache key, so the warm-up
    // would fetch a chunk the call site then loads and caches again.
    expect(read("BranchList.svelte")).toContain('import { loadMergeBranchDialog } from "./lazyLoaders";');
    expect(read("ViewGuideCard.svelte")).toContain('import { loadViewGuideArt } from "./lazyLoaders";');
    // The tour mounts through its gate (onboarding/ProductTourGate.svelte),
    // which App and the onboarding harness both render.
    expect(read("onboarding/ProductTourGate.svelte")).toContain('import { loadProductTour } from "../lazyLoaders";');
    for (const file of ["BranchList.svelte", "ViewGuideCard.svelte", "../../App.svelte", "onboarding/ProductTourGate.svelte"]) {
      expect(read(file)).not.toMatch(/=>\s*import\(\s*["'][^"']*\/(MergeBranchDialog|ViewGuideArt|ProductTour)\.svelte/);
    }
    expect(read("../../../harness/OnboardingHost.svelte")).toContain("onboarding/ProductTourGate.svelte");
    expect(loadProductTour).toBeTypeOf("function");
  });
});
