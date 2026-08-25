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
