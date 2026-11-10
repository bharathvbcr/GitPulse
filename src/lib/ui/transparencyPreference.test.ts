import { describe, expect, it, vi } from "vitest";
import {
  ACCESSIBILITY_COMMAND,
  TRANSPARENCY_ATTRIBUTE,
  REDUCED,
  UNKNOWN,
  applyTransparency,
  installTransparencyPreference,
  type AccessibilityDisplay,
  type TransparencySource,
} from "./transparencyPreference";

class FakeRoot {
  readonly attributes = new Map<string, string>();
  setAttribute(name: string, value: string) {
    this.attributes.set(name, value);
  }
  removeAttribute(name: string) {
    this.attributes.delete(name);
  }
  get stamp() {
    return this.attributes.get(TRANSPARENCY_ATTRIBUTE);
  }
}

const display = (
  reduceTransparency: AccessibilityDisplay["reduceTransparency"],
  seq: number,
  reason: string | null = null,
): AccessibilityDisplay => ({ reduceTransparency, reason, seq });

function harness(initial: Promise<AccessibilityDisplay> | AccessibilityDisplay) {
  const root = new FakeRoot();
  const report = vi.fn();
  let push: (d: AccessibilityDisplay) => void = () => {};
  let recheck: () => void = () => {};
  let next: Promise<AccessibilityDisplay> | AccessibilityDisplay = initial;
  const unsubscribe = vi.fn();
  const source: TransparencySource = {
    read: async () => next,
    subscribe: async (receive) => {
      push = receive;
      return unsubscribe;
    },
    onRecheck: (fn) => {
      recheck = fn;
      return () => {};
    },
    target: root,
    report,
  };
  return {
    root,
    report,
    unsubscribe,
    source,
    push: (d: AccessibilityDisplay) => push(d),
    recheck: () => recheck(),
    answer: (d: Promise<AccessibilityDisplay> | AccessibilityDisplay) => {
      next = d;
    },
  };
}

describe("applyTransparency", () => {
  it("stamps reduced and removes the attribute for standard", () => {
    const root = new FakeRoot();
    applyTransparency("reduced", root);
    expect(root.stamp).toBe(REDUCED);
    applyTransparency("standard", root);
    expect(root.attributes.has(TRANSPARENCY_ATTRIBUTE)).toBe(false);
  });

  it("stamps unknown as itself, never as reduced and never as absent", () => {
    const root = new FakeRoot();
    applyTransparency("unknown", root);
    expect(root.stamp).toBe(UNKNOWN);
    expect(root.stamp).not.toBe(REDUCED);
  });

  it("uses data-transparency, the attribute the CSS selectors name", () => {
    expect(TRANSPARENCY_ATTRIBUTE).toBe("data-transparency");
    expect(ACCESSIBILITY_COMMAND).toBe("cmd_get_accessibility_display");
  });

  it("does nothing without a document to stamp", () => {
    expect(() => applyTransparency("reduced", null)).not.toThrow();
  });
});

describe("installTransparencyPreference", () => {
  it("applies the initial value, then every change after it", async () => {
    const h = harness(display("reduced", 1));
    await installTransparencyPreference(h.source);
    expect(h.root.stamp).toBe(REDUCED);
    h.push(display("standard", 2));
    expect(h.root.attributes.has(TRANSPARENCY_ATTRIBUTE)).toBe(false);
    h.push(display("reduced", 3));
    expect(h.root.stamp).toBe(REDUCED);
  });

  it("does not let a late reply overwrite a newer event", async () => {
    const h = harness(display("standard", 5));
    await installTransparencyPreference(h.source);
    h.push(display("reduced", 9));
    h.answer(display("standard", 7));
    h.recheck();
    await Promise.resolve();
    await Promise.resolve();
    expect(h.root.stamp).toBe(REDUCED);
  });

  it("subscribes before it reads, so a change in between is not lost", async () => {
    const order: string[] = [];
    const root = new FakeRoot();
    await installTransparencyPreference({
      target: root,
      subscribe: async () => {
        order.push("subscribe");
        return () => {};
      },
      read: async () => {
        order.push("read");
        return display("standard", 1);
      },
    });
    expect(order).toEqual(["subscribe", "read"]);
  });

  it("reports unknown, with the reason, when the native read could not run", async () => {
    const h = harness(display("unknown", 1, "NSWorkspace is not loaded"));
    await installTransparencyPreference(h.source);
    expect(h.root.stamp).toBe(UNKNOWN);
    expect(h.report).toHaveBeenCalledWith(expect.stringContaining("NSWorkspace is not loaded"));
  });

  it("treats a rejected read as unknown rather than as not reduced", async () => {
    const root = new FakeRoot();
    root.setAttribute(TRANSPARENCY_ATTRIBUTE, REDUCED);
    const report = vi.fn();
    await installTransparencyPreference({
      target: root,
      report,
      subscribe: async () => () => {},
      read: async () => {
        throw new Error("ipc down");
      },
    });
    expect(root.stamp).toBe(UNKNOWN);
    expect(report).toHaveBeenCalledWith(expect.stringContaining("ipc down"));
  });

  it("treats a malformed payload as unknown", async () => {
    const h = harness({ nope: true } as unknown as AccessibilityDisplay);
    await installTransparencyPreference(h.source);
    expect(h.root.stamp).toBe(UNKNOWN);
  });

  it("keeps a good value but says so when later changes are not observed", async () => {
    const h = harness(display("reduced", 1, "changes to Reduce Transparency are not observed: x"));
    await installTransparencyPreference(h.source);
    expect(h.root.stamp).toBe(REDUCED);
    expect(h.report).toHaveBeenCalledWith(expect.stringContaining("not observed"));
  });

  it("stops listening and ignores late answers once stopped", async () => {
    const h = harness(display("standard", 1));
    const stop = await installTransparencyPreference(h.source);
    stop();
    expect(h.unsubscribe).toHaveBeenCalledOnce();
    h.push(display("reduced", 2));
    expect(h.root.attributes.has(TRANSPARENCY_ATTRIBUTE)).toBe(false);
  });

  it("is inert with no native side", async () => {
    const stop = await installTransparencyPreference(null);
    expect(stop).toBeTypeOf("function");
    expect(() => stop()).not.toThrow();
  });
});
