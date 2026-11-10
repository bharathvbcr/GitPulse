import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import type { ComponentProps } from "svelte";
import { compile } from "svelte/compiler";
import { render } from "svelte/server";
import Slider, { PAGE_STEPS, pageKeySteps, stepValue } from "./Slider.svelte";

const source = readFileSync(new URL("./Slider.svelte", import.meta.url), "utf8");

function rangeTag(html: string): string {
  const tag = /<input\b[^>]*type="range"[^>]*>/.exec(html)?.[0];
  if (!tag) throw new Error(`no range input in ${html}`);
  return tag;
}

describe("Slider", () => {
  it("compiles without warnings", () => {
    const { warnings } = compile(source, { generate: "client", filename: "Slider.svelte" });
    expect(warnings).toEqual([]);
  });

  describe("markup and accessibility", () => {
    it("is a native range input carrying min, max, step and value", () => {
      const tag = rangeTag(render(Slider, { props: { ariaLabel: "Scale", min: 0.75, max: 1.4, step: 0.05, value: 1 } }).body);
      expect(tag).toContain('min="0.75"');
      expect(tag).toContain('max="1.4"');
      expect(tag).toContain('step="0.05"');
      expect(tag).toContain('value="1"');
    });

    it("names a bare slider by its required aria-label", () => {
      const html = render(Slider, { props: { ariaLabel: "UI scale", min: 0, max: 10, value: 3 } }).body;
      expect(rangeTag(html)).toContain('aria-label="UI scale"');
      expect(html).not.toContain("<label");
    });

    it("names a labelled slider by a <label> that points at the input", () => {
      const html = render(Slider, { props: { label: "UI scale", id: "scale", min: 0, max: 10, value: 3 } }).body;
      expect(html).toMatch(/<label\b[^>]*for="scale"[^>]*>UI scale<\/label>/);
      expect(rangeTag(html)).toContain('id="scale"');
      expect(rangeTag(html)).not.toContain("aria-label=");
    });

    it("shows the value through format, and exposes the same text as aria-valuetext", () => {
      const html = render(Slider, {
        props: { label: "UI scale", min: 0.75, max: 1.4, step: 0.05, value: 1.25, format: (n: number) => `${Math.round(n * 100)}%` },
      }).body;
      expect(rangeTag(html)).toContain('aria-valuetext="125%"');
      expect(html).toContain(">125%</span>");
    });

    it("hides the visible value from assistive tech, because aria-valuetext already says it", () => {
      const html = render(Slider, { props: { label: "x", min: 0, max: 10, value: 4 } }).body;
      expect(html).toMatch(/<span[^>]*aria-hidden="true"[^>]*>4<\/span>/);
    });

    it("defaults to a step of one and a plain number label", () => {
      const html = render(Slider, { props: { ariaLabel: "x", min: 0, max: 10, value: 4 } }).body;
      expect(rangeTag(html)).toContain('step="1"');
      expect(rangeTag(html)).toContain('aria-valuetext="4"');
    });

    it("sets disabled and dims the control", () => {
      const tag = rangeTag(render(Slider, { props: { ariaLabel: "x", min: 0, max: 10, value: 4, disabled: true } }).body);
      expect(tag).toMatch(/\sdisabled(\s|=|>)/);
      expect(tag).toContain("disabled:opacity-40");
      expect(rangeTag(render(Slider, { props: { ariaLabel: "x", min: 0, max: 10, value: 4 } }).body)).not.toMatch(/\sdisabled(\s|=|>)/);
    });

    it("rejects a slider with neither a visible label nor an aria-label at compile time", () => {
      // @ts-expect-error a slider with no name is a type error
      const unnamed: ComponentProps<typeof Slider> = { min: 0, max: 1, value: 0 };
      const named: ComponentProps<typeof Slider> = { min: 0, max: 1, value: 0, ariaLabel: "x" };
      const labelled: ComponentProps<typeof Slider> = { min: 0, max: 1, value: 0, label: "x" };
      expect([unnamed, named, labelled]).toHaveLength(3);
    });
  });

  describe("keyboard", () => {
    it("leaves the arrow, Home and End keys to the native range input (one step each)", () => {
      // Only the page keys are intercepted; anything else returns before preventDefault.
      expect(pageKeySteps("ArrowUp")).toBe(0);
      expect(pageKeySteps("ArrowRight")).toBe(0);
      expect(pageKeySteps("Home")).toBe(0);
      expect(pageKeySteps("End")).toBe(0);
      expect(source).not.toMatch(/ArrowUp|ArrowDown|ArrowLeft|ArrowRight/);
    });

    it("moves ten steps for Page Up and Page Down", () => {
      expect(PAGE_STEPS).toBe(10);
      expect(pageKeySteps("PageUp")).toBe(10);
      expect(pageKeySteps("PageDown")).toBe(-10);
    });

    it("steps the value on its step grid", () => {
      expect(stepValue(5, 1, 0, 100, 1)).toBe(6);
      expect(stepValue(5, -1, 0, 100, 1)).toBe(4);
      expect(stepValue(20, PAGE_STEPS, 0, 100, 5)).toBe(70);
      expect(stepValue(70, -PAGE_STEPS, 0, 100, 5)).toBe(20);
    });

    it("clamps at both ends", () => {
      expect(stepValue(95, PAGE_STEPS, 0, 100, 1)).toBe(100);
      expect(stepValue(3, -PAGE_STEPS, 0, 100, 1)).toBe(0);
      expect(stepValue(100, 1, 0, 100, 1)).toBe(100);
      expect(stepValue(0, -1, 0, 100, 1)).toBe(0);
    });

    it("does not accumulate float drift on fractional steps", () => {
      // The UI scale slider: 0.75..1.4 by 0.05.
      expect(stepValue(0.75, PAGE_STEPS, 0.75, 1.4, 0.05)).toBe(1.25);
      expect(stepValue(1, 1, 0.75, 1.4, 0.05)).toBe(1.05);
      expect(stepValue(1.25, -PAGE_STEPS, 0.75, 1.4, 0.05)).toBe(0.75);
      let value = 0;
      for (let i = 0; i < 10; i += 1) value = stepValue(value, 1, 0, 1, 0.1);
      expect(value).toBe(1);
    });

    it("snaps an off-grid value to the grid before stepping", () => {
      expect(stepValue(7, 1, 0, 100, 5)).toBe(10);
      expect(stepValue(0.78, 1, 0.75, 1.4, 0.05)).toBe(0.85);
    });

    it("clamps to a max that is not on the grid", () => {
      // (1.4 - 0.75) / 0.05 = 13 steps, so 1.4 is reachable; 1.35 + 10 steps is clamped to it.
      expect(stepValue(1.35, PAGE_STEPS, 0.75, 1.4, 0.05)).toBe(1.4);
      // 0..10 by 3: the grid tops out at 9; a page jump still stops at the max, not past it.
      expect(stepValue(6, PAGE_STEPS, 0, 10, 3)).toBe(10);
    });

    it("refuses nonsense bounds without throwing", () => {
      expect(stepValue(5, 1, 10, 0, 1)).toBe(5);
      expect(stepValue(5, 1, 0, 10, 0)).toBe(5);
      expect(stepValue(5, 1, 0, 10, -1)).toBe(5);
      expect(stepValue(Number.NaN, 1, 2, 10, 1)).toBe(2);
    });

    it("hands the page keys to the component, and ignores them with a modifier or while disabled", () => {
      expect(source).toContain("event.preventDefault()");
      expect(source).toMatch(/if \(disabled \|\| event\.altKey \|\| event\.ctrlKey \|\| event\.metaKey \|\| event\.shiftKey\) return;/);
    });
  });

  describe("tokens", () => {
    it("uses the shared range styles, with no inline thumb or track recipe of its own", () => {
      expect(source).not.toMatch(/<style|-webkit-slider|accent-color/);
    });

    it("carries no literal palette color, hex, z-index, radius, duration or pixel font size", () => {
      expect(source).not.toMatch(/#[0-9a-fA-F]{3,8}\b|rgb\(|\b(?:zinc|slate|gray|amber|rose|emerald|sky)-\d|\bz-\[|rounded-\[|text-\[\d+px\]|\bduration-\d|\b\d+ms\b/);
    });
  });
});
