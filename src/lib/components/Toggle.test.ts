import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import type { ComponentProps } from "svelte";
import { compile } from "svelte/compiler";
import { render } from "svelte/server";
import Toggle, { requestedState } from "./Toggle.svelte";
import SettingToggle from "./SettingToggle.svelte";

const source = readFileSync(new URL("./Toggle.svelte", import.meta.url), "utf8");

/** The `<button role="switch">` start tag of a rendered toggle. */
function switchTag(html: string): string {
  const tag = /<button\b[^>]*role="switch"[^>]*>/.exec(html)?.[0];
  if (!tag) throw new Error(`no switch in ${html}`);
  return tag;
}

describe("Toggle", () => {
  it("compiles without warnings", () => {
    const { warnings } = compile(source, { generate: "client", filename: "Toggle.svelte" });
    expect(warnings).toEqual([]);
  });

  describe("accessibility attributes", () => {
    it("is a native button with role=switch, so Space and Enter press it", () => {
      const tag = switchTag(render(Toggle, { props: { ariaLabel: "Reduce motion" } }).body);
      expect(tag.startsWith("<button")).toBe(true);
      expect(tag).toContain('type="button"');
      expect(tag).toContain('role="switch"');
      // A native button needs no key handler and no tabindex of its own; adding
      // either would make Space or Enter fire twice or lose the default.
      expect(tag).not.toContain("tabindex");
      expect(source).not.toMatch(/onkeydown|onkeyup|onkeypress/);
    });

    it("reports aria-checked false then true", () => {
      expect(switchTag(render(Toggle, { props: { ariaLabel: "x" } }).body)).toContain('aria-checked="false"');
      expect(switchTag(render(Toggle, { props: { ariaLabel: "x", checked: true } }).body)).toContain('aria-checked="true"');
    });

    it("names a bare switch by its required aria-label and renders no label row", () => {
      const html = render(Toggle, { props: { ariaLabel: "Reduce motion" } }).body;
      expect(switchTag(html)).toContain('aria-label="Reduce motion"');
      expect(html).not.toContain("<label");
    });

    it("names a labelled switch by its visible text, pointing the label at the switch", () => {
      const html = render(Toggle, { props: { label: "Reduce motion", id: "motion" } }).body;
      expect(html).toMatch(/<label\b[^>]*for="motion"[^>]*>Reduce motion<\/label>/);
      expect(switchTag(html)).toContain('id="motion"');
      expect(switchTag(html)).toContain('aria-label="Reduce motion"');
    });

    it("lets aria-label override the visible text when the text needs context", () => {
      const html = render(Toggle, { props: { label: "Enable", ariaLabel: "Enable terminal bell" } }).body;
      expect(switchTag(html)).toContain('aria-label="Enable terminal bell"');
      expect(html).toContain(">Enable</label>");
    });

    it("generates the id from $props.id() when none is passed, so each instance on a page is unique", () => {
      // Server renders restart the counter, so uniqueness across instances is
      // Svelte's contract for $props.id(); what is checked here is that the
      // label and the switch still agree on the generated id.
      const html = render(Toggle, { props: { label: "Sound" } }).body;
      const forId = /<label[^>]*for="(toggle-[^"]+)"/.exec(html)?.[1];
      expect(forId).toBeTruthy();
      expect(switchTag(html)).toContain(`id="${forId}"`);
      expect(source).toMatch(/const uid = \$props\.id\(\);/);
    });

    it("renders the description under the label", () => {
      const html = render(Toggle, { props: { label: "Sound", description: "Plays a tone when a run ends." } }).body;
      expect(html).toContain("Plays a tone when a run ends.");
    });

    it("rejects a switch with neither a visible label nor an aria-label at compile time", () => {
      // @ts-expect-error a switch with no name is a type error
      const unnamed: ComponentProps<typeof Toggle> = { checked: true };
      const named: ComponentProps<typeof Toggle> = { checked: true, ariaLabel: "x" };
      const labelled: ComponentProps<typeof Toggle> = { checked: true, label: "x" };
      expect([unnamed, named, labelled]).toHaveLength(3);
    });
  });

  describe("states", () => {
    it("draws the 32x18 track and 14px knob", () => {
      const html = render(Toggle, { props: { ariaLabel: "x" } }).body;
      expect(html).toContain("h-[18px]");
      expect(html).toContain("w-8");
      expect(html).toContain("h-[14px] w-[14px]");
    });

    it("tints the checked track with the accent and the unchecked one with the border role", () => {
      expect(switchTag(render(Toggle, { props: { ariaLabel: "x", checked: true } }).body)).toContain("bg-accent");
      const off = switchTag(render(Toggle, { props: { ariaLabel: "x" } }).body);
      expect(off).toContain("bg-border");
      expect(off).not.toContain("bg-accent");
    });

    it("moves the knob only when checked, and keeps it white in both states and both themes, as SettingToggle always drew it", () => {
      const on = render(Toggle, { props: { ariaLabel: "x", checked: true } }).body;
      const off = render(Toggle, { props: { ariaLabel: "x" } }).body;
      expect(on).toContain("translate-x-[14px]");
      expect(off).toContain("translate-x-0");
      expect(off).not.toContain("translate-x-[14px]");
      for (const html of [on, off]) {
        expect(html).toMatch(/<span class="[^"]*\bbg-white\b/);
        expect(html).not.toMatch(/<span class="[^"]*\bbg-(?:onAccent|textPrimary)\b/);
      }
    });

    it("sets the disabled attribute and dims the whole labelled row", () => {
      const bare = render(Toggle, { props: { ariaLabel: "x", disabled: true } }).body;
      expect(switchTag(bare)).toMatch(/\sdisabled(\s|=|>)/);
      expect(switchTag(bare)).toContain("disabled:opacity-40");
      const row = render(Toggle, { props: { label: "Sound", disabled: true } }).body;
      expect(row).toContain("opacity-40");
    });

    it("leaves an enabled switch without the disabled attribute", () => {
      expect(switchTag(render(Toggle, { props: { ariaLabel: "x" } }).body)).not.toMatch(/\sdisabled(\s|=|>)/);
    });

    it("shows focus through the global button:focus-visible ring and removes none of it", () => {
      expect(source).not.toMatch(/outline-none|outline-hidden|focus:outline|focus-visible:/);
    });
  });

  describe("press behavior", () => {
    it("asks for the opposite value", () => {
      expect(requestedState(false, false)).toBe(true);
      expect(requestedState(true, false)).toBe(false);
    });

    it("asks for nothing while disabled", () => {
      expect(requestedState(false, true)).toBeNull();
      expect(requestedState(true, true)).toBeNull();
    });

    it("is controlled: it never assigns checked itself", () => {
      // The only `checked =` is the `= false` default of the destructured prop.
      expect(source).not.toMatch(/(?<![-\w])checked\s*=(?!\s*false)/);
      expect(source).not.toContain("$bindable");
      expect(source).toContain("onchange?.(next)");
    });
  });

  describe("tokens", () => {
    it("animates only colour and transform, on motion tokens, with no literal duration", () => {
      expect(source).toMatch(/transition-\[background-color\]/);
      // The bracket form is what the reduced-motion lists match (`[class*="transition-["]`);
      // plain `transition-transform` is in neither list and would keep animating.
      expect(source).toMatch(/transition-\[transform\]/);
      expect(source).not.toMatch(/\btransition-transform\b/);
      expect(source).toContain("duration-(--gp-dur-instant)");
      expect(source).toContain("duration-(--gp-dur-fast)");
      expect(source).not.toMatch(/\bduration-\d|\b\d+ms\b|cubic-bezier/);
      expect(source).not.toMatch(/transition-all|transition-\[(?:width|height)/);
    });

    it("carries no literal palette color, hex, z-index or radius (the knob's bg-white is the one fixed color, unchanged from SettingToggle)", () => {
      expect(source).not.toMatch(/#[0-9a-fA-F]{3,8}\b|rgb\(|\b(?:zinc|slate|gray|amber|rose|emerald|sky)-\d|\bz-\[|\bz-\d+\b|rounded-\[|text-\[\d+px\]/);
    });
  });
});

describe("SettingToggle delegates its switch to Toggle", () => {
  it("renders the same switch Toggle does, named by its label, inside the settings row", () => {
    const html = render(SettingToggle, { props: { label: "Reduce motion", description: "Drops entrances.", checked: true } }).body;
    const tag = switchTag(html);
    expect(tag).toContain('role="switch"');
    expect(tag).toContain('aria-checked="true"');
    expect(tag).toContain('aria-label="Reduce motion"');
    expect(tag).toMatch(/id="setting-toggle-[a-z0-9]+"/);
    expect(html).toMatch(/<label\b[^>]*for="setting-toggle-[a-z0-9]+"[^>]*>Reduce motion<\/label>/);
    expect(html).toContain("Drops entrances.");
  });

  it("points the label at the switch it rendered", () => {
    const html = render(SettingToggle, { props: { label: "Sound" } }).body;
    const forId = /<label[^>]*for="([^"]+)"/.exec(html)?.[1];
    expect(switchTag(html)).toContain(`id="${forId}"`);
  });

  it("keeps an explicit ariaLabel over the visible label", () => {
    const html = render(SettingToggle, { props: { label: "Reduce motion", ariaLabel: "Reduce interface motion" } }).body;
    expect(switchTag(html)).toContain('aria-label="Reduce interface motion"');
  });

  it("passes disabled to the switch and dims the label text", () => {
    const html = render(SettingToggle, { props: { label: "Sound", disabled: true } }).body;
    expect(switchTag(html)).toMatch(/\sdisabled(\s|=|>)/);
    expect(html).toContain("opacity-40");
  });
});
