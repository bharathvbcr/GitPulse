import { beforeEach, describe, expect, it, vi } from "vitest";
import { askConfirm } from "../stores/modalStore";
import { isLinkedWorktree, requestRepositoryTrust, type TrustPreview } from "./repositoryTrust";

vi.mock("../stores/modalStore", () => ({ askConfirm: vi.fn() }));

const main: TrustPreview = {
  path: "/repo",
  git_dir: "/repo/.git",
  common_dir: "/repo/.git",
  identity: "opaque-identity",
  trusted: false,
};
const worktree: TrustPreview = {
  ...main,
  path: "/repo/.claude/worktrees/feature",
  git_dir: "/repo/.git/worktrees/feature",
};

function invoker(preview: TrustPreview) {
  const calls: Array<{ command: string; args: unknown }> = [];
  const invokeFn = async (command: string, args?: unknown) => {
    calls.push({ command, args });
    return (command === "cmd_repository_trust" ? preview : undefined) as never;
  };
  return { calls, invokeFn };
}

async function messageFor(preview: TrustPreview): Promise<string> {
  const { invokeFn } = invoker(preview);
  vi.mocked(askConfirm).mockResolvedValue(false);
  await requestRepositoryTrust(preview.path, "Trust and Open", invokeFn);
  const { message } = vi.mocked(askConfirm).mock.calls[0][0];
  // An approval asked for with no message is the failure these tests exist to
  // catch, so it must not read as an empty string that passes every `toMatch`.
  if (message === undefined) throw new Error("the trust dialog asked for approval with no message");
  return message;
}

describe("repository trust approval", () => {
  beforeEach(() => vi.mocked(askConfirm).mockReset());

  it("distinguishes a linked worktree by its private Git directory", () => {
    expect(isLinkedWorktree(main)).toBe(false);
    expect(isLinkedWorktree(worktree)).toBe(true);
  });

  // The approval is repository-wide, so the dialog has to say so *before* it is
  // given. Consent to "this checkout" that silently covered its siblings would
  // be the widening happening behind the person granting it.
  it("states that the decision covers every worktree of the repository", async () => {
    const message = await messageFor(main);
    expect(message).toContain("/repo");
    expect(message).toMatch(/covers the whole repository/i);
    expect(message).toMatch(/every linked worktree/i);
  });

  it("names the repository a linked worktree belongs to", async () => {
    const message = await messageFor(worktree);
    expect(message).toContain("/repo/.claude/worktrees/feature");
    expect(message).toMatch(/linked worktree of the repository at \/repo\/\.git/);
    expect(message).toMatch(/covers that whole repository/i);
  });

  it("grants the inspected identity for the canonical path once approved", async () => {
    const { calls, invokeFn } = invoker(worktree);
    vi.mocked(askConfirm).mockResolvedValue(true);
    await expect(requestRepositoryTrust("/alias", "Trust and Open", invokeFn)).resolves.toBe(worktree.path);
    expect(calls.map(call => call.command)).toEqual(["cmd_repository_trust", "cmd_grant_repository_trust"]);
    expect(calls[1].args).toEqual({ repoPath: worktree.path, expectedIdentity: worktree.identity });
  });

  it("asks nothing and grants nothing for a checkout already covered", async () => {
    const { calls, invokeFn } = invoker({ ...worktree, trusted: true });
    await expect(requestRepositoryTrust(worktree.path, "Trust and Open", invokeFn)).resolves.toBe(worktree.path);
    expect(calls.map(call => call.command)).toEqual(["cmd_repository_trust"]);
    expect(askConfirm).not.toHaveBeenCalled();
  });

  it("returns null without granting when the approval is declined", async () => {
    const { calls, invokeFn } = invoker(main);
    vi.mocked(askConfirm).mockResolvedValue(false);
    await expect(requestRepositoryTrust(main.path, "Trust and Open", invokeFn)).resolves.toBeNull();
    expect(calls.map(call => call.command)).toEqual(["cmd_repository_trust"]);
  });
});
