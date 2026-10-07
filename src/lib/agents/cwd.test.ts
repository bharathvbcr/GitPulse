import { describe, expect, it } from "vitest";
import { get, writable } from "svelte/store";
import { identityKey } from "../repos/paths";
import {
  AGENT_CWD_CONCURRENCY,
  AGENT_CWD_DEADLINE_MS,
  MAX_AGENT_CWD_READS,
  agentCwdTargets,
  createAgentDirectorySweep,
  nearestContaining,
  readAgentCwds,
} from "./cwd";

function deferred<T>() {
  let resolve: (value: T) => void = () => {};
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

const context = (cwd: string | null) => ({ process: "claude", busy: false, cwd, repo_dir: null });

describe("readAgentCwds", () => {
  it("keeps only a directory the context parser accepted", async () => {
    const seen: string[] = [];
    const found = await readAgentCwds([" pty-1 ", "pty-1", "", "pty-2", "pty-3", "pty-4"], {
      read: async (id) => {
        seen.push(id);
        if (id === "pty-1") return context("/repo/.claude/worktrees/alpha");
        if (id === "pty-2") return context("   ");
        if (id === "pty-3") return { process: "claude", busy: "yes", cwd: "/repo", repo_dir: null };
        return context(null);
      },
    });
    expect(seen).toEqual(["pty-1", "pty-2", "pty-3", "pty-4"]);
    expect([...found.entries()]).toEqual([["pty-1", "/repo/.claude/worktrees/alpha"]]);
  });

  it("drops a rejected payload and still reads the sessions after it", async () => {
    const found = await readAgentCwds(["bad", "good"], {
      read: async (id) => {
        if (id === "bad") throw new Error("os declined");
        return context("/work/beta");
      },
    });
    expect([...found.entries()]).toEqual([["good", "/work/beta"]]);
  });

  it("does not start a read once the deadline has passed", async () => {
    let time = 0;
    const gate = deferred<void>();
    let started = 0;
    const ids = Array.from({ length: AGENT_CWD_CONCURRENCY + 2 }, (_, index) => `pty-${index}`);
    const pending = readAgentCwds(ids, {
      now: () => time,
      read: async (id) => {
        started += 1;
        if (started === AGENT_CWD_CONCURRENCY) time = AGENT_CWD_DEADLINE_MS;
        await gate.promise;
        return context(`/work/${id}`);
      },
    });
    expect(started).toBe(AGENT_CWD_CONCURRENCY);
    gate.resolve();
    const found = await pending;
    expect(found.size).toBe(AGENT_CWD_CONCURRENCY);
    expect(started).toBe(AGENT_CWD_CONCURRENCY);
  });

  it("stops taking session ids at the cap", async () => {
    const seen: string[] = [];
    const ids = Array.from({ length: MAX_AGENT_CWD_READS + 5 }, (_, index) => `pty-${index}`);
    await readAgentCwds(ids, {
      read: async (id) => {
        seen.push(id);
        return context(`/work/${id}`);
      },
    });
    expect(seen).toHaveLength(MAX_AGENT_CWD_READS);
    expect(seen.at(-1)).toBe(`pty-${MAX_AGENT_CWD_READS - 1}`);
  });
});

describe("agentCwdTargets", () => {
  const rec = (sessionId: string | null, label: string, fields: Record<string, unknown> = {}) =>
    ({ sessionId, label, ...fields });

  it("puts agents ahead of shells, so shells only fill what the cap leaves", () => {
    const shells = Array.from({ length: MAX_AGENT_CWD_READS }, (_, index) => rec(`sh-${index}`, "Shell"));
    const ids = agentCwdTargets([...shells, rec("claude-1", "Claude"), rec(null, "Codex"), rec("codex-1", "Codex")]);
    expect(ids.slice(0, 2)).toEqual(["claude-1", "codex-1"]);
    expect(ids).toHaveLength(MAX_AGENT_CWD_READS);
    expect(ids.filter((id) => id.startsWith("sh-"))).toHaveLength(MAX_AGENT_CWD_READS - 2);
  });

  it("does not change when only a title or a status changes", () => {
    const before = agentCwdTargets([rec("a", "Claude", { title: "one", status: "starting" }), rec("b", "Shell")]);
    const after = agentCwdTargets([rec("a", "Claude", { title: "two", status: "running" }), rec("b", "Shell")]);
    expect(after).toEqual(before);
  });
});

describe("createAgentDirectorySweep", () => {
  type Rec = { sessionId: string | null; label: string; title?: string; status?: string };
  const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

  function harness() {
    const sessions = writable<Rec[]>([]);
    const calls: string[][] = [];
    const answers = new Map<string, string>();
    const sweep = createAgentDirectorySweep(sessions, async (ids) => {
      calls.push([...ids]);
      return new Map(ids.filter((id) => answers.has(id)).map((id) => [id, answers.get(id)!]));
    });
    return { sessions, calls, answers, sweep };
  }

  it("reads when the set of sessions changes, and not on a title or status change", async () => {
    const { sessions, calls, answers, sweep } = harness();
    answers.set("a", "/repo/.claude/worktrees/alpha");
    sessions.set([{ sessionId: "a", label: "Claude", title: "one", status: "starting" }]);
    const seen: ReadonlyMap<string, string>[] = [];
    const stop = sweep.subscribe((value) => seen.push(value));
    await settle();
    expect(calls).toEqual([["a"]]);
    expect(get(sweep).get("a")).toBe("/repo/.claude/worktrees/alpha");

    sessions.set([{ sessionId: "a", label: "Claude", title: "two", status: "running" }]);
    await settle();
    expect(calls).toHaveLength(1);

    sessions.set([{ sessionId: "a", label: "Claude" }, { sessionId: "b", label: "Shell" }]);
    await settle();
    expect(calls.at(-1)).toEqual(["a", "b"]);
    stop();
  });

  it("does not read with nobody subscribed, and reads again on refresh", async () => {
    const { sessions, calls, sweep } = harness();
    sessions.set([{ sessionId: "a", label: "Claude" }]);
    await settle();
    expect(calls).toEqual([]);
    const stop = sweep.subscribe(() => {});
    await settle();
    sweep.refresh();
    await settle();
    expect(calls).toEqual([["a"], ["a"]]);
    stop();
    sweep.refresh();
    await settle();
    expect(calls).toHaveLength(2);
  });

  it("drops an older sweep that finishes after a newer one, and keeps the last answer on failure", async () => {
    const sessions = writable<Rec[]>([{ sessionId: "a", label: "Claude" }]);
    const slow = deferred<Map<string, string>>();
    let call = 0;
    const sweep = createAgentDirectorySweep(sessions, async () => {
      call += 1;
      if (call === 1) return slow.promise;
      if (call === 2) return new Map([["a", "/new"]]);
      throw new Error("os declined");
    });
    const stop = sweep.subscribe(() => {});
    sweep.refresh();
    await settle();
    slow.resolve(new Map([["a", "/old"]]));
    await settle();
    expect(get(sweep).get("a")).toBe("/new");
    sweep.refresh();
    await settle();
    expect(get(sweep).get("a")).toBe("/new");
    stop();
  });
});

describe("nearestContaining", () => {
  const PATHS = { caseInsensitive: true };
  const index = new Map([
    [identityKey("/Repo", PATHS), "main"],
    [identityKey("/repo/.claude/worktrees/alpha", PATHS), "alpha"],
  ]);

  it("matches the path itself, then the nearest ancestor", () => {
    expect(nearestContaining(index, "/repo/.claude/worktrees/alpha", PATHS)).toBe("alpha");
    expect(nearestContaining(index, "/REPO/.claude/worktrees/alpha/src/lib/", PATHS)).toBe("alpha");
    expect(nearestContaining(index, "/repo/src", PATHS)).toBe("main");
  });

  it("does not match a sibling that only shares a prefix, or anything outside", () => {
    expect(nearestContaining(index, "/repo/.claude/worktrees/alphabet", PATHS)).toBe("main");
    expect(nearestContaining(index, "/repository", PATHS)).toBeUndefined();
    expect(nearestContaining(index, "/elsewhere", PATHS)).toBeUndefined();
    expect(nearestContaining(index, "", PATHS)).toBeUndefined();
    expect(nearestContaining(new Map([["c:/work", "drive"]]), "C:\\work\\sub", PATHS)).toBe("drive");
  });
});
