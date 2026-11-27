import { describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({
  invoke: vi.fn().mockResolvedValue({}),
}));

vi.mock("../ipc/invoke", () => ({ invoke }));

import { buildResolvedProposalTree, loadConflictProposal } from "./conflictSession";

describe("conflict proposal IPC wrappers", () => {
  it("invokes cmd_load_conflict_proposal and cmd_build_resolved_proposal_tree with exact payloads", async () => {
    invoke.mockClear();
    await loadConflictProposal("/tmp/repo", "refs/gitpulse/proposals/0123456789abcdef/deadbeef");
    await buildResolvedProposalTree("/tmp/repo", {
      proposal_ref: "refs/gitpulse/proposals/0123456789abcdef/deadbeef",
      expected_proposal_commit_oid: "abc123",
      target_tip_oid: null,
      resolutions: [
        {
          file_path: "src/app.ts",
          revision: "rev-1",
          choice: "AcceptProposal",
        },
      ],
    });
    expect(invoke.mock.calls).toEqual([
      [
        "cmd_load_conflict_proposal",
        {
          repoPath: "/tmp/repo",
          proposalRef: "refs/gitpulse/proposals/0123456789abcdef/deadbeef",
        },
      ],
      [
        "cmd_build_resolved_proposal_tree",
        {
          repoPath: "/tmp/repo",
          request: {
            proposal_ref: "refs/gitpulse/proposals/0123456789abcdef/deadbeef",
            expected_proposal_commit_oid: "abc123",
            target_tip_oid: null,
            resolutions: [
              {
                file_path: "src/app.ts",
                revision: "rev-1",
                choice: "AcceptProposal",
              },
            ],
          },
        },
      ],
    ]);
  });
});
