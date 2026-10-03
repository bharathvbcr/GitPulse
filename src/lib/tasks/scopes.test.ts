import { readFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";
import { loadTaskScopes, MAX_SCOPE_IDS } from "./scopes";

function scopesFor(ids: string[]) {
  return ids.map((id) => ({ id, title: `title of ${id}`, status: "in_progress", planned_files: [], agent_appended_files: [], forbidden_changes: [], allowed_commands: [] }));
}

describe("loadTaskScopes", () => {
  it("reads a set in one request, once per distinct id, and leaves unknown ids absent", async () => {
    const call = vi.fn(async (_cmd: string, args: { taskIds: string[] }) => scopesFor(args.taskIds.filter((id) => id !== "GONE")));
    const scopes = await loadTaskScopes(call as never, "/repo", ["A", "B", "A", "GONE"]);

    expect(call).toHaveBeenCalledTimes(1);
    expect(call.mock.calls[0][1]).toEqual({ repoPath: "/repo", taskIds: ["A", "B", "GONE"] });
    expect(Object.keys(scopes).sort()).toEqual(["A", "B"]);
  });

  it("reads a list longer than the limit in full, in chunks the backend accepts", async () => {
    const call = vi.fn(async (_cmd: string, args: { taskIds: string[] }) => {
      if (args.taskIds.length > MAX_SCOPE_IDS) throw new Error("over the limit");
      return scopesFor(args.taskIds);
    });
    const ids = Array.from({ length: MAX_SCOPE_IDS * 2 + 1 }, (_, i) => `T${i}`);
    const scopes = await loadTaskScopes(call as never, "/repo", ids);

    expect(call).toHaveBeenCalledTimes(3);
    expect(Object.keys(scopes)).toHaveLength(ids.length);
  });

  it("asks nothing for no ids", async () => {
    const call = vi.fn();
    expect(await loadTaskScopes(call as never, "/repo", [])).toEqual({});
    expect(call).not.toHaveBeenCalled();
  });

  it("matches the limit the Rust command enforces", () => {
    const rust = readFileSync(new URL("../../../src-tauri/src/tasks/mod.rs", import.meta.url), "utf8");
    expect(rust).toContain(`pub const MAX_SCOPE_IDS: usize = ${MAX_SCOPE_IDS};`);
  });
});
