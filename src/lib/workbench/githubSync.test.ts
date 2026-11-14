import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "../ipc/invoke";
import { WorkbenchError } from "./client";
import {
  acceptGitHubProposal,
  deliverOutboxEntry,
  dismissGitHubProposal,
  enqueueOutboxWrite,
  pollGitHubIssue,
  previewOutboxWrite,
} from "./githubSync";
import { checkTasksReadiness } from "./readiness";

vi.mock("../ipc/invoke", () => ({ invoke: vi.fn() }));
const native = vi.mocked(invoke);

beforeEach(() => native.mockReset());

describe("readiness", () => {
  it("chunks batches above 100 and fails closed on missing or truncated entries", async () => {
    const ids = Array.from({ length: 105 }, (_, i) => `t-${i}`);
    native
      .mockResolvedValueOnce(
        JSON.stringify({
          ok: true,
          items: ids.slice(0, 99).map((id) => ({
            id,
            ready: true,
            ready_with_subtasks: true,
            readiness_reason: "ready",
            blocked_by: [],
            blocking: [],
            open_subtasks: [],
            open_subtask_count: 0,
            total_subtask_count: 0,
          })),
          truncated: true,
        }),
      )
      .mockResolvedValueOnce(
        JSON.stringify({
          ok: true,
          items: ids.slice(100).map((id) => ({
            id,
            ready: false,
            ready_with_subtasks: false,
            readiness_reason: "blocked_by_tasks",
            blocked_by: [{ id: "t-0", title: "Root blocker", status: "ready" }],
            blocking: [],
            open_subtasks: [],
            open_subtask_count: 0,
            total_subtask_count: 0,
          })),
          truncated: false,
        }),
      );

    const map = await checkTasksReadiness(ids);
    expect(native).toHaveBeenCalledTimes(2);
    expect(map.get("t-0")).toMatchObject({ ok: true, readiness: { ready: true } });
    expect(map.get("t-99")).toEqual({ ok: false, error: "readiness batch response was truncated" });
    expect(map.get("t-104")).toMatchObject({
      ok: true,
      readiness: { ready: false, blocked_by: [{ id: "t-0", title: "Root blocker", status: "ready" }] },
    });
  });
});

describe("githubSync & outbox", () => {
  it("polls inbound changes and accepts or dismisses proposals", async () => {
    const proposal = {
      id: "sync-1",
      revision: 2,
      task_id: "task-1",
      repo_slug: "acme/widgets",
      issue_number: 42,
      issue_url: "https://github.com/acme/widgets/issues/42",
      outbound_opt_in: false,
      last_seen: { title: "Local", body: "Local body", state: "open", labels: [] },
      pending_proposal: {
        observed_at: 1700000010,
        remote: { title: "Remote", body: "Updated", state: "open", labels: ["bug"] },
        remote_digest: "deadbeef",
        changed_fields: ["title", "body", "labels"],
        locked_skipped: ["title"],
        applicable_fields: ["body", "labels"],
        conflicting_fields: [],
        conflicts: {},
      },
      last_dismissed_digest: null,
      created_at: 1700000000,
      updated_at: 1700000010,
    };
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, item: proposal }));
    const polled = await pollGitHubIssue({
      id: "sync-1",
      repo_path: "/repos/widgets",
      request_id: "poll-1",
    });
    expect(polled.pending_proposal?.locked_skipped).toEqual(["title"]);

    native.mockResolvedValueOnce(
      JSON.stringify({ ok: true, item: { ...proposal, revision: 3, pending_proposal: null } }),
    );
    const accepted = await acceptGitHubProposal({
      id: "sync-1",
      request_id: "acc-1",
      expected_revision: 2,
      fields: ["body", "labels"],
    });
    expect(accepted.revision).toBe(3);

    native.mockResolvedValueOnce(
      JSON.stringify({ ok: true, item: { ...proposal, revision: 4, pending_proposal: null, last_dismissed_digest: "deadbeef" } }),
    );
    const dismissed = await dismissGitHubProposal({
      id: "sync-1",
      request_id: "dis-1",
      expected_revision: 3,
    });
    expect(dismissed.last_dismissed_digest).toBe("deadbeef");
  });

  it("refuses un-opted-in or unconfirmed outbox writes client-side before sending", async () => {
    await expect(
      enqueueOutboxWrite({
        task_id: "task-1",
        op: "create_issue",
        repo_slug: "acme/widgets",
        outbound_opt_in: false,
        confirmed: true,
        confirm_payload_digest: "abc123",
        idempotency_key: "task1-create-1",
      }),
    ).rejects.toMatchObject<Partial<WorkbenchError>>({ code: "outbound_disabled" });

    await expect(
      enqueueOutboxWrite({
        task_id: "task-1",
        op: "create_issue",
        repo_slug: "acme/widgets",
        outbound_opt_in: true,
        confirmed: false,
        confirm_payload_digest: "abc123",
        idempotency_key: "task1-create-1",
      }),
    ).rejects.toMatchObject<Partial<WorkbenchError>>({ code: "confirmation_required" });

    expect(native).not.toHaveBeenCalled();
  });

  it("previews, confirms, enqueues, and delivers through outbox.deliver", async () => {
    native.mockResolvedValueOnce(
      JSON.stringify({
        ok: true,
        preview: {
          sync_id: null,
          op: "create_issue",
          repo_slug: "acme/widgets",
          issue_number: null,
          outbound_opt_in: true,
          can_enqueue: true,
          payload: {
            op: "create_issue",
            task_id: "task-1",
            repo_slug: "acme/widgets",
            issue_number: null,
            fields: ["title", "body", "state", "labels"],
            title: "Ship queue",
            body: "Details",
            state: "open",
            labels: ["queue"],
          },
          payload_digest: "sha256digest",
          conflicts: {},
        },
      }),
    );
    const preview = await previewOutboxWrite({
      task_id: "task-1",
      op: "create_issue",
      repo_slug: "acme/widgets",
    });
    expect(preview.payload_digest).toBe("sha256digest");

    const enqueuedEntry = {
      id: "ob-1",
      revision: 1,
      sync_id: null,
      task_id: "task-1",
      op: "create_issue",
      repo_slug: "acme/widgets",
      issue_number: null,
      state: "pending",
      confirmed: true,
      outbound_opt_in: true,
      idempotency_key: "task1-create-1",
      payload_digest: "sha256digest",
      payload: preview.payload,
      attempts: 0,
      max_attempts: 5,
      next_attempt_at: 1700000000,
      last_preflight_at: null,
      delivered_at: null,
      last_error: null,
      conflict: null,
      created_at: 1700000000,
      updated_at: 1700000000,
    };
    native.mockResolvedValueOnce(JSON.stringify({ ok: true, item: enqueuedEntry }));
    const enqueued = await enqueueOutboxWrite({
      id: "ob-1",
      request_id: "enq-1",
      task_id: "task-1",
      op: "create_issue",
      repo_slug: "acme/widgets",
      outbound_opt_in: true,
      confirmed: true,
      confirm_payload_digest: preview.payload_digest,
      idempotency_key: "task1-create-1",
    });
    expect(enqueued.state).toBe("pending");

    native.mockResolvedValueOnce(
      JSON.stringify({
        ok: true,
        item: { ...enqueuedEntry, revision: 3, state: "delivered", issue_number: 77 },
      }),
    );
    const delivered = await deliverOutboxEntry({
      id: "ob-1",
      repo_path: "/repos/widgets",
      request_id: "del-1",
    });
    expect(delivered.state).toBe("delivered");
    expect(delivered.issue_number).toBe(77);
  });
});
