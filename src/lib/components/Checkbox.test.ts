import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import type { ComponentProps } from "svelte";
import { compile } from "svelte/compiler";
import { render } from "svelte/server";
import Checkbox, { requestedCheck } from "./Checkbox.svelte";

const source = readFileSync(new URL("./Checkbox.svelte", import.meta.url), "utf8");

/** The `<input type="checkbox">` start tag of a rendered checkbox. */
function boxTag(html: string): string {
  const tag = /<input\b[^>]*type="checkbox"[^>]*>/.exec(html)?.[0];
  if (!tag) throw new Error(`no checkbox in ${html}`);
  return tag;
}

describe("Checkbox", () => {
  it("compiles without warnings", () => {
    const { warnings } = compile(source, { generate: "client", filename: "Checkbox.svelte" });
    expect(warnings).toEqual([]);
  });

  describe("accessibility attributes", () => {
    it("is a native checkbox input wearing gp-check, so the browser owns the role, Space and form semantics", () => {
      const tag = boxTag(render(Checkbox, { props: { ariaLabel: "Select main" } }).body);
      expect(tag.startsWith("<input")).toBe(true);
      expect(tag).toContain('class="gp-check"');
      expect(tag).not.toContain("role=");
      // A native input needs no key handler and no tabindex; adding either
      // would make Space fire twice or lose the default.
      expect(tag).not.toContain("tabindex");
      expect(source).not.toMatch(/onkeydown|onkeyup|onkeypress|onclick/);
    });

    it("renders checked only when checked", () => {
      expect(boxTag(render(Checkbox, { props: { ariaLabel: "x" } }).body)).not.toMatch(/\schecked(\s|=|>)/);
      expect(boxTag(render(Checkbox, { props: { ariaLabel: "x", checked: true } }).body)).toMatch(/\schecked(\s|=|>)/);
    });

    it("reflects indeterminate on the input", () => {
      expect(boxTag(render(Checkbox, { props: { ariaLabel: "x" } }).body)).not.toContain("indeterminate");
      expect(boxTag(render(Checkbox, { props: { ariaLabel: "x", indeterminate: true } }).body)).toContain("indeterminate");
    });

    it("names a bare box by its required aria-label and renders no label row", () => {
      const html = render(Checkbox, { props: { ariaLabel: "Select feature/x" } }).body;
      expect(boxTag(html)).toContain('aria-label="Select feature/x"');
      expect(html).not.toContain("<label");
    });

    it("wraps a labelled box in one <label>, so the text toggles it, and names it by that text", () => {
      const html = render(Checkbox, { props: { label: "I confirm", id: "ok" } }).body;
      expect(html).toMatch(/<label\b[^>]*for="ok"[^>]*>[\s\S]*<input\b[\s\S]*I confirm[\s\S]*<\/label>/);
      expect(boxTag(html)).toContain('id="ok"');
      expect(boxTag(html)).toContain('aria-label="I confirm"');
    });

    it("lets aria-label override the visible text when the text needs context", () => {
      const html = render(Checkbox, { props: { label: "Select", ariaLabel: "Select all branches" } }).body;
      expect(boxTag(html)).toContain('aria-label="Select all branches"');
      expect(html).toContain(">Select<");
    });

    it("generates the id from $props.id() when none is passed", () => {
      const html = render(Checkbox, { props: { label: "Sure" } }).body;
      const forId = /<label[^>]*for="(checkbox-[^"]+)"/.exec(html)?.[1];
      expect(forId).toBeTruthy();
      expect(boxTag(html)).toContain(`id="${forId}"`);
      expect(source).toMatch(/const uid = \$props\.id\(\);/);
    });

    it("renders the description under the label and points aria-describedby at it", () => {
      const html = render(Checkbox, { props: { label: "Sure", id: "c", description: "This cannot be undone." } }).body;
      expect(html).toContain("This cannot be undone.");
      expect(boxTag(html)).toContain('aria-describedby="c-description"');
      expect(html).toContain('id="c-description"');
    });

    it("puts testId on the native input as data-testid, and nothing when it is not passed", () => {
      expect(boxTag(render(Checkbox, { props: { ariaLabel: "x", testId: "refine-all" } }).body)).toContain('data-testid="refine-all"');
      expect(boxTag(render(Checkbox, { props: { label: "Sure", testId: "sure" } }).body)).toContain('data-testid="sure"');
      expect(boxTag(render(Checkbox, { props: { ariaLabel: "x" } }).body)).not.toContain("data-testid");
    });

    it("has no aria-describedby without a description", () => {
      expect(boxTag(render(Checkbox, { props: { label: "Sure" } }).body)).not.toContain("aria-describedby");
    });

    it("rejects a box with neither a visible label nor an aria-label at compile time", () => {
      // @ts-expect-error a checkbox with no name is a type error
      const unnamed: ComponentProps<typeof Checkbox> = { checked: true };
      const named: ComponentProps<typeof Checkbox> = { checked: true, ariaLabel: "x" };
      const labelled: ComponentProps<typeof Checkbox> = { checked: true, label: "x" };
      expect([unnamed, named, labelled]).toHaveLength(3);
    });
  });

  describe("states", () => {
    it("sets the disabled attribute and dims the whole labelled row", () => {
      const bare = render(Checkbox, { props: { ariaLabel: "x", disabled: true } }).body;
      expect(boxTag(bare)).toMatch(/\sdisabled(\s|=|>)/);
      const row = render(Checkbox, { props: { label: "Sure", disabled: true } }).body;
      expect(row).toContain("opacity-40");
      expect(row).toContain("cursor-not-allowed");
    });

    it("leaves an enabled box without the disabled attribute", () => {
      expect(boxTag(render(Checkbox, { props: { ariaLabel: "x" } }).body)).not.toMatch(/\sdisabled(\s|=|>)/);
    });

    it("shows focus through the global input:focus-visible ring and removes none of it", () => {
      expect(source).not.toMatch(/outline-none|outline-hidden|focus:outline|focus-visible:/);
    });

    it("draws nothing itself: the box is the stylesheet's gp-check, with no switch geometry", () => {
      expect(source).not.toMatch(/role="switch"|translate-x|rounded-full|aria-checked/);
    });
  });

  describe("press behavior", () => {
    it("asks for the opposite value, and from indeterminate (not checked) asks for true", () => {
      expect(requestedCheck(false, false)).toBe(true);
      expect(requestedCheck(true, false)).toBe(false);
    });

    it("asks for nothing while disabled", () => {
      expect(requestedCheck(false, true)).toBeNull();
      expect(requestedCheck(true, true)).toBeNull();
    });

    it("is controlled: it restores the box and hands the decision to onchange", () => {
      expect(source).not.toContain("$bindable");
      expect(source).toContain("box.checked = checked;");
      expect(source).toContain("box.indeterminate = indeterminate;");
      expect(source).toContain("onchange?.(next)");
      // The only `checked =` outside the restore is the `= false` default of the destructured prop.
      expect(source.replace("box.checked = checked;", "")).not.toMatch(/(?<![-\w.])checked\s*=(?!\s*false)/);
    });
  });

  describe("tokens", () => {
    it("carries no literal palette color, hex, z-index, radius or duration", () => {
      expect(source).not.toMatch(/#[0-9a-fA-F]{3,8}\b|rgb\(|\b(?:zinc|slate|gray|amber|rose|emerald|sky)-\d|\bz-\[|\bz-\d+\b|rounded-\[|text-\[\d+px\]|\bduration-\d|\b\d+ms\b|transition/);
    });
  });
});
