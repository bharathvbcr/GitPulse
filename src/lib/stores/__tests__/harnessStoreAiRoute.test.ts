import { afterEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  createHarnessStore,
  deriveAiReadiness,
  describeAiTarget,
  type AiStatus,
  type ClaudeCredentialState,
  type HarnessState,
} from "../harnessStore";

/**
 * GitPulse's own AI features follow the Manvi pane's choice: Claude when
 * `manviClaude` is set, the saved local selection otherwise. Readiness is one
 * derived value; vitest never runs `$effect`, so it is tested as a pure
 * function.
 */

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

const ai = (over: Partial<AiStatus> = {}) =>
  ({ ready: true, detail: "", selected: { base_url: "http://127.0.0.1:11434/v1", model: "qwen" }, ...over }) as AiStatus;

const credential = (over: Partial<ClaudeCredentialState["status"] & object> | null): ClaudeCredentialState => ({
  status: over === null ? null : { keySaved: true, account: null, accountError: null, ...over },
  error: null,
});

const claude = { model: "claude-sonnet-5-5" };
const readiness = (state: Pick<HarnessState, "manviClaude" | "ai" | "claudeCredential">) => deriveAiReadiness(state);

describe("aiReadiness", () => {
  it("cloud is ready when the credential has no problem", () => {
    const r = readiness({ manviClaude: claude, ai: null, claudeCredential: credential({}) });
    expect(r).toEqual({ ready: true, target: "cloud", model: "claude-sonnet-5-5", reason: "" });
  });

  it("cloud is not ready with a credential problem, and says why", () => {
    const r = readiness({
      manviClaude: claude,
      ai: ai(),
      claudeCredential: credential({ keySaved: false }),
    });
    expect(r.ready).toBe(false);
    expect(r.target).toBe("cloud");
    expect(r.reason).toContain("No Claude credential");
  });

  it("cloud is not ready while the credential has not been checked", () => {
    const r = readiness({ manviClaude: claude, ai: ai(), claudeCredential: credential(null) });
    expect(r).toMatchObject({ ready: false, target: "cloud", reason: "Checking the Claude credential…" });
  });

  it("a failed credential check is not ready and is not 'no credential'", () => {
    const r = readiness({
      manviClaude: claude,
      ai: ai(),
      claudeCredential: { status: null, error: "keychain locked" },
    });
    expect(r.ready).toBe(false);
    expect(r.reason).toContain("Couldn't check the Claude credential. keychain locked");
  });

  it("an invalid Claude model id is never ready, and never borrows local readiness", () => {
    const r = readiness({ manviClaude: { model: "gpt-5" }, ai: ai(), claudeCredential: credential({}) });
    expect(r).toMatchObject({ ready: false, target: "cloud" });
  });

  it("local reads local discovery", () => {
    expect(readiness({ manviClaude: null, ai: ai(), claudeCredential: credential(null) })).toEqual({
      ready: true,
      target: "local",
      model: "qwen",
      reason: "",
    });
    const down = readiness({
      manviClaude: null,
      ai: ai({ ready: false, detail: "No local model server answered.", selected: null }),
      claudeCredential: credential(null),
    });
    expect(down).toMatchObject({ ready: false, target: "local", reason: "No local model server answered." });
    expect(readiness({ manviClaude: null, ai: null, claudeCredential: credential(null) }).ready).toBe(false);
  });

  it("local readiness is unaffected by the credential", () => {
    expect(readiness({ manviClaude: null, ai: ai(), claudeCredential: credential({ keySaved: false }) }).ready).toBe(true);
  });

  it("names the destination for tooltips", () => {
    expect(describeAiTarget({ ready: true, target: "local", model: "q", reason: "" })).toBe("the local model");
    expect(describeAiTarget({ ready: true, target: "cloud", model: "claude-sonnet-5-5", reason: "" })).toBe(
      "claude-sonnet-5-5 (Claude API, via Manvi)",
    );
  });
});

describe("AI calls forward the effective selection", () => {
  afterEach(() => {
    try {
      localStorage.removeItem("gitpulse_manvi_claude_model");
      localStorage.removeItem("gitpulse_ai_model");
    } catch {
      /* no storage in this environment */
    }
  });

  function backend() {
    const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
    const invokeFn = (async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "cmd_claude_key_status") return { key_saved: true, account: null, account_error: null };
      calls.push({ cmd, args: args ?? {} });
      return { text: "ok" };
    }) as Invoke;
    return { invokeFn, calls };
  }

  const aiCalls: Array<[string, (s: ReturnType<typeof createHarnessStore>) => Promise<unknown>]> = [
    ["cmd_ai_generate_commit_message", (s) => s.generateCommitMessage("/repo")],
    ["cmd_ai_explain_commit", (s) => s.explainCommit("/repo", "abc")],
    ["cmd_ai_suggest_branch_name", (s) => s.suggestBranchName("/repo")],
    ["cmd_ai_fix_health", (s) => s.fixHealth("/repo", "report")],
    ["cmd_ai_coverage_report", (s) => s.coverageReport("/repo", "report")],
  ];

  it.each(aiCalls)("%s sends provider anthropic and the Claude model when Manvi is on Claude", async (cmd, call) => {
    const { invokeFn, calls } = backend();
    const store = createHarnessStore({ invoke: invokeFn, confirm: async () => false });
    store.selectModel({ base_url: "http://127.0.0.1:11434/v1", model: "qwen" });
    store.selectManviClaude(claude);
    await call(store);
    const sent = calls.find((c) => c.cmd === cmd)!;
    expect(sent.args).toMatchObject({ provider: "anthropic", model: "claude-sonnet-5-5" });
    // The saved local server is not sent alongside it.
    expect(sent.args.baseUrl).toBeUndefined();
  });

  it.each(aiCalls)("%s sends the saved local selection, and no provider, otherwise", async (cmd, call) => {
    const { invokeFn, calls } = backend();
    const store = createHarnessStore({ invoke: invokeFn, confirm: async () => false });
    store.selectModel({ base_url: "http://127.0.0.1:11434/v1", model: "qwen" });
    await call(store);
    const sent = calls.find((c) => c.cmd === cmd)!;
    expect(sent.args).toMatchObject({ baseUrl: "http://127.0.0.1:11434/v1", model: "qwen" });
    expect(sent.args.provider).toBeUndefined();
  });

  it("a Claude choice that is not a model id throws instead of going local", async () => {
    const { invokeFn, calls } = backend();
    const store = createHarnessStore({ invoke: invokeFn, confirm: async () => false });
    store.selectModel({ base_url: "http://127.0.0.1:11434/v1", model: "qwen" });
    store.selectManviClaude({ model: "not-claude" });
    await expect(store.explainCommit("/repo", "abc")).rejects.toThrow(/not a Claude model id/);
    expect(calls.filter((c) => c.cmd === "cmd_ai_explain_commit")).toHaveLength(0);
  });

  it("choosing Claude clears the credential answer, then asks for it", async () => {
    const { invokeFn } = backend();
    const store = createHarnessStore({ invoke: invokeFn, confirm: async () => false });
    store.selectManviClaude(claude);
    expect(get(store).claudeCredential).toEqual({ status: null, error: null });
    await store.refreshClaudeCredential();
    expect(get(store).claudeCredential.status?.keySaved).toBe(true);
  });
});
