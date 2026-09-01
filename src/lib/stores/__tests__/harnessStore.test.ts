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
