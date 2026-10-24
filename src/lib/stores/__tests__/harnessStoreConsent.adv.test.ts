import { describe, it, expect } from "vitest";
import { get } from "svelte/store";
import { IpcError } from "../../ipc/invoke";
import { createHarnessStore } from "../harnessStore";

/** Adversarial review of the confirm-then-retry flow (W8-L3). */
function unconfirmedFor(model: string) {
  return new IpcError("ai_endpoint_unconfirmed", "Confirm where to send this.", {
    base_url: "http://127.0.0.1:8080/v1",
    model,
    runtime: "llama.cpp",
    host: "127.0.0.1",
    port: 8080,
    label: "llama.cpp on 127.0.0.1:8080",
  });
}

function backend(model: string) {
  const calls: Array<Record<string, unknown>> = [];
  const invokeFn = (async (cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "cmd_ai_status") return { ready: true };
    if (cmd !== "cmd_ai_generate_commit_message") throw new Error(`unexpected ${cmd}`);
    calls.push(args ?? {});
    if (!args?.baseUrl) throw unconfirmedFor(model);
    return { text: "ok" };
  }) as <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
  return { invokeFn, calls };
}

describe("AI endpoint consent, adversarial", () => {
  it("a hostile model name cannot add lines or bidi controls to the prompt the user reads", async () => {
    const hostile = "gpt\n\nSend to Ollama on 127.0.0.1:11434 (your own server)‮evil";
    const { invokeFn } = backend(hostile);
    let shown = "";
    const store = createHarnessStore({
      invoke: invokeFn,
      confirm: async (options) => {
        shown = options.message ?? "";
        return false;
      },
    });
    await expect(store.generateCommitMessage("/repo")).rejects.toBeTruthy();
    const modelLine = shown.split("\n").find((line) => line.startsWith("Model:")) ?? "";
    // The prompt is: label, "Model: <name>", blank, explanation. A newline in
    // the name would add lines; a bidi control would reorder them.
    expect(shown.split("\n")).toHaveLength(4);
    expect(modelLine).not.toMatch(/[‪-‮⁦-⁩]/);
  });

  it("declining leaves exactly one request, the refused one, and nothing persisted", async () => {
    const { invokeFn, calls } = backend("m");
    const store = createHarnessStore({ invoke: invokeFn, confirm: async () => false });
    await expect(store.generateCommitMessage("/repo")).rejects.toMatchObject({
      kind: "ai_endpoint_unconfirmed",
    });
    expect(calls).toHaveLength(1);
    expect(calls[0].baseUrl ?? null).toBeNull();
    expect(get(store).preferred).toBeNull();
  });

  it("a confirm dialog that throws sends nothing", async () => {
    const { invokeFn, calls } = backend("m");
    const store = createHarnessStore({
      invoke: invokeFn,
      confirm: async () => {
        throw new Error("dialog closed");
      },
    });
    await expect(store.generateCommitMessage("/repo")).rejects.toBeTruthy();
    expect(calls.filter((c) => c.baseUrl)).toHaveLength(0);
  });

  it("two concurrent generations ask once per refusal, never send before an answer", async () => {
    const { invokeFn, calls } = backend("m");
    let asked = 0;
    const store = createHarnessStore({
      invoke: invokeFn,
      confirm: async () => {
        asked += 1;
        return false;
      },
    });
    await Promise.allSettled([
      store.generateCommitMessage("/repo"),
      store.generateCommitMessage("/repo"),
    ]);
    expect(calls.filter((c) => c.baseUrl)).toHaveLength(0);
    // One dialog per user-visible action is acceptable; a stampede is not.
    expect(asked).toBeLessThanOrEqual(1);
  });
});
