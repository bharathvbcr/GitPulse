import { describe, it, expect } from "vitest";
import { get } from "svelte/store";
import {
  createHarnessStore,
  verdictDetail,
  verdictLabel,
  type AiStatus,
  type HarnessStatus,
  type PolicyVerdict,
} from "../harnessStore";
import type { CatchUp } from "../../ingest/types";
import type { LedgerEvent, LedgerStatus } from "../../ledger/types";

function verdict(overrides: Partial<PolicyVerdict>): PolicyVerdict {
  return {
    status: "allowed",
    checked: true,
    target: "git commit -m x",
    rule: "",
    severity: "",
    reason: "",
    demoted: "",
    task_id: "",
    grant_id: "",
    granted_by: "",
    widened: "",
    degraded: [],
    detail: "",
    detail_code: "",
    ...overrides,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (err: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const caughtUp = (recorded = 0): CatchUp => ({
  recorded,
  transcripts: 0,
  skipped_lines: 0,
  reflog_entries: 0,
  error: "",
});

const recording = (repoPath: string): LedgerStatus => ({
  recording: true,
  path: `${repoPath}/.devcouncil/ledger.sqlite`,
  dropped: 0,
  error: "",
  error_code: "",
});

function ledgerEvent(
  repoPath: string,
  id: number,
  ulid: string,
  label: string,
): LedgerEvent {
  return {
    id,
    ulid,
    ts_utc: `2026-09-01T12:00:${String(id % 60).padStart(2, "0")}Z`,
    schema_version: 1,
    repo_path: repoPath,
    worktree_path: null,
    actor_kind: "agent",
    actor_id: null,
    session_id: null,
    task_id: null,
    action: "git.commit",
    object: label,
    argv_json: null,
    outcome: "ok",
    verdict_json: null,
    before_ref: null,
    after_ref: null,
    duration_ms: null,
    detail_json: null,
  };
}

function ledgerInvoke(
  ledgers: Map<string, LedgerEvent[]>,
  tailCalls: Array<{ repoPath: string; cursor: number }> = [],
) {
  return (async (cmd: string, args?: Record<string, unknown>) => {
    const repoPath = String(args?.repoPath ?? "");
    if (cmd === "cmd_catch_up") return caughtUp();
    if (cmd === "cmd_ledger_status") return recording(repoPath);
    if (cmd === "cmd_ledger_tail") {
      const cursor = Number(args?.cursor ?? 0);
      const limit = Number(args?.limit ?? 200);
      tailCalls.push({ repoPath, cursor });
      return (ledgers.get(repoPath) ?? [])
        .filter((event) => event.id > cursor)
        .slice(0, limit);
    }
    throw new Error(`unexpected ${cmd}`);
  }) as <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
}

const harnessOk = { available: true, binary: "manvi", protocol: 1, posture: "host", ops: [], error: "", error_code: "" } as HarnessStatus;

function aiWith(model: string, harness: HarnessStatus = harnessOk): AiStatus {
  return {
    harness,
    endpoints: [],
    selected: { base_url: "http://x", model },
    model_info: null,
    model_detail: "",
    ready: true,
    detail: "",
  } as AiStatus;
}

describe("probe token staleness", () => {
  it("ignores a late resolution from an older overlapping probe", async () => {
    const calls: Array<{ cmd: string; d: ReturnType<typeof deferred<never>> }> = [];
    const store = createHarnessStore({
      invoke: (async (cmd: string) => {
        const d = deferred<never>();
        calls.push({ cmd, d });
        return d.promise;
      }) as never,
    });

    const first = store.refreshAi();
    const second = store.refreshAi();
    // Two probes in flight.
    expect(calls.filter((c) => c.cmd === "cmd_ai_status")).toHaveLength(2);
    // The NEWER one resolves first and must stick.
    calls[1].d.resolve(aiWith("new-model") as never);
    await second;
    expect(get(store).ai?.selected?.model).toBe("new-model");
    expect(get(store).isProbing).toBe(false);

    // The stale one resolves late; it must not overwrite or re-enter probing.
    calls[0].d.resolve(aiWith("stale-model") as never);
    await first;
    expect(get(store).ai?.selected?.model).toBe("new-model");
    expect(get(store).isProbing).toBe(false);
  });

  it("a stale error resolution does not clobber fresh state", async () => {
    const calls: Array<{ cmd: string; d: ReturnType<typeof deferred<never>> }> = [];
    const store = createHarnessStore({
      invoke: (async (cmd: string) => {
        const d = deferred<never>();
        calls.push({ cmd, d });
        return d.promise;
      }) as never,
    });

    const first = store.refreshAi();
    const second = store.refreshAi();
    calls[1].d.resolve(aiWith("fresh") as never);
    await second;

    calls[0].d.reject(new Error("boom") as never);
    await first;
    expect(get(store).ai?.selected?.model).toBe("fresh");
    expect(get(store).error).toBeNull();
  });

  it("reconnect's sidecar handshake cannot be overwritten by a stale refresh", async () => {
    const calls: Array<{ cmd: string; d: ReturnType<typeof deferred<never>> }> = [];
    const store = createHarnessStore({
      invoke: (async (cmd: string) => {
        const d = deferred<never>();
        calls.push({ cmd, d });
        return d.promise;
      }) as never,
    });
    const flush = async () => {
      for (let i = 0; i < 25; i += 1) await Promise.resolve();
    };

    const staleRefresh = store.refresh();
    const reconnect = store.reconnect();
    // refresh() is still waiting on its sidecar handshake, so only two
    // commands are in flight so far.
    expect(calls.map((c) => c.cmd)).toEqual(["cmd_harness_status", "cmd_harness_reconnect"]);

    // Reconnect finishes its handshake, which chains into its own AI probe.
    calls[1].d.resolve({ ...harnessOk, posture: "reconnected" } as never);
    await flush();
    expect(calls.map((c) => c.cmd)).toEqual([
      "cmd_harness_status",
      "cmd_harness_reconnect",
      "cmd_ai_status",
    ]);
    // The probe payload carries the restarted sidecar's status, as the real
    // command does.
    calls[2].d.resolve(aiWith("reconnected-model", { ...harnessOk, posture: "reconnected" }) as never);
    await reconnect;
    expect(get(store).harness?.posture).toBe("reconnected");
    expect(get(store).ai?.selected?.model).toBe("reconnected-model");

    // The pre-reconnect handshake fails late — ignored wholesale.
    calls[0].d.reject(new Error("late failure") as never);
    await staleRefresh;
    expect(get(store).harness?.posture).toBe("reconnected");
    expect(get(store).error).toBeNull();
  });
});

describe("harness error retention", () => {
  it("keeps a sidecar connection error when a later AI probe reports a clean nested harness", async () => {
    const store = createHarnessStore({
      invoke: (async (cmd: string) => {
        if (cmd === "cmd_harness_status") {
          return {
            ...harnessOk,
            available: false,
            error: "sidecar not running",
            error_code: "not_installed",
          };
        }
        if (cmd === "cmd_ai_status") {
          return aiWith("m", { ...harnessOk, error: "", error_code: "" });
        }
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    });
    await store.refresh();
    expect(get(store).harness?.error).toBe("sidecar not running");
    expect(get(store).harness?.error_code).toBe("not_installed");
    expect(get(store).ai?.selected?.model).toBe("m");
  });

  it("does not keep a stale connected state when status and AI revalidation both reject", async () => {
    let failing = false;
    const store = createHarnessStore({
      invoke: (async (cmd: string) => {
        if (failing) {
          throw new Error(
            cmd + " transport unavailable; Authorization: Bearer opaque-status-secret",
          );
        }
        if (cmd === "cmd_harness_status") return harnessOk;
        if (cmd === "cmd_ai_status") return aiWith("m");
        throw new Error("unexpected " + cmd);
      }) as never,
    });

    await store.refresh();
    expect(get(store).harness?.available).toBe(true);

    failing = true;
    await store.refresh();

    expect(get(store).harness?.available).toBe(false);
    expect(get(store).harness?.error).toContain("transport unavailable");
    expect(get(store).harness?.error).toContain("Authorization: Bearer <redacted>");
    expect(get(store).harness?.error).not.toContain("opaque-status-secret");
    expect(get(store).error).toContain("transport unavailable");
  });
});

describe("repository-scoped ledger projection", () => {
  const repoA = "/repos/A";
  const repoB = "/repos/B";

  it("keeps independent cursors and identities for equal row ids in different repositories", async () => {
    const calls: Array<{ repoPath: string; cursor: number }> = [];
    const ledgers = new Map([
      [
        repoA,
        [
          ledgerEvent(repoA, 5, "01A00000000000000000000005", "A five"),
          ledgerEvent(repoA, 100, "01A00000000000000000000100", "A hundred"),
        ],
      ],
      [repoB, [ledgerEvent(repoB, 5, "01B00000000000000000000005", "B five")]],
    ]);
    const store = createHarnessStore({ invoke: ledgerInvoke(ledgers, calls) });

    await store.catchUp(repoA);
    const aFiveIdentity = get(store).actions[0].identity;
    expect(get(store).ledgerCursor).toBe(100);

    await store.catchUp(repoB);
    const bState = get(store);
    expect(bState.ledgerCursor).toBe(5);
    expect(bState.ledger?.path).toBe(`${repoB}/.devcouncil/ledger.sqlite`);
    expect(bState.actions.map((action) => action.label)).toEqual(["B five"]);
    expect(calls).toContainEqual({ repoPath: repoB, cursor: 0 });
    expect(bState.actions[0].identity).not.toBe(aFiveIdentity);

    store.activateRepository(repoA);
    expect(get(store).ledgerCursor).toBe(100);
    expect(get(store).ledger?.path).toBe(`${repoA}/.devcouncil/ledger.sqlite`);
    expect(get(store).actions.map((action) => action.label)).toEqual([
      "A five",
      "A hundred",
    ]);
  });

  it("clears only the visible journal without replaying durable rows on the next sync", async () => {
    const calls: Array<{ repoPath: string; cursor: number }> = [];
    const ledgers = new Map([
      [repoA, [ledgerEvent(repoA, 9, "01A00000000000000000000009", "durable")]],
    ]);
    const store = createHarnessStore({ invoke: ledgerInvoke(ledgers, calls) });

    await store.catchUp(repoA);
    expect(get(store).ledgerCursor).toBe(9);
    store.clearActions();
    expect(get(store).actions).toEqual([]);

    await store.syncLedger(repoA);

    expect(get(store).actions).toEqual([]);
    expect(get(store).ledgerCursor).toBe(9);
    expect(calls.at(-1)).toEqual({ repoPath: repoA, cursor: 9 });
  });

  it("updates an inactive repository bucket without leaking a late event into the active journal", async () => {
    const aStatus = deferred<LedgerStatus>();
    const aTail = deferred<LedgerEvent[]>();
    const store = createHarnessStore({
      invoke: (async (cmd: string, args?: Record<string, unknown>) => {
        const path = String(args?.repoPath ?? "");
        if (cmd === "cmd_catch_up") return caughtUp();
        if (cmd === "cmd_ledger_status") {
          return path === repoA ? aStatus.promise : recording(path);
        }
        if (cmd === "cmd_ledger_tail") {
          return path === repoA
            ? aTail.promise
            : [ledgerEvent(repoB, 5, "01B00000000000000000000005", "B active")];
        }
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    });

    await store.catchUp(repoB);
    const lateA = store.syncLedger(repoA);
    aStatus.resolve(recording(repoA));
    await Promise.resolve();
    aTail.resolve([
      ledgerEvent(repoA, 7, "01A00000000000000000000007", "A late"),
    ]);
    await lateA;

    expect(get(store).actions.map((action) => action.label)).toEqual(["B active"]);
    store.activateRepository(repoA);
    expect(get(store).actions.map((action) => action.label)).toEqual(["A late"]);
  });

  it("rejects rapid A/B/A catch-up responses that arrive out of order", async () => {
    const pending: Array<{
      repoPath: string;
      response: ReturnType<typeof deferred<CatchUp>>;
    }> = [];
    const ledgers = new Map([
      [repoA, [ledgerEvent(repoA, 10, "01A00000000000000000000010", "A fresh")]],
      [repoB, [ledgerEvent(repoB, 3, "01B00000000000000000000003", "B stale")]],
    ]);
    const store = createHarnessStore({
      invoke: (async (cmd: string, args?: Record<string, unknown>) => {
        const repoPath = String(args?.repoPath ?? "");
        if (cmd === "cmd_catch_up") {
          const response = deferred<CatchUp>();
          pending.push({ repoPath, response });
          return response.promise;
        }
        if (cmd === "cmd_ledger_status") return recording(repoPath);
        if (cmd === "cmd_ledger_tail") {
          const cursor = Number(args?.cursor ?? 0);
          return (ledgers.get(repoPath) ?? []).filter((event) => event.id > cursor);
        }
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    });

    const firstA = store.catchUp(repoA);
    const middleB = store.catchUp(repoB);
    const secondA = store.catchUp(repoA);
    expect(pending.map((call) => call.repoPath)).toEqual([repoA, repoB, repoA]);

    pending[2].response.resolve(caughtUp(30));
    await secondA;
    expect(get(store).catchUp?.recorded).toBe(30);
    expect(get(store).actions.map((action) => action.label)).toEqual(["A fresh"]);

    pending[1].response.resolve(caughtUp(20));
    await middleB;
    pending[0].response.resolve(caughtUp(10));
    await firstA;

    expect(get(store).catchUp?.recorded).toBe(30);
    expect(get(store).ledgerCursor).toBe(10);
    expect(get(store).actions.map((action) => action.label)).toEqual(["A fresh"]);
  });

  it("keeps catch-up metadata when its own append notification starts a ledger sync", async () => {
    const catchUpResponse = deferred<CatchUp>();
    const ledgers = new Map([
      [repoA, [ledgerEvent(repoA, 4, "01A00000000000000000000004", "caught up")]],
    ]);
    const invokeLedger = ledgerInvoke(ledgers);
    const store = createHarnessStore({
      invoke: (async (cmd: string, args?: Record<string, unknown>) => {
        if (cmd === "cmd_catch_up") return catchUpResponse.promise;
        return invokeLedger(cmd, args);
      }) as never,
    });

    const catchingUp = store.catchUp(repoA);
    await store.syncLedger(repoA);
    catchUpResponse.resolve(caughtUp(4));
    await catchingUp;

    expect(get(store).catchUp?.recorded).toBe(4);
    expect(get(store).ledgerCursor).toBe(4);
    expect(get(store).actions.map((action) => action.label)).toEqual(["caught up"]);
  });

  it("does not collide an ephemeral action with a durable row using the same numeric id", async () => {
    const ledgers = new Map<string, LedgerEvent[]>();
    const store = createHarnessStore({ invoke: ledgerInvoke(ledgers) });
    await store.catchUp(repoA);
    store.recordAction({ kind: "commit", label: "ephemeral", ok: true }, 1);
    const ephemeralId = get(store).actions[0].id;
    ledgers.set(repoA, [
      ledgerEvent(repoA, ephemeralId, "01A0000000000000000000000E", "durable"),
    ]);

    await store.syncLedger(repoA);

    const state = get(store);
    expect(state.actions.map((action) => action.label).sort()).toEqual([
      "durable",
      "ephemeral",
    ]);
    expect(new Set(state.actions.map((action) => action.identity)).size).toBe(2);
  });

  it("uses one bucket for normalized spellings of the active canonical path", () => {
    const store = createHarnessStore({ invoke: ledgerInvoke(new Map()) });
    store.activateRepository(`${repoA}/`);
    store.recordAction({ kind: "commit", label: "canonical", ok: true }, 1);

    store.activateRepository(repoA);

    expect(get(store).actions.map((action) => action.label)).toEqual(["canonical"]);
  });

  it("adopts the backend canonical bucket when an append notification uses a symlink spelling", async () => {
    const canonical = "/real/repository";
    const alias = "/linked/repository";
    let exposeEvent = false;
    const store = createHarnessStore({
      invoke: (async (cmd: string, args?: Record<string, unknown>) => {
        if (cmd === "cmd_catch_up") return caughtUp();
        if (cmd === "cmd_ledger_status") return recording(canonical);
        if (cmd === "cmd_ledger_tail") {
          const cursor = Number(args?.cursor ?? 0);
          return exposeEvent && cursor < 8
            ? [
                ledgerEvent(
                  canonical,
                  8,
                  "01C00000000000000000000008",
                  "canonical event",
                ),
              ]
            : [];
        }
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    });

    await store.catchUp(canonical);
    exposeEvent = true;
    await store.syncLedger(alias);

    expect(get(store).actions.map((action) => action.label)).toEqual([
      "canonical event",
    ]);
    store.activateRepository(alias);
    expect(get(store).ledgerCursor).toBe(8);
    store.activateRepository(canonical);
    expect(get(store).ledgerCursor).toBe(8);
  });

  it("projects main and linked worktree addresses through one family bucket without duplicates", async () => {
    const main = "/repos/project";
    const linked = "/repos/project-worktrees/task";
    const row = {
      ...ledgerEvent(main, 12, "01F00000000000000000000012", "linked edit"),
      worktree_path: linked,
    };
    const tailCalls: Array<{ repoPath: string; cursor: number }> = [];
    const store = createHarnessStore({
      invoke: (async (cmd: string, args?: Record<string, unknown>) => {
        const repoPath = String(args?.repoPath ?? "");
        if (cmd === "cmd_catch_up") return caughtUp();
        if (cmd === "cmd_ledger_status") return recording(main);
        if (cmd === "cmd_ledger_tail") {
          const cursor = Number(args?.cursor ?? 0);
          tailCalls.push({ repoPath, cursor });
          return cursor < row.id ? [row] : [];
        }
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    });

    await store.catchUp(linked);
    expect(get(store).actions.map((action) => action.label)).toEqual(["linked edit"]);
    expect(get(store).ledgerCursor).toBe(12);

    // A main-checkout notification tails from the shared cursor, rather than
    // replaying the linked row into a second frontend bucket.
    await store.syncLedger(main);
    expect(get(store).actions.map((action) => action.label)).toEqual(["linked edit"]);
    expect(tailCalls).toContainEqual({ repoPath: main, cursor: 12 });

    store.activateRepository(linked);
    expect(get(store).actions).toHaveLength(1);
    store.activateRepository(main);
    expect(get(store).actions).toHaveLength(1);
  });

  it("does not let an older alias sync invalidate a newer canonical sync", async () => {
    const canonical = "/real/repository";
    const alias = "/linked/repository";
    const aliasStatus = deferred<LedgerStatus>();
    const canonicalStatus = deferred<LedgerStatus>();
    const newestTail = deferred<LedgerEvent[]>();
    let tailCalls = 0;
    const store = createHarnessStore({
      invoke: (async (cmd: string, args?: Record<string, unknown>) => {
        const repoPath = String(args?.repoPath ?? "");
        if (cmd === "cmd_ledger_status") {
          return repoPath === alias ? aliasStatus.promise : canonicalStatus.promise;
        }
        if (cmd === "cmd_ledger_tail") {
          tailCalls += 1;
          return tailCalls === 1 ? newestTail.promise : [];
        }
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    });
    store.activateRepository(canonical);

    const olderAlias = store.syncLedger(alias);
    const newerCanonical = store.syncLedger(canonical);
    canonicalStatus.resolve(recording(canonical));
    await Promise.resolve();
    aliasStatus.resolve(recording(canonical));
    await Promise.resolve();
    newestTail.resolve([
      ledgerEvent(canonical, 9, "01C00000000000000000000009", "newest event"),
    ]);
    await Promise.all([olderAlias, newerCanonical]);

    expect(tailCalls).toBe(1);
    expect(get(store).ledgerCursor).toBe(9);
    expect(get(store).actions.map((action) => action.label)).toEqual([
      "newest event",
    ]);
  });

  it("retains catch-up metadata when a concurrent sync canonicalizes its repository alias", async () => {
    const canonical = "/real/repository";
    const alias = "/linked/repository";
    const catchUpResponse = deferred<CatchUp>();
    const store = createHarnessStore({
      invoke: (async (cmd: string) => {
        if (cmd === "cmd_catch_up") return catchUpResponse.promise;
        if (cmd === "cmd_ledger_status") return recording(canonical);
        if (cmd === "cmd_ledger_tail") return [];
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    });

    const catchingUp = store.catchUp(alias);
    await store.syncLedger(alias);
    catchUpResponse.resolve(caughtUp(11));
    await catchingUp;

    expect(get(store).catchUp?.recorded).toBe(11);
    store.activateRepository(canonical);
    expect(get(store).catchUp?.recorded).toBe(11);
    store.activateRepository(alias);
    expect(get(store).catchUp?.recorded).toBe(11);
  });

  it("keeps the newest catch-up when canonical and alias runs merge", async () => {
    const canonical = "/real/repository";
    const alias = "/linked/repository";
    const pending = new Map<string, ReturnType<typeof deferred<CatchUp>>>();
    const store = createHarnessStore({
      invoke: (async (cmd: string, args?: Record<string, unknown>) => {
        const repoPath = String(args?.repoPath ?? "");
        if (cmd === "cmd_catch_up") {
          const response = deferred<CatchUp>();
          pending.set(repoPath, response);
          return response.promise;
        }
        if (cmd === "cmd_ledger_status") return recording(canonical);
        if (cmd === "cmd_ledger_tail") return [];
        throw new Error(`unexpected ${cmd}`);
      }) as never,
    });

    const olderCanonical = store.catchUp(canonical);
    const newerAlias = store.catchUp(alias);
    pending.get(alias)!.resolve(caughtUp(22));
    await newerAlias;
    pending.get(canonical)!.resolve(caughtUp(11));
    await olderCanonical;

    expect(get(store).catchUp?.recorded).toBe(22);
    store.activateRepository(canonical);
    expect(get(store).catchUp?.recorded).toBe(22);
  });

  it("files a late ephemeral action under the repository that initiated it", () => {
    const store = createHarnessStore({ invoke: ledgerInvoke(new Map()) });
    store.activateRepository(repoB);

    store.recordAction({
      repoPath: repoA,
      kind: "commit",
      label: "late A mutation",
      ok: true,
    });

    expect(get(store).actions).toEqual([]);
    store.activateRepository(repoA);
    expect(get(store).actions.map((action) => action.label)).toEqual([
      "late A mutation",
    ]);
  });

  it("keeps a late A verdict out of B while retaining it for A", () => {
    const store = createHarnessStore({ invoke: ledgerInvoke(new Map()) });
    const verdictA = verdict({ target: "A first" });
    const verdictB = verdict({ target: "B current" });
    const lateVerdictA = verdict({ target: "A late" });

    store.activateRepository(repoA);
    store.recordVerdict(verdictA, repoA);
    store.activateRepository(repoB);
    store.recordVerdict(verdictB, repoB);
    store.recordVerdict(lateVerdictA, repoA);

    expect(get(store).lastVerdict?.target).toBe("B current");
    store.activateRepository(repoA);
    expect(get(store).lastVerdict?.target).toBe("A late");
  });

  it("refuses a backend row whose stored repository identity crosses buckets", async () => {
    const ledgers = new Map([
      [repoA, [ledgerEvent(repoB, 9, "01B00000000000000000000009", "wrong repo")]],
    ]);
    const store = createHarnessStore({ invoke: ledgerInvoke(ledgers) });

    await store.catchUp(repoA);

    expect(get(store).actions).toEqual([]);
    expect(get(store).ledgerCursor).toBe(9);
    expect(get(store).ledger?.recording).toBe(false);
    expect(get(store).ledger?.error_code).toBe("repository_mismatch");
  });
});

describe("policy verdict rendering", () => {
  it("never labels an unchecked action the same as an allowed one", () => {
    const allowed = verdict({ status: "allowed" });
    const unchecked = verdict({
      status: "unchecked",
      checked: false,
      detail: "no `manvi` binary on PATH",
      detail_code: "not_installed",
    });

    expect(verdictLabel(allowed)).not.toBe(verdictLabel(unchecked));
    expect(verdictLabel(unchecked)).toContain("not checked");
    expect(verdictDetail(unchecked)).toContain("no `manvi` binary");
  });

  it("distinguishes a demoted allow from a clean one", () => {
    const demoted = verdict({
      status: "demoted",
      rule: "command.not_allowed",
      severity: "soft",
      demoted: "serve.posture=host: allowlist not enforced",
    });
    expect(verdictLabel(demoted)).not.toBe(verdictLabel(verdict({ status: "allowed" })));
    expect(verdictDetail(demoted)).toContain("allowlist not enforced");
  });

  it("names the rule that blocked an action", () => {
    const blocked = verdict({
      status: "blocked",
      rule: "command.force_push",
      severity: "hard",
      reason: "Force pushes are not allowed.",
    });
    expect(verdictLabel(blocked)).toContain("blocked");
    expect(verdictDetail(blocked)).toBe("command.force_push: Force pushes are not allowed.");
  });
});
