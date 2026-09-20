import { describe, expect, it } from "vitest";
import { nextHostedTerminals } from "./repoHosts";
import { MAX_TERMINAL_TABS } from "./tabs";
import { MAX_OPEN_TABS } from "../repos/tabModel";

/**
 * The hosting rule under churn.
 *
 * `nextHostedTerminals` decides which repository tabs own a mounted terminal
 * panel, and a mounted panel spawns a PTY. So this is not a rendering
 * question: every element this set gains that the user did not ask for is a
 * process started in a repository they were only reading, and a slot taken
 * from the process-global `MAX_PTY_SESSIONS` budget.
 *
 * Tested as invariants over randomized sequences rather than as a handful of
 * transitions. The regression it replaces — every visited repository latching
 * a panel — was invisible to the unit tests because each individual
 * transition looked correct in isolation; only "walk around the workspace for
 * a while and see what accumulated" shows it.
 */

function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), a | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** A workspace the model walks: open repository tabs and each one's dock. */
interface World {
  openTabs: string[];
  activeTabId: string | null;
  /** Repository tabs on which the USER has opened the terminal. */
  docks: Set<string>;
}

function step(world: World, hosted: ReadonlySet<string>): Set<string> {
  return nextHostedTerminals(
    hosted,
    world.openTabs,
    world.activeTabId,
    world.activeTabId !== null && world.docks.has(world.activeTabId),
  );
}

describe("terminal hosting under churn", () => {
  it("never hosts a repository whose dock the user never opened", () => {
    for (let seed = 1; seed <= 40; seed += 1) {
      const random = mulberry32(seed);
      const all = Array.from({ length: MAX_OPEN_TABS }, (_, i) => `tab-${i}`);
      const world: World = { openTabs: [...all], activeTabId: all[0], docks: new Set() };
      let hosted: ReadonlySet<string> = new Set<string>();
      /** Every tab that was ever active while ITS dock was open. */
      const everInvited = new Set<string>();

      for (let i = 0; i < 400; i += 1) {
        const roll = random();
        if (roll < 0.55) {
          // Switch repository tabs — the motion that used to spawn shells.
          world.activeTabId = world.openTabs[Math.floor(random() * world.openTabs.length)] ?? null;
        } else if (roll < 0.7 && world.activeTabId) {
          world.docks.add(world.activeTabId);
        } else if (roll < 0.8 && world.activeTabId) {
          world.docks.delete(world.activeTabId);
        } else if (roll < 0.9 && world.openTabs.length > 1) {
          const victim = world.openTabs[Math.floor(random() * world.openTabs.length)];
          world.openTabs = world.openTabs.filter((id) => id !== victim);
          world.docks.delete(victim);
          if (world.activeTabId === victim) world.activeTabId = world.openTabs[0] ?? null;
        } else {
          const candidate = all[Math.floor(random() * all.length)];
          if (!world.openTabs.includes(candidate)) world.openTabs.push(candidate);
        }

        if (world.activeTabId && world.docks.has(world.activeTabId)) {
          everInvited.add(world.activeTabId);
        }
        hosted = step(world, hosted);

        // 1. A panel only ever exists for an OPEN repository tab: closing the
        //    tab is what disposes the panel and kills its shells.
        for (const id of hosted) {
          expect(world.openTabs, `seed ${seed} step ${i}`).toContain(id);
        }
        // 2. THE DECOUPLING. Nothing is hosted that the user did not open a
        //    terminal on while looking at it. Merely visiting a repository —
        //    however many times, with a shell running elsewhere — hosts
        //    nothing.
        for (const id of hosted) {
          expect(everInvited, `seed ${seed} step ${i}: ${id} hosted uninvited`).toContain(id);
        }
      }
    }
  });

  it("does not grow the hosted set while the user walks past closed docks", () => {
    const all = Array.from({ length: MAX_OPEN_TABS }, (_, i) => `tab-${i}`);
    // One repository has a shell; every other dock is shut.
    const world: World = { openTabs: [...all], activeTabId: "tab-0", docks: new Set(["tab-0"]) };
    let hosted: ReadonlySet<string> = step(world, new Set<string>());
    expect(hosted).toEqual(new Set(["tab-0"]));

    for (let lap = 0; lap < 20; lap += 1) {
      for (const id of all) {
        world.activeTabId = id;
        const before = hosted;
        hosted = step(world, hosted);
        if (id !== "tab-0") {
          // Identity, not just equality: an unchanged membership must return
          // the SAME set, or the dock's `$effect` reassigns state every
          // switch and re-renders every panel for nothing.
          expect(hosted).toBe(before);
        }
      }
    }
    expect(hosted).toEqual(new Set(["tab-0"]));
  });

  it("stays within the PTY budget when the user opens a dock in every repository", () => {
    // MAX_OPEN_TABS (24) exceeds MAX_TERMINAL_TABS (16), so a workspace CAN
    // ask for more panels than there are session slots. Hosting does not
    // enforce that ceiling — the registry does, with a visible spawn error —
    // but the shape of the excess is worth pinning: it is bounded by the
    // repositories the user actually opened a dock on, never by the number
    // they visited.
    expect(MAX_OPEN_TABS).toBeGreaterThan(MAX_TERMINAL_TABS);
    const all = Array.from({ length: MAX_OPEN_TABS }, (_, i) => `tab-${i}`);
    const invited = all.slice(0, 5);
    const world: World = { openTabs: [...all], activeTabId: null, docks: new Set(invited) };
    let hosted: ReadonlySet<string> = new Set<string>();

    for (const id of all) {
      world.activeTabId = id;
      hosted = step(world, hosted);
    }
    expect(hosted).toEqual(new Set(invited));
    expect(hosted.size).toBeLessThanOrEqual(MAX_TERMINAL_TABS);
  });

  it("drops panels for closed tabs even in a burst that closes most of the workspace", () => {
    const all = Array.from({ length: MAX_OPEN_TABS }, (_, i) => `tab-${i}`);
    const world: World = { openTabs: [...all], activeTabId: null, docks: new Set(all) };
    let hosted: ReadonlySet<string> = new Set<string>();
    for (const id of all) {
      world.activeTabId = id;
      hosted = step(world, hosted);
    }
    expect(hosted.size).toBe(MAX_OPEN_TABS);

    world.openTabs = ["tab-3"];
    world.activeTabId = "tab-3";
    hosted = step(world, hosted);
    expect(hosted).toEqual(new Set(["tab-3"]));

    world.openTabs = [];
    world.activeTabId = null;
    hosted = step(world, hosted);
    expect(hosted.size).toBe(0);
  });

  it("re-hosts a reopened repository only when its dock is opened again", () => {
    const world: World = { openTabs: ["a", "b"], activeTabId: "a", docks: new Set(["a"]) };
    let hosted: ReadonlySet<string> = step(world, new Set<string>());
    expect(hosted).toEqual(new Set(["a"]));

    // Close "a" — the panel goes, and with it the shells.
    world.openTabs = ["b"];
    world.activeTabId = "b";
    world.docks.delete("a");
    hosted = step(world, hosted);
    expect(hosted.size).toBe(0);

    // Reopening the repository must NOT resurrect a panel by itself; the old
    // one is gone and a new one is a new process.
    world.openTabs = ["b", "a"];
    world.activeTabId = "a";
    hosted = step(world, hosted);
    expect(hosted.size).toBe(0);

    world.docks.add("a");
    hosted = step(world, hosted);
    expect(hosted).toEqual(new Set(["a"]));
  });

  it("ignores an active id that is not an open tab, however often it recurs", () => {
    const world: World = { openTabs: ["a"], activeTabId: "ghost", docks: new Set(["ghost", "a"]) };
    let hosted: ReadonlySet<string> = new Set<string>();
    for (let i = 0; i < 100; i += 1) hosted = step(world, hosted);
    expect(hosted.size).toBe(0);
  });
});
