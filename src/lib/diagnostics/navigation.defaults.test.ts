import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { installNavigationDiagnostics } from "./navigation";

/** The probe's real browser wiring, with the browser's globals stood in for. */
describe("navigation diagnostics against the browser's own globals", () => {
  const frames: Array<() => void> = [];
  const observe = vi.fn();
  const disconnect = vi.fn();
  let notify: (() => void) | null = null;
  let visibility: "visible" | "hidden" = "visible";

  beforeEach(() => {
    vi.useFakeTimers();
    frames.length = 0;
    observe.mockReset();
    disconnect.mockReset();
    notify = null;
    visibility = "visible";
    vi.stubGlobal("requestAnimationFrame", (callback: () => void) => { frames.push(callback); return frames.length; });
    vi.stubGlobal("MutationObserver", class {
      constructor(onChange: () => void) { notify = onChange; }
      observe = observe;
      disconnect = disconnect;
    });
    vi.stubGlobal("document", { body: { id: "body" }, get visibilityState() { return visibility; } });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  it("watches the document body and times frames through requestAnimationFrame", async () => {
    const warn = vi.fn();
    const nav = installNavigationDiagnostics({ warn });
    nav.navigate("a", "repository/a");
    expect(observe).toHaveBeenCalledWith({ id: "body" }, { childList: true, subtree: true, characterData: true, attributes: true });
    // Two animation frames are asked for in a row: the second only once the first has run.
    expect(frames).toHaveLength(1);
    frames.splice(0).forEach((run) => run());
    expect(frames).toHaveLength(1);
    frames.splice(0).forEach((run) => run());
    notify?.();
    // Settles quietly: nothing slow, so nothing is reported, and the observer is released.
    await vi.advanceTimersByTimeAsync(1_000);
    expect(warn).not.toHaveBeenCalled();
    expect(disconnect).toHaveBeenCalledTimes(1);
  });

  it("does not time a navigation made while the document is hidden", () => {
    visibility = "hidden";
    const nav = installNavigationDiagnostics({ warn: vi.fn() });
    nav.navigate("a", "repository/a");
    expect(observe).not.toHaveBeenCalled();
    expect(frames).toHaveLength(0);
  });

  it("releases the observer when stopped mid-navigation, and ignores later navigations", () => {
    const nav = installNavigationDiagnostics({ warn: vi.fn() });
    nav.navigate("a", "repository/a");
    nav.stop();
    expect(disconnect).toHaveBeenCalledTimes(1);
    nav.navigate("b", "repository/b");
    expect(observe).toHaveBeenCalledTimes(1);
  });

  it("falls back to watching nothing where there is no MutationObserver", async () => {
    vi.stubGlobal("MutationObserver", undefined);
    const warn = vi.fn();
    const nav = installNavigationDiagnostics({ warn });
    nav.navigate("a", "repository/a");
    frames.splice(0).forEach((run) => run());
    frames.splice(0).forEach((run) => run());
    await vi.advanceTimersByTimeAsync(1_000);
    expect(warn).not.toHaveBeenCalled();
    nav.stop();
  });
});
