import { readFileSync } from "node:fs";
import { writable } from "svelte/store";
import { describe, expect, it } from "vitest";
import {
  HEADER_LABELS_ATTRIBUTE,
  HEADER_LABEL_MODES,
  applyHeaderLabels,
  installHeaderLabels,
  isHeaderLabelMode,
  readHeaderLabelMode,
  type HeaderLabelMode,
} from "./headerLabels";

function fakeRoot() {
  const attributes = new Map<string, string>();
  return {
    attributes,
    setAttribute: (name: string, value: string) => void attributes.set(name, value),
    removeAttribute: (name: string) => void attributes.delete(name),
  };
}

describe("isHeaderLabelMode", () => {
  it("accepts exactly the modes a control can produce", () => {
    for (const mode of HEADER_LABEL_MODES) expect(isHeaderLabelMode(mode)).toBe(true);
    for (const value of ["", "words", "ICONS", 1, true, null, undefined, {}]) {
      expect(isHeaderLabelMode(value), JSON.stringify(value)).toBe(false);
    }
  });
});

describe("readHeaderLabelMode", () => {
  it("maps the boolean the setting used to be stored as", () => {
    expect(readHeaderLabelMode({ showHeaderActionLabels: false }, "auto")).toBe("icons");
    expect(readHeaderLabelMode({ showHeaderActionLabels: true }, "auto")).toBe("auto");
  });

  it("prefers the current key over a stale old one", () => {
    expect(readHeaderLabelMode({ headerLabels: "auto", showHeaderActionLabels: false }, "auto")).toBe("auto");
    expect(readHeaderLabelMode({ headerLabels: "icons", showHeaderActionLabels: true }, "auto")).toBe("icons");
  });

  it("ignores anything it cannot read, then falls back", () => {
    expect(readHeaderLabelMode({}, "auto")).toBe("auto");
    expect(readHeaderLabelMode({ headerLabels: "words" }, "auto")).toBe("auto");
    expect(readHeaderLabelMode({ showHeaderActionLabels: "no" }, "auto")).toBe("auto");
    expect(readHeaderLabelMode({ showHeaderActionLabels: 0 }, "icons")).toBe("icons");
  });
});

describe("applyHeaderLabels", () => {
  it("is present only for Icons only, so absent means the width decides", () => {
    const root = fakeRoot();
    applyHeaderLabels("icons", root);
    expect(root.attributes.get(HEADER_LABELS_ATTRIBUTE)).toBe("icons");
    applyHeaderLabels("auto", root);
    expect(root.attributes.has(HEADER_LABELS_ATTRIBUTE)).toBe(false);
  });

  it("does nothing, and does not throw, without a document", () => {
    expect(() => applyHeaderLabels("icons")).not.toThrow();
  });
});

describe("installHeaderLabels", () => {
  it("follows the store, including the first value", () => {
    const root = fakeRoot();
    const prefs = writable<{ headerLabels: HeaderLabelMode }>({ headerLabels: "icons" });
    const stop = installHeaderLabels(prefs, root);
    expect(root.attributes.get(HEADER_LABELS_ATTRIBUTE)).toBe("icons");
    prefs.set({ headerLabels: "auto" });
    expect(root.attributes.has(HEADER_LABELS_ATTRIBUTE)).toBe(false);
    stop();
    prefs.set({ headerLabels: "icons" });
    expect(root.attributes.has(HEADER_LABELS_ATTRIBUTE)).toBe(false);
  });
});

describe("headerLabels.css", () => {
  const css = readFileSync(new URL("./headerLabels.css", import.meta.url), "utf8");

  it("answers the attribute the store writes", () => {
    expect(css).toContain(`html[${HEADER_LABELS_ATTRIBUTE}="icons"]`);
  });

  it("targets the tier classes the header's labels carry", () => {
    // These are the classes lane A's header components put on a label; a label
    // moved to another tier has to move here too, and this names what breaks.
    expect(css).toContain('[class*="@min-[1100px]:inline"]');
    expect(css).toContain('[class*="@min-[1100px]:hidden"]');
    expect(css).toContain('[class*="max-[1099px]:sr-only"]');
  });
});
