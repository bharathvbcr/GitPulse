import { describe, expect, it, vi, beforeEach } from "vitest";
import {
  batchCreateTasksFromIssues,
  createTaskFromIssue,
  findTaskForIssue,
  formatAcceptanceCriteria,
  formatAgentDescription,
  inferTaskKind,
  inferTaskPriority,
  inferTaskSeverity,
  isIssueInTask,
  isIssueInTasks,
  issueToTaskDraft,
  listAllRepositoryTasks,
  MAX_ISSUE_TASK_DESCRIPTION_BYTES,
  MAX_ISSUE_TASK_LABELS,
  MAX_ISSUE_TASK_TITLE,
  sanitizeIssueLabels,
  sanitizeIssueTitle,
} from "./issueTask";
import type { IssueInfo } from "../ops/model";
import type { TaskCard } from "./client";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

import { invoke } from "@tauri-apps/api/core";

function sampleIssue(overrides: Partial<IssueInfo> = {}): IssueInfo {
  return {
    number: 42,
    title: "Race condition in watcher sync loop",
    state: "open",
    url: "https://github.com/bharathvbcr/GitPulse/issues/42",
    labels: ["bug", "priority:high"],
    updated_at: "2026-09-21T07:00:00Z",
    author: "octocat",
    ...overrides,
  };
}

describe("sanitizeIssueTitle", () => {
  it("formats standard issue titles with issue number prefix", () => {
    expect(sanitizeIssueTitle("Broken scroll", 10)).toBe("[#10] Broken scroll");
  });

  it("handles empty or whitespace-only titles by defaulting to issue number", () => {
    expect(sanitizeIssueTitle("", 10)).toBe("[#10] Issue #10");
    expect(sanitizeIssueTitle("   ", 15)).toBe("[#15] Issue #15");
    expect(sanitizeIssueTitle(null, 12)).toBe("[#12] Issue #12");
  });

  it("strips ASCII control characters and newline escapes", () => {
    const dirty = "Title with \x00 null \x1F and \r\n newlines \x7F DEL";
    expect(sanitizeIssueTitle(dirty, 5)).toBe("[#5] Title with   null   and   newlines   DEL");
  });

  it("truncates excessively long titles to MAX_ISSUE_TASK_TITLE", () => {
    const huge = "a".repeat(500);
    const sanitized = sanitizeIssueTitle(huge, 100);
    expect(sanitized.length).toBe(MAX_ISSUE_TASK_TITLE);
    expect(sanitized.endsWith("…")).toBe(true);
  });
});

describe("inferTaskKind", () => {
  it("infers 'bug' for bug-related labels", () => {
    expect(inferTaskKind(["bug"])).toBe("bug");
    expect(inferTaskKind(["defect"])).toBe("bug");
    expect(inferTaskKind(["fix"])).toBe("bug");
    expect(inferTaskKind(["crash"])).toBe("bug");
    expect(inferTaskKind([])).toBe("bug");
  });

  it("infers 'feature' for enhancement or proposal labels", () => {
    expect(inferTaskKind(["enhancement"])).toBe("feature");
    expect(inferTaskKind(["feature"])).toBe("feature");
    expect(inferTaskKind(["feat: UI", "design"])).toBe("feature");
    expect(inferTaskKind(["proposal"])).toBe("feature");
  });

  it("infers 'task' for documentation, chore, or maintenance", () => {
    expect(inferTaskKind(["documentation"])).toBe("task");
    expect(inferTaskKind(["docs"])).toBe("task");
    expect(inferTaskKind(["chore"])).toBe("task");
    expect(inferTaskKind(["refactor"])).toBe("task");
  });
});

describe("inferTaskPriority and severity", () => {
  it("maps critical/urgent labels to priority 3 and critical severity", () => {
    expect(inferTaskPriority(["p0"])).toBe(3);
    expect(inferTaskPriority(["critical"])).toBe(3);
    expect(inferTaskPriority(["priority:urgent"])).toBe(3);
    expect(inferTaskSeverity(["critical"])).toBe("critical");
    expect(inferTaskSeverity(["sev1"])).toBe("critical");
  });

  it("maps high priority labels to priority 2 and high severity", () => {
    expect(inferTaskPriority(["p1"])).toBe(2);
    expect(inferTaskPriority(["priority:high"])).toBe(2);
    expect(inferTaskPriority(["important"])).toBe(2);
    expect(inferTaskSeverity(["sev2"])).toBe("high");
  });

  it("maps low priority labels to priority 0", () => {
    expect(inferTaskPriority(["p3"])).toBe(0);
    expect(inferTaskPriority(["minor"])).toBe(0);
    expect(inferTaskPriority(["trivial"])).toBe(0);
  });

  it("defaults to priority 1 and null severity", () => {
    expect(inferTaskPriority([])).toBe(1);
    expect(inferTaskPriority(["random"])).toBe(1);
    expect(inferTaskSeverity([])).toBeNull();
  });
});

describe("sanitizeIssueLabels", () => {
  it("always includes 'github-issue' and 'issue-<num>'", () => {
    const labels = sanitizeIssueLabels(["frontend"], 42);
    expect(labels).toContain("github-issue");
    expect(labels).toContain("issue-42");
    expect(labels).toContain("frontend");
  });

  it("deduplicates labels case-insensitively", () => {
    const labels = sanitizeIssueLabels(["Bug", "BUG", "bug"], 1);
    const bugCount = labels.filter((l) => l === "bug").length;
    expect(bugCount).toBe(1);
  });

  it("caps total labels at MAX_ISSUE_TASK_LABELS", () => {
    const manyLabels = Array.from({ length: 50 }, (_, i) => `label-${i}`);
    const sanitized = sanitizeIssueLabels(manyLabels, 10);
    expect(sanitized.length).toBeLessThanOrEqual(MAX_ISSUE_TASK_LABELS);
  });
});

describe("formatAgentDescription", () => {
  it("includes issue metadata and agent instructions", () => {
    const issue = sampleIssue();
    const desc = formatAgentDescription(issue);
    expect(desc).toContain("GitHub Issue #42: Race condition in watcher sync loop");
    expect(desc).toContain("https://github.com/bharathvbcr/GitPulse/issues/42");
    expect(desc).toContain("@octocat");
    expect(desc).toContain("Coding Agent Workflow");
    expect(desc).toContain("Locate & Audit");
  });

  it("bounds output strictly within 64 KB", () => {
    const hugeGuidance = "x".repeat(100_000);
    const issue = sampleIssue();
    const desc = formatAgentDescription(issue, hugeGuidance);
    const bytes = new TextEncoder().encode(desc).length;
    expect(bytes).toBeLessThanOrEqual(MAX_ISSUE_TASK_DESCRIPTION_BYTES);
    expect(desc).toContain("Description truncated");
  });
});

describe("formatAcceptanceCriteria", () => {
  it("generates clear resolution and verification criteria", () => {
    const issue = sampleIssue();
    const criteria = formatAcceptanceCriteria(issue);
    expect(criteria).toHaveLength(2);
    expect(criteria[0]).toContain("Resolve GitHub issue #42");
    expect(criteria[1]).toContain("automated test suite");
  });
});

describe("isIssueInTask and findTaskForIssue", () => {
  const card1: TaskCard = {
    id: "task-1",
    revision: 1,
    updated_at: 100,
    title: "[#42] Fix watcher race",
    kind: "bug",
    status: "inbox",
    priority: 2,
    severity: null,
    owner: null,
    due_at: null,
    labels: ["github-issue", "issue-42"],
    repository_ids: ["repo-1"],
    primary_repository_id: "repo-1",
    home_workspace_id: null,
    position: 0,
  };

  const card2: TaskCard = {
    id: "task-2",
    revision: 1,
    updated_at: 100,
    title: "Unrelated task without number",
    kind: "feature",
    status: "inbox",
    priority: 1,
    severity: null,
    owner: null,
    due_at: null,
    labels: ["ui"],
    repository_ids: ["repo-1"],
    primary_repository_id: "repo-1",
    home_workspace_id: null,
    position: 1,
  };

  it("matches by label or title", () => {
    expect(isIssueInTask(card1, 42)).toBe(true);
    expect(isIssueInTask(card2, 42)).toBe(false);
    expect(isIssueInTask(card1, 99)).toBe(false);
  });

  it("finds matching task from list", () => {
    const tasks = [card1, card2];
    expect(findTaskForIssue(tasks, 42)).toBe(card1);
    expect(findTaskForIssue(tasks, 99)).toBeUndefined();
    expect(isIssueInTasks(tasks, 42)).toBe(true);
    expect(isIssueInTasks(tasks, 99)).toBe(false);
  });

  it("does NOT match cross-referenced issue numbers in other task titles (false positive prevention)", () => {
    const cardReferencing42: TaskCard = {
      id: "task-100",
      revision: 1,
      updated_at: 100,
      title: "[#100] Fix regression introduced in #42",
      kind: "bug",
      status: "inbox",
      priority: 2,
      severity: null,
      owner: null,
      due_at: null,
      labels: ["github-issue", "issue-100"],
      repository_ids: ["repo-1"],
      primary_repository_id: "repo-1",
      home_workspace_id: null,
      position: 0,
    };
    expect(isIssueInTask(cardReferencing42, 100)).toBe(true);
    // Crucial check: Issue #42 must NOT match cardReferencing42!
    expect(isIssueInTask(cardReferencing42, 42)).toBe(false);
  });

  it("matches supported title prefix variations for issue numbers", () => {
    const cardColon: TaskCard = { ...card1, title: "#42: Fix watcher race", labels: [] };
    const cardDash: TaskCard = { ...card1, title: "#42 - Fix watcher race", labels: [] };
    const cardTag: TaskCard = { ...card1, title: "[Issue #42] Fix watcher race", labels: [] };
    const cardSpace: TaskCard = { ...card1, title: "#42 Fix watcher race", labels: [] };
    const cardOther: TaskCard = { ...card1, title: "#420 Fix watcher race", labels: [] };

    expect(isIssueInTask(cardColon, 42)).toBe(true);
    expect(isIssueInTask(cardDash, 42)).toBe(true);
    expect(isIssueInTask(cardTag, 42)).toBe(true);
    expect(isIssueInTask(cardSpace, 42)).toBe(true);
    expect(isIssueInTask(cardOther, 42)).toBe(false);
  });

  it("matches tasks regardless of task status (inbox, ready, in_progress, review, done)", () => {
    const statuses = ["inbox", "ready", "in_progress", "review", "done"] as const;
    for (const status of statuses) {
      const card: TaskCard = { ...card1, status };
      expect(isIssueInTask(card, 42)).toBe(true);
    }
  });
});

describe("issueToTaskDraft", () => {
  it("converts IssueInfo to TaskDraft with all constraints satisfied", () => {
    const issue = sampleIssue();
    const draft = issueToTaskDraft(issue, "repo-123");

    expect(draft.title).toBe("[#42] Race condition in watcher sync loop");
    expect(draft.kind).toBe("bug");
    expect(draft.status).toBe("inbox");
    expect(draft.priority).toBe(2);
    expect(draft.primary_repository_id).toBe("repo-123");
    expect(draft.repository_ids).toEqual(["repo-123"]);
    expect(draft.labels).toContain("github-issue");
    expect(draft.labels).toContain("issue-42");
    expect(draft.acceptance_criteria.length).toBeGreaterThanOrEqual(2);
    expect(draft.description).toContain("## GitHub Issue #42");
  });
});

describe("createTaskFromIssue and batchCreateTasksFromIssues", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("registers repository and writes task through Tauri IPC", async () => {
    // 1. mock cmd_workbench_register_repository
    vi.mocked(invoke).mockImplementation(async (cmd: string, args?: unknown) => {
      if (cmd === "cmd_workbench_register_repository") {
        return JSON.stringify({
          ok: true,
          repository: {
            id: "repo-abc",
            revision: 1,
            updated_at: 100,
            name: "gitpulse",
            identity_key: "local:/path",
            remote_url: null,
          },
        });
      }
      if (cmd === "cmd_workbench_request") {
        const body = (args as { method: string; input: string }).input;
        const parsed = JSON.parse(body);
        return JSON.stringify({
          ok: true,
          item: {
            id: parsed.id,
            revision: 1,
            updated_at: 101,
            due_at: parsed.due_at ?? null,
            severity: parsed.severity ?? null,
            owner: parsed.owner ?? null,
            home_workspace_id: parsed.home_workspace_id ?? null,
            ...parsed,
          },
        });
      }
      return JSON.stringify({ ok: true });
    });

    const issue = sampleIssue();
    const task = await createTaskFromIssue(issue, "/path/to/repo");

    expect(task.id).toBeDefined();
    expect(task.title).toBe("[#42] Race condition in watcher sync loop");
    expect(task.primary_repository_id).toBe("repo-abc");
    expect(invoke).toHaveBeenCalledWith("cmd_workbench_register_repository", expect.any(Object));
  });

  it("skips issues that already have tasks during batch creation", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string, args?: unknown) => {
      if (cmd === "cmd_workbench_register_repository") {
        return JSON.stringify({
          ok: true,
          repository: {
            id: "repo-abc",
            revision: 1,
            updated_at: 100,
            name: "gitpulse",
            identity_key: "local:/path",
            remote_url: null,
          },
        });
      }
      if (cmd === "cmd_workbench_request") {
        const body = (args as { method: string; input: string }).input;
        const parsed = JSON.parse(body);
        return JSON.stringify({
          ok: true,
          item: {
            id: parsed.id,
            revision: 1,
            updated_at: 101,
            due_at: parsed.due_at ?? null,
            severity: parsed.severity ?? null,
            owner: parsed.owner ?? null,
            home_workspace_id: parsed.home_workspace_id ?? null,
            ...parsed,
          },
        });
      }
      return JSON.stringify({ ok: true });
    });

    const existingCard: TaskCard = {
      id: "task-10",
      revision: 1,
      updated_at: 100,
      title: "[#10] Already imported",
      kind: "bug",
      status: "inbox",
      priority: 1,
      severity: null,
      owner: null,
      due_at: null,
      labels: ["issue-10"],
      repository_ids: ["repo-abc"],
      primary_repository_id: "repo-abc",
      home_workspace_id: null,
      position: 0,
    };

    const issues: IssueInfo[] = [
      sampleIssue({ number: 10, title: "Already imported" }),
      sampleIssue({ number: 11, title: "New issue 11" }),
      sampleIssue({ number: 12, title: "New issue 12" }),
    ];

    const result = await batchCreateTasksFromIssues(issues, "/path/to/repo", [existingCard]);

    expect(result.skipped).toBe(1);
    expect(result.created).toHaveLength(2);
    expect(result.created[0].title).toBe("[#11] New issue 11");
    expect(result.created[1].title).toBe("[#12] New issue 12");
  });
});

describe("listAllRepositoryTasks", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("returns an empty array when repositoryId is blank", async () => {
    const tasks = await listAllRepositoryTasks("");
    expect(tasks).toEqual([]);
  });

  it("paginates tasks across multiple pages until all are retrieved", async () => {
    const cardA: TaskCard = {
      id: "task-a",
      revision: 1,
      updated_at: 100,
      title: "[#1] First issue",
      kind: "bug",
      status: "in_progress",
      priority: 1,
      severity: null,
      owner: null,
      due_at: null,
      labels: ["issue-1"],
      repository_ids: ["repo-x"],
      primary_repository_id: "repo-x",
      home_workspace_id: null,
      position: 0,
    };
    const cardB: TaskCard = {
      id: "task-b",
      revision: 1,
      updated_at: 101,
      title: "[#2] Second issue",
      kind: "feature",
      status: "done",
      priority: 2,
      severity: null,
      owner: null,
      due_at: null,
      labels: ["issue-2"],
      repository_ids: ["repo-x"],
      primary_repository_id: "repo-x",
      home_workspace_id: null,
      position: 1,
    };

    vi.mocked(invoke).mockImplementation(async (cmd: string, args?: unknown) => {
      if (cmd === "cmd_workbench_request") {
        const payload = args as { method: string; input: string };
        const parsed = JSON.parse(payload.input);
        if (parsed.cursor === "page-2") {
          return JSON.stringify({
            ok: true,
            items: [cardB],
            total: 2,
            shown: 1,
            has_more: false,
            next_cursor: null,
          });
        }
        return JSON.stringify({
          ok: true,
          items: [cardA],
          total: 2,
          shown: 1,
          has_more: true,
          next_cursor: "page-2",
        });
      }
      return JSON.stringify({ ok: true });
    });

    const tasks = await listAllRepositoryTasks("repo-x", 100);
    expect(tasks).toHaveLength(2);
    expect(tasks[0].id).toBe("task-a");
    expect(tasks[0].status).toBe("in_progress");
    expect(tasks[1].id).toBe("task-b");
    expect(tasks[1].status).toBe("done");
  });

  it("respects maxTasks budget and caps results", async () => {
    const card = (i: number): TaskCard => ({
      id: `task-${i}`,
      revision: 1,
      updated_at: 100,
      title: `Task ${i}`,
      kind: "task",
      status: "ready",
      priority: 1,
      severity: null,
      owner: null,
      due_at: null,
      labels: [],
      repository_ids: ["repo-x"],
      primary_repository_id: "repo-x",
      home_workspace_id: null,
      position: i,
    });

    vi.mocked(invoke).mockImplementation(async () => {
      return JSON.stringify({
        ok: true,
        items: [card(1), card(2), card(3), card(4)],
        total: 4,
        shown: 4,
        has_more: false,
        next_cursor: null,
      });
    });

    const tasks = await listAllRepositoryTasks("repo-x", 2);
    expect(tasks).toHaveLength(2);
    expect(tasks[0].id).toBe("task-1");
    expect(tasks[1].id).toBe("task-2");
  });
});
