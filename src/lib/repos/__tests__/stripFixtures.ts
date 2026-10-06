/** Test-only builders for strip layouts. Lives under __tests__, so coverage and the app never see it. */
import type { OpenRepoTab } from "../../stores/repoStore";

export function repoTab(path: string, extras: Partial<OpenRepoTab> = {}): OpenRepoTab {
  const name = path.split("/").filter(Boolean).pop() ?? path;
  return {
    id: path,
    path,
    name,
    label: name,
    pinned: false,
    group: null,
    color: null,
    family: null,
    familyRoot: null,
    isActive: false,
    isBare: false,
    isDirty: false,
    isLoading: false,
    error: null,
    currentBranch: null,
    conflictedCount: 0,
    changedCount: 0,
    ...extras,
  };
}

/** A checkout of the repository at `root`: family key = `${root}/.git`. */
export function checkout(root: string, path: string, extras: Partial<OpenRepoTab> = {}): OpenRepoTab {
  return repoTab(path, { family: `${root}/.git`, familyRoot: root, ...extras });
}

/** Deterministic PRNG (mulberry32) so a failing fuzz case reproduces from its seed. */
export function rng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
