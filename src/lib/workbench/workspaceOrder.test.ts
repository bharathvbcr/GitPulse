import { describe, expect, it, vi } from "vitest";
import { WorkbenchError, type Workspace } from "./client";
import { applyMove, moveWrites, navigatorOrder, WORKSPACE_SPACING, type MoveIO } from "./workspaceOrder";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const group = (id: string, position: number, pinned = false, revision = 1) => ({ id, position, pinned, revision });
const full = (id: string, position: number, revision = 1): Workspace => ({ id, revision, updated_at: 1, name: id, description: "Keep me", icon: "", color: "", position, pinned: false, archived: false, repository_ids: ["r1"] });

describe("navigatorOrder", () => {
  it("draws pinned workspaces first, then by position, with id breaking ties", () => {
    expect(navigatorOrder([group("c", 1), group("a", 5, true), group("b", 1), group("d", 0)]).map((g) => g.id)).toEqual(["a", "d", "b", "c"]);
  });
});

describe("moveWrites", () => {
  it("moves with a single write when there is room between the new neighbours", () => {
    const groups = [group("a", 100), group("b", 200), group("c", 300)];
    expect(moveWrites(groups, "c", -1)).toEqual([{ id: "c", revision: 1, position: 150 }]);
    expect(moveWrites(groups, "a", 1)).toEqual([{ id: "a", revision: 1, position: 250 }]);
    expect(moveWrites(groups, "b", -1)).toEqual([{ id: "b", revision: 1, position: 50 }]);
    expect(moveWrites(groups, "b", 1)).toEqual([{ id: "b", revision: 1, position: 301 }]);
  });

  it("refuses to move past either end, or across the pinned boundary", () => {
    const groups = [group("p", 900, true), group("a", 100), group("b", 200)];
    expect(moveWrites(groups, "a", -1)).toBeNull();
    expect(moveWrites(groups, "b", 1)).toBeNull();
    expect(moveWrites(groups, "p", 1)).toBeNull();
    expect(moveWrites(groups, "missing", 1)).toBeNull();
  });

  it("re-spaces the group when neighbours leave no integer between them", () => {
    const groups = [group("a", 1), group("b", 2), group("c", 3)];
    const writes = moveWrites(groups, "c", -1);
    expect(writes).toEqual([
      { id: "a", revision: 1, position: WORKSPACE_SPACING },
      { id: "c", revision: 1, position: 2 * WORKSPACE_SPACING },
      { id: "b", revision: 1, position: 3 * WORKSPACE_SPACING },
    ]);
    // And the order those writes produce is the order that was asked for.
    const after = groups.map((g) => ({ ...g, position: writes?.find((w) => w.id === g.id)?.position ?? g.position }));
    expect(navigatorOrder(after).map((g) => g.id)).toEqual(["a", "c", "b"]);
  });

  it("produces the requested order for every single-step move", () => {
    const groups = [group("a", 10), group("b", 11), group("c", 40), group("d", 41), group("e", 90)];
    const ids = groups.map((g) => g.id);
    for (const id of ids) {
      for (const delta of [-1, 1] as const) {
        const writes = moveWrites(groups, id, delta);
        const from = ids.indexOf(id), to = from + delta;
        if (to < 0 || to >= ids.length) { expect(writes).toBeNull(); continue; }
        const expected = ids.filter((x) => x !== id);
        expected.splice(to, 0, id);
        const after = groups.map((g) => ({ ...g, position: writes?.find((w) => w.id === g.id)?.position ?? g.position }));
        expect(navigatorOrder(after).map((g) => g.id), `${id} ${delta}`).toEqual(expected);
      }
    }
  });
});

describe("applyMove", () => {
  it("writes only the position, against the revision the navigator showed", async () => {
    const io: MoveIO = { get: vi.fn(async (id) => full(id, 100)), put: vi.fn(async (input) => ({ ...full(String(input.id), Number(input.position)), revision: 2 })) };
    expect(await applyMove([{ id: "a", revision: 1, position: 50 }], io)).toEqual({ written: ["a"], failed: [] });
    expect(io.put).toHaveBeenCalledWith(expect.objectContaining({ id: "a", expected_revision: 1, position: 50, description: "Keep me", repository_ids: ["r1"] }));
  });

  it("refuses a workspace edited since it was drawn, and reports it rather than calling the move done", async () => {
    const io: MoveIO = {
      get: vi.fn(async (id) => full(id, 100, id === "b" ? 2 : 1)),
      put: vi.fn(async (input) => full(String(input.id), Number(input.position), 2)),
    };
    const result = await applyMove([{ id: "a", revision: 1, position: 1 }, { id: "b", revision: 1, position: 2 }], io);
    expect(result.written).toEqual(["a"]);
    expect(result.failed).toHaveLength(1);
    expect(result.failed[0]?.id).toBe("b");
    expect(io.put).toHaveBeenCalledTimes(1);
  });

  it("does not accept a reply that names another position", async () => {
    const io: MoveIO = { get: vi.fn(async (id) => full(id, 100)), put: vi.fn(async (input) => full(String(input.id), 7, 2)) };
    const result = await applyMove([{ id: "a", revision: 1, position: 50 }], io);
    expect(result.written).toEqual([]);
    expect(result.failed[0]?.error).toContain("does not match");
  });

  it("keeps going past a failed write so the report covers every workspace", async () => {
    const io: MoveIO = { get: vi.fn(async (id) => { if (id === "a") throw new WorkbenchError("store_error", "busy"); return full(id, 1); }), put: vi.fn(async (input) => full(String(input.id), Number(input.position), 2)) };
    const result = await applyMove([{ id: "a", revision: 1, position: 5 }, { id: "b", revision: 1, position: 6 }], io);
    expect(result).toMatchObject({ written: ["b"], failed: [{ id: "a" }] });
  });
});
