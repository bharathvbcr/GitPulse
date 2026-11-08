import { describe, expect, it, vi } from "vitest";
import type { AgentInboxThread, InboxDecomposeReceipt, InboxMessageReceipt, InboxSnapshot } from "./inboxTypes";
import { WorkbenchError, type Repository, type Task, type TaskCard, type TaskRun } from "../workbench/client";
import {
  batchResolveThreadsWithNote,
  batchWithdrawOpenThreads,
  buildInboxHandoffPrompt,
  createChildSubtasksForParent,
  extractTaskIdFromThreadKey,
  isMobileOriginated,
  kickOffAgentForTask,
  kickOffAgentFromInboxThread,
  loadChildSubtasks,
  loadLinkedTaskThreads,
  matchesTaskThread,
  parseMobileKickoffDirective,
  parseMobileTaskBreakdown,
  postTaskAgentAsk,
  replyToTaskThread,
  selectVisibleThreads,
  toggleThreadSelection,
} from "./collaboration";

function makeThread(key: string, status: AgentInboxThread["status"], body = "Ask body", from = "claude-worker"): AgentInboxThread {
  const msg = {
    seq: 1,
    from_agent: from,
    to_agent: "developer",
    kind: status === "open" ? "ask" : "reply",
    body,
    branch: "feat/collab",
    created_at_ms: 1700000000000,
    thread_key: key,
  };
  return {
    thread_key: key,
    title: `Title ${key}`,
    status,
    ask_type: status === "open" ? "do" : null,
    latest_msg: msg,
    messages: [msg],
    from_agent: from,
    to_agent: "developer",
    updated_at_ms: 1700000000000,
  };
}

function makeTask(id: string, overrides: Partial<Task> = {}): Task {
  return {
    id,
    revision: 3,
    updated_at: 1700000000,
    title: `Task ${id}`,
    description: "Parent description",
    kind: "feature",
    status: "ready",
    priority: 2,
    severity: null,
    owner: null,
    due_at: null,
    labels: ["collab"],
    acceptance_criteria: ["Criterion A", "Criterion B"],
    repository_ids: ["repo-1"],
    primary_repository_id: "repo-1",
    home_workspace_id: null,
    position: 100,
    archived: false,
    completed_at: null,
    checklist: [],
    links: [],
    ...overrides,
  };
}

function makeRepo(id = "repo-1", path = "/workspace/gitpulse"): Repository {
  return {
    id,
    revision: 2,
    updated_at: 1700000000,
    name: "GitPulse",
    identity_key: path,
    remote_url: null,
  };
}

function makeRun(id: string, kind: "managed" | "external_terminal", provider: "claude" | "agy" | "codex"): TaskRun {
  return {
    id,
    revision: 1,
    updated_at: 1700000000,
    kind,
    task_id: "task-1",
    source_revision: 3,
    task_title: "Task task-1",
    repository_id: "repo-1",
    provider,
    permission_mode: "edit",
    state: "running",
    cwd: "/workspace/gitpulse",
    created_at: 1700000000,
    expires_at: 1700003600,
    session_id: "550e8400-e29b-41d4-a716-446655440000",
    exit_code: null,
    reason: "",
    outcome_uncertain: false,
  };
}

describe("Agent collaboration & batch inbox triage", () => {
  it("toggles individual thread keys and selects visible threads immutably", () => {
    const t1 = makeThread("t-1", "open");
    const t2 = makeThread("t-2", "answered");
    const selected = selectVisibleThreads([t1, t2]);
    expect([...selected]).toEqual(["t-1", "t-2"]);

    const afterRemove = toggleThreadSelection(selected, "t-1");
    expect([...afterRemove]).toEqual(["t-2"]);
    expect([...selected]).toEqual(["t-1", "t-2"]);

    const afterAdd = toggleThreadSelection(afterRemove, "t-3");
    expect([...afterAdd]).toEqual(["t-2", "t-3"]);
  });

  it("batch withdraws only open selected threads and preserves non-open threads", async () => {
    const threads = [
      makeThread("open-1", "open"),
      makeThread("ans-1", "answered"),
      makeThread("open-2", "open"),
    ];
    const invokeMock = vi.fn(async (_cmd: string, args?: Record<string, unknown>) => ({
      ok: true,
      thread_key: String(args?.threadKey ?? ""),
      message: {
        seq: 99,
        from_agent: "developer",
        to_agent: "claude-worker",
        kind: "withdraw",
        body: `Ask withdrawn: ${String(args?.reason ?? "")}`,
        branch: null,
        created_at_ms: 1700000050000,
        thread_key: String(args?.threadKey ?? ""),
      },
    } satisfies InboxMessageReceipt));

    const res = await batchWithdrawOpenThreads(
      {
        repoPath: "/workspace/gitpulse",
        threads,
        selectedKeys: new Set(["open-1", "ans-1", "open-2"]),
        reason: "Superseded by Round 2",
      },
      { invoke: invokeMock as never }
    );

    expect(res.withdrawnKeys).toEqual(["open-1", "open-2"]);
    expect(res.skippedCount).toBe(1);
    expect(res.errors).toEqual([]);
    expect(invokeMock).toHaveBeenCalledTimes(2);
    expect(res.updatedThreads.find((t) => t.thread_key === "open-1")?.status).toBe("withdrawn");
    expect(res.updatedThreads.find((t) => t.thread_key === "ans-1")?.status).toBe("answered");
    expect(res.updatedThreads.find((t) => t.thread_key === "open-2")?.status).toBe("withdrawn");
  });

  it("records partial errors during batch withdraw without losing successful updates and supports offline fallback", async () => {
    const threads = [makeThread("open-ok", "open"), makeThread("open-err", "open")];
    const invokeMock = vi.fn(async (_cmd: string, args?: Record<string, unknown>) => {
      if (args?.threadKey === "open-err") throw new Error("disk full");
      return {
        ok: true,
        thread_key: "open-ok",
        message: {
          seq: 2,
          from_agent: "developer",
          to_agent: null,
          kind: "withdraw",
          body: "Withdrawn",
          branch: null,
          created_at_ms: 1700000010000,
          thread_key: "open-ok",
        },
      };
    });

    const partial = await batchWithdrawOpenThreads(
      {
        repoPath: "/workspace/gitpulse",
        threads,
        selectedKeys: new Set(["open-ok", "open-err"]),
      },
      { invoke: invokeMock as never }
    );
    expect(partial.withdrawnKeys).toEqual(["open-ok"]);
    expect(partial.errors).toEqual(["open-err: disk full"]);
    expect(partial.updatedThreads[0].status).toBe("withdrawn");
    expect(partial.updatedThreads[1].status).toBe("open");

    const offline = await batchWithdrawOpenThreads({
      threads: [makeThread("offline-1", "open")],
      selectedKeys: new Set(["offline-1"]),
    });
    expect(offline.withdrawnKeys).toEqual(["offline-1"]);
    expect(offline.updatedThreads[0].status).toBe("withdrawn");
  });

  it("batch posts resolution notes across selected non-withdrawn threads and validates note bounds", async () => {
    await expect(
      batchResolveThreadsWithNote({
        threads: [makeThread("open-1", "open")],
        selectedKeys: new Set(["open-1"]),
        note: "   ",
      })
    ).rejects.toThrow(/Resolution note is required/);

    await expect(
      batchResolveThreadsWithNote({
        threads: [makeThread("open-1", "open")],
        selectedKeys: new Set(["open-1"]),
        note: "x".repeat(9000),
      })
    ).rejects.toThrow(/exceeds/);

    const invokeMock = vi.fn(async (_cmd: string, args?: Record<string, unknown>) => ({
      ok: true,
      thread_key: String(args?.threadKey ?? ""),
      message: {
        seq: 12,
        from_agent: "developer",
        to_agent: String(args?.toAgent ?? ""),
        kind: String(args?.kind ?? "reply"),
        body: String(args?.body ?? ""),
        branch: null,
        created_at_ms: 1700000060000,
        thread_key: String(args?.threadKey ?? ""),
      },
    } satisfies InboxMessageReceipt));

    const threads = [
      makeThread("open-1", "open"),
      makeThread("ans-1", "answered"),
      makeThread("withdrawn-1", "withdrawn"),
    ];
    const res = await batchResolveThreadsWithNote(
      {
        repoPath: "/workspace/gitpulse",
        threads,
        selectedKeys: new Set(["open-1", "ans-1", "withdrawn-1"]),
        note: "Resolved in commit abc1234",
      },
      { invoke: invokeMock as never }
    );

    expect(res.resolvedKeys).toEqual(["open-1", "ans-1"]);
    expect(res.skippedCount).toBe(1);
    expect(invokeMock).toHaveBeenCalledTimes(2);
    expect(invokeMock.mock.calls[0][1]).toMatchObject({ kind: "reply", threadKey: "open-1" });
    expect(invokeMock.mock.calls[1][1]).toMatchObject({ kind: "note", threadKey: "ans-1" });
    expect(res.updatedThreads[0].status).toBe("answered");

    const offline = await batchResolveThreadsWithNote({
      threads: [makeThread("open-local", "open")],
      selectedKeys: new Set(["open-local"]),
      note: "Done locally",
    });
    expect(offline.resolvedKeys).toEqual(["open-local"]);
    expect(offline.updatedThreads[0].status).toBe("answered");
  });

  it("kicks off real Managed (warm multi-turn) and Terminal worktree sessions for a board task", async () => {
    const task = makeTask("task-1");
    const repo = makeRepo("repo-1", "/workspace/gitpulse");
    const prepareMock = vi.fn(async (prep) => makeRun(prep.id, prep.kind ?? "external_terminal", prep.provider as "claude" | "agy" | "codex"));
    const startMock = vi.fn(async (run) => ({ ok: true as const, run }));
    let seq = 0;

    const managedRes = await kickOffAgentForTask(
      {
        taskId: "task-1",
        provider: "agy",
        mode: "managed",
        repositories: [repo],
        openTabs: [{ path: "/workspace/gitpulse", label: "GitPulse" }],
      },
      {
        getTask: async () => task,
        getRepository: async () => repo,
        registerRepository: async () => repo,
        prepareTaskRun: prepareMock,
        startPreparedAttempt: startMock,
        newID: () => `id-${++seq}`,
      }
    );

    expect(prepareMock).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: "managed",
        task_id: "task-1",
        provider: "agy",
        repo_path: "/workspace/gitpulse",
        worktree: true,
      })
    );
    expect(startMock).toHaveBeenCalledWith(expect.objectContaining({ provider: "agy" }), {
      maxContinuations: 8,
    });
    expect(managedRes.checkoutPath).toBe("/workspace/gitpulse");

    // Also auto-registers repoPath when task has no primary_repository_id
    const unlinkedTask = makeTask("task-unlinked", { primary_repository_id: "", repository_ids: [] });
    const registerMock = vi.fn(async () => repo);
    await kickOffAgentForTask(
      {
        taskId: "task-unlinked",
        provider: "claude",
        mode: "terminal",
        repoPath: "/workspace/gitpulse",
      },
      {
        getTask: async () => unlinkedTask,
        getRepository: async () => repo,
        registerRepository: registerMock,
        prepareTaskRun: prepareMock,
        startPreparedAttempt: startMock,
        newID: () => `id-${++seq}`,
      }
    );
    expect(registerMock).toHaveBeenCalledWith("/workspace/gitpulse");
    expect(startMock).toHaveBeenLastCalledWith(expect.objectContaining({ provider: "claude" }), {});

    // When a repository's identity_key is "local:/workspace/bare.git" (or "local:repo-no-roots" with no working tree),
    // kickOffAgentForTask must fall back to normalized params.repoPath instead of passing raw "local:...".
    const repoNoRoots: Repository = {
      ...repo,
      id: "repo-no-roots",
      identity_key: "local:/workspace/bare.git",
    };
    const taskNoRoots = makeTask("task-no-roots", {
      primary_repository_id: "repo-no-roots",
      repository_ids: ["repo-no-roots"],
    });
    const resFallback = await kickOffAgentForTask(
      {
        taskId: "task-no-roots",
        provider: "claude",
        mode: "managed",
        repoPath: "/workspace/fallback-checkout",
        repositories: [repoNoRoots],
      },
      {
        getTask: async () => taskNoRoots,
        getRepository: async () => repoNoRoots,
        registerRepository: async () => repoNoRoots,
        prepareTaskRun: prepareMock,
        startPreparedAttempt: startMock,
        newID: () => `id-${++seq}`,
      }
    );
    expect(resFallback.checkoutPath).toBe("/workspace/fallback-checkout");
    expect(prepareMock).toHaveBeenLastCalledWith(
      expect.objectContaining({
        repo_path: "/workspace/fallback-checkout",
      })
    );
  });

  it("kicks off agents from Inbox threads for linked tasks, unlinked terminal tabs, and unlinked managed runs while posting handoff messages", async () => {
    expect(extractTaskIdFromThreadKey("task-gp-123")).toBe("gp-123");
    expect(extractTaskIdFromThreadKey("decide-arch")).toBeNull();

    const task = makeTask("gp-123");
    const repo = makeRepo("repo-1", "/workspace/gitpulse");
    const invokeMock = vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "cmd_inbox_decompose_task") {
        return {
          ok: true,
          item_id: "gp-decomposed-1",
          thread_key: "task-gp-decomposed-1",
          subtasks: [],
          inbox_message: makeThread("task-gp-decomposed-1", "open").latest_msg,
        } satisfies InboxDecomposeReceipt;
      }
      return {
        ok: true,
        thread_key: String(args?.threadKey ?? ""),
        message: {
          seq: 5,
          from_agent: "developer",
          to_agent: String(args?.toAgent ?? ""),
          kind: "handoff",
          body: String(args?.body ?? ""),
          branch: null,
          created_at_ms: 1700000090000,
          thread_key: String(args?.threadKey ?? ""),
        },
      } satisfies InboxMessageReceipt;
    });
    const terminalRequestMock = vi.fn(async () => {});
    const prepareMock = vi.fn(async (prep) => makeRun(prep.id, prep.kind ?? "external_terminal", prep.provider as "claude" | "agy" | "codex"));
    const startMock = vi.fn(async (run) => ({ ok: true as const, run }));

    // 1. Linked task thread -> launches board task run + posts handoff message
    const linkedThread = makeThread(
      "task-gp-123",
      "open",
      JSON.stringify({ ask_type: "do", task_id: "gp-123", question: "Fix worktree seat", steps: ["Copy .claude-account"] })
    );
    const r1 = await kickOffAgentFromInboxThread(
      {
        thread: linkedThread,
        ask: { ask_type: "do", task_id: "gp-123", question: "Fix worktree seat", steps: ["Copy .claude-account"] },
        provider: "claude",
        mode: "managed",
        repoPath: "/workspace/gitpulse",
        repositories: [repo],
      },
      {
        invoke: invokeMock as never,
        getTask: async () => task,
        getRepository: async () => repo,
        registerRepository: async () => repo,
        prepareTaskRun: prepareMock,
        startPreparedAttempt: startMock,
        requestTerminalLaunch: terminalRequestMock,
        newID: () => "id-linked",
      }
    );
    expect(r1.launchedVia).toBe("task_run");
    expect(invokeMock).toHaveBeenCalledWith(
      "cmd_worktree_send_message",
      expect.objectContaining({ threadKey: "task-gp-123", toAgent: "claude", kind: "handoff" })
    );

    // 2. Unlinked thread in Terminal mode -> opens pre-seeded terminal tab + posts handoff message
    const unlinkedThread = makeThread("decide-cache-42", "open", "Investigate cache eviction");
    const prompt = buildInboxHandoffPrompt(unlinkedThread, {
      ask_type: "decide",
      question: "LRU or TinyLFU?",
      options: ["LRU", "TinyLFU"],
      recommendation: "TinyLFU",
    });
    expect(prompt).toContain("decide-cache-42");
    expect(prompt).toContain("LRU, TinyLFU");

    const r2 = await kickOffAgentFromInboxThread(
      {
        thread: unlinkedThread,
        ask: null,
        provider: "agy",
        mode: "terminal",
        repoPath: "/workspace/gitpulse",
      },
      {
        invoke: invokeMock as never,
        getTask: async () => {
          throw new WorkbenchError("not_found", "Task not found");
        },
        getRepository: async () => repo,
        registerRepository: async () => repo,
        prepareTaskRun: prepareMock,
        startPreparedAttempt: startMock,
        requestTerminalLaunch: terminalRequestMock,
        newID: () => "id-term",
      }
    );
    expect(r2.launchedVia).toBe("terminal_tab");
    expect(terminalRequestMock).toHaveBeenCalledWith(
      "/workspace/gitpulse",
      "agy",
      expect.stringContaining("decide-cache-42")
    );

    // 3. Unlinked thread in Managed mode -> decomposes/files board task first + launches warm managed run
    const r3 = await kickOffAgentFromInboxThread(
      {
        thread: unlinkedThread,
        ask: { ask_type: "do", steps: ["Step A"] },
        provider: "codex",
        mode: "managed",
        repoPath: "/workspace/gitpulse",
        repositories: [repo],
      },
      {
        invoke: invokeMock as never,
        getTask: async (id) => makeTask(id),
        getRepository: async () => repo,
        registerRepository: async () => repo,
        prepareTaskRun: prepareMock,
        startPreparedAttempt: startMock,
        requestTerminalLaunch: terminalRequestMock,
        newID: () => "id-mgd",
      }
    );
    expect(r3.launchedVia).toBe("task_run");
    expect(invokeMock).toHaveBeenCalledWith(
      "cmd_inbox_decompose_task",
      expect.objectContaining({ repoPath: "/workspace/gitpulse", title: unlinkedThread.title })
    );
  });

  it("creates child subtasks linked to parent, loads child subtasks via lineage, and manages task-linked inbox threads", async () => {
    const parent = makeTask("parent-1");
    const putMock = vi.fn(async (body: Record<string, unknown>) =>
      makeTask(String(body.id), {
        title: String(body.title),
        status: "ready",
        links: [{ kind: "parent", item_id: "parent-1" }],
      })
    );
    const invokeMock = vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "cmd_inbox_read") {
        const tMatch = makeThread(
          "task-parent-1",
          "open",
          JSON.stringify({ ask_type: "do", task_id: "parent-1", steps: ["Sub 1"] })
        );
        const tOther = makeThread("other-thread", "open", "Unrelated");
        return {
          ok: true,
          repo_path: "/workspace/gitpulse",
          threads: [tMatch, tOther],
          lanes: [],
          total: 2,
        } satisfies InboxSnapshot;
      }
      return {
        ok: true,
        thread_key: String(args?.threadKey ?? "task-parent-1"),
        message: {
          seq: 8,
          from_agent: "developer",
          to_agent: String(args?.toAgent ?? "claude"),
          kind: String(args?.kind ?? "ask"),
          body: String(args?.body ?? ""),
          branch: null,
          created_at_ms: 1700000100000,
          thread_key: String(args?.threadKey ?? "task-parent-1"),
        },
      } satisfies InboxMessageReceipt;
    });

    let idSeq = 0;
    const created = await createChildSubtasksForParent(
      {
        parentTask: parent,
        subtaskTitles: ["- Implement API", "2. Write adversarial tests"],
        repoPath: "/workspace/gitpulse",
      },
      {
        putTask: putMock,
        invoke: invokeMock as never,
        newID: () => `sub-${++idSeq}`,
      }
    );

    expect(created).toHaveLength(2);
    expect(putMock).toHaveBeenCalledTimes(2);
    expect(putMock.mock.calls[0][0]).toMatchObject({
      title: "Implement API",
      links: [{ kind: "parent", item_id: "parent-1" }],
    });
    expect(invokeMock).toHaveBeenCalledWith(
      "cmd_inbox_post",
      expect.objectContaining({ threadKey: "task-parent-1" })
    );

    // Load child subtasks via listTasks + getBoardLineage
    const cards: TaskCard[] = [
      parent,
      { ...created[0], id: "sub-1", title: "Implement API" },
      { ...created[1], id: "sub-2", title: "Write adversarial tests" },
      makeTask("unrelated-9"),
    ];
    const children = await loadChildSubtasks("parent-1", "repo-1", {
      listTasks: async () => ({ items: cards, total: 4, shown: 4, has_more: false, next_cursor: null }),
      getBoardLineage: async () =>
        new Map([
          ["parent-1", { parent_item_id: null, children: { total: 2, done: 0 } }],
          ["sub-1", { parent_item_id: "parent-1", children: { total: 0, done: 0 } }],
          ["sub-2", { parent_item_id: "parent-1", children: { total: 0, done: 0 } }],
          ["unrelated-9", { parent_item_id: "other-parent", children: { total: 0, done: 0 } }],
        ]),
    });
    expect(children.map((c) => c.id)).toEqual(["sub-1", "sub-2"]);

    // Linked inbox thread filtering & replies/asks
    const linked = await loadLinkedTaskThreads("/workspace/gitpulse", "parent-1", {
      invoke: invokeMock as never,
    });
    expect(linked.map((t) => t.thread_key)).toEqual(["task-parent-1"]);
    expect(
      matchesTaskThread(
        makeThread("custom-key", "open", JSON.stringify({ ask_type: "answer", task_id: "parent-1" })),
        "parent-1"
      )
    ).toBe(true);

    await replyToTaskThread(
      {
        repoPath: "/workspace/gitpulse",
        threadKey: "task-parent-1",
        toAgent: "claude-worker",
        body: "Go ahead with SQLite",
      },
      { invoke: invokeMock as never }
    );
    expect(invokeMock).toHaveBeenCalledWith(
      "cmd_worktree_send_message",
      expect.objectContaining({ threadKey: "task-parent-1", body: "Go ahead with SQLite" })
    );

    await postTaskAgentAsk(
      {
        repoPath: "/workspace/gitpulse",
        taskId: "parent-1",
        taskTitle: "Task parent-1",
        toAgent: "agy",
        question: "Please review worktree propagation edge cases",
      },
      { invoke: invokeMock as never }
    );
    expect(invokeMock).toHaveBeenCalledWith(
      "cmd_inbox_post",
      expect.objectContaining({ threadKey: "task-parent-1", toAgent: "agy", kind: "ask" })
    );
  });

  it("enforces boundary validations, prompt truncation, and error propagation across collaboration helpers", async () => {
    // 1. Oversized handoff prompt (> 8 KB) truncates cleanly and includes summary
    const hugeThread = makeThread("huge-1", "open", "x".repeat(12_000));
    const truncated = buildInboxHandoffPrompt(hugeThread, {
      ask_type: "review",
      prompt: "Audit large diff",
      summary: "x".repeat(10_000),
    });
    expect(new TextEncoder().encode(truncated).length).toBeLessThanOrEqual(8_000);
    expect(truncated).toContain("Summary:");
    expect(truncated).toContain("[handoff truncated]");

    const plainPrompt = buildInboxHandoffPrompt(makeThread("plain-1", "open", "Plain body"), null);
    expect(plainPrompt).toContain("Latest message: Plain body");

    // 2. Partial error during batchResolveThreadsWithNote
    const batchRes = await batchResolveThreadsWithNote(
      {
        repoPath: "/workspace/gitpulse",
        threads: [makeThread("ok-1", "open"), makeThread("fail-1", "open")],
        selectedKeys: new Set(["ok-1", "fail-1"]),
        note: "Batch note",
      },
      {
        invoke: vi.fn(async (_cmd: string, args?: Record<string, unknown>) => {
          if (args?.threadKey === "fail-1") throw new Error("bus locked");
          return { ok: true, thread_key: "ok-1", message: null };
        }) as never,
      }
    );
    expect(batchRes.resolvedKeys).toEqual(["ok-1"]);
    expect(batchRes.errors).toEqual(["fail-1: bus locked"]);

    // 3. kickOffAgentForTask boundary errors: missing repo link, missing checkout, failed start
    const orphanTask = makeTask("orphan", { primary_repository_id: "", repository_ids: [] });
    await expect(
      kickOffAgentForTask(
        { taskId: "orphan", provider: "claude", mode: "terminal" },
        { getTask: async () => orphanTask }
      )
    ).rejects.toThrow(/Link a repository/);

    const blankRepo = makeRepo("repo-blank", "   ");
    await expect(
      kickOffAgentForTask(
        { taskId: "t-blank", provider: "claude", mode: "terminal" },
        {
          getTask: async () => makeTask("t-blank", { primary_repository_id: "repo-blank" }),
          getRepository: async () => blankRepo,
        }
      )
    ).rejects.toThrow(/Open the task's repository checkout/);

    const repo = makeRepo("repo-1", "/workspace/gitpulse");
    await expect(
      kickOffAgentForTask(
        { taskId: "t-fail", provider: "claude", mode: "managed" },
        {
          getTask: async () => makeTask("t-fail"),
          getRepository: async () => repo,
          prepareTaskRun: async (p) => makeRun(p.id, "managed", "claude"),
          startPreparedAttempt: async (run) => ({ ok: false, run, error: "manvi host busy" }),
          newID: () => "run-fail",
        }
      )
    ).rejects.toThrow("manvi host busy");

    // 4. kickOffAgentFromInboxThread: non-not_found error rethrows; missing repoPath rejects; not_found falls back
    const taskThread = makeThread("task-gp-404", "open", "Body");
    await expect(
      kickOffAgentFromInboxThread(
        {
          thread: taskThread,
          ask: null,
          provider: "claude",
          mode: "terminal",
          repoPath: "/workspace/gitpulse",
        },
        {
          getTask: async () => {
            throw new Error("permission denied");
          },
        }
      )
    ).rejects.toThrow("permission denied");

    await expect(
      kickOffAgentFromInboxThread({
        thread: makeThread("unlinked-no-repo", "open"),
        ask: null,
        provider: "claude",
        mode: "terminal",
        repoPath: "   ",
      })
    ).rejects.toThrow(/Open a repository/);

    const fallbackLaunch = await kickOffAgentFromInboxThread(
      {
        thread: taskThread,
        ask: null,
        provider: "claude",
        mode: "terminal",
        repoPath: "/workspace/gitpulse",
      },
      {
        getTask: async () => {
          throw new WorkbenchError("not_found", "Task not found");
        },
        requestTerminalLaunch: async () => {},
        invoke: vi.fn(async () => ({ ok: true, thread_key: "task-gp-404", message: null })) as never,
      }
    );
    expect(fallbackLaunch.launchedVia).toBe("terminal_tab");

    // 5. Empty input & global-scope edge cases
    await expect(
      createChildSubtasksForParent({ parentTask: makeTask("p-1"), subtaskTitles: ["   ", " -  "] })
    ).rejects.toThrow(/Enter at least one subtask line/);

    const emptyChildren = await loadChildSubtasks("p-1", undefined, {
      listTasks: async () => ({ items: [], total: 0, shown: 0, has_more: false, next_cursor: null }),
    });
    expect(emptyChildren).toEqual([]);

    expect(await loadLinkedTaskThreads("", "p-1")).toEqual([]);
    expect(matchesTaskThread(makeThread("unrelated", "open", "not json"), "p-1")).toBe(false);

    await expect(
      replyToTaskThread({ repoPath: "/workspace/gitpulse", threadKey: "t-1", body: "   " })
    ).rejects.toThrow(/Reply text is required/);

    await expect(
      postTaskAgentAsk({
        repoPath: "/workspace/gitpulse",
        taskId: "p-1",
        taskTitle: "Title",
        toAgent: "",
        question: "   ",
      })
    ).rejects.toThrow(/Ask or handoff prompt is required/);
  });

  it("identifies Graft Mobile senders and parses mobile kickoff & task breakdown directives", () => {
    expect(isMobileOriginated("graft-mobile")).toBe(true);
    expect(isMobileOriginated("  GRAFT-BRIDGE ")).toBe(true);
    expect(isMobileOriginated("claude-worker-1")).toBe(false);
    expect(isMobileOriginated("")).toBe(false);
    expect(isMobileOriginated(null)).toBe(false);

    // 1. JSON mobile kickoff ask emitted by GitPulseAgentBusReader.recordMobileKickoff
    const jsonKickoff = JSON.stringify({
      ask_type: "do",
      kind: "kickoff",
      provider: "agy",
      mode: "terminal",
      session_id: "mobile-agy-123",
      title: "Fix mobile sync",
      prompt: "Run vitest and verify AgentInbox badges",
      question:
        "[KICKOFF provider=agy mode=terminal session=mobile-agy-123 task=gp-77 branch=feat/mobile] Fix mobile sync\nRun vitest and verify AgentInbox badges",
      task_id: "gp-77",
      branch: "feat/mobile",
    });
    expect(parseMobileKickoffDirective(jsonKickoff)).toEqual({
      provider: "agy",
      mode: "terminal",
      sessionId: "mobile-agy-123",
      taskId: "gp-77",
      branch: "feat/mobile",
      title: "Fix mobile sync",
      prompt: "Run vitest and verify AgentInbox badges",
    });

    // 2. Plain bracket kickoff directive
    const bracketKickoff =
      "[KICKOFF provider=codex mode=managed session=mobile-codex-9] Audit replay guard\nCheck 600s freshness window";
    expect(parseMobileKickoffDirective(bracketKickoff)).toEqual({
      provider: "codex",
      mode: "managed",
      sessionId: "mobile-codex-9",
      taskId: undefined,
      branch: undefined,
      title: "Audit replay guard",
      prompt: "Check 600s freshness window",
    });

    // 3. Adversarial invalid kickoff inputs return null
    expect(parseMobileKickoffDirective("")).toBeNull();
    expect(parseMobileKickoffDirective("[KICKOFF provider=unknown mode=managed] Title\nPrompt")).toBeNull();
    expect(parseMobileKickoffDirective("[KICKOFF provider=claude mode=background] Title\nPrompt")).toBeNull();
    expect(parseMobileKickoffDirective("Just a normal message")).toBeNull();

    // 4. JSON task breakdown emitted by GitPulseAgentBusReader.postTaskDecomposition
    const jsonBreakdown = JSON.stringify({
      ask_type: "do",
      kind: "decompose",
      title: "Ship Round 3 Mobile Control",
      question: "[TASK BREAKDOWN] Ship Round 3 Mobile Control\n1. Add ControlAgent.swift\n2. Build Android view",
      steps: ["Add ControlAgent.swift", "Build Android view"],
      assign_agent: "claude",
      task_id: "gp-300",
      branch: "feat/round3",
    });
    expect(parseMobileTaskBreakdown(jsonBreakdown)).toEqual({
      title: "Ship Round 3 Mobile Control",
      steps: ["Add ControlAgent.swift", "Build Android view"],
      assignAgent: "claude",
      taskId: "gp-300",
      branch: "feat/round3",
    });

    // 5. subtasks[].assignee fallback when top-level assign_agent is omitted
    const subtasksOnlyBreakdown = JSON.stringify({
      ask_type: "do",
      title: "Decompose via subtasks array",
      question: "[TASK BREAKDOWN] Decompose via subtasks array\n1. Step A\n2. Step B",
      steps: ["Step A", "Step B"],
      subtasks: [
        { key: "SUB-1", title: "Step A", assignee: "agy", status: "todo" },
        { key: "SUB-2", title: "Step B", assignee: "agy", status: "todo" },
      ],
      task_id: "gp-301",
    });
    expect(parseMobileTaskBreakdown(subtasksOnlyBreakdown)).toEqual({
      title: "Decompose via subtasks array",
      steps: ["Step A", "Step B"],
      assignAgent: "agy",
      taskId: "gp-301",
      branch: undefined,
    });

    // 6. Plain text [TASK BREAKDOWN] directive
    const textBreakdown = "[TASK BREAKDOWN] Refactor auth\n1. Extract token parser\n2. Add unit tests";
    expect(parseMobileTaskBreakdown(textBreakdown)).toEqual({
      title: "Refactor auth",
      steps: ["Extract token parser", "Add unit tests"],
    });

    // 7. Adversarial invalid breakdown inputs return null
    expect(parseMobileTaskBreakdown("[TASK BREAKDOWN] Title without steps")).toBeNull();
    expect(parseMobileTaskBreakdown(JSON.stringify({ kind: "decompose", title: "Empty", steps: [] }))).toBeNull();
  });
});

