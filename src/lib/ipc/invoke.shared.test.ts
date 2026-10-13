import { describe, expect, it, vi } from "vitest";
import { withDeferralRetry } from "./invoke";
import { SHARED_READ_COMMANDS } from "./sharedReads";
import { readCommandsSource } from "../../../scripts/commands-source";

/** A backend that answers each call when the test says so. */
function backend() {
  const pending: Array<{ cmd: string; args: unknown; answer: (value: unknown) => void; fail: (error: unknown) => void }> = [];
  const raw = vi.fn(
    (cmd: string, args?: unknown) =>
      new Promise<unknown>((answer, fail) => {
        pending.push({ cmd, args, answer, fail });
      }),
  );
  const invoke = withDeferralRetry(raw as never, { random: () => 0, whenShown: () => Promise.resolve() });
  return { raw, pending, invoke };
}

describe("identical concurrent reads reach the backend once", () => {
  it("answers two identical in-flight reads with one call", async () => {
    const b = backend();
    const first = b.invoke<string>("cmd_get_status", { repoPath: "/r" });
    const second = b.invoke<string>("cmd_get_status", { repoPath: "/r" });
    expect(b.raw).toHaveBeenCalledTimes(1);
    b.pending[0].answer("clean");
    await expect(Promise.all([first, second])).resolves.toEqual(["clean", "clean"]);
  });

  it("treats the same arguments in another key order as the same call", async () => {
    const b = backend();
    void b.invoke("cmd_get_commit_graph", { repoPath: "/r", maxCommits: 10 });
    void b.invoke("cmd_get_commit_graph", { maxCommits: 10, repoPath: "/r" });
    expect(b.raw).toHaveBeenCalledTimes(1);
  });

  it("keeps different arguments, and different commands, apart", () => {
    const b = backend();
    void b.invoke("cmd_get_status", { repoPath: "/a" });
    void b.invoke("cmd_get_status", { repoPath: "/b" });
    void b.invoke("cmd_list_branches", { repoPath: "/a" });
    expect(b.raw).toHaveBeenCalledTimes(3);
  });

  it("starts a fresh call once the shared one has settled", async () => {
    const b = backend();
    const first = b.invoke("cmd_get_status", { repoPath: "/r" });
    b.pending[0].answer(1);
    await first;
    const second = b.invoke("cmd_get_status", { repoPath: "/r" });
    expect(b.raw).toHaveBeenCalledTimes(2);
    b.pending[1].answer(2);
    await expect(second).resolves.toBe(2);
  });

  it("gives every joined caller the failure, and the next call starts afresh", async () => {
    const b = backend();
    const first = b.invoke("cmd_get_status", { repoPath: "/r" }).catch((e: unknown) => e);
    const second = b.invoke("cmd_get_status", { repoPath: "/r" }).catch((e: unknown) => e);
    b.pending[0].fail("not a git repository");
    await expect(Promise.all([first, second])).resolves.toEqual(["not a git repository", "not a git repository"]);
    void b.invoke("cmd_get_status", { repoPath: "/r" });
    expect(b.raw).toHaveBeenCalledTimes(2);
  });

  it("one caller aborting leaves the shared call running for the other", async () => {
    const b = backend();
    const controller = new AbortController();
    const aborting = b.invoke("cmd_get_status", { repoPath: "/r" }, { signal: controller.signal }).catch((e: unknown) => e);
    const staying = b.invoke<string>("cmd_get_status", { repoPath: "/r" });
    expect(b.raw).toHaveBeenCalledTimes(1);
    controller.abort(new Error("gave up"));
    await expect(aborting).resolves.toMatchObject({ message: "gave up" });
    b.pending[0].answer("clean");
    await expect(staying).resolves.toBe("clean");
    expect(b.raw).toHaveBeenCalledTimes(1);
  });

  it("one caller's deadline leaves the shared call running for the other", async () => {
    vi.useFakeTimers();
    try {
      const b = backend();
      const impatient = b.invoke("cmd_get_status", { repoPath: "/r" }, { deadlineMs: 1_000 }).catch((e: unknown) => e);
      const patient = b.invoke<string>("cmd_get_status", { repoPath: "/r" });
      await vi.advanceTimersByTimeAsync(1_001);
      await expect(impatient).resolves.toMatchObject({ name: "DeadlineError" });
      b.pending[0].answer("clean");
      await expect(patient).resolves.toBe("clean");
      expect(b.raw).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("the shared-read allowlist", () => {
  it("never joins a command that is not named in it", () => {
    const b = backend();
    for (const cmd of ["cmd_commit", "cmd_stage_file", "cmd_push", "cmd_stash_apply", "cmd_delete_branch", "cmd_write_file_content"]) {
      expect(SHARED_READ_COMMANDS.has(cmd), cmd).toBe(false);
      void b.invoke(cmd, { repoPath: "/r" });
      void b.invoke(cmd, { repoPath: "/r" });
    }
    // Two identical writes are two writes.
    expect(b.raw).toHaveBeenCalledTimes(12);
  });

  it("names only commands the backend registers, and only read-shaped ones", () => {
    const source = readCommandsSource();
    for (const cmd of SHARED_READ_COMMANDS) {
      expect(source, `${cmd} is not a command any more`).toMatch(new RegExp(`fn ${cmd}\\b`));
      expect(cmd, `${cmd} does not read as a read`).toMatch(
        /^cmd_(get_|list_|stash_list$|repo_operation$|last_fetch_at$|branch_stats$)/,
      );
    }
  });

  it("does not join calls whose arguments are not plain data", () => {
    const b = backend();
    const cyclic: Record<string, unknown> = { repoPath: "/r" };
    cyclic.self = cyclic;
    void b.invoke("cmd_get_status", cyclic as never);
    void b.invoke("cmd_get_status", cyclic as never);
    expect(b.raw).toHaveBeenCalledTimes(2);
  });
});
