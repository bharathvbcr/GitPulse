import { afterEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { memoryStorage } from "../../repos/persist";

const KEY = "gitpulse_interface_prefs";

async function load(stored?: Record<string, unknown>) {
  const storage = memoryStorage(stored ? { [KEY]: JSON.stringify(stored) } : {});
  Object.defineProperty(globalThis, "window", {
    value: { localStorage: storage },
    configurable: true,
    writable: true,
  });
  vi.resetModules();
  const { interfaceStore } = await import("../interfaceStore");
  return { interfaceStore, storage };
}

describe("interfaceStore preferredEditor", () => {
  const restore = Object.getOwnPropertyDescriptor(globalThis, "window");
  afterEach(() => {
    if (restore) Object.defineProperty(globalThis, "window", restore);
    else Reflect.deleteProperty(globalThis, "window");
    vi.resetModules();
  });

  it("defaults to auto", async () => {
    expect(get((await load()).interfaceStore).preferredEditor).toBe("auto");
    expect(get((await load({})).interfaceStore).preferredEditor).toBe("auto");
  });

  it("reads anything but the four names as auto, so a stored value can never name a program", async () => {
    for (const bad of ["code", "vim", "/usr/bin/vim", "", 1, null, true, ["zed"], "VSCODE"]) {
      expect(get((await load({ preferredEditor: bad })).interfaceStore).preferredEditor).toBe("auto");
    }
    for (const good of ["auto", "vscode", "cursor", "zed"]) {
      expect(get((await load({ preferredEditor: good })).interfaceStore).preferredEditor).toBe(good);
    }
  });

  it("persists a change and restores the default on reset", async () => {
    const { interfaceStore, storage } = await load();
    interfaceStore.setPreferredEditor("zed");
    expect(get(interfaceStore).preferredEditor).toBe("zed");
    interfaceStore.flush();
    expect(JSON.parse(storage.getItem(KEY) ?? "{}").preferredEditor).toBe("zed");
    interfaceStore.reset();
    expect(get(interfaceStore).preferredEditor).toBe("auto");
  });
});
