import { describe, expect, it, vi } from "vitest";
import { WorkbenchError, type Repository, type Workspace } from "./client";
import { importSummary, importTabGroups, tabGroups, type ImportIO } from "./workspaceImport";
import { WORKSPACE_SPACING } from "./workspaceOrder";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const repoFor = (path: string): Repository => {
  // Two worktrees of one repository register to one record, as the host does.
  const name = path.replace(/-wt$/, "").split("/").pop() ?? path;
  return { id: `repo-${name}`, revision: 1, updated_at: 1, name, identity_key: `local:/code/${name}/.git`, remote_url: null };
};
function io(overrides: Partial<ImportIO> = {}): ImportIO & { puts: Record<string, unknown>[] } {
  const puts: Record<string, unknown>[] = [];
  return {
    puts,
    register: async (path) => repoFor(path),
    put: async (input) => { puts.push(input); return { ...(input as unknown as Workspace), revision: 1, updated_at: 1 }; },
    ...overrides,
  };
}

describe("tabGroups", () => {
  it("collects named groups in strip order, ignores ungrouped tabs, and carries the group color", () => {
    const plans = tabGroups(
      [
        { path: "/code/api", group: "Backend" },
        { path: "/code/notes", group: null },
        { path: "/code/web", group: "Frontend" },
        { path: "/code/db", group: "  backend " },
        { path: "/code/api", group: "Backend" },
        { path: "/code/scratch", group: "   " },
      ],
      [{ group: "frontend", color: "blue" }],
    );
    expect(plans).toEqual([
      { name: "Backend", color: "", paths: ["/code/api", "/code/db"] },
      { name: "Frontend", color: "blue", paths: ["/code/web"] },
    ]);
  });
});

describe("importTabGroups", () => {
  it("creates one workspace per new group, after the existing ones, with each repository once", async () => {
    const port = io();
    const report = await importTabGroups(
      [{ name: "Backend", color: "", paths: ["/code/api", "/code/api-wt", "/code/db"] }, { name: "Frontend", color: "blue", paths: ["/code/web"] }],
      [{ name: "Research", position: 5000 }],
      port,
    );
    expect(report).toEqual({ created: [{ name: "Backend", repositories: 2 }, { name: "Frontend", repositories: 1 }], skipped: [], failed: [] });
    expect(port.puts.map((put) => [put.name, put.position, put.repository_ids, put.color, put.expected_revision])).toEqual([
      ["Backend", 5000 + WORKSPACE_SPACING, ["repo-api", "repo-db"], "", 0],
      ["Frontend", 5000 + 2 * WORKSPACE_SPACING, ["repo-web"], "blue", 0],
    ]);
    expect(importSummary(report)).toBe("Created 2 workspaces.");
  });

  it("skips a group whose name a workspace already has, whatever its case, and never writes to it", async () => {
    const port = io();
    const report = await importTabGroups([{ name: "backend", color: "", paths: ["/code/api"] }], [{ name: " Backend", position: 1 }], port);
    expect(report.skipped).toEqual([{ name: "backend", reason: "A workspace with this name already exists." }]);
    expect(port.puts).toEqual([]);
    expect(importSummary(report)).toBe("Skipped 1 group that already has a workspace.");
  });

  it("is idempotent across runs: a second import of the same groups creates nothing", async () => {
    const port = io();
    const plans = [{ name: "Backend", color: "", paths: ["/code/api"] }];
    await importTabGroups(plans, [], port);
    const created = port.puts.map((put) => ({ name: String(put.name), position: Number(put.position) }));
    const again = await importTabGroups(plans, created, port);
    expect(again.created).toEqual([]);
    expect(again.skipped).toHaveLength(1);
    expect(port.puts).toHaveLength(1);
  });

  it("reports a repository that could not be registered, and still creates the group from the rest", async () => {
    const port = io({ register: async (path) => { if (path === "/gone") throw new WorkbenchError("repository_unavailable", "not a git repository"); return repoFor(path); } });
    const report = await importTabGroups([{ name: "Mixed", color: "", paths: ["/gone", "/code/api"] }], [], port);
    expect(report.created).toEqual([{ name: "Mixed", repositories: 1 }]);
    expect(report.failed[0]?.error).toContain("/gone");
    expect(importSummary(report)).toBe("Created 1 workspace, 1 could not be imported.");
  });

  it("creates nothing for a group none of whose repositories could be registered", async () => {
    const port = io({ register: async () => { throw new WorkbenchError("repository_unavailable", "missing"); } });
    const report = await importTabGroups([{ name: "Gone", color: "", paths: ["/a"] }], [], port);
    expect(report.created).toEqual([]);
    expect(report.failed).toHaveLength(1);
    expect(port.puts).toEqual([]);
  });

  it("reports a refused workspace write and carries on with the next group", async () => {
    let calls = 0;
    const port = io({ put: async (input) => { calls++; if (calls === 1) throw new WorkbenchError("invalid_input", "name too long"); return { ...(input as unknown as Workspace), revision: 1, updated_at: 1 }; } });
    const report = await importTabGroups([{ name: "A", color: "", paths: ["/code/a"] }, { name: "B", color: "", paths: ["/code/b"] }], [], port);
    expect(report.failed).toEqual([{ name: "A", error: expect.stringContaining("name too long") }]);
    expect(report.created).toEqual([{ name: "B", repositories: 1 }]);
  });
});
