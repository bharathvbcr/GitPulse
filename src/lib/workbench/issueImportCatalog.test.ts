// ft-038e6ba5: a failed or pending task-catalog read must not look like an
// empty catalog. The panel used to keep repoTasks = [] after a failed read and
// pass it to the batch import, which then re-created every existing task.
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";

const puts: Array<Record<string, unknown>> = [];
vi.mock("./client", async (orig) => {
  const actual = await orig<typeof import("./client")>();
  return {
    ...actual,
    registerRepository: vi.fn(async () => ({ id: "repo1", revision: 1, updated_at: 1, name: "r", identity_key: "k", remote_url: null })),
    putTask: vi.fn(async (input: Record<string, unknown>) => {
      puts.push(input);
      return { ...input, revision: 1 };
    }),
  };
});

const issues = [{ number: 1, title: "Existing issue", url: "https://example.invalid/1", labels: [], state: "OPEN", author: "a", created_at: "", updated_at: "", body: "" }];

describe("ft-038e6ba5 batch issue import with an unread task catalog", () => {
  it("refuses, writing nothing, while the catalog is unread (null)", async () => {
    const { batchCreateTasksFromIssues, TASK_CATALOG_UNREAD_MESSAGE } = await import("./issueTask");
    await expect(batchCreateTasksFromIssues(issues as never, "/r", null)).rejects.toThrow(TASK_CATALOG_UNREAD_MESSAGE);
    expect(puts.length, "items.put calls for an unreadable catalog").toBe(0);
  });

  it("still imports against a catalog that was read and is really empty", async () => {
    const { batchCreateTasksFromIssues } = await import("./issueTask");
    const result = await batchCreateTasksFromIssues(issues as never, "/r", []);
    expect(result.created.length).toBe(1);
  });
});

describe("ManviOpsPanel keeps an unread catalog distinct from an empty one", () => {
  const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "components", "ManviOpsPanel.svelte"), "utf8");
  it("types the catalog as nullable and clears it on a failed read", () => {
    expect(source).toContain("let repoTasks = $state<TaskCard[] | null>(null);");
    expect(source).toMatch(/catch \(err\) \{[^}]*repoTasks = null;\s*repoTasksError = formatError\(err\);/s);
    expect(source).not.toMatch(/repoTasks = \[\];/);
  });
  it("gates single and batch import on a known catalog and offers a retry", () => {
    expect(source).toContain("repoTasks === null");
    expect(source).toContain("Retry task list");
    expect(source).toContain("Task import paused");
  });
});
