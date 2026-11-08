import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import type { AgentInboxThread } from "../agents/inboxTypes";
import AgentInbox from "./AgentInbox.svelte";

describe("AgentInbox Adversarial & Stress Suite", () => {
  it("handles malformed JSON and non-JSON message bodies gracefully without throwing", () => {
    const malformedThreads: AgentInboxThread[] = [
      {
        thread_key: "thread-corrupt-json",
        title: "Corrupt JSON Ask",
        status: "open",
        ask_type: "decide",
        from_agent: "agent-broken",
        to_agent: "developer",
        updated_at_ms: Date.now(),
        latest_msg: {
          seq: 1,
          from_agent: "agent-broken",
          to_agent: "developer",
          kind: "ask",
          body: "{\"ask_type\": \"decide\", broken json...",
          branch: "main",
          created_at_ms: Date.now(),
          thread_key: "thread-corrupt-json",
        },
        messages: [
          {
            seq: 1,
            from_agent: "agent-broken",
            to_agent: "developer",
            kind: "ask",
            body: "{\"ask_type\": \"decide\", broken json...",
            branch: "main",
            created_at_ms: Date.now(),
            thread_key: "thread-corrupt-json",
          },
        ],
      },
      {
        thread_key: "thread-raw-text",
        title: "Raw Plain Text Note",
        status: "open",
        ask_type: null,
        from_agent: "agent-raw",
        to_agent: null,
        updated_at_ms: Date.now(),
        latest_msg: {
          seq: 2,
          from_agent: "agent-raw",
          to_agent: null,
          kind: "note",
          body: "Just plain text without any JSON formatting whatsoever.",
          branch: null,
          created_at_ms: Date.now(),
          thread_key: "thread-raw-text",
        },
        messages: [
          {
            seq: 2,
            from_agent: "agent-raw",
            to_agent: null,
            kind: "note",
            body: "Just plain text without any JSON formatting whatsoever.",
            branch: null,
            created_at_ms: Date.now(),
            thread_key: "thread-raw-text",
          },
        ],
      },
    ];

    expect(() => {
      const html = render(AgentInbox, { props: { initialThreads: malformedThreads } }).body;
      expect(html).toContain("Corrupt JSON Ask");
      expect(html).toContain("Raw Plain Text Note");
      // Thread 1 is selected by default; its raw body snippet is displayed safely
      expect(html).toContain("broken json");
    }).not.toThrow();
  });

  it("handles oversized 16 KiB message bodies without UI crash or truncation errors", () => {
    const hugeBodyText = "x".repeat(16 * 1024);
    const oversizedThread: AgentInboxThread[] = [
      {
        thread_key: "thread-huge-body",
        title: "16 KiB Payload Thread",
        status: "open",
        ask_type: null,
        from_agent: "agent-heavy",
        to_agent: "developer",
        updated_at_ms: Date.now(),
        latest_msg: {
          seq: 10,
          from_agent: "agent-heavy",
          to_agent: "developer",
          kind: "note",
          body: hugeBodyText,
          branch: "main",
          created_at_ms: Date.now(),
          thread_key: "thread-huge-body",
        },
        messages: [
          {
            seq: 10,
            from_agent: "agent-heavy",
            to_agent: "developer",
            kind: "note",
            body: hugeBodyText,
            branch: "main",
            created_at_ms: Date.now(),
            thread_key: "thread-huge-body",
          },
        ],
      },
    ];

    expect(() => {
      const html = render(AgentInbox, { props: { initialThreads: oversizedThread } }).body;
      expect(html).toContain("16 KiB Payload Thread");
      expect(html).toContain("agent-heavy");
    }).not.toThrow();
  });

  it("handles special characters, unicode, and emojis in thread keys and titles", () => {
    const complexThreads: AgentInboxThread[] = [
      {
        thread_key: "feat/ui#issue-102:branch/sub-task.v1@beta [urgent]",
        title: "Complex Slug & Title: <script>alert(1)</script>",
        status: "open",
        ask_type: "answer",
        from_agent: "agent-sec",
        to_agent: "developer",
        updated_at_ms: Date.now(),
        latest_msg: {
          seq: 20,
          from_agent: "agent-sec",
          to_agent: "developer",
          kind: "ask",
          body: JSON.stringify({
            ask_type: "answer",
            question: "Is XSS sanitized?",
          }),
          branch: "feat/sec",
          created_at_ms: Date.now(),
          thread_key: "feat/ui#issue-102:branch/sub-task.v1@beta [urgent]",
        },
        messages: [
          {
            seq: 20,
            from_agent: "agent-sec",
            to_agent: "developer",
            kind: "ask",
            body: JSON.stringify({
              ask_type: "answer",
              question: "Is XSS sanitized?",
            }),
            branch: "feat/sec",
            created_at_ms: Date.now(),
            thread_key: "feat/ui#issue-102:branch/sub-task.v1@beta [urgent]",
          },
        ],
      },
      {
        thread_key: "🚀-launch-task-🔥",
        title: "Unicode 🚀 Rocket Mission",
        status: "answered",
        ask_type: "do",
        from_agent: "agent-rocket",
        to_agent: null,
        updated_at_ms: Date.now(),
        latest_msg: {
          seq: 21,
          from_agent: "agent-rocket",
          to_agent: null,
          kind: "reply",
          body: "Launch verified",
          branch: null,
          created_at_ms: Date.now(),
          thread_key: "🚀-launch-task-🔥",
        },
        messages: [
          {
            seq: 21,
            from_agent: "agent-rocket",
            to_agent: null,
            kind: "reply",
            body: "Launch verified",
            branch: null,
            created_at_ms: Date.now(),
            thread_key: "🚀-launch-task-🔥",
          },
        ],
      },
    ];

    const html = render(AgentInbox, { props: { initialThreads: complexThreads } }).body;
    expect(html).toContain("Complex Slug &amp; Title");
    expect(html).toContain("&lt;script>alert(1)&lt;/script>");
    expect(html).toContain("Unicode 🚀 Rocket Mission");
    expect(html).toContain("🚀-launch-task-🔥");
  });

  it("renders all four structured ask types (decide, do, answer, review)", () => {
    const askTypesThreads: AgentInboxThread[] = [
      {
        thread_key: "t-decide",
        title: "Decision Ask",
        status: "open",
        ask_type: "decide",
        from_agent: "agent-1",
        to_agent: "dev",
        updated_at_ms: Date.now() - 1000,
        latest_msg: {
          seq: 1,
          from_agent: "agent-1",
          to_agent: "dev",
          kind: "ask",
          body: JSON.stringify({
            ask_type: "decide",
            question: "Choose algorithm",
            options: ["A*", "Dijkstra"],
            recommendation: "A*",
          }),
          branch: "main",
          created_at_ms: Date.now() - 1000,
          thread_key: "t-decide",
        },
        messages: [],
      },
      {
        thread_key: "t-do",
        title: "Checklist Ask",
        status: "open",
        ask_type: "do",
        from_agent: "agent-2",
        to_agent: "dev",
        updated_at_ms: Date.now() - 2000,
        latest_msg: {
          seq: 2,
          from_agent: "agent-2",
          to_agent: "dev",
          kind: "ask",
          body: JSON.stringify({
            ask_type: "do",
            question: "Perform database migration",
            steps: ["Backup data", "Run migrate script", "Verify row counts"],
          }),
          branch: "main",
          created_at_ms: Date.now() - 2000,
          thread_key: "t-do",
        },
        messages: [],
      },
      {
        thread_key: "t-answer",
        title: "Question Ask",
        status: "open",
        ask_type: "answer",
        from_agent: "agent-3",
        to_agent: "dev",
        updated_at_ms: Date.now() - 3000,
        latest_msg: {
          seq: 3,
          from_agent: "agent-3",
          to_agent: "dev",
          kind: "ask",
          body: JSON.stringify({
            ask_type: "answer",
            question: "What is the desired timeout?",
            prompt: "Please specify timeout in seconds",
          }),
          branch: "main",
          created_at_ms: Date.now() - 3000,
          thread_key: "t-answer",
        },
        messages: [],
      },
      {
        thread_key: "t-review",
        title: "Code Review Ask",
        status: "open",
        ask_type: "review",
        from_agent: "agent-4",
        to_agent: "dev",
        updated_at_ms: Date.now() - 4000,
        latest_msg: {
          seq: 4,
          from_agent: "agent-4",
          to_agent: "dev",
          kind: "ask",
          body: JSON.stringify({
            ask_type: "review",
            question: "Review PR #45",
            branch: "feat/inbox",
            summary: "Added agent bus ring buffer and ask cards",
          }),
          branch: "feat/inbox",
          created_at_ms: Date.now() - 4000,
          thread_key: "t-review",
        },
        messages: [],
      },
    ];

    const html = render(AgentInbox, { props: { initialThreads: askTypesThreads } }).body;
    expect(html).toContain("Decision Ask");
    expect(html).toContain("Checklist Ask");
    expect(html).toContain("Question Ask");
    expect(html).toContain("Code Review Ask");
    expect(html).toContain("4 open asks");
  });

  it("correctly identifies withdrawn asks and renders withdrawn status badge", () => {
    const withdrawnThreads: AgentInboxThread[] = [
      {
        thread_key: "t-withdrawn",
        title: "Withdrawn Request",
        status: "withdrawn",
        ask_type: "decide",
        from_agent: "agent-1",
        to_agent: "dev",
        updated_at_ms: Date.now(),
        latest_msg: {
          seq: 5,
          from_agent: "agent-1",
          to_agent: "dev",
          kind: "withdraw",
          body: "Ask withdrawn: Obsoleted by architecture update",
          branch: null,
          created_at_ms: Date.now(),
          thread_key: "t-withdrawn",
        },
        messages: [],
      },
    ];

    const html = render(AgentInbox, { props: { initialThreads: withdrawnThreads } }).body;
    expect(html).toContain("Withdrawn Request");
    expect(html).toContain("withdrawn");
  });
});
