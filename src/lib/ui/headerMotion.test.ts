import { readFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";
import { DURATION_MS, EXIT_MS } from "../motion/tokens";
import {
  HEADER_ENTER_MS,
  HEADER_EXIT_MS,
  HEADER_FRAMES,
  headerTransition,
  markEnter,
  markExit,
  popupEnter,
  swapRow,
  tabEnter,
  tabExit,
} from "./headerMotion";

const read = (path: string) => readFileSync(new URL(`../../${path}`, import.meta.url), "utf8");
const KINDS = Object.keys(HEADER_FRAMES) as (keyof typeof HEADER_FRAMES)[];
const REDUCED = { matchMedia: () => ({ matches: true }) } as unknown as Pick<Window, "matchMedia">;
const FULL = { matchMedia: () => ({ matches: false }) } as unknown as Pick<Window, "matchMedia">;

describe("header motion tokens (HEADER_SPEC section 6)", () => {
  it("enters at the fast token and exits at the exit cap, never longer than 70 ms", () => {
    expect(HEADER_ENTER_MS).toBe(DURATION_MS.fast);
    expect(HEADER_EXIT_MS).toBe(EXIT_MS.fast);
    expect(HEADER_EXIT_MS).toBeLessThanOrEqual(70);
    for (const kind of KINDS) {
      expect(headerTransition(kind, true, FULL).duration).toBe(DURATION_MS.fast);
      const out = headerTransition(kind, false, FULL).duration as number;
      expect(out).toBeLessThanOrEqual(70);
      expect(out).toBeLessThan(headerTransition(kind, true, FULL).duration as number);
    }
  });

  it("gives each Svelte transition its own frame and direction, under the live preference", () => {
    // Svelte passes (node, params, { direction }); the wrappers ignore them and read the live preference.
    const wrappers = [
      [tabEnter, "tab", true],
      [tabExit, "tab", false],
      [popupEnter, "popup", true],
      [markEnter, "mark", true],
      [markExit, "mark", false],
    ] as const;
    for (const [wrapper, kind, entering] of wrappers) {
      const config = wrapper(undefined, undefined, { direction: entering ? "in" : "out" });
      expect(config).toEqual(headerTransition(kind, entering));
      expect(config.css).toBe(HEADER_FRAMES[kind]);
    }
    // A swapped pair would play the exit curve on the way in.
    expect(tabEnter().easing).not.toBe(tabExit().easing);
    expect(markEnter().easing).toBe(tabEnter().easing);
  });

  it("is instant under reduced motion, for every piece and both directions", () => {
    for (const kind of KINDS) {
      expect(headerTransition(kind, true, REDUCED).duration).toBe(0);
      expect(headerTransition(kind, false, REDUCED).duration).toBe(0);
    }
  });

  it("animates transform and opacity only", () => {
    for (const kind of KINDS) {
      for (const t of [0, 0.25, 0.5, 1]) {
        const css = HEADER_FRAMES[kind](t);
        const properties = [...css.matchAll(/([a-z-]+)\s*:/g)].map((match) => match[1]);
        expect(properties.length).toBeGreaterThan(0);
        for (const property of properties) expect(["opacity", "transform"]).toContain(property);
      }
      expect(HEADER_FRAMES[kind](1)).toMatch(/opacity: 1;/);
      expect(HEADER_FRAMES[kind](0)).toMatch(/opacity: 0;/);
    }
  });

  it("carries no millisecond or curve literal of its own", () => {
    const source = read("lib/ui/headerMotion.ts").replace(/\/\*[\s\S]*?\*\//g, "");
    expect(source).not.toMatch(/\b\d+\s*ms\b/);
    expect(source).not.toMatch(/cubic-bezier\(/);
    expect(source).not.toMatch(/duration:\s*\d/);
  });

  it("fades row 2's controls over the tokens, and settles at once when motion is reduced", () => {
    const animate = vi.fn(() => ({ onfinish: null, oncancel: null, cancel: vi.fn() }) as unknown as Animation);
    const element = { animate } as unknown as HTMLElement;
    const onDone = vi.fn();
    expect(swapRow(element, false, onDone, REDUCED)).toBeNull();
    expect(onDone).toHaveBeenCalledTimes(1);
    expect(animate).not.toHaveBeenCalled();

    swapRow(element, false, vi.fn(), FULL);
    swapRow(element, true, vi.fn(), FULL);
    const calls = animate.mock.calls as unknown as [Record<string, unknown>, { duration: number }][];
    expect(Object.keys(calls[0][0])).toEqual(["opacity"]);
    expect(calls[0][1].duration).toBe(EXIT_MS.fast);
    expect(calls[1][1].duration).toBe(DURATION_MS.fast);
  });
});

describe("where the header uses it", () => {
  const tabBar = read("lib/components/RepoTabBar.svelte");

  it("opens and closes repository tabs and stacks", () => {
    // On the keyed item's own wrapper: a transition on the shell, inside an {#if} of the each, would be local to that {#if} and not play.
    expect(tabBar).toMatch(/<div class="flex shrink-0 items-center" data-tab-motion in:tabEnter out:tabExit>/);
    expect(tabBar.match(/in:tabEnter/g)).toHaveLength(1);
  });

  it("moves the attention mark in and out on a tab and on a stack head", () => {
    expect(tabBar.match(/in:markEnter\s+out:markExit/g)).toHaveLength(2);
  });

  it("opens and closes the overflow menu and the hover card with the popup motion, not the unrelated pop", () => {
    expect(tabBar.match(/in:popupEnter\s+out:popoverExit/g)).toHaveLength(2);
    const card = tabBar.slice(tabBar.indexOf("data-tab-card={cardTab.id}"));
    expect(card.slice(0, card.indexOf("</div>"))).not.toContain("gp-pop");
  });

  it("brings the Fetch-all pill and its ring in and out", () => {
    const actions = read("lib/components/WorkspaceActions.svelte");
    expect(actions).toContain("in:markEnter out:markExit");
    // The ring's dash is set per update: only the pill's opacity and scale move.
    expect(actions).toContain("never animated");
  });

  it("fades row 2's controls under a global surface", () => {
    const bar = read("lib/components/ViewBar.svelte");
    expect(bar).toContain("swapRow(el, show");
    expect(bar).toContain("bind:this={controls}");
  });
});
