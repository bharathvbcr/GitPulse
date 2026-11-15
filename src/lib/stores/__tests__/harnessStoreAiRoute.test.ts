import { afterEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { IpcError } from "../../ipc/invoke";
import {
  createHarnessStore,
  deriveAiReadiness,
  describeAiTarget,
  type AiStatus,
  type ClaudeCredentialState,
  type ManviStatus,
} from "../harnessStore";

/**
 * GitPulse's own AI features follow what Manvi itself is set to run on
 * (`manvi use`, read by the backend into `ai.manvi`). Readiness is one derived
 * value; vitest never runs `$effect`, so it is tested as a pure function.
 */

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

const local = { base_url: "http://127.0.0.1:11434/v1", model: "qwen3" };
const claude = { provider: "anthropic" as const, model: "claude-sonnet-5-5" };

const manvi = (over: Partial<ManviStatus> = {}): ManviStatus => ({
  state: "ready",
  detail: "",
  mode: "cloud",
  provider: "anthropic",
  provider_from: "config",
  model: "claude-sonnet-5-5",
  model_from: "config",
  shadowed_by: [],
  user_config: "/u/manvi.toml",
  local_base_url: local.base_url,
  local_model: local.model,
  cloud_model: "claude-sonnet-5-5",
  warnings: [],
  unsupported: "",
  selection: claude,
  ...over,
});

const localManvi = (over: Partial<ManviStatus> = {}) =>
  manvi({ mode: "local", provider: "local", model: "qwen3", selection: { provider: "local", ...local }, ...over });

const ai = (m: ManviStatus, over: Partial<AiStatus> = {}) =>
  ({
    harness: { available: true, binary: "manvi", protocol: 1, posture: "host", ops: [], error: "", error_code: "" },
    endpoints: [],
    ready: true,
    detail: "",
    selected: local,
    manvi: m,
    ...over,
  }) as unknown as AiStatus;

const credential = (over: Partial<NonNullable<ClaudeCredentialState["status"]>> | null): ClaudeCredentialState => ({
  status: over === null ? null : { keySaved: true, account: null, accountError: null, ...over },
  error: null,
});

const unreadable = (state: ManviStatus["state"]) =>
  manvi({ state, detail: "why", mode: "", provider: "", model: "", selection: null });

describe("aiReadiness", () => {
  it("cloud is ready when the credential has no problem", () => {
    const r = deriveAiReadiness({ ai: ai(manvi(), { ready: false }), claudeCredential: credential({}) });
    expect(r).toEqual({ ready: true, target: "cloud", via: "manvi", model: "claude-sonnet-5-5", reason: "" });
  });

  it("cloud is not ready with a credential problem, and says why", () => {
    const r = deriveAiReadiness({ ai: ai(manvi()), claudeCredential: credential({ keySaved: false }) });
    expect(r).toMatchObject({ ready: false, target: "cloud", via: "manvi" });
    expect(r.reason).toContain("No Claude credential");
  });

  it("cloud is not ready while the credential has not been checked", () => {
    const r = deriveAiReadiness({ ai: ai(manvi()), claudeCredential: credential(null) });
    expect(r).toMatchObject({ ready: false, target: "cloud", reason: "Checking the Claude credential…" });
  });

  it("a failed credential check is not ready and is not 'no credential'", () => {
    const r = deriveAiReadiness({ ai: ai(manvi()), claudeCredential: { status: null, error: "keychain locked" } });
    expect(r.ready).toBe(false);
    expect(r.reason).toContain("Couldn't check the Claude credential. keychain locked");
  });

  it("local mode through Manvi is ready when the local server is, and ignores the credential", () => {
    const up = deriveAiReadiness({ ai: ai(localManvi()), claudeCredential: credential({ keySaved: false }) });
    expect(up).toEqual({ ready: true, target: "local", via: "manvi", model: "qwen3", reason: "" });
    const down = deriveAiReadiness({
      ai: ai(localManvi(), { ready: false, detail: "No local model server answered." }),
      claudeCredential: credential(null),
    });
    expect(down).toMatchObject({ ready: false, target: "local", via: "manvi", reason: "No local model server answered." });
  });

  it("a mode GitPulse cannot use is not ready and never borrows local readiness", () => {
    const r = deriveAiReadiness({
      ai: ai(manvi({ mode: "other", provider: "gemini", selection: null, unsupported: "Manvi is set to gemini." })),
      claudeCredential: credential({}),
    });
    expect(r).toMatchObject({ ready: false, via: "manvi", reason: "Manvi is set to gemini." });
  });

  it("with Manvi unreadable (or unasked) it is GitPulse's own loopback client's verdict", () => {
    for (const state of ["not_installed", "needs_update", "failed"] as const) {
      const r = deriveAiReadiness({ ai: ai(unreadable(state)), claudeCredential: credential(null) });
      expect(r).toEqual({ ready: true, target: "local", via: "loopback", model: "qwen3", reason: "" });
    }
    const none = deriveAiReadiness({ ai: null, claudeCredential: credential(null) });
    expect(none).toMatchObject({ ready: false, via: "loopback", reason: "Local models have not been checked." });
  });

  it("names the destination for tooltips", () => {
    const named = (s: ManviStatus, cred = credential({})) => describeAiTarget(deriveAiReadiness({ ai: ai(s), claudeCredential: cred }));
    expect(named(manvi())).toBe("claude-sonnet-5-5 (Claude API, via Manvi)");
    expect(named(localManvi())).toBe("qwen3 (local model, via Manvi)");
    expect(named(unreadable("needs_update"))).toBe("the local model");
  });
});

describe("the store", () => {
  afterEach(() => {
    try {
      localStorage.removeItem("gitpulse_ai_model");
    } catch {
      /* no storage in this environment */
    }
  });

  function backend(status: ManviStatus) {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    const invokeFn = (async (cmd: string, args?: Record<string, unknown>) => {
      calls.push({ cmd, args: args ?? {} });
      if (cmd === "cmd_claude_key_status") return { key_saved: true, account: null, account_error: null };
      if (cmd === "cmd_ai_status") return ai(status);
      if (cmd === "cmd_manvi_use") return status;
      return { text: "ok" };
    }) as Invoke;
    return { invokeFn, calls };
  }

  it.each([
    ["cmd_ai_generate_commit_message", (s: ReturnType<typeof createHarnessStore>) => s.generateCommitMessage("/repo")],
    ["cmd_ai_explain_commit", (s: ReturnType<typeof createHarnessStore>) => s.explainCommit("/repo", "abc")],
    ["cmd_ai_suggest_branch_name", (s: ReturnType<typeof createHarnessStore>) => s.suggestBranchName("/repo")],
    ["cmd_ai_fix_health", (s: ReturnType<typeof createHarnessStore>) => s.fixHealth("/repo", "report")],
    ["cmd_ai_coverage_report", (s: ReturnType<typeof createHarnessStore>) => s.coverageReport("/repo", "report")],
  ])("%s leaves the route to the backend and sends only the saved loopback fallback", async (cmd, call) => {
    const { invokeFn, calls } = backend(manvi());
    const store = createHarnessStore({ invoke: invokeFn, confirm: async () => false });
    await store.selectModel(null);
    await call(store);
    const sent = calls.find((c) => c.cmd === cmd)!;
    expect(sent.args.provider).toBeUndefined();
    expect(sent.args).toMatchObject({ baseUrl: null, model: null });
  });

  it("the Manvi writer sends the mode and shapes, then re-reads status and clears the credential answer", async () => {
    const { invokeFn, calls } = backend(manvi());
    const store = createHarnessStore({ invoke: invokeFn, confirm: async () => false });
    await store.useManvi("cloud", { model: "claude-opus-5-5" });
    expect(calls.find((c) => c.cmd === "cmd_manvi_use")!.args).toEqual({
      mode: "cloud",
      model: "claude-opus-5-5",
      baseUrl: null,
    });
    expect(calls.some((c) => c.cmd === "cmd_ai_status")).toBe(true);
    await store.useManvi("local", { baseUrl: local.base_url, model: "qwen3" });
    expect(calls.filter((c) => c.cmd === "cmd_manvi_use")[1].args).toEqual({
      mode: "local",
      model: "qwen3",
      baseUrl: local.base_url,
    });
  });

  it("picking a local model writes it to Manvi when Manvi can be written, and only saves it when not", async () => {
    const live = backend(manvi());
    const store = createHarnessStore({ invoke: live.invokeFn, confirm: async () => false });
    await store.refreshAi();
    await store.selectModel({ base_url: local.base_url, model: "llama" });
    expect(live.calls.find((c) => c.cmd === "cmd_manvi_use")!.args).toMatchObject({ mode: "local", model: "llama" });
    expect(get(store).preferred).toEqual({ base_url: local.base_url, model: "llama" });

    const old = backend(unreadable("needs_update"));
    const fallbackStore = createHarnessStore({ invoke: old.invokeFn, confirm: async () => false });
    await fallbackStore.refreshAi();
    await fallbackStore.selectModel({ base_url: local.base_url, model: "llama" });
    expect(old.calls.some((c) => c.cmd === "cmd_manvi_use")).toBe(false);
    expect(get(fallbackStore).preferred).toEqual({ base_url: local.base_url, model: "llama" });
  });

  it("accepting the loopback consent dialog remembers GitPulse's own selection and never writes Manvi's config", async () => {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    const invokeFn = (async (cmd: string, args?: Record<string, unknown>) => {
      calls.push({ cmd, args: args ?? {} });
      if (cmd === "cmd_ai_status") return ai(localManvi());
      if (cmd === "cmd_manvi_use") return localManvi();
      if (cmd === "cmd_ai_generate_commit_message" && !args?.baseUrl) {
        throw new IpcError("ai_endpoint_unconfirmed", "Confirm where to send this.", {
          base_url: "http://127.0.0.1:8080/v1",
          model: "found",
          runtime: "llama.cpp",
          host: "127.0.0.1",
          port: 8080,
          label: "llama.cpp on 127.0.0.1:8080",
        });
      }
      return { text: "ok" };
    }) as Invoke;
    const store = createHarnessStore({ invoke: invokeFn, confirm: async () => true });
    await store.refreshAi();
    await store.selectModel(null);
    await store.generateCommitMessage("/repo");
    await Promise.resolve();
    expect(get(store).preferred).toEqual({ base_url: "http://127.0.0.1:8080/v1", model: "found" });
    expect(calls.some((c) => c.cmd === "cmd_manvi_use")).toBe(false);
  });

  it("asks for the Claude credential when the status says cloud, and not for local", async () => {
    const cloud = backend(manvi());
    const cloudStore = createHarnessStore({ invoke: cloud.invokeFn, confirm: async () => false });
    await cloudStore.refreshAi();
    await Promise.resolve();
    expect(cloud.calls.some((c) => c.cmd === "cmd_claude_key_status")).toBe(true);

    const localSide = backend(localManvi());
    const localStore = createHarnessStore({ invoke: localSide.invokeFn, confirm: async () => false });
    await localStore.refreshAi();
    expect(localSide.calls.some((c) => c.cmd === "cmd_claude_key_status")).toBe(false);
  });

  it("the retired local copy of the Claude choice is cleared and read by nothing", () => {
    try {
      localStorage.setItem("gitpulse_manvi_claude_model", "claude-sonnet-5-5");
    } catch {
      return;
    }
    const store = createHarnessStore({ invoke: backend(manvi()).invokeFn });
    expect(localStorage.getItem("gitpulse_manvi_claude_model")).toBeNull();
    expect("manviClaude" in get(store)).toBe(false);
  });
});
