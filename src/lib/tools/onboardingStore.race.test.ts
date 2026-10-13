import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

const invokeMock = vi.fn();
const listenMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invokeMock(...args) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: (...args: unknown[]) => listenMock(...args) }));

const ladderFor = (tool: string, selected: string) => ({ tool, selected, rungs: [] });

type Gate = { resolve: (value: unknown) => void };

async function freshStore() {
  vi.resetModules();
  return import("./onboardingStore");
}

beforeEach(() => {
  invokeMock.mockReset();
  listenMock.mockReset();
  listenMock.mockImplementation(async () => () => {});
});

describe("setup wizard ladder replies are ticketed", () => {
  it("drops a slow reply for the tool the user switched away from", async () => {
    const gates = new Map<string, Gate>();
    invokeMock.mockImplementation((cmd: string, args?: { tool?: string }) => {
      if (cmd === "cmd_tool_ladder") {
        return new Promise((resolve) => gates.set(args?.tool ?? "", { resolve }));
      }
      return Promise.resolve({});
    });
    const store = await freshStore();
    store.openSetupWizard("devmap");
    store.setWizardTool("manvi");
    // The switch clears what the previous tool's ladder left behind.
    expect(get(store.onboardingStore.ladder)).toBeNull();

    gates.get("manvi")!.resolve(ladderFor("manvi", "toolchain_remote"));
    await vi.waitFor(() => expect(get(store.onboardingStore.ladder)?.selected).toBe("toolchain_remote"));
    gates.get("devmap")!.resolve(ladderFor("devmap", "local_checkout"));
    await new Promise((r) => setTimeout(r, 10));

    expect(get(store.onboardingStore.ladder)?.tool).toBe("manvi");
    expect(get(store.onboardingStore.ladder)?.selected).toBe("toolchain_remote");
    // The late devmap reply must not start a devmap preflight either.
    expect(invokeMock.mock.calls.filter(([cmd, a]) => cmd === "cmd_tool_preflight" && a?.tool === "devmap")).toHaveLength(0);
  });

  it("clears the ladder and preflight at the moment of the switch", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: { tool?: string }) =>
      cmd === "cmd_tool_ladder" ? ladderFor(args?.tool ?? "", "path") : { tool: args?.tool, requirements: [], estimate: "s" },
    );
    const store = await freshStore();
    store.openSetupWizard("devmap");
    await vi.waitFor(() => expect(get(store.onboardingStore.preflight)).not.toBeNull());
    invokeMock.mockImplementation(() => new Promise(() => {}));
    store.setWizardTool("manvi");
    expect(get(store.onboardingStore.ladder)).toBeNull();
    expect(get(store.onboardingStore.preflight)).toBeNull();
  });
});

describe("tool-install-progress listener", () => {
  it("registers exactly one listener for quick open + install", async () => {
    let release!: (fn: () => void) => void;
    listenMock.mockImplementation(() => new Promise((resolve) => { release = resolve; }));
    invokeMock.mockResolvedValue({});
    const store = await freshStore();
    store.openSetupWizard("devmap");
    const install = store.runInstall("devmap");
    store.openSetupWizard("devmap");
    await new Promise((r) => setTimeout(r, 5));
    release(() => {});
    await install;
    expect(listenMock).toHaveBeenCalledTimes(1);
  });

  it("retries registration after a failed listen instead of caching the failure", async () => {
    listenMock.mockRejectedValueOnce(new Error("no bridge"));
    invokeMock.mockResolvedValue({});
    const store = await freshStore();
    await store.runInstall("devmap");
    await store.runInstall("devmap");
    expect(listenMock).toHaveBeenCalledTimes(2);
  });
});
