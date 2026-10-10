import { describe, expect, it, vi } from "vitest";

vi.mock("./ptyBus.tauri", () => ({ ptyBus: { subscribe: () => () => {} } }));
vi.mock("../ipc/invoke", () => ({ invoke: vi.fn() }));

const { quitRisks } = await import("./quitGuard");
import type { TerminalSessionRecord } from "./sessionRegistry";

const record = (patch: Partial<TerminalSessionRecord>): TerminalSessionRecord => ({
  key: "k", repoPath: "/repos/app", label: "Shell", status: "running", close: async () => {}, ...patch,
});
const listing = (id: string, launcher: string | null = "claude") => ({
  id, shell: "claude", cwd: "/repos/app", repo: "/repos/app", launcher, run_id: null, detached: true,
});

function host(sessions: unknown, contexts: Record<string, unknown> = {}) {
  return vi.fn(async (command: string, args?: Record<string, unknown>) => {
    if (command === "cmd_terminal_sessions") return sessions;
    if (command === "cmd_terminal_context") return contexts[String(args?.sessionId)] ?? { process: null, busy: false, cwd: null, repo_dir: null };
    throw new Error(`unexpected ${command}`);
  }) as never;
}

describe("what quitting would end", () => {
  it("counts a launched agent once, even though the host lists it too", async () => {
    const records = [record({ key: "a", label: "Claude", launcher: "claude", sessionId: "term-1" })];
    const risks = await quitRisks(records, { invoke: host([listing("term-1")]) });
    expect(risks).toEqual([{ label: "Claude", repoPath: "/repos/app" }]);
  });

  it("counts a host session the registry does not hold", async () => {
    // The defect: only the registry was read, so a session the host kept after
    // a reload, or past a lowered limit, died with the app unasked.
    const risks = await quitRisks([], { invoke: host([listing("term-9"), listing("term-8", null)]) });
    expect(risks.map((risk) => risk.label)).toEqual(["claude", "Terminal"]);
  });

  it("counts an agent typed into a shell tab, and not a shell running something else", async () => {
    const records = [
      record({ key: "s1", label: "Shell", sessionId: "term-1", launcher: "shell" }),
      record({ key: "s2", label: "Shell 2", sessionId: "term-2", launcher: "shell" }),
    ];
    const invoke = host([listing("term-1"), listing("term-2")], {
      "term-1": { process: "claude", busy: true, cwd: null, repo_dir: null },
      "term-2": { process: "vim", busy: true, cwd: null, repo_dir: null },
    });
    const risks = await quitRisks(records, { invoke });
    expect(risks).toEqual([{ label: "Shell (claude)", repoPath: "/repos/app" }]);
  });

  it("goes ahead on what it knows when a question cannot be answered, and says so", async () => {
    const onError = vi.fn();
    const invoke = vi.fn(async () => { throw new Error("host busy"); }) as never;
    const risks = await quitRisks([record({ key: "a", label: "Claude", launcher: "claude", sessionId: "t" })], { invoke, onError });
    expect(risks).toEqual([{ label: "Claude", repoPath: "/repos/app" }]);
    expect(onError).toHaveBeenCalledWith("the host's session list", expect.any(Error));
  });
});
