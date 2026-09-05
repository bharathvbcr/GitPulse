import { readFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";
import { guardedDismiss } from "./modalGuard";

describe("guardedDismiss", () => {
  it("closes when the modal is idle", () => {
    const onClose = vi.fn();
    guardedDismiss(false, onClose);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("is a no-op while the operation runs — dismiss cannot bypass a disabled Cancel", () => {
    const onClose = vi.fn();
    guardedDismiss(true, onClose);
    expect(onClose).not.toHaveBeenCalled();
  });

  it("tolerates a missing onClose either way", () => {
    expect(() => guardedDismiss(false)).not.toThrow();
    expect(() => guardedDismiss(true)).not.toThrow();
  });
});

describe("every modal with an in-flight operation uses this owner", () => {
  it("is the single implementation of the rule, not a third copy", () => {
    // The helper documented the rule and had no callers; both modals carried
    // their own inline copy of it. A rule with two implementations is a rule
    // that will drift.
    const read = (name: string) =>
      readFileSync(new URL(`./${name}.svelte`, import.meta.url), "utf8");
    for (const [name, busy] of [
      ["CloneModal", "isCloning"],
      ["RebaseModal", "isExecuting"],
    ] as const) {
      const source = read(name);
      expect(source).toContain("guardedDismiss");
      expect(source).toContain(`guardedDismiss(${busy}, onClose)`);
      // The inline shape this replaced.
      expect(source).not.toContain(`if (${busy}) return;\n    onClose?.();`);
    }
  });
});
