import { afterEach, describe, expect, it, vi } from "vitest";
import { confirmAndUninstall } from "./confirmUninstall";

const deps = (answer: boolean) => ({
  askConfirm: vi.fn(async () => answer),
  uninstallExternalTool: vi.fn(async () => "removed"),
});

const original = Object.getOwnPropertyDescriptor(globalThis, "window");
afterEach(() => {
  if (original) Object.defineProperty(globalThis, "window", original);
  else Reflect.deleteProperty(globalThis, "window");
});

describe("confirmAndUninstall", () => {
  it("removes the tool once the in-app dialog is confirmed, naming it and marking it destructive", async () => {
    const d = deps(true);
    await expect(confirmAndUninstall("devmap", d)).resolves.toBe(true);
    expect(d.uninstallExternalTool).toHaveBeenCalledExactlyOnceWith("devmap");
    expect(d.askConfirm).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Remove devmap?", confirmLabel: "Remove", destructive: true }),
    );
  });

  it("does nothing when the dialog is cancelled", async () => {
    const d = deps(false);
    await expect(confirmAndUninstall("manvi", d)).resolves.toBe(false);
    expect(d.uninstallExternalTool).not.toHaveBeenCalled();
  });

  it("still uninstalls where window.confirm answers null, as WKWebView does", async () => {
    const confirm = vi.fn(() => null);
    Object.defineProperty(globalThis, "window", { value: { confirm }, configurable: true, writable: true });
    const d = deps(true);
    await expect(confirmAndUninstall("devmap", d)).resolves.toBe(true);
    expect(d.uninstallExternalTool).toHaveBeenCalledTimes(1);
    expect(confirm).not.toHaveBeenCalled();
  });

  it("lets an uninstall failure reach the caller", async () => {
    const d = deps(true);
    d.uninstallExternalTool.mockRejectedValueOnce(new Error("busy"));
    await expect(confirmAndUninstall("devmap", d)).rejects.toThrow("busy");
  });
});
