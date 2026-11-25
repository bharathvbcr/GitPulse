/**
 * The hand-off between Mavi and the terminal dock, with the dock, the
 * repository tabs and the Mavi store faked. What matters is the order: one
 * process per conversation, nothing stopped before every check passes, and a
 * failed move leaves the conversation where it was and says so.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { writable } from "svelte/store";
import type { ThreadRecord } from "./threads";

const UUID = "0d235a3d-03cf-44a9-ad04-effb59321456";

const fake = vi.hoisted(() => ({
  log: [] as string[],
  transcript: { state: "found", reason: null as string | null } as { state: string; reason: string | null },
  hosted: new Map<string, { sessionId: string; repoPath: string; startDir: string | null; mode: null }>(),
  opened: true as boolean,
  launchError: null as Error | null,
  phases: new Map<string, string>(),
  records: [] as unknown[],
  check: { state: "found", reason: null as string | null, root: "/r" as string | null },
}));

vi.mock("../terminal/sessionRegistry", async () => {
  const { writable: w } = await import("svelte/store");
  const sessions = w<unknown[]>([]);
  return {
    terminalSessions: sessions,
    runningAgents: (records: { sessionId?: string; launcher?: string }[]) =>
      records.filter((r) => r.sessionId && r.launcher && r.launcher !== "shell"),
  };
});
vi.mock("../terminal/sessionLimit", async () => {
  const { writable: w } = await import("svelte/store");
  return { terminalSessionLimit: w(8) };
});
vi.mock("../terminal/transcripts", () => ({
  checkTranscripts: async (ids: string[]) => new Map(ids.map((id) => [id.toLowerCase(), fake.transcript])),
}));
vi.mock("../terminal/hostedAgent", () => ({ hostedClaude: { byTerminal: (id: string) => fake.hosted.get(id) } }));
vi.mock("../terminal/sessionFocus", () => ({
  focusTerminalSession: async (record: { key: string }) => {
    fake.log.push(`focus ${record.key}`);
    return { ok: true };
  },
}));
vi.mock("../terminal/launchRequests", () => ({
  terminalLaunchRequests: {
    request: async (path: string, launcher: string, _p?: unknown, _q?: unknown, options?: { resume?: { sessionId: string } }) => {
      fake.log.push(`launch ${launcher} ${path}${options?.resume ? ` resume ${options.resume.sessionId}` : ""}`);
      if (fake.launchError) throw fake.launchError;
    },
  },
}));
vi.mock("../stores/repoStore", async () => {
  const { writable: w } = await import("svelte/store");
  const state = w({ currentPath: "/r" });
  return {
    repoStore: {
      subscribe: state.subscribe,
      openRepo: async (path: string, options?: { onReady?: (p: string) => void }) => {
        fake.log.push(`open ${path}`);
        if (fake.opened) options?.onReady?.(path);
        return fake.opened;
      },
      setTerminalOpen: (open: boolean) => fake.log.push(`dock ${open}`),
    },
  };
});
vi.mock("./client", () => ({
  maviClient: { conversationCheck: async () => fake.check },
}));
vi.mock("./store.svelte", () => {
  const store = {
    get records() {
      return fake.records as ThreadRecord[];
    },
    queues: {} as Record<string, string[]>,
    // Every live thread's summary; the move counts them against the host's limit.
    get summaries() {
      return Object.fromEntries([...fake.phases].map(([key, phase]) => [key, { phase }]));
    },
    record: (key: string) => (fake.records as ThreadRecord[]).find((r) => r.key === key),
    summaryOf: (record: ThreadRecord) => {
      const phase = fake.phases.get(record.key);
      return phase ? { phase } : null;
    },
    stop: async (key: string) => {
      fake.log.push(`stop ${key}`);
      fake.phases.set(key, "ended");
    },
    waitForEnd: async (key: string) => fake.phases.get(key) === "ended",
    markMoved: (key: string, moved: boolean) => {
      fake.log.push(`moved ${key} ${moved}`);
      const r = (fake.records as ThreadRecord[]).find((x) => x.key === key);
      if (r) r.movedToTerminalAt = moved ? 1 : null;
    },
    resume: async (key: string) => {
      fake.log.push(`resume ${key}`);
    },
    adopt: async (options: { root: string; conversationId: string }) => {
      fake.log.push(`adopt ${options.root} ${options.conversationId}`);
      return "new-key";
    },
  };
  return { maviStore: store };
});

import { bringBackToMavi, continueInMavi, continueInTerminal, openShellHere } from "./handoff";
import { terminalSessions } from "../terminal/sessionRegistry";

const record = (extra: Partial<ThreadRecord> = {}): ThreadRecord => ({
  key: "k",
  sessionId: "mavi-1",
  title: "Fix it",
  target: { kind: "local", root: "/r" },
  mode: "ask",
  model: null,
  conversationId: UUID,
  createdAt: 0,
  lastActivityAt: 0,
  seenAt: 0,
  movedToTerminalAt: null,
  ...extra,
});

const tabs = terminalSessions as unknown as ReturnType<typeof writable<unknown[]>>;

beforeEach(() => {
  fake.log = [];
  fake.transcript = { state: "found", reason: null };
  fake.hosted.clear();
  fake.opened = true;
  fake.launchError = null;
  fake.phases.clear();
  fake.records = [record()];
  fake.check = { state: "found", reason: null, root: "/r" };
  tabs.set([]);
});

describe("continueInTerminal", () => {
  it("marks the thread moved, ends the Mavi agent, then starts the tab resuming the conversation", async () => {
    fake.phases.set("k", "idle");
    expect(await continueInTerminal("k")).toEqual({ ok: true });
    expect(fake.log).toEqual(["open /r", "moved k true", "stop k", `launch claude /r resume ${UUID}`, "dock true"]);
  });

  it("takes the move back when the tab cannot start", async () => {
    fake.launchError = new Error("PTY limit");
    const result = await continueInTerminal("k");
    expect(result).toMatchObject({ ok: false });
    expect(fake.log.at(-1)).toBe("moved k false");
    expect(fake.records[0]).toMatchObject({ movedToTerminalAt: null });
  });

  it("refuses before stopping anything when the transcript is missing, unsure, or the tab cannot open", async () => {
    fake.transcript = { state: "absent", reason: null };
    expect(await continueInTerminal("k")).toMatchObject({ ok: false, reason: expect.stringContaining("nothing to move") });
    fake.transcript = { state: "error", reason: "unreadable" };
    expect(await continueInTerminal("k")).toMatchObject({ ok: false, reason: expect.stringContaining("unreadable") });
    fake.transcript = { state: "found", reason: null };
    fake.opened = false;
    expect(await continueInTerminal("k")).toMatchObject({ ok: false, reason: expect.stringContaining("Nothing was moved") });
    expect(fake.log.some((l) => l.startsWith("stop") || l.startsWith("moved"))).toBe(false);
    expect(await continueInTerminal("gone")).toEqual({ ok: false, reason: "That thread is gone." });
  });

  it("with the conversation already in a tab, ends the Mavi side and shows the tab instead of running both", async () => {
    fake.phases.set("k", "idle");
    fake.hosted.set("pty-1", { sessionId: UUID, repoPath: "/r", startDir: null, mode: null });
    tabs.set([{ key: "tab-1", repoPath: "/r", label: "Claude", launcher: "claude", status: "running", sessionId: "pty-1" }]);
    expect(await continueInTerminal("k")).toMatchObject({ ok: true, note: expect.stringContaining("already open") });
    expect(fake.log).toEqual(["stop k", "moved k true", "focus tab-1"]);
  });
});

describe("bringBackToMavi", () => {
  it("resumes here once no tab holds the conversation, and refuses (showing the tab) while one does", async () => {
    fake.records = [record({ movedToTerminalAt: 1 })];
    expect(await bringBackToMavi("k")).toEqual({ ok: true });
    expect(fake.log).toEqual(["resume k", "moved k false"]);

    fake.log = [];
    fake.hosted.set("pty-1", { sessionId: UUID, repoPath: "/r", startDir: null, mode: null });
    tabs.set([{ key: "tab-1", repoPath: "/r", label: "Claude", launcher: "claude", status: "running", sessionId: "pty-1" }]);
    expect(await bringBackToMavi("k")).toMatchObject({ ok: false, reason: expect.stringContaining("still running") });
    expect(fake.log).toEqual(["focus tab-1"]);
    expect(await bringBackToMavi("gone")).toMatchObject({ ok: false });
  });
});

describe("openShellHere", () => {
  it("opens a shell in a local thread's worktree, and refuses an SSH thread", async () => {
    expect(await openShellHere("k")).toEqual({ ok: true });
    expect(fake.log).toEqual(["open /r", "launch shell /r", "dock true"]);
    fake.records = [record({ target: { kind: "ssh", host: "gpu", dir: "~/x" } })];
    expect(await openShellHere("k")).toMatchObject({ ok: false });
    fake.records = [record()];
    fake.launchError = new Error("no room");
    expect(await openShellHere("k")).toEqual({ ok: false, reason: "no room" });
  });
});

describe("continueInMavi", () => {
  const claudeTab = (status = "running") => ({
    key: "tab-1",
    repoPath: "/r",
    label: "Claude",
    launcher: "claude",
    status,
    sessionId: "pty-1",
    title: "Port the CLI",
    close: async () => {
      fake.log.push("close tab-1");
      tabs.set([claudeTab("exited")]);
    },
  });

  beforeEach(() => {
    fake.records = [];
    fake.hosted.set("pty-1", { sessionId: UUID.toUpperCase(), repoPath: "/r", startDir: null, mode: null });
    tabs.set([claudeTab()]);
  });

  it("checks, asks, ends the tab, and only then starts the Mavi agent", async () => {
    const confirm = vi.fn(async () => true);
    expect(await continueInMavi("tab-1", confirm)).toEqual({ ok: true, key: "new-key" });
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ root: "/r", mode: "ask", conversationId: UUID, title: "Port the CLI" }));
    expect(fake.log).toEqual(["close tab-1", `adopt /r ${UUID}`]);
  });

  it("refuses before the tab ends: a conversation saved elsewhere, a busy worktree, or a declined question", async () => {
    fake.check = { state: "elsewhere", reason: null, root: null };
    expect(await continueInMavi("tab-1", async () => true)).toMatchObject({ ok: false, reason: expect.stringContaining("subdirectory") });

    fake.check = { state: "found", reason: null, root: "/r" };
    fake.records = [record({ key: "busy" })];
    fake.phases.set("busy", "working");
    expect(await continueInMavi("tab-1", async () => true)).toMatchObject({ ok: false, reason: expect.stringContaining("already working") });

    fake.records = [];
    expect(await continueInMavi("tab-1", async () => false)).toEqual({ ok: false, reason: "" });
    expect(await continueInMavi("no-tab", async () => true)).toMatchObject({ ok: false, reason: "That terminal tab is gone." });
    expect(fake.log).toEqual([]);
  });
});
