import { describe, expect, it, vi } from "vitest";
import { decideQuitTerminals, parseBackendStatus, quitQuestion, type Ask } from "./quitChoice";

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
