import { describe, expect, it } from "vitest";
import { anchorOwnsOpenPopup, remigrateTitle, tipGuideOf, tipTextOf, tooltipAnchorFromTarget, type TipHost } from "./tipText";

class FakeTipHost implements TipHost {
  attributes = new Map<string, string>();
  nodeName: string;
  constructor(
    attributes: Record<string, string> = {},
    private text = "",
    options: { nodeName?: string; closest?: () => unknown } = {},
  ) {
    for (const [name, value] of Object.entries(attributes)) {
      this.attributes.set(name, value);
    }
    this.nodeName = options.nodeName ?? "DIV";
    this._closest = options.closest ?? (() =>
      this.hasAttribute("title") ||
      this.hasAttribute("data-tip-text") ||
      this.hasAttribute("data-tip-guide")
        ? this
        : null
    );
  }
  private _closest: () => unknown;
  closest(_selectors: string): unknown {
    return this._closest();
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

  it("takes a title written back after migration as the newer text", () => {
    // A branch row re-renders when its age or counts change, and Svelte
    // writes the new text into `title` on a button already migrated. Reading
    // the stored text first left that title on — the OS bubble came back over
    // the styled one — and showed the text from before the change.
    const el = new FakeTipHost({ title: "main\nAlex · just now" }, "main");
    tipTextOf(el);
    el.setAttribute("title", "main\nRobin · 1 min ago");
    expect(tipTextOf(el)).toBe("main\nRobin · 1 min ago");
    expect(el.getAttribute("data-tip-text")).toBe("main\nRobin · 1 min ago");
    expect(el.hasAttribute("title")).toBe(false);
  });

  it("keeps a mirrored accessible name in step with the title it came from", () => {
    const el = new FakeTipHost({ title: "Pin branch" }, "");
    tipTextOf(el);
    el.setAttribute("title", "Unpin branch");
    tipTextOf(el);
    expect(el.getAttribute("aria-label")).toBe("Unpin branch");
  });

  it("never rewrites an author's own label, even one that matched the old title", () => {
    const el = new FakeTipHost({ title: "Close", "aria-label": "Close" }, "");
    tipTextOf(el);
    el.setAttribute("title", "Close (⌘W)");
    tipTextOf(el);
    expect(el.getAttribute("aria-label")).toBe("Close");
  });

  it("drops a mirrored name when the title it came from empties", () => {
    const el = new FakeTipHost({ title: "Pin" }, "");
    tipTextOf(el);
    el.setAttribute("title", "");
    expect(tipTextOf(el)).toBe("");
    expect(el.hasAttribute("aria-label")).toBe(false);
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

describe("tooltipAnchorFromTarget", () => {
  it("resolves a titled control the way the global tooltip does", () => {
    const button = new FakeTipHost({ title: "Pin branch" }, "★");
    expect(tooltipAnchorFromTarget(button)).toBe(button);
    expect(button.getAttribute("data-tip-text")).toBe("Pin branch");
  });

  it("does not steal a graph node hover via a titled canvas ancestor", () => {
    const gutter = new FakeTipHost({
      title: "Wide graph — scroll horizontally to see more lanes",
    });
    const canvas = new FakeTipHost({}, "", {
      nodeName: "CANVAS",
      closest: () => gutter,
    });
    expect(tooltipAnchorFromTarget(canvas)).toBeNull();
    // Ancestor must stay untouched: migrating it would still pop the layout
    // hint on the next non-canvas mouseover.
    expect(gutter.getAttribute("title")).toBe(
      "Wide graph — scroll horizontally to see more lanes",
    );
    expect(gutter.getAttribute("data-tip-text")).toBeNull();
  });

  it("still tooltips a canvas that carries its own title", () => {
    const canvas = new FakeTipHost({ title: "Commit graph" }, "", {
      nodeName: "CANVAS",
    });
    expect(tooltipAnchorFromTarget(canvas)).toBe(canvas);
    expect(canvas.getAttribute("data-tip-text")).toBe("Commit graph");
  });

  it("resolves a destination-guide tab without migrating a title", () => {
    const tab = new FakeTipHost({ "data-tip-guide": "work" }, "Work");
    expect(tipGuideOf(tab)).toBe("work");
    expect(tooltipAnchorFromTarget(tab)).toBe(tab);
    expect(tab.getAttribute("data-tip-text")).toBeNull();
  });

  it("strips a leftover title so the OS bubble cannot flash beside the card", () => {
    const tab = new FakeTipHost(
      { "data-tip-guide": "history:diff", title: "Diff (⌥2)" },
      "Diff",
    );
    expect(tooltipAnchorFromTarget(tab)).toBe(tab);
    expect(tab.getAttribute("title")).toBeNull();
    expect(tab.getAttribute("data-tip-text")).toBeNull();
  });

  it("ignores a whitespace-only guide and falls through to title text", () => {
    const el = new FakeTipHost({ "data-tip-guide": "  ", title: "Save" }, "Save");
    expect(tipGuideOf(el)).toBeNull();
    expect(tooltipAnchorFromTarget(el)).toBe(el);
    expect(el.getAttribute("data-tip-text")).toBe("Save");
  });
});

describe("anchorOwnsOpenPopup", () => {
  it("is true only for a control whose own popup is open", () => {
    expect(anchorOwnsOpenPopup(new FakeTipHost({ "aria-expanded": "true", title: "Toggle" }))).toBe(true);
    expect(anchorOwnsOpenPopup(new FakeTipHost({ "aria-expanded": "false", title: "Toggle" }))).toBe(false);
    expect(anchorOwnsOpenPopup(new FakeTipHost({ title: "Toggle" }))).toBe(false);
  });
});

describe("remigrateTitle", () => {
  it("takes a written-back title off an anchor already taken over", () => {
    const el = new FakeTipHost({ title: "feature/a\nAlex" }, "feature/a");
    tipTextOf(el);
    el.setAttribute("title", "feature/a\nRobin");
    expect(remigrateTitle(el)).toBe("feature/a\nRobin");
    expect(el.hasAttribute("title")).toBe(false);
  });

  it("leaves an element it never took over for its first hover", () => {
    const el = new FakeTipHost({ title: "Save" }, "Save");
    expect(remigrateTitle(el)).toBeNull();
    expect(el.getAttribute("title")).toBe("Save");
    expect(el.hasAttribute("data-tip-text")).toBe(false);
  });

  it("strips a title from a destination-guide anchor without inventing text", () => {
    const el = new FakeTipHost({ "data-tip-guide": "work", title: "Work" });
    expect(remigrateTitle(el)).toBeNull();
    expect(el.hasAttribute("title")).toBe(false);
    expect(el.hasAttribute("data-tip-text")).toBe(false);
  });

  it("does nothing when no title came back", () => {
    const el = new FakeTipHost({ "data-tip-text": "Pin" });
    expect(remigrateTitle(el)).toBeNull();
    expect(el.getAttribute("data-tip-text")).toBe("Pin");
  });
});
