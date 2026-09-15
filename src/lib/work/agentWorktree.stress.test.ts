import { describe, expect, it } from "vitest";
import { agentKind, agentKindsOn, agentLayout, agentSessionSlug, isAgentWorktree } from "./agentWorktree";

/**
 * Adversarial and randomised input for the agent-worktree detector.
 *
 * The corpus in `agentWorktree.contract.test.ts` pins the cases we decided
 * about. This file attacks the parts nobody decided about: paths no agent
 * would ever create, and the invariants that have to survive whatever a
 * filesystem hands us. A detector that throws takes the Work view's whole
 * row list with it, and one that answers inconsistently puts an agent chip
 * on a person's own checkout.
 */

const ADVERSARIAL: readonly string[] = [
  "",
  " ",
  "\0",
  "\n",
  "\t",
  "/",
  "//",
  "///////",
  "\\\\",
  "\\/\\/\\/",
  ".".repeat(10_000),
  "/".repeat(10_000),
  "/repo/" + "../".repeat(200) + ".claude/worktrees/x",
  "/repo/.git/worktrees/" + "a".repeat(4000),
  "/repo/.git/worktrees/feature",
  "/repo/.GIT/worktrees/feature",
  "/repo/.GIT/worktrees",
  "/repo/.Git/WORKTREES/feature",
  "/repo/worktrees/feature",
  "/repo/.claude",
  "/repo/.claude/",
  "/repo/.claude/not-worktrees/x",
  "/.claude/worktrees/",
  "claude/worktrees/x",
  "/home/claude/worktrees/x",
  "/repo/.claude/worktrees/" + "slug/".repeat(50) + "file.ts",
  "C:\\repo\\.git\\worktrees\\feature",
  "C:\\repo\\.claude\\worktrees\\slug",
  "/repo/./.claude/worktrees/x",
  "/repo/../worktrees/x",
  "/repo/..foo/worktrees/x",
  "/repo/.../worktrees/x",
  // An ancestor called `worktrees`, in every spelling that used to redirect
  // the slug away from the session it belongs to.
  "/srv/worktrees/app/.claude/worktrees/session-a",
  "/worktrees/.claude/worktrees/session-b",
  "C:\\worktrees\\app\\.cursor\\worktrees\\session-c",
  "/repo/.claude/worktrees/slug/worktrees/nested",
  // Deeply nested agent layouts: the outermost must win, every time.
  "/a/.claude/worktrees/one/.cursor/worktrees/two/.codex/worktrees/three",
  // A pathological run of hidden directories that never reaches `worktrees`.
  "/" + Array.from({ length: 2_000 }, (_, i) => `.d${i}`).join("/"),
  // …and one that reaches it only at the very end.
  "/" + Array.from({ length: 2_000 }, (_, i) => `.d${i}`).join("/") + "/worktrees/final",
  "\u202e/repo/.claude/worktrees/rtl",
  "/repo/.клод/worktrees/сессия",
  "/repo/.\u0000claude/worktrees/x",
];

/** Deterministic PRNG: a fuzz run that cannot be reproduced is not evidence. */
function rng(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state * 1664525 + 1013904223) >>> 0;
    return state / 0x1_0000_0000;
  };
}

const ALPHABET = ["a", "z", ".", "..", ".git", ".GIT", ".claude", "worktrees", "Worktrees", "", " ", "-", "\u00e9", "0"];

function randomPath(next: () => number): string {
  const depth = 1 + Math.floor(next() * 12);
  const parts: string[] = [];
  for (let i = 0; i < depth; i += 1) {
    parts.push(ALPHABET[Math.floor(next() * ALPHABET.length)]);
  }
  const separator = next() < 0.5 ? "/" : "\\";
  return (next() < 0.5 ? separator : "") + parts.join(separator);
}

/** Swaps every separator for the other platform's spelling. */
function otherSeparators(path: string): string {
  return path.includes("\\") ? path.replace(/\\/g, "/") : path.replace(/\//g, "\\");
}

describe("agent worktree detector under adversarial paths", () => {
  it("never throws and never labels git internals as an agent", () => {
    for (const path of ADVERSARIAL) {
      expect(() => isAgentWorktree(path), path).not.toThrow();
      expect(() => agentKind(path), path).not.toThrow();
      expect(() => agentSessionSlug(path), path).not.toThrow();
      if (/[\\/]\.git[\\/]/i.test(path)) {
        expect(isAgentWorktree(path), path).toBe(false);
        expect(agentKind(path), path).toBe("");
      }
    }
  });

  it("stays bounded across hundreds of mixed paths", () => {
    const paths = Array.from({ length: 400 }, (_, i) => {
      if (i % 4 === 0) return `/repo/.claude/worktrees/s${i}`;
      if (i % 4 === 1) return `/repo/.cursor/worktrees/s${i}`;
      if (i % 4 === 2) return `/repo/.git/worktrees/s${i}`;
      return `/repo/wt/${i}`;
    });
    const kinds = agentKindsOn(paths);
    expect(kinds).toEqual(["claude", "cursor"]);
    expect(paths.filter(isAgentWorktree)).toHaveLength(200);
  });

  it("reads the outermost layout when several are nested", () => {
    const nested = "/a/.claude/worktrees/one/.cursor/worktrees/two/.codex/worktrees/three";
    expect(agentLayout(nested)).toEqual({ kind: "claude", slug: "one" });
  });
});

describe("agent worktree detector invariants", () => {
  const CASES = [...ADVERSARIAL, ...Array.from({ length: 4_000 }, (() => { const next = rng(0x5eed); return () => randomPath(next); })())];

  it("answers the same question consistently, whatever the input", () => {
    let matched = 0;
    let rejected = 0;
    for (const path of CASES) {
      const layout = agentLayout(path);
      if (layout) matched += 1;
      else rejected += 1;
      // The three public entry points are views of one scan. If they can
      // disagree, the count in a tile and the chip on a row describe
      // different worlds.
      expect(isAgentWorktree(path), path).toBe(layout !== null);
      expect(agentKind(path), path).toBe(layout?.kind ?? "");
      expect(agentSessionSlug(path), path).toBe(layout?.slug ?? "");
      if (!layout) continue;
      // A kind is a real directory name: never empty, never dotted, never git.
      expect(layout.kind.length, path).toBeGreaterThan(0);
      expect(layout.kind.startsWith("."), path).toBe(false);
      expect(layout.kind.toLowerCase(), path).not.toBe("git");
      // Whatever the slug is, it is a segment of the path it came from —
      // never a fragment, and never a name borrowed from somewhere else.
      if (layout.slug) {
        expect(path.split(/[\\/]+/).filter(Boolean), path).toContain(layout.slug);
      }
    }
    // A generator that stopped producing matches would leave every invariant
    // above technically satisfied and checking nothing — the same shape of
    // failure as a scan that could not run reporting a clean result. Both
    // outcomes have to be represented for this to be evidence of anything.
    expect(matched, "the fuzz corpus produced no agent layouts at all").toBeGreaterThan(50);
    expect(rejected, "the fuzz corpus produced nothing but agent layouts").toBeGreaterThan(50);
  });

  it("gives Windows and POSIX spellings of one path the same answer", () => {
    // The half nobody developing on macOS ever types, and so the half that
    // rots silently.
    for (const path of CASES) {
      expect(agentLayout(otherSeparators(path)), path).toEqual(agentLayout(path));
    }
  });

  it("is unmoved by redundant separators", () => {
    for (const path of CASES.slice(0, 1_000)) {
      const padded = path.replace(/\//g, "///").replace(/\\/g, "\\\\");
      expect(agentLayout(padded), path).toEqual(agentLayout(path));
    }
  });

  it("does not change its mind about a session because of what is below it", () => {
    // A file deep inside a session directory must report the same session as
    // the session directory itself — including when the nested path contains
    // another `worktrees` directory.
    for (const suffix of ["/src", "/src/lib/x.ts", "/worktrees", "/worktrees/other", "/.git/worktrees/z"]) {
      for (const base of [
        "/repo/.claude/worktrees/slug",
        "/srv/worktrees/app/.cursor/worktrees/slug",
        "C:\\repo\\.claude\\worktrees\\slug",
      ]) {
        const separator = base.includes("\\") ? suffix.replace(/\//g, "\\") : suffix;
        expect(agentLayout(base + separator), base + separator).toEqual(agentLayout(base));
      }
    }
  });

  it("stays linear on hostile input", () => {
    // Not a benchmark — a tripwire. The previous implementation matched with
    // a regex over a string it rebuilt with a `replace` loop; anything that
    // reintroduces backtracking or quadratic normalisation blows past this
    // budget long before it reaches a user's Work view. The budget is loose
    // on purpose: a CI box under load must not fail this for being slow.
    const hostile = [
      "/" + ".claude/".repeat(20_000) + "worktrees/slug",
      "/" + "a/".repeat(50_000) + ".claude/worktrees/slug",
      "/repo/" + ".".repeat(100_000) + "/worktrees/x",
      "/repo/.claude/worktrees/" + "x".repeat(200_000),
      "/" + "/".repeat(200_000) + ".claude/worktrees/s",
    ];
    const started = performance.now();
    for (let round = 0; round < 20; round += 1) {
      for (const path of hostile) {
        expect(() => agentLayout(path), path).not.toThrow();
      }
    }
    expect(performance.now() - started).toBeLessThan(5_000);
  });

  it("holds the layout it found however long the session name is", () => {
    const long = "s".repeat(50_000);
    expect(agentLayout(`/repo/.claude/worktrees/${long}`)).toEqual({ kind: "claude", slug: long });
  });
});
