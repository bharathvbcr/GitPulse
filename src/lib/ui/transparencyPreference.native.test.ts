import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/**
 * The native wiring of installTransparencyPreference: the default source the
 * app uses inside Tauri (`cmd_get_accessibility_display`, the
 * `gitpulse-accessibility-display` event, and a re-read on window focus).
 * transparencyPreference.test.ts injects a fake source; this drives the real
 * one against mocked Tauri entry points, so the command and event names, the
 * focus re-read and the failure reporting are pinned where they are used.
 */
const listen = vi.fn();
const invoke = vi.fn();
let tauri = true;

vi.mock("@tauri-apps/api/event", () => ({ listen: (...args: unknown[]) => listen(...args) }));
vi.mock("../ipc/invoke", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
vi.mock("../platform", () => ({ isTauri: () => tauri }));

import {
  ACCESSIBILITY_COMMAND,
  ACCESSIBILITY_EVENT,
  TRANSPARENCY_ATTRIBUTE,
  installTransparencyPreference,
  type AccessibilityDisplay,
} from "./transparencyPreference";

class FakeRoot {
  readonly attributes = new Map<string, string>();
  setAttribute(name: string, value: string) {
    this.attributes.set(name, value);
  }
  removeAttribute(name: string) {
    this.attributes.delete(name);
  }
}

const display = (
  reduceTransparency: AccessibilityDisplay["reduceTransparency"],
  seq: number,
  reason: string | null = null,
): AccessibilityDisplay => ({ reduceTransparency, reason, seq });

let root: FakeRoot;
let focusListeners: Array<() => void>;
let pushEvent: (payload: AccessibilityDisplay) => void;
const unlisten = vi.fn();

beforeEach(() => {
  tauri = true;
  root = new FakeRoot();
  focusListeners = [];
  pushEvent = () => {};
  listen.mockReset();
  invoke.mockReset();
  unlisten.mockReset();
  listen.mockImplementation(async (_event: string, handler: (e: { payload: AccessibilityDisplay }) => void) => {
    pushEvent = (payload) => handler({ payload });
    return unlisten;
  });
  vi.stubGlobal("document", { documentElement: root });
  vi.stubGlobal("window", {
    addEventListener: (name: string, fn: () => void) => {
      if (name === "focus") focusListeners.push(fn);
    },
    removeEventListener: (name: string, fn: () => void) => {
      if (name === "focus") focusListeners = focusListeners.filter((other) => other !== fn);
    },
  });
  vi.spyOn(console, "warn").mockImplementation(() => {});
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("installTransparencyPreference against the Tauri bridge", () => {
  it("asks for the current setting through the documented command and stamps it", async () => {
    invoke.mockResolvedValue(display("reduced", 1));
    await installTransparencyPreference();
    expect(invoke).toHaveBeenCalledWith(ACCESSIBILITY_COMMAND);
    expect(root.attributes.get(TRANSPARENCY_ATTRIBUTE)).toBe("reduced");
  });

  it("listens on the documented event and follows what it says", async () => {
    invoke.mockResolvedValue(display("standard", 1));
    await installTransparencyPreference();
    expect(listen).toHaveBeenCalledWith(ACCESSIBILITY_EVENT, expect.any(Function));
    expect(root.attributes.has(TRANSPARENCY_ATTRIBUTE)).toBe(false);
    pushEvent(display("reduced", 2));
    expect(root.attributes.get(TRANSPARENCY_ATTRIBUTE)).toBe("reduced");
    pushEvent(display("standard", 3));
    expect(root.attributes.has(TRANSPARENCY_ATTRIBUTE)).toBe(false);
  });

  it("re-reads the setting when the window regains focus, and lets go of the listener on stop", async () => {
    invoke.mockResolvedValueOnce(display("standard", 1));
    const stop = await installTransparencyPreference();
    expect(focusListeners).toHaveLength(1);
    invoke.mockResolvedValueOnce(display("reduced", 2));
    focusListeners[0]();
    await vi.waitFor(() => expect(root.attributes.get(TRANSPARENCY_ATTRIBUTE)).toBe("reduced"));
    expect(invoke).toHaveBeenCalledTimes(2);

    stop();
    expect(focusListeners).toHaveLength(0);
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it("stamps unknown, and says why, when the native read fails", async () => {
    invoke.mockRejectedValue(new Error("command not found"));
    await installTransparencyPreference();
    expect(root.attributes.get(TRANSPARENCY_ATTRIBUTE)).toBe("unknown");
    expect(console.warn).toHaveBeenCalledWith(expect.stringContaining("command not found"));
  });

  it("surfaces a reason the native side gives for an answer it could not fully stand behind", async () => {
    invoke.mockResolvedValue(display("reduced", 1, "changes will not be observed"));
    await installTransparencyPreference();
    expect(root.attributes.get(TRANSPARENCY_ATTRIBUTE)).toBe("reduced");
    expect(console.warn).toHaveBeenCalledWith("Reduce Transparency: changes will not be observed");
  });

  it("does nothing outside Tauri", async () => {
    tauri = false;
    const stop = await installTransparencyPreference();
    expect(invoke).not.toHaveBeenCalled();
    expect(listen).not.toHaveBeenCalled();
    expect(root.attributes.size).toBe(0);
    expect(stop()).toBeUndefined();
  });
});
