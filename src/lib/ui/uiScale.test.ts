import { describe, expect, it, vi } from "vitest";
import {
  applyUiScale,
  MAX_UI_SCALE,
  MIN_UI_SCALE,
  normalizeUiScale,
} from "./uiScale";

function fakeRoot() {
  const props = new Map<string, string>();
  return {
    props,
    style: {
      setProperty: (name: string, value: string) => void props.set(name, value),
    },
  };
}

describe("normalizeUiScale", () => {
  it("clamps to the supported range", () => {
    expect(normalizeUiScale(0.1)).toBe(MIN_UI_SCALE);
    expect(normalizeUiScale(9)).toBe(MAX_UI_SCALE);
    expect(normalizeUiScale(1.25)).toBe(1.25);
  });

  it("refuses non-finite values rather than forwarding them to the webview", () => {
    // NaN reaches WKWebView's setPageZoom as NaN and blanks the window.
    expect(normalizeUiScale(Number.NaN)).toBe(1);
    expect(normalizeUiScale(Number.POSITIVE_INFINITY)).toBe(1);
  });
});

describe("applyUiScale", () => {
  it("drives the native zoom when one is available", async () => {
    const setZoom = vi.fn(async () => {});
    const root = fakeRoot();

    const mode = await applyUiScale(1.25, { setZoom, root });

    expect(mode).toBe("webview-zoom");
    expect(setZoom).toHaveBeenCalledWith(1.25);
  });

  it("leaves the CSS scale at identity when native zoom applied", async () => {
    // Both mechanisms firing would scale the root font on top of a webview
    // that has already scaled the whole document.
    const root = fakeRoot();

    await applyUiScale(1.5, { setZoom: async () => {}, root });

    expect(root.props.get("--ui-font-scale")).toBe("1");
  });

  it("writes the variable to the element the stylesheet reads it from", async () => {
    // The defect this module replaces: the value was written to a descendant
    // of the element whose rule consumed it, so it could never apply.
    const root = fakeRoot();

    const mode = await applyUiScale(1.25, { setZoom: null, root });

    expect(mode).toBe("css-fallback");
    expect(root.props.get("--ui-font-scale")).toBe("1.25");
  });

  it("falls back to CSS when the native zoom call is refused", async () => {
    const root = fakeRoot();
    const setZoom = vi.fn(async () => {
      throw new Error("webview.setZoom not permitted");
    });

    const mode = await applyUiScale(1.4, { setZoom, root });

    expect(mode).toBe("css-fallback");
    expect(root.props.get("--ui-font-scale")).toBe("1.4");
  });

  it("clamps before the value reaches either mechanism", async () => {
    const setZoom = vi.fn(async () => {});
    await applyUiScale(12, { setZoom, root: fakeRoot() });
    expect(setZoom).toHaveBeenCalledWith(MAX_UI_SCALE);
  });
});

describe("the stylesheet contract the fallback depends on", () => {
  it("reads the scale from :root, not from body", async () => {
    const css = await import("node:fs").then((fs) =>
      fs.readFileSync(new URL("../../app.css", import.meta.url), "utf8"),
    );
    // The regression: `body { font-size: calc(12px * var(--ui-font-scale)) }`
    // read the variable on an ANCESTOR of the element that declared it.
    const bodyBlock = css.slice(css.indexOf("  body {"));
    expect(bodyBlock.slice(0, bodyBlock.indexOf("}"))).not.toContain(
      "--ui-font-scale",
    );
    expect(css).toMatch(/html\s*\{[^}]*font-size:\s*calc\([^)]*--ui-font-scale/);
  });
});
