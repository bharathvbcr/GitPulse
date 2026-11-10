import { readFileSync } from "node:fs";
import { afterEach, describe, expect, it } from "vitest";
import { createRawSnippet } from "svelte";
import { compile } from "svelte/compiler";
import { render } from "svelte/server";
import Dialog, { closesOnKey, preferredFocus, sheetOffset, sheetSlide } from "./Dialog.svelte";
import { BACKDROP_IN_MS, BACKDROP_OUT_MS, CARD_IN_MS, CARD_OUT_MS } from "../ui/transitions";
import { setReduceMotionOverride, type MediaMatch } from "../motion/easing";
import { LAYERS } from "../ui/layers";
import { trapFocus } from "../ui/focusTrap";

const source = readFileSync(new URL("./Dialog.svelte", import.meta.url), "utf8");

const text = (value: string) => createRawSnippet(() => ({ render: () => `<p>${value}</p>` }));

function dialog(props: Record<string, unknown> = {}): string {
  return render(Dialog, {
    props: { open: true, title: "Delete branch?", children: text("body text"), ...props },
  } as never).body;
}

const media = (matches: boolean): MediaMatch => ({ matchMedia: () => ({ matches }) as MediaQueryList });

afterEach(() => setReduceMotionOverride(false));

describe("Dialog markup", () => {
  it("compiles without warnings", () => {
    const { warnings } = compile(source, { generate: "client", filename: "Dialog.svelte" });
    expect(warnings).toEqual([]);
  });

  it("renders nothing while closed", () => {
    expect(dialog({ open: false })).not.toContain("role=\"dialog\"");
  });

  it("is a modal dialog named by its title", () => {
    const html = dialog();
    expect(html).toContain('role="dialog"');
    expect(html).toContain('aria-modal="true"');
    const labelledBy = /aria-labelledby="([^"]+)"/.exec(html)?.[1];
    expect(labelledBy).toBeTruthy();
    // Svelte's server output brackets dynamic text in <!--[0--> comments.
    expect(html).toMatch(new RegExp(`<h2 id="${labelledBy}"[^>]*>(?:<!--[^>]*-->)*Delete branch\\?`));
    // aria-modal is the marker isModalOccupied() looks for.
    expect(html.match(/aria-modal="true"/g)).toHaveLength(1);
  });

  it("lays the card on a scrim at the modal layer, from LAYERS and not a literal", () => {
    const html = dialog();
    expect(html).toContain("gp-scrim");
    expect(html).toContain("gp-card");
    expect(html).toContain(`z-index: ${LAYERS.MODAL};`);
    expect(source).toContain("LAYERS.MODAL");
    expect(source).not.toMatch(/z-\[|\bz-\d+\b|z-index:\s*\d/);
  });

  it("accepts a snippet as the title", () => {
    const html = dialog({ title: createRawSnippet(() => ({ render: () => "<span>Rebase <b>main</b></span>" })) });
    expect(html).toContain("<span>Rebase <b>main</b></span>");
  });

  it("renders the body and, only when given, the footer", () => {
    const withFooter = dialog({ footer: text("footer buttons") });
    expect(withFooter).toContain("body text");
    expect(withFooter).toContain("data-dialog-footer");
    expect(withFooter).toContain("footer buttons");
    expect(dialog()).not.toContain("data-dialog-footer");
  });

  describe("sizes", () => {
    it("md is 480 wide and lg is 720, centred in the scrim", () => {
      const md = dialog({ size: "md" });
      expect(md).toContain('data-dialog-size="md"');
      expect(md).toContain("max-w-120");
      expect(md).toContain("items-center justify-center");
      const lg = dialog({ size: "lg" });
      expect(lg).toContain('data-dialog-size="lg"');
      expect(lg).toContain("max-w-180");
      // Tailwind's spacing unit is 4px: 120 * 4 = 480, 180 * 4 = 720.
      expect(120 * 4).toBe(480);
      expect(180 * 4).toBe(720);
    });

    it("sheet is a full-height panel anchored to the chosen edge", () => {
      const right = dialog({ size: "sheet" });
      expect(right).toContain('data-dialog-size="sheet"');
      expect(right).toContain("h-full");
      expect(right).toContain("justify-end");
      expect(right).not.toContain("items-center justify-center");
      expect(right).toContain("var(--gp-r-xl) 0 0 var(--gp-r-xl)");
      const left = dialog({ size: "sheet", edge: "left" });
      expect(left).toContain("justify-start");
      expect(left).toContain("0 var(--gp-r-xl) var(--gp-r-xl) 0");
    });

    it("rounds md and lg with the xl radius token, never a literal", () => {
      expect(dialog()).toContain("border-radius: var(--gp-r-xl)");
      expect(source).not.toMatch(/rounded-\[|rounded-3xl|\d+px\s*\)?;?"/);
    });
  });

  describe("dismissal", () => {
    it("shows a labelled close button when it can be dismissed", () => {
      expect(dialog()).toMatch(/<button[^>]*aria-label="Close"/);
    });

    it("drops the close button when dismissable is false", () => {
      expect(dialog({ dismissable: false })).not.toContain('aria-label="Close"');
    });

    it("closes on Escape only when dismissable", () => {
      expect(closesOnKey({ key: "Escape" }, true)).toBe(true);
      expect(closesOnKey({ key: "Escape" }, false)).toBe(false);
    });

    it("ignores every other key", () => {
      for (const key of ["Enter", " ", "Tab", "a", "ArrowDown", "Backspace"]) {
        expect(closesOnKey({ key }, true), key).toBe(false);
      }
    });

    it("does not close on the Escape that cancels an IME composition", () => {
      expect(closesOnKey({ key: "Escape", isComposing: true }, true)).toBe(false);
      expect(closesOnKey({ key: "Escape", keyCode: 229 }, true)).toBe(false);
    });

    it("guards close() itself, so no path closes a non-dismissable dialog", () => {
      expect(source).toMatch(/function close\(\) \{\s*if \(!dismissable\) return;/);
    });

    it("closes on a scrim click only when the press began on the scrim", () => {
      expect(source).toContain("pressedOnScrim = event.target === event.currentTarget;");
      expect(source).toContain("const hit = pressedOnScrim && event.target === event.currentTarget;");
    });
  });
});

describe("Dialog enter and exit", () => {
  it("gives the scrim a fade in and a fade out", () => {
    expect(source).toContain("in:fade={backdropFade()}");
    expect(source).toContain("out:fade={backdropFadeOut()}");
  });

  it("gives the centered card a scale in and a scale out", () => {
    expect(source).toContain("in:scale|global={cardScale()}");
    expect(source).toContain("out:scale|global={cardScaleOut()}");
  });

  it("gives the sheet a slide in and a slide out", () => {
    expect(source).toContain("in:sheetSlide|global={{ edge }}");
    expect(source).toContain("out:sheetSlide|global={{ edge, outro: true }}");
  });

  it("plays the card's transitions globally, because it sits in a nested block", () => {
    // Svelte transitions are local by default: they play only when the block
    // that directly holds the element is created or destroyed. The card is
    // inside `{#if sheet}`, inside `{#if open}`, so a local outro would be
    // skipped when `open` goes false, leaving only the scrim's fade. Found by
    // listing document.getAnimations() after Escape: the scrim had its 70 ms
    // outro and the card had none.
    for (const directive of source.match(/\b(?:in|out):(?:scale|sheetSlide)[^=\s]*=/g) ?? []) {
      expect(directive, directive).toContain("|global");
    }
    expect(source.match(/\b(?:in|out):(?:scale|sheetSlide)\|global=/g)).toHaveLength(4);
  });

  it("has exactly as many outros as intros, so no surface lacks one", () => {
    const intros = source.match(/\bin:[a-zA-Z]+(?:\|global)?=/g) ?? [];
    const outros = source.match(/\bout:[a-zA-Z]+(?:\|global)?=/g) ?? [];
    expect(intros).toHaveLength(3);
    expect(outros).toHaveLength(intros.length);
    const kind = (directive: string) => directive.replace(/^(?:in|out):/, "");
    expect(outros.map(kind).sort()).toEqual(intros.map(kind).sort());
  });

  it("carries no literal duration or easing of its own", () => {
    expect(source).not.toMatch(/\b\d+ms\b|duration:\s*\d|cubic-bezier|\bdelay:\s*\d/);
  });

  describe("sheetSlide", () => {
    const node = {} as Element;

    it("enters on the card's slow token and exits on its shorter exit token", () => {
      const enter = sheetSlide(node, { edge: "right", media: media(false) });
      const exit = sheetSlide(node, { edge: "right", outro: true, media: media(false) });
      expect(enter.duration).toBe(CARD_IN_MS);
      expect(exit.duration).toBe(CARD_OUT_MS);
      expect(exit.duration as number).toBeLessThan(enter.duration as number);
      // The scrim obeys the same rule, which is what the card is paired with.
      expect(BACKDROP_OUT_MS).toBeLessThan(BACKDROP_IN_MS);
    });

    it("slides from the right edge in and back out", () => {
      const enter = sheetSlide(node, { edge: "right", media: media(false) });
      expect(enter.css?.(0, 1)).toBe("transform: translateX(100%);");
      expect(enter.css?.(1, 0)).toBe("transform: translateX(0%);");
      const exit = sheetSlide(node, { edge: "right", outro: true, media: media(false) });
      expect(exit.css?.(0, 1)).toBe("transform: translateX(100%); opacity: 0;");
      expect(exit.css?.(1, 0)).toBe("transform: translateX(0%); opacity: 1;");
    });

    it("slides from the left edge the other way", () => {
      const enter = sheetSlide(node, { edge: "left", media: media(false) });
      expect(enter.css?.(0, 1)).toBe("transform: translateX(-100%);");
      expect(sheetOffset(0.5, "left")).toBe(-50);
      expect(sheetOffset(0.5, "right")).toBe(50);
    });

    it("never overshoots the edge it is anchored to, even when the spring does", () => {
      expect(sheetOffset(1.09, "right")).toBe(0);
      expect(sheetOffset(1.09, "left")).toBe(0);
      expect(sheetOffset(-0.2, "right")).toBe(100);
      const enter = sheetSlide(node, { edge: "right", media: media(false) });
      expect(enter.css?.(1.09, -0.09)).toBe("transform: translateX(0%);");
    });

    it("uses transform and opacity only", () => {
      const enter = sheetSlide(node, { edge: "right", media: media(false) });
      const exit = sheetSlide(node, { edge: "right", outro: true, media: media(false) });
      for (const css of [enter.css?.(0.5, 0.5) ?? "", exit.css?.(0.5, 0.5) ?? ""]) {
        for (const property of css.split(";").map((part) => part.split(":")[0].trim()).filter(Boolean)) {
          expect(["transform", "opacity"], property).toContain(property);
        }
      }
    });

    it("drops to zero duration under the system's reduce-motion preference", () => {
      expect(sheetSlide(node, { edge: "right", media: media(true) }).duration).toBe(0);
      expect(sheetSlide(node, { edge: "right", outro: true, media: media(true) }).duration).toBe(0);
    });

    it("drops to zero duration under the in-app reduce-motion switch, whatever the system says", () => {
      setReduceMotionOverride(true);
      expect(sheetSlide(node, { edge: "right", media: media(false) }).duration).toBe(0);
    });
  });
});

describe("Dialog focus", () => {
  type FakeNode = { querySelector: (s: string) => FakeRegion | null };
  type FakeRegion = { querySelectorAll: (s: string) => FakeFocusable[] };
  type FakeFocusable = { id: string; hasAttribute: (n: string) => boolean; getAttribute: (n: string) => string | null };

  const focusable = (id: string): FakeFocusable => ({ id, hasAttribute: () => false, getAttribute: () => null });
  const card = (body: FakeFocusable[], footer: FakeFocusable[]): FakeNode => ({
    querySelector: (selector) => {
      if (selector === "[data-dialog-body]") return { querySelectorAll: () => body };
      if (selector === "[data-dialog-footer]") return { querySelectorAll: () => footer };
      return null;
    },
  });

  it("lands on the first control in the body, not on the header's Close button", () => {
    const field = focusable("branch-name");
    expect(preferredFocus(card([field, focusable("second")], [focusable("ok")]))).toBe(field);
  });

  it("falls to the footer when the body has nothing focusable", () => {
    const ok = focusable("ok");
    expect(preferredFocus(card([], [ok, focusable("cancel")]))).toBe(ok);
  });

  it("falls to the card itself when only the Close button exists", () => {
    const empty = card([], []);
    expect(preferredFocus(empty)).toBe(empty);
  });

  it("is wired into the focus trap, with initialFocus taking precedence", () => {
    expect(source).toContain("use:trapped");
    expect(source).toMatch(/return trapFocus\(card, \{\s*initial: \(\) => \{\s*const requested = initialFocus \? initialFocus\(\) : null;\s*if \(requested\) return requested;/);
    expect(source).toContain("preferredFocus(card)");
  });
});

/**
 * The shell's trap, as the Dialog wires it: trapFocus records the opener on
 * mount, cycles Tab inside the card, and hands focus back on destroy. The
 * action has its own suite; this drives it through the same options Dialog
 * passes, against a stand-in document, because the unit environment is Node.
 */
describe("Dialog trap behavior through trapFocus", () => {
  class Fake {
    attrs = new Map<string, string>();
    listeners = new Map<string, (e: unknown) => void>();
    children: Fake[] = [];
    isConnected = true;
    constructor(readonly name: string, attrs: Record<string, string> = {}) {
      for (const [k, v] of Object.entries(attrs)) this.attrs.set(k, v);
    }
    getAttribute(n: string) {
      return this.attrs.get(n) ?? null;
    }
    hasAttribute(n: string) {
      return this.attrs.has(n);
    }
    focus() {
      (globalThis as unknown as { document: { activeElement: unknown } }).document.activeElement = this;
    }
    addEventListener(type: string, fn: (e: unknown) => void) {
      this.listeners.set(type, fn);
    }
    removeEventListener(type: string) {
      this.listeners.delete(type);
    }
    querySelectorAll() {
      return this.children;
    }
  }

  const g = globalThis as unknown as Record<string, unknown>;
  afterEach(() => {
    delete g.document;
    delete g.HTMLElement;
  });

  function setup() {
    g.HTMLElement = Fake;
    const opener = new Fake("opener");
    g.document = { activeElement: opener };
    const field = new Fake("field");
    const close = new Fake("close");
    const ok = new Fake("ok");
    const card = new Fake("card", { tabindex: "-1" });
    card.children = [close, field, ok];
    return { opener, field, close, ok, card };
  }

  const active = () => (g.document as { activeElement: Fake }).activeElement;

  it("moves focus inside on open to the element Dialog asks for", () => {
    const { card, field } = setup();
    const action = trapFocus(card as unknown as HTMLElement, { initial: () => field as unknown as HTMLElement });
    expect(active()).toBe(field);
    action.destroy();
  });

  it("wraps Tab at both ends so focus cannot leave the card", () => {
    const { card, ok, close } = setup();
    const action = trapFocus(card as unknown as HTMLElement, { initial: () => ok as unknown as HTMLElement });
    const tab = (shiftKey: boolean) => card.listeners.get("keydown")?.({ key: "Tab", shiftKey, preventDefault() {} });
    tab(false);
    expect(active()).toBe(close);
    tab(true);
    expect(active()).toBe(ok);
    action.destroy();
  });

  it("returns focus to the opener when the dialog is destroyed", () => {
    const { card, field, opener } = setup();
    const action = trapFocus(card as unknown as HTMLElement, { initial: () => field as unknown as HTMLElement });
    expect(active()).toBe(field);
    action.destroy();
    expect(active()).toBe(opener);
  });

  it("does not steal focus back to an opener that left the page", () => {
    const { card, field, opener } = setup();
    const action = trapFocus(card as unknown as HTMLElement, { initial: () => field as unknown as HTMLElement });
    opener.isConnected = false;
    action.destroy();
    expect(active()).toBe(field);
  });
});
