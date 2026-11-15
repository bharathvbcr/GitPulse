import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { get, writable } from "svelte/store";
import {
  applyResourceBudgets,
  bindResourceBudgets,
  COMPACT_BUDGETS,
  type ResourceBudgets,
  type ResourceBudgetTargets,
} from "./resourceBudgets";
import { MAX_VISIBLE_SESSIONS } from "./terminal/attendance";
import { GPU_HIDE_GRACE_MS, gpuPolicy, gpuSlots } from "./terminal/gpuRenderer";
import { GRAPH_CACHE_LIMIT } from "./stores/graphLimits";
import { graphStore } from "./stores/graphStore";

function targets() {
  const calls = { cap: [] as number[], grace: [] as number[], limit: [] as number[] };
  const policy = {
    _grace: 123,
    get hideGraceMs() {
      return this._grace;
    },
    set hideGraceMs(value: number) {
      this._grace = value;
      calls.grace.push(value);
    },
  };
  const t: ResourceBudgetTargets = {
    slots: { setCap: (n) => void calls.cap.push(n) },
    policy,
    graph: { setCacheLimit: (n) => void calls.limit.push(n) },
  };
  return { t, calls, policy };
}

const roomy: ResourceBudgets = { gpu_contexts: 8, gpu_hide_grace_ms: 300_000, graph_cache_repositories: 6 };

describe("the singletons start at the compact budgets", () => {
  it("never allocate roomy-sized pools before a profile arrives", () => {
    expect(COMPACT_BUDGETS).toEqual({ gpu_contexts: 2, gpu_hide_grace_ms: 15_000, graph_cache_repositories: 2 });
    expect(gpuSlots.cap()).toBe(COMPACT_BUDGETS.gpu_contexts);
    expect(gpuPolicy.hideGraceMs).toBe(COMPACT_BUDGETS.gpu_hide_grace_ms);
    expect(graphStore.cacheLimit()).toBe(COMPACT_BUDGETS.graph_cache_repositories);
  });
});

describe("applyResourceBudgets", () => {
  it("hands each consumer its own budget", () => {
    const { t, calls } = targets();
    applyResourceBudgets(roomy, t);
    expect(calls).toEqual({ cap: [8], grace: [300_000], limit: [6] });
  });

  it("ignores a field that is not a usable number and applies the rest", () => {
    const { t, calls } = targets();
    applyResourceBudgets(
      { gpu_contexts: Number.NaN, gpu_hide_grace_ms: Number.POSITIVE_INFINITY, graph_cache_repositories: 3 },
      t,
    );
    expect(calls).toEqual({ cap: [], grace: [], limit: [3] });
  });

  it("floors fractions and never goes below one context or one repository", () => {
    const { t, calls } = targets();
    applyResourceBudgets({ gpu_contexts: 0, gpu_hide_grace_ms: -5, graph_cache_repositories: 2.9 }, t);
    expect(calls).toEqual({ cap: [1], grace: [0], limit: [2] });
  });

  it("takes an absurdly large budget at face value without overflowing", () => {
    const { t, calls } = targets();
    applyResourceBudgets({ gpu_contexts: 1e9, gpu_hide_grace_ms: 1e12, graph_cache_repositories: 1e9 }, t);
    expect(calls).toEqual({ cap: [1e9], grace: [1e12], limit: [1e9] });
  });
});

describe("bindResourceBudgets", () => {
  it("uses the compact budgets until the first profile, then follows it", () => {
    const { t, calls } = targets();
    const profile = writable<{ budgets: ResourceBudgets } | null>(null);
    const stop = bindResourceBudgets(profile, t);
    expect(calls).toEqual({ cap: [2], grace: [15_000], limit: [2] });
    profile.set({ budgets: roomy });
    expect(calls.cap).toEqual([2, 8]);
    expect(calls.grace).toEqual([15_000, 300_000]);
    expect(calls.limit).toEqual([2, 6]);
    stop();
    profile.set({ budgets: COMPACT_BUDGETS });
    expect(calls.cap).toEqual([2, 8]);
  });

  it("is harmless when the same profile arrives twice", () => {
    const { t, calls } = targets();
    const profile = writable<{ budgets: ResourceBudgets } | null>({ budgets: roomy });
    bindResourceBudgets(profile, t);
    profile.set({ budgets: { ...roomy } });
    profile.set({ budgets: { ...roomy } });
    expect(calls.cap.every((n) => n === 8)).toBe(true);
    expect(get(profile)?.budgets).toEqual(roomy);
  });
});

describe("roomy is what shipped before there was a profile", () => {
  it("equals the three constants the budgets replaced", () => {
    expect(roomy.gpu_contexts).toBe(MAX_VISIBLE_SESSIONS);
    expect(roomy.gpu_hide_grace_ms).toBe(GPU_HIDE_GRACE_MS);
    expect(roomy.graph_cache_repositories).toBe(GRAPH_CACHE_LIMIT);
  });

});

/**
 * The page carries one row of the Rust table, the compact one, because it is
 * what every consumer needs before the first status arrives. This reads that
 * table out of `profile.rs` so the copy cannot drift from its owner, and
 * reads the roomy row too, which must stay what shipped before profiles.
 */
describe("the budgets the page carries are the Rust table's rows", () => {
  const source = readFileSync(resolve(__dirname, "../../src-tauri/src/sysload/profile.rs"), "utf8");

  const row = (tier: "Compact" | "Standard" | "Roomy"): ResourceBudgets => {
    const block = new RegExp(`Self::${tier} => ResourceBudgets \\{([^}]*)\\}`).exec(source);
    expect(block, `the ${tier} arm of ProfileTier::budgets`).not.toBeNull();
    const body = block?.[1] ?? "";
    const number = (field: keyof ResourceBudgets): number => {
      const match = new RegExp(`${field}:\\s*([0-9_ *]+),`).exec(body);
      expect(match, `${tier}.${field}`).not.toBeNull();
      return (match?.[1] ?? "")
        .split("*")
        .reduce((product, factor) => product * Number(factor.replace(/_/g, "").trim()), 1);
    };
    return {
      gpu_contexts: number("gpu_contexts"),
      gpu_hide_grace_ms: number("gpu_hide_grace_ms"),
      graph_cache_repositories: number("graph_cache_repositories"),
    };
  };

  it("COMPACT_BUDGETS is the compact row", () => {
    expect(COMPACT_BUDGETS).toEqual(row("Compact"));
  });

  it("the roomy row is the three constants the budgets replaced", () => {
    expect(row("Roomy")).toEqual(roomy);
  });

  it("the three rows ascend, so a smaller machine never holds more", () => {
    const [c, s, r] = [row("Compact"), row("Standard"), row("Roomy")];
    for (const field of ["gpu_contexts", "gpu_hide_grace_ms", "graph_cache_repositories"] as const) {
      expect(c[field], field).toBeLessThan(s[field]);
      expect(s[field], field).toBeLessThan(r[field]);
    }
  });
});
