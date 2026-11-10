import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import type { AgentInboxThread } from "../agents/inboxTypes";
import AgentInbox from "./AgentInbox.svelte";

const mockThreads: AgentInboxThread[] = [
  {
    thread_key: "thread-decide-1",
    title: "Database Architecture Selection",
    status: "open",
    ask_type: "decide",
    from_agent: "claude-worker-1",
    to_agent: "developer",
    updated_at_ms: Date.now() - 60000,
    latest_msg: {
      seq: 1,
      from_agent: "claude-worker-1",
      to_agent: "developer",
      kind: "ask",
      body: JSON.stringify({
        ask_type: "decide",
        question: "Should we use SQLite or JSON Lines for agent coordination bus?",
        options: ["SQLite", "JSON Lines", "PostgreSQL"],
        recommendation: "SQLite",
      }),
      branch: "feat/agent-bus",
      created_at_ms: Date.now() - 60000,
      thread_key: "thread-decide-1",
    },
    messages: [
      {
        seq: 1,
        from_agent: "claude-worker-1",
        to_agent: "developer",
        kind: "ask",
        body: JSON.stringify({
          ask_type: "decide",
          question: "Should we use SQLite or JSON Lines for agent coordination bus?",
          options: ["SQLite", "JSON Lines", "PostgreSQL"],
          recommendation: "SQLite",
        }),
        branch: "feat/agent-bus",
        created_at_ms: Date.now() - 60000,
        thread_key: "thread-decide-1",
      },
    ],
  },
  {
    thread_key: "thread-do-2",
    title: "Execute Manual Database Seed",
    status: "open",
    ask_type: "do",
    from_agent: "claude-worker-2",
    to_agent: null,
    updated_at_ms: Date.now() - 120000,
    latest_msg: {
      seq: 2,
      from_agent: "claude-worker-2",
      to_agent: null,
      kind: "ask",
      body: JSON.stringify({
        ask_type: "do",
        question: "Perform verification checklist before release",
        steps: ["Verify schema migrations", "Check test pass rate", "Confirm secret redaction"],
      }),
      branch: "main",
      created_at_ms: Date.now() - 120000,
      thread_key: "thread-do-2",
    },
    messages: [
      {
        seq: 2,
        from_agent: "claude-worker-2",
        to_agent: null,
        kind: "ask",
        body: JSON.stringify({
          ask_type: "do",
          question: "Perform verification checklist before release",
          steps: ["Verify schema migrations", "Check test pass rate", "Confirm secret redaction"],
        }),
        branch: "main",
        created_at_ms: Date.now() - 120000,
        thread_key: "thread-do-2",
      },
    ],
  },
  {
    thread_key: "thread-answered-3",
    title: "Auth Route Configuration",
    status: "answered",
    ask_type: "answer",
    from_agent: "claude-worker-3",
    to_agent: "developer",
    updated_at_ms: Date.now() - 3600000,
    latest_msg: {
      seq: 4,
      from_agent: "developer",
      to_agent: "claude-worker-3",
      kind: "reply",
      body: "Use Bearer token in Authorization header",
      branch: null,
      created_at_ms: Date.now() - 3600000,
      thread_key: "thread-answered-3",
    },
    messages: [
      {
        seq: 3,
        from_agent: "claude-worker-3",
        to_agent: "developer",
        kind: "ask",
        body: JSON.stringify({
          ask_type: "answer",
          question: "Which authentication header format should the client send?",
        }),
        branch: "feat/auth",
        created_at_ms: Date.now() - 7200000,
        thread_key: "thread-answered-3",
      },
      {
        seq: 4,
        from_agent: "developer",
        to_agent: "claude-worker-3",
        kind: "reply",
        body: "Use Bearer token in Authorization header",
        branch: null,
        created_at_ms: Date.now() - 3600000,
        thread_key: "thread-answered-3",
      },
    ],
  },
];

describe("AgentInbox", () => {
  it("renders the inbox root element with data-testid", () => {
    const html = render(AgentInbox, { props: { initialThreads: [] } }).body;
    expect(html).toContain('data-testid="agent-inbox"');
    expect(html).toContain("Agent inbox");
    expect(html).toContain('placeholder="Search threads…"');
  });

  it("displays empty state when no threads are present", () => {
    const html = render(AgentInbox, { props: { initialThreads: [] } }).body;
    expect(html).toContain('data-testid="empty-threads"');
    expect(html).toContain("No threads found");
  });

  it("renders thread items with title, badges, agent chips, and timestamps", () => {
    const html = render(AgentInbox, { props: { initialThreads: mockThreads } }).body;
    expect(html).toContain("Database Architecture Selection");
    expect(html).toContain("Execute Manual Database Seed");
    expect(html).toContain("Auth Route Configuration");
    expect(html).toContain("thread-decide-1");
    expect(html).toContain("thread-do-2");
    expect(html).toContain("claude-worker-1");
    expect(html).toContain("claude-worker-2");
  });

  it("displays open asks badge when unresolved asks exist", () => {
    const html = render(AgentInbox, { props: { initialThreads: mockThreads } }).body;
    expect(html).toContain('data-testid="open-asks-badge"');
    expect(html).toContain("2 open asks");
  });

  it("renders structured decide ask widget with option choices and recommendation", () => {
    const html = render(AgentInbox, { props: { initialThreads: mockThreads } }).body;
    expect(html).toContain('data-testid="structured-ask-box"');
    expect(html).toContain("Structured decision request");
    expect(html).toContain("Should we use SQLite or JSON Lines for agent coordination bus?");
    expect(html).toContain("SQLite");
    expect(html).toContain("JSON Lines");
    expect(html).toContain("Recommended");
    expect(html).toContain('data-testid="confirm-decision-btn"');
  });

  it("renders thread status pills and withdraw trigger button", () => {
    const html = render(AgentInbox, { props: { initialThreads: mockThreads } }).body;
    expect(html).toContain("open");
    expect(html).toContain("answered");
    expect(html).toContain("Withdraw ask");
    expect(html).toContain('data-testid="new-ask-btn"');
    expect(html).toContain('data-testid="decompose-task-btn"');
  });

  it("renders active lanes strip and cross-schema Graft structured ask fields", () => {
    const graftThreads: AgentInboxThread[] = [
      {
        thread_key: "graft-decide-idx",
        title: "Graft Bridge Decision",
        status: "open",
        ask_type: "decide",
        from_agent: "graft-bridge",
        to_agent: "agy-worker-1",
        updated_at_ms: Date.now(),
        latest_msg: {
          seq: 10,
          from_agent: "graft-bridge",
          to_agent: "agy-worker-1",
          kind: "ask",
          body: JSON.stringify({
            ask_type: "decide",
            title: "Select IPC transport from Graft Bridge",
            summary: "Posted from macOS Graft companion form",
            options: ["Unix Socket", "Shared JSON Ring"],
            default_option: 1,
          }),
          branch: "feat/graft-bridge",
          created_at_ms: Date.now(),
          thread_key: "graft-decide-idx",
        },
        messages: [],
      },
    ];
    const html = render(AgentInbox, {
      props: {
        initialThreads: graftThreads,
        initialLanes: [
          {
            agent_id: "agy-worker-1",
            branch: "feat/graft-bridge",
            worktree_path: null,
            sparse_cone: [],
            claimed_files: ["macos/Sources/GraftAppCore/Views/GitPulseBridgeView.swift"],
            claimed_symbols: [],
            updated_at_ms: Date.now(),
          },
        ],
      },
    }).body;
    expect(html).toContain('data-testid="active-lanes-strip"');
    expect(html).toContain("Active worktrees (1):");
    expect(html).toContain("agy-worker-1");
    expect(html).toContain("feat/graft-bridge");
    expect(html).toContain("Select IPC transport from Graft Bridge");
    expect(html).toContain("Posted from macOS Graft companion form");
    expect(html).toContain("Recommended");
  });

  it("renders Linear-style subtask rollup progress on open checklist asks", () => {
    const doOnlyThreads: AgentInboxThread[] = [mockThreads[1]];
    const html = render(AgentInbox, { props: { initialThreads: doOnlyThreads } }).body;
    expect(html).toContain('data-testid="subtask-rollup"');
    expect(html).toContain("0 of 3 subtasks done (0%)");
    expect(html).toContain("Verify schema migrations");
  });

  it("keeps structured ask widget mounted when an open thread receives a follow-up progress note", () => {
    const now = Date.now();
    const askWithFollowUpNote: AgentInboxThread[] = [
      {
        thread_key: "thread-open-with-note",
        title: "Approve Schema Cutover",
        status: "open",
        ask_type: "decide",
        from_agent: "agy-worker-1",
        to_agent: "developer",
        updated_at_ms: now,
        latest_msg: {
          seq: 2,
          from_agent: "agy-worker-1",
          to_agent: "developer",
          kind: "note",
          body: "Still waiting on approval; staging dry-run succeeded.",
          branch: "feat/cutover",
          created_at_ms: now,
          thread_key: "thread-open-with-note",
        },
        messages: [
          {
            seq: 1,
            from_agent: "agy-worker-1",
            to_agent: "developer",
            kind: "ask",
            body: JSON.stringify({
              ask_type: "decide",
              question: "Proceed with zero-downtime cutover?",
              options: ["Proceed now", "Defer to maintenance window"],
              recommendation: "Proceed now",
            }),
            branch: "feat/cutover",
            created_at_ms: now - 5000,
            thread_key: "thread-open-with-note",
          },
          {
            seq: 2,
            from_agent: "agy-worker-1",
            to_agent: "developer",
            kind: "note",
            body: "Still waiting on approval; staging dry-run succeeded.",
            branch: "feat/cutover",
            created_at_ms: now,
            thread_key: "thread-open-with-note",
          },
        ],
      },
    ];

    const html = render(AgentInbox, { props: { initialThreads: askWithFollowUpNote } }).body;
    expect(html).toContain('data-testid="structured-ask-box"');
    expect(html).toContain("Proceed with zero-downtime cutover?");
    expect(html).toContain("Proceed now");
    expect(html).toContain("Still waiting on approval; staging dry-run succeeded.");
  });

  it("renders Open Task when currentAsk.task_id and onopenTask are provided", () => {
    const taskLinked: AgentInboxThread[] = [
      {
        ...mockThreads[0],
        thread_key: "task-gp-42",
        latest_msg: {
          ...mockThreads[0].latest_msg,
          body: JSON.stringify({
            ask_type: "do",
            task_id: "gp-42",
            question: "Implement batch triage",
            steps: ["Step 1"],
          }),
        },
        messages: [
          {
            ...mockThreads[0].messages[0],
            body: JSON.stringify({
              ask_type: "do",
              task_id: "gp-42",
              question: "Implement batch triage",
              steps: ["Step 1"],
            }),
          },
        ],
      },
    ];
    const withHandler = render(AgentInbox, {
      props: { initialThreads: taskLinked, onopenTask: async () => {} },
    }).body;
    expect(withHandler).toContain("Open task");

    const withoutHandler = render(AgentInbox, {
      props: { initialThreads: taskLinked },
    }).body;
    expect(withoutHandler).not.toContain("Open task");
  });

  it("renders Vespin-inspired thread selection checkboxes, select-visible action, and Kick Off Agent bar", () => {
    const html = render(AgentInbox, {
      props: {
        initialThreads: mockThreads,
        initialSelectedKeys: ["thread-decide-1", "thread-do-2"],
      },
    }).body;
    expect(html).toContain('data-testid="thread-select-thread-decide-1"');
    expect(html).toContain('data-testid="inbox-batch-toolbar"');
    expect(html).toContain('data-testid="batch-select-visible-btn"');
    expect(html).toContain('data-testid="batch-clear-btn"');
    expect(html).toContain('data-testid="batch-withdraw-btn"');
    expect(html).toContain('data-testid="batch-resolve-btn"');
    expect(html).toContain('data-testid="kickoff-agent-bar"');
    expect(html).toContain('data-testid="kickoff-agent-btn"');
    expect(html).toContain("Kick off agent");
    expect(html).toContain("Claude");
    expect(html).toContain("Antigravity");
    expect(html).toContain("Codex");
  });

  it("renders Phone · Graft Mobile badge and 1-click Run session / Apply subtasks buttons for mobile threads", () => {
    const mobileKickoffThread: AgentInboxThread = {
      thread_key: "kickoff-claude-1",
      title: "Run Claude Managed Session",
      status: "open",
      ask_type: "do",
      from_agent: "graft-mobile",
      to_agent: "claude",
      updated_at_ms: Date.now(),
      latest_msg: {
        seq: 10,
        from_agent: "graft-mobile",
        to_agent: "claude",
        kind: "ask",
        body: JSON.stringify({
          ask_type: "do",
          kind: "kickoff",
          provider: "claude",
          mode: "managed",
          title: "Run Claude Managed Session",
          prompt: "Audit mobile bridge state",
          question:
            "[KICKOFF provider=claude mode=managed session=mobile-claude-1] Run Claude Managed Session\nAudit mobile bridge state",
        }),
        branch: "feat/mobile",
        created_at_ms: Date.now(),
        thread_key: "kickoff-claude-1",
      },
      messages: [
        {
          seq: 10,
          from_agent: "graft-mobile",
          to_agent: "claude",
          kind: "ask",
          body: JSON.stringify({
            ask_type: "do",
            kind: "kickoff",
            provider: "claude",
            mode: "managed",
            title: "Run Claude Managed Session",
            prompt: "Audit mobile bridge state",
            question:
              "[KICKOFF provider=claude mode=managed session=mobile-claude-1] Run Claude Managed Session\nAudit mobile bridge state",
          }),
          branch: "feat/mobile",
          created_at_ms: Date.now(),
          thread_key: "kickoff-claude-1",
        },
      ],
    };

    const kickoffHtml = render(AgentInbox, {
      props: { initialThreads: [mobileKickoffThread] },
    }).body;
    expect(kickoffHtml).toContain("Phone · Graft Mobile");
    expect(kickoffHtml).toContain('data-testid="mobile-origin-badge"');
    expect(kickoffHtml).toContain('data-testid="mobile-launch-session-btn"');
    expect(kickoffHtml).toContain("Run session");

    const mobileBreakdownThread: AgentInboxThread = {
      ...mobileKickoffThread,
      thread_key: "decompose-mobile-1",
      title: "Decompose Mobile UI",
      latest_msg: {
        ...mobileKickoffThread.latest_msg,
        thread_key: "decompose-mobile-1",
        body: JSON.stringify({
          ask_type: "do",
          kind: "decompose",
          title: "Decompose Mobile UI",
          question: "[TASK BREAKDOWN] Decompose Mobile UI\n1. Wire protocol\n2. Android view",
          steps: ["Wire protocol", "Android view"],
        }),
      },
      messages: [
        {
          ...mobileKickoffThread.messages[0],
          thread_key: "decompose-mobile-1",
          body: JSON.stringify({
            ask_type: "do",
            kind: "decompose",
            title: "Decompose Mobile UI",
            question: "[TASK BREAKDOWN] Decompose Mobile UI\n1. Wire protocol\n2. Android view",
            steps: ["Wire protocol", "Android view"],
          }),
        },
      ],
    };

    const breakdownHtml = render(AgentInbox, {
      props: { initialThreads: [mobileBreakdownThread] },
    }).body;
    expect(breakdownHtml).toContain("Phone · Graft Mobile");
    expect(breakdownHtml).toContain('data-testid="mobile-apply-subtasks-btn"');
    expect(breakdownHtml).toContain("Apply subtasks");

    const agentThreadWithMobileReply: AgentInboxThread = {
      ...mockThreads[0],
      status: "answered",
      latest_msg: {
        seq: 11,
        from_agent: "graft-mobile",
        to_agent: "claude-worker-1",
        kind: "reply",
        body: "Selected option: Option A",
        branch: "feat/auth",
        created_at_ms: Date.now(),
        thread_key: mockThreads[0].thread_key,
      },
      messages: [
        ...mockThreads[0].messages,
        {
          seq: 11,
          from_agent: "graft-mobile",
          to_agent: "claude-worker-1",
          kind: "reply",
          body: "Selected option: Option A",
          branch: "feat/auth",
          created_at_ms: Date.now(),
          thread_key: mockThreads[0].thread_key,
        },
      ],
    };
    const replyHtml = render(AgentInbox, {
      props: { initialThreads: [agentThreadWithMobileReply] },
    }).body;
    expect(replyHtml).toContain('data-testid="timeline-mobile-origin-badge"');
    expect(replyHtml).toContain("Selected option: Option A");
  });
});

