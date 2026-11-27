import { describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({
  invoke: vi.fn().mockResolvedValue({ total_refs: 0, namespaces: [] }),
}));

vi.mock("../ipc/invoke", () => ({ invoke }));

import { nonStandardRefInventory } from "./model";

describe("nonStandardRefInventory IPC wrapper", () => {
  it("invokes cmd_non_standard_ref_inventory with repoPath", async () => {
    invoke.mockClear();
    await nonStandardRefInventory("/tmp/repo");
    expect(invoke.mock.calls).toEqual([
      ["cmd_non_standard_ref_inventory", { repoPath: "/tmp/repo" }],
    ]);
  });
});
