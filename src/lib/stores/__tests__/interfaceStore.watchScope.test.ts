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

describe("interfaceStore watchScope", () => {
  const restore = Object.getOwnPropertyDescriptor(globalThis, "window");
  afterEach(() => {
    if (restore) Object.defineProperty(globalThis, "window", restore);
    else Reflect.deleteProperty(globalThis, "window");
    vi.resetModules();
  });

  it("defaults to narrow", async () => {
    expect(get((await load()).interfaceStore).watchScope).toBe("narrow");
    expect(get((await load({})).interfaceStore).watchScope).toBe("narrow");
  });

  it("reads an unknown stored value as narrow and a valid one as itself", async () => {
    for (const bad of ["wide", "", 1, null, true, ["whole"]]) {
      expect(get((await load({ watchScope: bad })).interfaceStore).watchScope).toBe("narrow");
    }
    expect(get((await load({ watchScope: "whole" })).interfaceStore).watchScope).toBe("whole");
  });

  it("persists a change and restores the default on reset", async () => {
    const { interfaceStore, storage } = await load();
    interfaceStore.setWatchScope("whole");
    expect(get(interfaceStore).watchScope).toBe("whole");
    interfaceStore.flush();
    expect(JSON.parse(storage.getItem(KEY) ?? "{}").watchScope).toBe("whole");
    interfaceStore.reset();
    expect(get(interfaceStore).watchScope).toBe("narrow");
  });
});
