import { describe, expect, it } from "vitest";
import { mulberry32 } from "../__tests__/prng";
import {
  findTaskForIssue,
  isIssueInTask,
  issueToTaskDraft,
  MAX_ISSUE_TASK_DESCRIPTION_BYTES,
  MAX_ISSUE_TASK_LABELS,
  MAX_ISSUE_TASK_TITLE,
  MAX_LABEL_LENGTH,
  sanitizeIssueLabels,
} from "./issueTask";
import type { IssueInfo } from "../ops/model";
import type { TaskCard } from "./client";

describe("issueTask adversarial stress testing", () => {
  const rand = mulberry32(0x1550e_7a5c); // "issue_task"

  const HOSTILE_STRINGS = [
    "",
    "   ",
    "\0",
    "\x00\x01\x02\x1F\x7F",
    "\r\n\t\v\f",
    "\u200B\u200C\u200D\uFEFF", // zero-width
    "🚀🔥💥🎉👾",
    "𝓤𝓷𝓲𝓬𝓸𝓭𝓮 𝓂𝒶𝓉𝒽",
    "البرمجة بلغة جافا سكريبت", // Arabic RTL
    "这是一段很长的中文字符串测试", // Chinese
    "DROP TABLE tasks; --",
    "<script>alert('xss')</script>",
    "&quot;&amp;&lt;&gt;",
    "\\u0000\\x00",
    "A".repeat(10_000),
    "B".repeat(100_000),
  ];

  function randomString(maxLen = 100): string {
    const choice = rand();
    if (choice < 0.3) {
      return HOSTILE_STRINGS[Math.floor(rand() * HOSTILE_STRINGS.length)];
    }
    const len = Math.floor(rand() * maxLen);
    let str = "";
    for (let i = 0; i < len; i++) {
      const code = Math.floor(rand() * 128);
      str += String.fromCharCode(code);
    }
    return str;
  }

  function randomIssue(i: number): IssueInfo {
    const numRoll = rand();
    const issueNum =
      numRoll < 0.05
        ? 0
        : numRoll < 0.1
          ? -1
          : numRoll < 0.15
            ? Number.NaN
            : numRoll < 0.2
              ? Number.POSITIVE_INFINITY
              : (i % 50_000) + 1;

    const labelCount = Math.floor(rand() * 50);
    const labels: string[] = [];
    for (let j = 0; j < labelCount; j++) {
      labels.push(randomString(40));
    }

    return {
      number: issueNum as number,
      title: randomString(2000),
      state: rand() > 0.5 ? "open" : "closed",
      url: randomString(500),
      labels,
      updated_at: randomString(100),
      author: randomString(50),
    };
  }

  // Coverage instrumentation pushed this past 15s during ci:local. The
  // bound is the run, not the invariant.
  it("survives 2,500 hostile issues without breaking task invariants", { timeout: 60_000 }, () => {
    const totalIterations = 2_500;
    const encoder = new TextEncoder();

    for (let i = 0; i < totalIterations; i++) {
      const issue = randomIssue(i);
      const repoId = rand() > 0.1 ? `repo-${i % 20}` : "";

      const draft = issueToTaskDraft(issue, repoId);

      // Invariant 1: Title bounds and non-empty
      expect(draft.title.length).toBeGreaterThan(0);
      expect(draft.title.length).toBeLessThanOrEqual(MAX_ISSUE_TASK_TITLE);
      expect(/[\x00-\x1F\x7F]/.test(draft.title)).toBe(false);

      // Invariant 2: Description byte bounds
      const descBytes = encoder.encode(draft.description).length;
      expect(descBytes).toBeLessThanOrEqual(MAX_ISSUE_TASK_DESCRIPTION_BYTES);

      // Invariant 3: Label bounds & normalization
      expect(draft.labels.length).toBeLessThanOrEqual(MAX_ISSUE_TASK_LABELS);
      expect(new Set(draft.labels).size).toBe(draft.labels.length);
      for (const label of draft.labels) {
        expect(label.length).toBeGreaterThan(0);
        expect(label.length).toBeLessThanOrEqual(MAX_LABEL_LENGTH);
        expect(/[\x00-\x1F\x7F]/.test(label)).toBe(false);
      }

      // Invariant 4: Kind is recognized
      expect(["bug", "feature", "task"]).toContain(draft.kind);

      // Invariant 5: Priority within 0..3
      expect(draft.priority).toBeGreaterThanOrEqual(0);
      expect(draft.priority).toBeLessThanOrEqual(3);
      expect(Number.isInteger(draft.priority)).toBe(true);

      // Invariant 6: Status is valid
      expect(["inbox", "ready", "in_progress", "review", "done"]).toContain(draft.status);

      // Invariant 7: Acceptance criteria is non-empty array of strings
      expect(Array.isArray(draft.acceptance_criteria)).toBe(true);
      expect(draft.acceptance_criteria.length).toBeGreaterThanOrEqual(1);

      // Invariant 8: Repository ID integrity
      if (repoId) {
        expect(draft.primary_repository_id).toBe(repoId);
        expect(draft.repository_ids).toContain(repoId);
      }
    }
  });

  it("accurately discriminates between similar issue numbers without false positives", () => {
    // Test boundary: issue 1 vs 10 vs 100 vs 1000
    const issues = [1, 10, 100, 1000, 10000, 42, 420, 2, 20];
    const tasks: TaskCard[] = issues.map((num, idx) => ({
      id: `task-${num}`,
      revision: 1,
      updated_at: 100,
      title: `[#${num}] Task for issue ${num}`,
      kind: "bug",
      status: "inbox",
      priority: 1,
      severity: null,
      owner: null,
      due_at: null,
      labels: ["github-issue", `issue-${num}`],
      repository_ids: ["repo-1"],
      primary_repository_id: "repo-1",
      home_workspace_id: null,
      position: idx,
    }));

    for (const num of issues) {
      const match = findTaskForIssue(tasks, num);
      expect(match).toBeDefined();
      expect(match?.id).toBe(`task-${num}`);
      expect(isIssueInTask(match!, num)).toBe(true);

      // Ensure no false match for sibling numbers
      for (const other of issues) {
        if (other !== num) {
          expect(isIssueInTask(match!, other)).toBe(false);
        }
      }
    }

    // Negative numbers or non-existing numbers never match
    expect(findTaskForIssue(tasks, 0)).toBeUndefined();
    expect(findTaskForIssue(tasks, -1)).toBeUndefined();
    expect(findTaskForIssue(tasks, 999)).toBeUndefined();
  });

  it("never confuses substring numbers with exact issue numbers", () => {
    // #42 must not match #420, #142, #4242, or #4200
    const trickyTasks: TaskCard[] = [
      {
        id: "task-420",
        revision: 1,
        updated_at: 100,
        title: "[#420] Substring match test",
        kind: "bug",
        status: "inbox",
        priority: 1,
        severity: null,
        owner: null,
        due_at: null,
        labels: ["github-issue", "issue-420"],
        repository_ids: ["repo-1"],
        primary_repository_id: "repo-1",
        home_workspace_id: null,
        position: 0,
      },
      {
        id: "task-142",
        revision: 1,
        updated_at: 100,
        title: "[#142] Another prefix test",
        kind: "bug",
        status: "inbox",
        priority: 1,
        severity: null,
        owner: null,
        due_at: null,
        labels: ["github-issue", "issue-142"],
        repository_ids: ["repo-1"],
        primary_repository_id: "repo-1",
        home_workspace_id: null,
        position: 1,
      },
    ];

    expect(isIssueInTask(trickyTasks[0], 42)).toBe(false);
    expect(isIssueInTask(trickyTasks[1], 42)).toBe(false);
    expect(findTaskForIssue(trickyTasks, 42)).toBeUndefined();
  });

  it("handles extreme label mixtures and deduplication under heavy stress", () => {
    const rawLabels = [
      "BUG",
      "bug",
      "Bug",
      "  bug  ",
      "bug\x00",
      "FEATURE",
      "feature",
      ...Array.from({ length: 100 }, (_, i) => `Duplicate-${i % 10}`),
      ...Array.from({ length: 50 }, () => ""),
      ...Array.from({ length: 50 }, () => "   "),
      ...Array.from({ length: 20 }, () => "\x00\x01\x1F"),
      ...Array.from({ length: 30 }, (_, i) => `Unique-Extra-${i}`),
    ];

    const sanitized = sanitizeIssueLabels(rawLabels, 77);

    expect(sanitized.length).toBeLessThanOrEqual(MAX_ISSUE_TASK_LABELS);
    expect(new Set(sanitized).size).toBe(sanitized.length);
    expect(sanitized).toContain("github-issue");
    expect(sanitized).toContain("issue-77");
    expect(sanitized).toContain("bug");
    expect(sanitized).toContain("feature");
    expect(sanitized.every((l) => l.trim().length > 0)).toBe(true);
    expect(sanitized.every((l) => !/[\x00-\x1F\x7F]/.test(l))).toBe(true);
  });

  it("never produces false positive matches when task titles cross-reference other issues", () => {
    // 1,000 tasks that each mention other issue numbers in their title body
    const count = 1_000;
    const tasks: TaskCard[] = [];

    for (let i = 1; i <= count; i++) {
      const referenced1 = ((i * 7) % count) + 1;
      const referenced2 = ((i * 13) % count) + 1;
      tasks.push({
        id: `task-${i}`,
        revision: 1,
        updated_at: 100,
        title: `[#${i}] Fix regression in #${referenced1} and see PR #${referenced2} for details`,
        kind: "bug",
        status: i % 2 === 0 ? "in_progress" : "done",
        priority: 1,
        severity: null,
        owner: null,
        due_at: null,
        labels: ["github-issue", `issue-${i}`],
        repository_ids: ["repo-1"],
        primary_repository_id: "repo-1",
        home_workspace_id: null,
        position: i,
      });
    }

    for (let i = 1; i <= count; i++) {
      const task = tasks[i - 1];
      const referenced1 = ((i * 7) % count) + 1;
      const referenced2 = ((i * 13) % count) + 1;

      // Must match its own issue number
      expect(isIssueInTask(task, i)).toBe(true);

      // Must NOT match referenced issues if they differ from i
      if (referenced1 !== i) {
        expect(isIssueInTask(task, referenced1)).toBe(false);
      }
      if (referenced2 !== i) {
        expect(isIssueInTask(task, referenced2)).toBe(false);
      }
    }
  });
});
