import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { LISTING_RETRY_DELAYS_MS, retryListing } from "./retryListing";

describe("retryListing", () => {
  const noSleep = async () => {};
  it("returns the first success without waiting", async () => {
    const slept: number[] = [];
    expect(await retryListing(async () => 7, { sleep: async (ms) => { slept.push(ms); } })).toBe(7);
    expect(slept).toEqual([]);
  });

  it("a transient failure still restores: tries again after each pause", async () => {
    let calls = 0;
    const slept: number[] = [];
    const result = await retryListing(async () => { calls += 1; if (calls < 3) throw new Error("host busy"); return "listed"; }, { sleep: async (ms) => { slept.push(ms); } });
    expect(result).toBe("listed");
    expect(calls).toBe(3);
    expect(slept).toEqual([...LISTING_RETRY_DELAYS_MS]);
  });

  it("is bounded: three tries, then the last error stands", async () => {
    let calls = 0;
    await expect(retryListing(async () => { calls += 1; throw new Error(`failure ${calls}`); }, { sleep: noSleep })).rejects.toThrow("failure 3");
    expect(calls).toBe(LISTING_RETRY_DELAYS_MS.length + 1);
  });
});

describe("retryListing stays out of the terminal dock's modules", () => {
  it("imports nothing, so the entry chunk's session bridge does not pull in the split-tree model", () => {
    const here = dirname(fileURLToPath(import.meta.url));
    expect(readFileSync(join(here, "retryListing.ts"), "utf8")).not.toMatch(/^\s*import\s/m);
    const bridge = readFileSync(join(here, "..", "components", "SessionNotificationBridge.svelte"), "utf8");
    expect(bridge).toContain('from "../terminal/retryListing"');
    expect(bridge).not.toMatch(/from\s+["'][^"']*\/reloadRestore["']/);
  });
});
