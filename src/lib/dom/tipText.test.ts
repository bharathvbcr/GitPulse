import { describe, expect, it } from "vitest";
import { tipTextOf, type TipHost } from "./tipText";

class FakeTipHost implements TipHost {
  attributes = new Map<string, string>();
  constructor(
    attributes: Record<string, string> = {},
    private text = "",
  ) {
    for (const [name, value] of Object.entries(attributes)) {
      this.attributes.set(name, value);
    }
  }
  getAttribute(name: string): string | null {
    return this.attributes.has(name) ? (this.attributes.get(name) as string) : null;
  }
  hasAttribute(name: string): boolean {
    return this.attributes.has(name);
  }
  setAttribute(name: string, value: string): void {
    this.attributes.set(name, value);
  }
  removeAttribute(name: string): void {
    this.attributes.delete(name);
  }
  get textContent(): string | null {
    return this.text;
  }
}

describe("tipTextOf", () => {
  it("migrates title to data-tip-text and removes the native bubble", () => {
    const el = new FakeTipHost({ title: "Save changes" });
    expect(tipTextOf(el)).toBe("Save changes");
    expect(el.getAttribute("data-tip-text")).toBe("Save changes");
    expect(el.hasAttribute("title")).toBe(false);
  });

  it("mirrors the title into aria-label for icon-only controls", () => {
    const el = new FakeTipHost({ title: "Close" }, "");
    tipTextOf(el);
    expect(el.getAttribute("aria-label")).toBe("Close");
  });

  it("does not overwrite an existing non-empty aria-label", () => {
    const el = new FakeTipHost({ title: "Close", "aria-label": "Close tab" }, "");
    tipTextOf(el);
    expect(el.getAttribute("aria-label")).toBe("Close tab");
  });

  it("respects aria-labelledby as an existing name", () => {
    const el = new FakeTipHost({
      title: "Close",
      "aria-labelledby": "close-label",
    });
    tipTextOf(el);
    expect(el.hasAttribute("aria-label")).toBe(false);
  });

  it("skips the mirror when visible text already names the element", () => {
    const el = new FakeTipHost({ title: "Save" }, " Save ");
    tipTextOf(el);
    expect(el.hasAttribute("aria-label")).toBe(false);
  });

  it("is idempotent once migrated", () => {
    const el = new FakeTipHost({ title: "Pin" }, "");
    expect(tipTextOf(el)).toBe("Pin");
    expect(tipTextOf(el)).toBe("Pin");
    // No re-migration churn: no duplicate title handling.
    expect(el.hasAttribute("title")).toBe(false);
  });

  it("returns empty for elements without any tooltip source", () => {
    const el = new FakeTipHost();
    expect(tipTextOf(el)).toBe("");
    expect(el.attributes.size).toBe(0);
  });

  it("migrates an empty title without inventing an accessible name", () => {
    const el = new FakeTipHost({ title: "" }, "");
    expect(tipTextOf(el)).toBe("");
    expect(el.getAttribute("data-tip-text")).toBe("");
    expect(el.hasAttribute("aria-label")).toBe(false);
    expect(el.hasAttribute("title")).toBe(false);
  });
});
