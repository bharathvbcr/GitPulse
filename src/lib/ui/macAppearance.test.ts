import { afterEach, describe, expect, it, vi } from "vitest";
import { render } from "svelte/server";
import { isMacOS } from "../platform";
import { cardScale, cardScaleOut, liquidSelection } from "./transitions";
import ViewTabBar from "../components/ViewTabBar.svelte";

afterEach(() => vi.unstubAllGlobals());

describe("Mac appearance boundary", () => {
  it.each([
    ["MacIntel", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", 0, true],
    ["", "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0)", 0, true],
    ["Win32", "Mozilla/5.0 (Windows NT 10.0)", 0, false],
    ["Linux x86_64", "Mozilla/5.0 (X11; Linux x86_64)", 0, false],
    ["iPhone", "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)", 5, false],
    ["iPad", "Mozilla/5.0 (iPad; CPU OS 18_0 like Mac OS X)", 5, false],
    ["MacIntel", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", 5, false],
    ["", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)", 5, false],
    ["", "", 0, false],
  ])("classifies %s / %s (%s touch points)", (platform, userAgent, maxTouchPoints, expected) => {
    vi.stubGlobal("navigator", { platform, userAgent, maxTouchPoints });
    expect(isMacOS()).toBe(expected);
  });

  it("is safe without a navigator", () => {
    vi.stubGlobal("navigator", undefined);
    expect(isMacOS()).toBe(false);
  });

  it("uses a softer Mac entrance and preserves a fast exit", () => {
    vi.stubGlobal("navigator", { platform: "MacIntel", maxTouchPoints: 0 });
    const entrance = cardScale(null);
    expect(entrance.duration).toBe(260);
    expect(entrance.start).toBe(0.985);
    expect(cardScaleOut(null).duration).toBe(60);
    expect(cardScaleOut(null).start).toBe(0.985);
  });

  it("resolves reduced motion again for each Mac transition", () => {
    vi.stubGlobal("navigator", { platform: "MacIntel", maxTouchPoints: 0 });
    let reduced = false;
    vi.stubGlobal("window", { matchMedia: () => ({ matches: reduced }) });
    expect(cardScale().duration).toBe(260);
    reduced = true;
    expect(cardScale().duration).toBe(0);
    expect(cardScaleOut().duration).toBe(0);
    reduced = false;
    expect(cardScale().duration).toBe(260);
  });

  it("renders one decorative liquid selection on Mac without duplicating tab semantics", () => {
    vi.stubGlobal("navigator", { platform: "MacIntel", maxTouchPoints: 0 });
    const body = render(ViewTabBar).body;
    expect(body.match(/class="gp-liquid-selection gp-gpu"/g)).toHaveLength(1);
    expect(body.match(/aria-selected="true"/g)).toHaveLength(1);
    expect(body).toContain('aria-hidden="true"');
  });

  it("resolves liquid motion preferences when the transition starts, after the pill mounted", () => {
    vi.stubGlobal("navigator", { platform: "MacIntel", maxTouchPoints: 0 });
    let reduced = false;
    vi.stubGlobal("window", { matchMedia: () => ({ matches: reduced }) });
    const { duration } = liquidSelection();
    expect(typeof duration).toBe("function");
    if (typeof duration !== "function") throw new Error("Liquid motion must resolve live preferences");
    expect(duration(80)).toBe(280);
    reduced = true;
    expect(duration(80)).toBe(0);
    reduced = false;
    expect(duration(80)).toBe(280);
  });

  it("keeps the standard selection on other platforms", () => {
    vi.stubGlobal("navigator", { platform: "Win32" });
    const body = render(ViewTabBar).body;
    expect(body).not.toContain("gp-liquid-selection");
    expect(body.match(/aria-selected="true"/g)).toHaveLength(1);
  });
});
