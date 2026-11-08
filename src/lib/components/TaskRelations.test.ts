import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";
import { render } from "svelte/server";
import TaskRelations from "./TaskRelations.svelte";
import type { Task, TaskCard } from "../workbench/client";
import type { AgentInboxThread } from "../agents/inboxTypes";

const relationsSource = readFileSync(new URL("./TaskRelations.svelte", import.meta.url), "utf8");
const editorSource = readFileSync(new URL("./TaskEditor.svelte", import.meta.url), "utf8");

const savedParent: Task = {
  id: "task-parent-1",
  revision: 2,
  updated_at: 1700000000,
  title: "Upgrade Multi-Agent Coordination",
  description: "Support subtask fan-out",
  kind: "feature",
  status: "in_progress",
  priority: 1,
  severity: null,
  owner: null,
  due_at: null,
  labels: [],
  acceptance_criteria: [
    "- Propagate .claude-account to linked worktrees",
    "- Support warm multi-turn managed continuations",
  ],
  repository_ids: ["repo-1"],
  primary_repository_id: "repo-1",
  home_workspace_id: null,
  position: 1,
  archived: false,
  completed_at: null,
  checklist: [{ text: "Verify IPC contract", done: true }],
  links: [],
};

const sampleSubtasks: TaskCard[] = [
  {
    id: "sub-1",
    revision: 1,
    updated_at: 1700000010,
    title: "Propagate .claude-account to linked worktrees",
    kind: "subtask",
    status: "done",
    priority: 1,
    severity: null,
    owner: "claude",
    due_at: null,
    labels: [],
    repository_ids: ["repo-1"],
    primary_repository_id: "repo-1",
    home_workspace_id: null,
    position: 2,
    archived: false,
    completed_at: 1700000020,
  },
  {
    id: "sub-2",
    revision: 1,
    updated_at: 1700000015,
    title: "Support warm multi-turn managed continuations",
    kind: "subtask",
    status: "ready",
    priority: 1,
    severity: null,
    owner: null,
    due_at: null,
    labels: [],
    repository_ids: ["repo-1"],
    primary_repository_id: "repo-1",
    home_workspace_id: null,
    position: 3,
    archived: false,
    completed_at: null,
  },
];

const sampleThreads: AgentInboxThread[] = [
  {
    thread_key: "task-task-parent-1",
    title: "Task Breakdown: Upgrade Multi-Agent Coordination",
    status: "open",
    ask_type: "answer",
    from_agent: "agy-worker",
    to_agent: "developer",
    updated_at_ms: 1700000050000,
    latest_msg: {
      seq: 3,
      from_agent: "agy-worker",
      to_agent: "developer",
      kind: "ask",
      body: JSON.stringify({
        ask_type: "answer",
        task_id: "task-parent-1",
        question: "Should max_continuations default to 0 for non-interactive waves?",
      }),
      branch: "feat/warm",
      created_at_ms: 1700000050000,
      thread_key: "task-task-parent-1",
    },
    messages: [
      {
        seq: 3,
        from_agent: "agy-worker",
        to_agent: "developer",
        kind: "ask",
        body: JSON.stringify({
          ask_type: "answer",
          task_id: "task-parent-1",
          question: "Should max_continuations default to 0 for non-interactive waves?",
        }),
        branch: "feat/warm",
        created_at_ms: 1700000050000,
        thread_key: "task-task-parent-1",
      },
    ],
  },
];

describe("TaskRelations Linear-style subtask breakdown & collaboration", () => {
  it("compiles cleanly without warnings", () => {
    const { warnings } = compile(relationsSource, {
      generate: "client",
      filename: "TaskRelations.svelte",
    });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });

  it("renders inline subtask breakdown composer, split-criteria button, child subtask kickoff controls, and linked inbox threads", () => {
    const html = render(TaskRelations, {
      props: {
        checklist: savedParent.checklist,
        links: savedParent.links,
        taskId: savedParent.id,
        task: savedParent,
        acceptanceCriteria: savedParent.acceptance_criteria,
        repositoryPath: "/workspace/gitpulse",
        initialSubtasks: sampleSubtasks,
        initialInboxThreads: sampleThreads,
        onchange: () => {},
      },
    }).body;

    expect(html).toContain('data-testid="task-breakdown-section"');
    expect(html).toContain('data-testid="task-breakdown-input"');
    expect(html).toContain('data-testid="task-breakdown-submit"');
    expect(html).toContain('data-testid="task-split-criteria-btn"');
    expect(html).toContain("Split acceptance criteria into subtasks");

    expect(html).toContain('data-testid="task-child-subtasks"');
    expect(html).toContain("1 of 2 subtasks done (50%)");
    expect(html).toContain("Propagate .claude-account to linked worktrees");
    expect(html).toContain("Support warm multi-turn managed continuations");
    expect(html).toContain('data-testid="subtask-kickoff-claude-sub-2"');
    expect(html).toContain('data-testid="subtask-kickoff-agy-sub-2"');
    expect(html).toContain('data-testid="subtask-kickoff-codex-sub-2"');

    expect(html).toContain('data-testid="task-linked-inbox"');
    expect(html).toContain("Should max_continuations default to 0 for non-interactive waves?");
    expect(html).toContain('data-testid="task-inbox-ask-input"');
  });

  it("passes task, acceptanceCriteria, repositories, openTabs, and repositoryPath from TaskEditor into TaskRelations", () => {
    expect(editorSource).toMatch(/<TaskRelations[\s\S]*?task=\{current\}/);
    expect(editorSource).toMatch(/<TaskRelations[\s\S]*?acceptanceCriteria=\{criteriaList\}/);
    expect(editorSource).toMatch(/<TaskRelations[\s\S]*?repositories=\{known\}/);
  });
});
