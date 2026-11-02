import { describe, expect, it, vi } from "vitest";
import { decideQuitManagedRuns, decideQuitTerminals, parseBackendStatus, quitManagedQuestion, quitQuestion, type Ask, type AskStop } from "./quitChoice";

function listing(id: string, launcher: string | null, extra: Record<string, unknown> = {}) {
  return { id, shell: "/bin/zsh", cwd: "/r", repo: "/repos/app", launcher, run_id: null, detached: false, ...extra };
}

function invoker(backend: unknown, sessions: unknown) {
  return (async (command: string) => {
    if (command === "cmd_terminal_backend") {
      if (backend instanceof Error) throw backend;
      return backend;
    }
    if (command === "cmd_terminal_sessions") {
      if (sessions instanceof Error) throw sessions;
      return sessions;
    }
    throw new Error(`unexpected ${command}`);
  }) as <T>(command: string) => Promise<T>;
}

const host = { backend: "host", trouble: [] };

describe("decideQuitTerminals", () => {
  it("never asks in-process: the agent check there still applies", async () => {
    const ask = vi.fn<Ask>();
    const decision = await decideQuitTerminals({ ask, invoke: invoker({ backend: "in-process", trouble: [] }, []) });
    expect(decision).toEqual({ backend: "in-process" });
    expect(ask).not.toHaveBeenCalled();
  });

  it("leaves a host with nothing running alone without asking", async () => {
    const ask = vi.fn<Ask>();
    const ending = [listing("term-1-1", "claude", { ending: true })];
    expect(await decideQuitTerminals({ ask, invoke: invoker(host, ending) })).toEqual({ backend: "host", terminals: "leave" });
    expect(ask).not.toHaveBeenCalled();
  });

  it("asks when the host holds sessions, and maps each answer", async () => {
    const sessions = [listing("term-1-1", "claude"), listing("term-1-2", null)];
    for (const [answer, expected] of [
      ["confirm", { backend: "host", terminals: "leave" }],
      ["alternate", { backend: "host", terminals: "stop" }],
      [null, { backend: "host", cancelled: true }],
    ] as const) {
      const ask = vi.fn<Ask>(async () => answer);
      expect(await decideQuitTerminals({ ask, invoke: invoker(host, sessions) })).toEqual(expected);
      const question = ask.mock.calls[0][0];
      expect(question.message).toContain("2 terminal sessions are still running");
      expect(question.message).toContain("• claude — /repos/app");
      expect(question.message).toContain("• Terminal — /repos/app");
      expect(question.confirmLabel).toBe("Quit and Leave Running");
      expect(question.alternateLabel).toBe("Stop Them and Quit");
    }
  });

  it("asks anyway, saying so, when the session list cannot be read", async () => {
    const onError = vi.fn();
    const ask = vi.fn<Ask>(async () => "alternate");
    const decision = await decideQuitTerminals({ ask, invoke: invoker(host, new Error("host gone")), onError });
    expect(decision).toEqual({ backend: "host", terminals: "stop" });
    expect(ask.mock.calls[0][0].message).toContain("could not list them");
    expect(onError).toHaveBeenCalledWith("the terminal host's sessions", expect.any(Error));
  });

  it("treats an unreadable backend as in-process, whose question warns that agents end", async () => {
    const onError = vi.fn();
    const ask = vi.fn<Ask>();
    expect(await decideQuitTerminals({ ask, invoke: invoker({ backend: "cloud" }, []), onError })).toEqual({ backend: "in-process" });
    expect(onError).toHaveBeenCalled();
    expect(ask).not.toHaveBeenCalled();
  });
});

describe("quitQuestion", () => {
  it("lists at most five and counts the rest", () => {
    const many = Array.from({ length: 8 }, (_, i) => ({ label: `s${i}`, repo: "/r" }));
    const question = quitQuestion(many);
    expect(question.message.match(/^• /gm)).toHaveLength(5);
    expect(question.message).toContain("…and 3 more");
    expect(quitQuestion([{ label: "claude", repo: "/r" }]).alternateLabel).toBe("Stop It and Quit");
  });
});

describe("parseBackendStatus", () => {
  it("accepts only the two backends and a list of strings", () => {
    expect(parseBackendStatus({ backend: "host", trouble: ["skew"] })).toEqual({ backend: "host", trouble: ["skew"] });
    for (const bad of [null, [], { backend: "host" }, { backend: "x", trouble: [] }, { backend: "host", trouble: [1] }]) {
      expect(parseBackendStatus(bad)).toBeNull();
    }
  });
});

function managedRun(id: string, over: Record<string, unknown> = {}) {
  return { id, kind: "managed", state: "running", provider: "codex", task_title: `Task ${id}`, cwd: `/repos/app/.gitpulse/worktrees/${id}`, ...over } as unknown as import("../workbench/client").TaskRun;
}

describe("decideQuitManagedRuns", () => {
  it("asks nothing when the list was read in full and no managed run is working", async () => {
    const ask = vi.fn<AskStop>();
    const listRuns = vi.fn(async () => ({
      complete: true,
      // A terminal attempt is not stopped by the sidecar going away, a prepared one has not started, and an exited one is over.
      runs: [managedRun("a", { kind: "external_terminal" }), managedRun("b", { state: "prepared" }), managedRun("c", { state: "exited" })],
    }));
    expect(await decideQuitManagedRuns({ ask, listRuns })).toEqual({ cancelled: false, stopping: 0 });
    expect(ask).not.toHaveBeenCalled();
  });

  it("names how many managed runs quitting stops, counting starting, running and unresolved ones", async () => {
    const ask = vi.fn<AskStop>(async () => true);
    const listRuns = async () => ({ complete: true, runs: [managedRun("a"), managedRun("b", { state: "starting" }), managedRun("c", { state: "unresolved", provider: "claude" }), managedRun("d", { state: "prepared" })] });
    expect(await decideQuitManagedRuns({ ask, listRuns, displayName: (path) => path.split("/").pop() ?? path })).toEqual({ cancelled: false, stopping: 3 });
    const question = ask.mock.calls[0][0];
    expect(question.title).toBe("Quit While 3 Managed Runs Are Working?");
    expect(question.message).toContain("3 managed runs will be stopped");
    expect(question.message).toContain("• codex — Task a — a");
    expect(question.message).toContain("• claude — Task c — c");
    expect(question.message).not.toContain("Task d");
    expect(question.confirmLabel).toBe("Stop and Quit");
    expect(question.cancelLabel).toBe("Keep Working");
    expect(question.destructive).toBe(true);
  });

  it("is a cancellation when the person keeps working", async () => {
    const ask = vi.fn<AskStop>(async () => false);
    expect(await decideQuitManagedRuns({ ask, listRuns: async () => ({ complete: true, runs: [managedRun("a")] }) })).toEqual({ cancelled: true });
    expect(ask.mock.calls[0][0].title).toBe("Quit While a Managed Run Is Working?");
    expect(ask.mock.calls[0][0].message).toContain("1 managed run will be stopped");
  });

  it("asks, as a floor, when the list was cut short, even with none counted", async () => {
    const ask = vi.fn<AskStop>(async () => false);
    expect(await decideQuitManagedRuns({ ask, listRuns: async () => ({ complete: false, runs: [] }) })).toEqual({ cancelled: true });
    expect(ask.mock.calls[0][0].message).toContain("could not be read in full");
    ask.mockClear();
    await decideQuitManagedRuns({ ask, listRuns: async () => ({ complete: false, runs: [managedRun("a")] }) });
    expect(ask.mock.calls[0][0].message).toContain("at least 1 managed run will be stopped");
  });

  it("asks anyway, saying so, when the list cannot be read at all", async () => {
    const onError = vi.fn();
    const ask = vi.fn<AskStop>(async () => true);
    const failure = new Error("workbench gone");
    expect(await decideQuitManagedRuns({ ask, listRuns: async () => { throw failure; }, onError })).toEqual({ cancelled: false, stopping: null });
    expect(ask.mock.calls[0][0].message).toContain("could not list them");
    expect(onError).toHaveBeenCalledWith("the managed runs", failure);
  });

  it("lists at most five and counts the rest", () => {
    const many = Array.from({ length: 8 }, (_, i) => managedRun(`r${i}`));
    const question = quitManagedQuestion(many, true);
    expect(question.message.match(/^• /gm)).toHaveLength(5);
    expect(question.message).toContain("…and 3 more");
  });
});
