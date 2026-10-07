import { afterEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

vi.mock("../stores/repoStore", () => ({ repoStore: { subscribe: (fn: (value: unknown) => void) => { fn({ currentPath: "/work", openTabs: [] }); return () => {}; } } }));
vi.mock("../stores/interfaceStore", () => ({ interfaceStore: { setTaskHandoff: vi.fn(), setGlobalSurface: vi.fn() } }));
vi.mock("./client", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./client")>()),
  explainError: (cause: unknown) => (cause instanceof Error ? cause.message : String(cause)),
  findConversation: vi.fn(),
  launchManagedRun: vi.fn(),
}));

const { startPreparedAttempt, stopWatchingAttempt, watchedAttempts } = await import("./taskTerminal");
const { attemptNotices, clearAttemptNotice, consumeTaskTerminal, consumeTaskTerminalRequest, enqueueTaskTerminal, hasTaskTerminalRequest, noteAttempt, pruneTaskTerminals, taskTerminalRequests } = await import("../terminal/taskLaunches");
type Deps = import("./taskTerminal").PreparedAttemptDeps;
type Start = import("./taskTerminal").AttemptStart;
type Run = import("./client").TaskRun;

const NOW = 1_800_000_000_000;
const run = (id: string, fields: Partial<Run> = {}): Run => ({
  id, revision: 1, updated_at: NOW / 1000, kind: "external_terminal", task_id: "t", source_revision: 3, task_title: "Fix",
  repository_id: "r", provider: "claude", permission_mode: "ask", state: "prepared", cwd: `/work/${id}`, created_at: NOW / 1000,
  expires_at: NOW / 1000 + 300, session_id: null, exit_code: null, reason: "", outcome_uncertain: false, ...fields,
});

/** A clock and timers the test advances, and every toast said. */
function harness(start: (run: Run) => Promise<Start>, extra: Partial<Deps> = {}) {
  let now = NOW;
  const timers = new Map<number, { at: number; fn: () => void }>();
  let next = 1;
  const toasts: { kind: string; message: string; id: string; dismissed: boolean; action: boolean }[] = [];
  const add = (kind: string) => (message: string, action?: unknown) => {
    const id = `toast-${toasts.length}`;
    toasts.push({ kind, message, id, dismissed: false, action: !!action });
    return id;
  };
  const remember = vi.fn();
  const deps: Deps = {
    startTerminal: start,
    launchManaged: vi.fn(),
    remember,
    show: vi.fn(async () => "opened"),
    toast: { info: add("info"), success: add("success"), error: add("error"), dismiss: (id) => { const t = toasts.find((x) => x.id === id); if (t) t.dismissed = true; } },
    now: () => now,
    setTimer: (fn, ms) => { const id = next++; timers.set(id, { at: now + ms, fn }); return id as unknown as ReturnType<typeof setTimeout>; },
    clearTimer: (handle) => { timers.delete(handle as unknown as number); },
    ...extra,
  };
  /** Advances the clock, firing due timers in order. */
  const advance = (ms: number) => {
    const until = now + ms;
    for (;;) {
      const due = [...timers.entries()].filter(([, t]) => t.at <= until).sort((a, b) => a[1].at - b[1].at)[0];
      if (!due) break;
      timers.delete(due[0]);
      now = Math.max(now, due[1].at);
      due[1].fn();
    }
    now = until;
  };
  return { deps, toasts, remember, advance, timers };
}

/** What the real `startTaskTerminal` does with the queue, by outcome. */
function queuedStart(outcome: Start["kind"], gate: Promise<void> = Promise.resolve()) {
  return async (r: Run): Promise<Start> => {
    if (outcome === "failed") { await gate; return { kind: "failed", checkout: r.cwd, reason: "Too many open repositories (max 24)." }; }
    enqueueTaskTerminal({ runId: r.id, repoPath: r.cwd, provider: r.provider, title: r.task_title });
    await gate;
    if (outcome === "waiting") return { kind: "waiting", reason: "checkout", checkout: r.cwd };
    return { kind: "started" };
  };
}

afterEach(() => {
  for (const request of get(taskTerminalRequests)) consumeTaskTerminalRequest(request);
  for (const id of get(attemptNotices).keys()) clearAttemptNotice(id);
  for (const id of watchedAttempts()) stopWatchingAttempt(id);
});

describe("startPreparedAttempt", () => {
  it("says 'Starting…' until the tab reports its process running, then confirms", async () => {
    const h = harness(queuedStart("started"));
    const result = await startPreparedAttempt(run("a"), {}, h.deps);
    expect(result.ok).toBe(true);
    expect(h.toasts.map((t) => [t.kind, t.message])).toEqual([["info", "Starting Claude Code on revision 3…"]]);
    expect(get(attemptNotices).get("a")?.phase).toBe("starting");
    // The dock's tab takes the request and its process starts.
    consumeTaskTerminal("a");
    noteAttempt("a", "running", "Claude Code is running.");
    expect(h.toasts.map((t) => [t.kind, t.message, t.dismissed])).toEqual([
      ["info", "Starting Claude Code on revision 3…", true],
      ["success", "Claude Code started on revision 3.", false],
    ]);
    expect(watchedAttempts()).toEqual([]);
  });

  it("turns a spawn the host refused in the hidden tab into an error toast and an error row", async () => {
    const h = harness(queuedStart("started"));
    await startPreparedAttempt(run("a"), {}, h.deps);
    consumeTaskTerminal("a");
    noteAttempt("a", "failed", "Claude Code 1.0.3 is too old for --setting-sources.");
    expect(h.toasts.at(-1)).toMatchObject({ kind: "error", message: "Claude Code did not start: Claude Code 1.0.3 is too old for --setting-sources." });
    expect(get(attemptNotices).get("a")).toMatchObject({ phase: "failed", tone: "error" });
  });

  it("records a refused open as a failure with its reason, not 'queued'", async () => {
    const h = harness(queuedStart("failed"));
    const result = await startPreparedAttempt(run("a"), {}, h.deps);
    expect(result).toMatchObject({ ok: false, error: "Could not open a: Too many open repositories (max 24)." });
    expect(get(attemptNotices).get("a")).toMatchObject({ phase: "failed", text: "Could not open a: Too many open repositories (max 24)." });
    expect(hasTaskTerminalRequest("a")).toBe(false);
    expect(h.toasts.at(-1)?.kind).toBe("error");
  });

  it("records 'no checkout contains it' from the start itself", async () => {
    const h = harness(async () => { throw new Error("No Git checkout contains /gone, so its terminal cannot start."); });
    const result = await startPreparedAttempt(run("a", { cwd: "/gone" }), {}, h.deps);
    expect(result.ok).toBe(false);
    expect(get(attemptNotices).get("a")?.text).toMatch(/No Git checkout contains \/gone/);
  });

  it("withdraws a request still waiting at the preparation's expiry, and says so", async () => {
    const h = harness(queuedStart("waiting"));
    await startPreparedAttempt(run("a"), {}, h.deps);
    expect(get(attemptNotices).get("a")).toMatchObject({ phase: "waiting", text: "Waiting for a to open." });
    h.advance(300_000 + 5_000);
    expect(hasTaskTerminalRequest("a")).toBe(false);
    expect(get(attemptNotices).get("a")?.text).toMatch(/expired before its terminal could start/);
    // No retry is offered on a preparation nothing can claim.
    expect(h.toasts.at(-1)).toMatchObject({ kind: "error", action: false });
    expect(h.timers.size).toBe(0);
  });

  it("keeps following a spawn already in flight at expiry, and hears its answer", async () => {
    const h = harness(queuedStart("started"));
    await startPreparedAttempt(run("a"), {}, h.deps);
    consumeTaskTerminal("a"); // the tab took it; the claim is in flight
    h.advance(305_000);
    expect(watchedAttempts()).toEqual(["a"]);
    noteAttempt("a", "running", "Claude Code is running.");
    expect(h.toasts.at(-1)?.kind).toBe("success");
    expect(h.timers.size).toBe(0);
  });

  it("says nothing more about an attempt cancelled while its open was answered", async () => {
    let release = () => {};
    const gate = new Promise<void>((resolve) => { release = resolve; });
    const h = harness(queuedStart("waiting", gate));
    const pending = startPreparedAttempt(run("a"), {}, h.deps);
    // The pane's Cancel preparation.
    consumeTaskTerminal("a"); stopWatchingAttempt("a"); clearAttemptNotice("a");
    release();
    expect((await pending).ok).toBe(false);
    expect(get(attemptNotices).has("a")).toBe(false);
    expect(hasTaskTerminalRequest("a")).toBe(false);
  });

  it("never lets 'waiting for the checkout' overwrite a start the tab already reported", async () => {
    let release = () => {};
    const gate = new Promise<void>((resolve) => { release = resolve; });
    const h = harness(queuedStart("waiting", gate));
    const pending = startPreparedAttempt(run("a"), {}, h.deps);
    consumeTaskTerminal("a");
    noteAttempt("a", "running", "Claude Code is running.");
    release();
    await pending;
    expect(get(attemptNotices).get("a")?.phase).toBe("running");
  });

  it("remembers the settings it was launched with, never bypass", async () => {
    const h = harness(queuedStart("started"));
    await startPreparedAttempt(run("a"), { remember: { provider: "codex", kind: "external_terminal", permission: "bypass" } }, h.deps);
    expect(h.remember).toHaveBeenCalledWith({ provider: "codex", kind: "external_terminal", permission: "ask" });
    await startPreparedAttempt(run("b"), { remember: { provider: "claude", kind: "external_terminal", permission: "inspect" } }, h.deps);
    expect(h.remember).toHaveBeenLastCalledWith({ provider: "claude", kind: "external_terminal", permission: "inspect" });
  });

  it("starts a managed attempt and records a failed start on its row", async () => {
    const launchManaged = vi.fn(async (id: string) => run(id, { kind: "managed", state: "running", session_id: "m" }));
    const h = harness(queuedStart("started"), { launchManaged });
    expect(await startPreparedAttempt(run("m", { kind: "managed" }), {}, h.deps)).toMatchObject({ ok: true, run: { state: "running" } });
    expect(get(attemptNotices).get("m")?.phase).toBe("running");
    launchManaged.mockRejectedValueOnce(new Error("Manvi is not installed"));
    const failed = await startPreparedAttempt(run("n", { kind: "managed" }), {}, h.deps);
    expect(failed.ok).toBe(false);
    expect(get(attemptNotices).get("n")).toMatchObject({ phase: "failed" });
    expect(get(attemptNotices).get("n")?.text).toMatch(/Manvi is not installed.*Start or recover managed launch/);
  });
});

/*
 * Launch, dispose, cancel and run-state changes in random order. The
 * invariant: an accepted run is started, explicitly cancelled, or shown as
 * an error — never silently abandoned — and no request outlives its run.
 */
describe("startPreparedAttempt under random interleavings", () => {
  function rng(seed: number) {
    let x = seed >>> 0 || 1;
    return () => { x ^= x << 13; x >>>= 0; x ^= x >>> 17; x ^= x << 5; x >>>= 0; return x / 0x1_0000_0000; };
  }
  type Event = "resolve-open" | "tab-takes" | "tab-starts" | "tab-fails" | "cancel" | "dispose" | "ended-elsewhere" | "tick";

  it("holds for 400 seeded schedules", async () => {
    for (let seed = 1; seed <= 400; seed += 1) {
      const random = rng(seed);
      const id = `s${seed}`;
      const outcome = (["started", "waiting", "failed"] as const)[Math.floor(random() * 3)];
      let release = () => {};
      const gate = new Promise<void>((resolve) => { release = resolve; });
      const h = harness(queuedStart(outcome, gate));
      let cancelled = false;
      let endedElsewhere = false;
      let taken = false;
      let reported = false;
      let disposed = false;
      // The form's part: start the owner, then (maybe) be destroyed. The
      // owner's work must not depend on which.
      const launch = startPreparedAttempt(run(id), {}, h.deps).then((result) => (disposed ? null : result));
      const events: Event[] = ["resolve-open", "tab-takes", "tab-starts", "tab-fails", "cancel", "dispose", "ended-elsewhere", "tick", "tick"];
      for (let i = events.length - 1; i > 0; i -= 1) { const j = Math.floor(random() * (i + 1)); [events[i], events[j]] = [events[j], events[i]]; }
      for (const event of events) {
        switch (event) {
          case "resolve-open": release(); await Promise.resolve(); await Promise.resolve(); break;
          case "tab-takes": if (!cancelled && hasTaskTerminalRequest(id)) { consumeTaskTerminal(id); taken = true; } break;
          case "tab-starts": if (taken && !reported) { noteAttempt(id, "running", "running"); reported = true; } break;
          case "tab-fails": if (taken && !reported && random() < 0.5) { noteAttempt(id, "failed", "claude: command not found"); reported = true; } break;
          case "cancel": if (!taken && !cancelled && random() < 0.4) { cancelled = true; consumeTaskTerminal(id); stopWatchingAttempt(id); clearAttemptNotice(id); } break;
          case "dispose": disposed = true; break;
          case "ended-elsewhere": if (!taken && !cancelled && random() < 0.2) { endedElsewhere = true; pruneTaskTerminals([{ id, state: "cancelled", expires_at: 0 }], NOW); } break;
          case "tick": h.advance(Math.floor(random() * 200_000)); break;
        }
      }
      release();
      await launch;
      // A tab that took the request always reports, eventually.
      if (taken && !reported) noteAttempt(id, random() < 0.5 ? "running" : "failed", "late");
      h.advance(20 * 60_000);
      const where = `seed ${seed} (${outcome}; ${events.join(",")})`;
      const notice = get(attemptNotices).get(id);
      // No request outlives its run, and nothing is still being watched.
      expect(hasTaskTerminalRequest(id), where).toBe(false);
      expect(watchedAttempts(), where).not.toContain(id);
      expect(h.timers.size, where).toBe(0);
      if (cancelled || endedElsewhere) continue;
      // Started, or shown as an error: never a start left "in progress".
      expect(["running", "failed"], where).toContain(notice?.phase);
      // The "Starting…" toast never outlives the decision.
      expect(h.toasts.filter((t) => t.kind === "info" && !t.dismissed), where).toEqual([]);
      for (const request of get(taskTerminalRequests)) consumeTaskTerminalRequest(request);
      clearAttemptNotice(id);
    }
  });
});
