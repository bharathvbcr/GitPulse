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

describe("interfaceStore externalEditor", () => {
  const restore = Object.getOwnPropertyDescriptor(globalThis, "window");
  afterEach(() => {
    if (restore) Object.defineProperty(globalThis, "window", restore);
    else Reflect.deleteProperty(globalThis, "window");
    vi.resetModules();
  });

  it("defaults to auto", async () => {
    expect(get((await load()).interfaceStore).externalEditor).toBe("auto");
    expect(get((await load({})).interfaceStore).externalEditor).toBe("auto");
  });

  /** The value is sent to the backend as an editor name, so nothing but the four names may survive a read. */
  it("reads anything but the four editor names as auto", async () => {
    for (const bad of ["vim", "/bin/sh", "Code", "code --wait", "", 1, null, true, ["zed"], "toString", "__proto__"]) {
      expect(get((await load({ externalEditor: bad })).interfaceStore).externalEditor).toBe("auto");
    }
    for (const good of ["auto", "code", "cursor", "zed"]) {
      expect(get((await load({ externalEditor: good })).interfaceStore).externalEditor).toBe(good);
    }
  });

  it("persists a change, refuses an unknown name, and restores the default on reset", async () => {
    const { interfaceStore, storage } = await load();
    interfaceStore.setExternalEditor("cursor");
    expect(get(interfaceStore).externalEditor).toBe("cursor");
    interfaceStore.flush();
    expect(JSON.parse(storage.getItem(KEY) ?? "{}").externalEditor).toBe("cursor");
    interfaceStore.setExternalEditor("emacs" as never);
    expect(get(interfaceStore).externalEditor).toBe("auto");
    interfaceStore.setExternalEditor("zed");
    interfaceStore.reset();
    expect(get(interfaceStore).externalEditor).toBe("auto");
  });
});
