import { describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({
  invoke: vi.fn().mockResolvedValue({}),
}));

vi.mock("../ipc/invoke", () => ({ invoke }));

import {
  diffTurnCheckpoint,
  filterTurnCheckpoints,
  formatOmissionSummary,
  formatRelativeTurnTime,
  listTurnCheckpoints,
  totalOmittedFiles,
  type TurnCheckpointOmissions,
  type TurnCheckpointSummary,
} from "./turnCheckpoints";

function baseOmissions(overrides: Partial<TurnCheckpointOmissions> = {}): TurnCheckpointOmissions {
  return {
    untracked_found: 2,
    untracked_admitted: 2,
    env_files: 0,
    secret_flagged: 0,
    unreadable_or_special: 0,
    too_large: 0,
    nested_repos: 0,
    tool_state: 0,
    wholesale: null,
    wholesale_detail: null,
    cap_files: 200,
    cap_bytes: 5_000_000,
    found_bytes: 1_024,
    ...overrides,
  };
}

function sampleTurn(overrides: Partial<TurnCheckpointSummary> = {}): TurnCheckpointSummary {
  return {
    id: "turn-001",
    ref_name: "refs/gitpulse/turns/repo/turn-001",
    commit: "abcdef123456",
    tree: "123456abcdef",
    parent: "000000111111",
    created_at: 1_700_000_000,
    label: "Fix auth retry",
    event: "turn_end",
    session_id: "sess-42",
    branch: "feat/auth",
    files: 3,
    additions: 20,
    deletions: 5,
    stats_complete: true,
    omissions: baseOmissions(),
    ...overrides,
  };
}

describe("turnCheckpoints IPC wrappers", () => {
  it("calls list and diff commands with exact parameter names", async () => {
    invoke.mockClear();
    await listTurnCheckpoints("/tmp/repo", 25);
    await diffTurnCheckpoint("/tmp/repo", "turn-001", "turn-000");
    expect(invoke.mock.calls).toEqual([
      ["cmd_list_turn_checkpoints", { repoPath: "/tmp/repo", limit: 25 }],
      ["cmd_diff_turn_checkpoint", { repoPath: "/tmp/repo", id: "turn-001", against: "turn-000" }],
    ]);
  });
});

describe("turnCheckpoints helpers", () => {
  it("counts and formats guardrail omissions", () => {
    const omissions = baseOmissions({
      untracked_found: 5,
      untracked_admitted: 3,
      env_files: 1,
      secret_flagged: 2,
      too_large: 1,
      wholesale: "cap_bytes",
      wholesale_detail: "12 MB",
    });
    expect(totalOmittedFiles(omissions)).toBe(6);
    expect(formatOmissionSummary(omissions)).toEqual([
      "Skipped untracked snapshot (cap_bytes: 12 MB)",
      "1 .env file excluded",
      "2 secret-flagged files excluded",
      "1 oversized file skipped",
    ]);

    const perFileOmissions = baseOmissions({
      untracked_found: 4,
      untracked_admitted: 1,
      unreadable_or_special: 2,
      nested_repos: 1,
    });
    expect(totalOmittedFiles(perFileOmissions)).toBe(6);
    expect(formatOmissionSummary(perFileOmissions)).toEqual([
      "3 untracked files skipped by cap",
      "2 unreadable or special files skipped",
      "1 nested repository skipped",
    ]);
  });

  it("filters turns across label, branch, session, id, and commit", () => {
    const turns = [
      sampleTurn({ id: "t-1", label: "Implement billing", branch: "feat/pay" }),
      sampleTurn({ id: "t-2", label: "Refactor sidebar", session_id: "codex-9" }),
    ];
    expect(filterTurnCheckpoints(turns, "")).toHaveLength(2);
    expect(filterTurnCheckpoints(turns, "feat/pay").map((t) => t.id)).toEqual(["t-1"]);
    expect(filterTurnCheckpoints(turns, "codex-9").map((t) => t.id)).toEqual(["t-2"]);
  });

  it("formats relative timestamps cleanly", () => {
    const now = 1_700_005_000;
    expect(formatRelativeTurnTime(now - 30, now)).toBe("just now");
    expect(formatRelativeTurnTime(now - 300, now)).toBe("5m ago");
    expect(formatRelativeTurnTime(now - 7_200, now)).toBe("2h ago");
    expect(formatRelativeTurnTime(now - 172_800, now)).toBe("2d ago");
  });
});
