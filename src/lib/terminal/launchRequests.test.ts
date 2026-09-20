import { afterEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { createTerminalLaunchRequests, agentPromptArgs, PROMPT_LAUNCHERS } from "./launchRequests";
import { PROVIDER_LABELS } from "../workbench/taskHandoff";

afterEach(() => vi.useRealTimers());

describe("terminal agent launch requests", () => {
  it("delivers once to the requested repository and acknowledges tab creation", async () => {
    const requests = createTerminalLaunchRequests();
    const launched = requests.request("/a", "codex", "Generate coverage");
    expect(requests.take("/b")).toBeNull();
    expect(get(requests)?.repoPath).toBe("/a");
    const request = requests.take("/a");
    expect(request?.prompt).toBe("Generate coverage");
    expect(requests.take("/a")).toBeNull();
    request?.complete();
    await expect(launched).resolves.toBeUndefined();
    expect(get(requests)).toBeNull();
  });

  it("carries a promptless launch for every launcher, so one channel serves 'new shell' too", async () => {
    // Widened rather than duplicated: adding a fourth request store beside
    // this one would mean a second copy of claim, capacity refusal and the
    // lazily-mounted-panel handoff.
    for (const kind of ["shell", "claude", "manvi", "codex"] as const) {
      const requests = createTerminalLaunchRequests();
      const launched = requests.request("/a", kind);
      const request = requests.take("/a");
      expect(request?.launcher).toBe(kind);
      expect(request?.prompt).toBeUndefined();
      request?.complete();
      await expect(launched).resolves.toBeUndefined();
    }
  });

  it("still refuses a prompt for a launcher that cannot take one", async () => {
    const requests = createTerminalLaunchRequests();
    // Validated at the boundary, before any panel is asked to open a tab.
    const refused = await requests
      .request("/a", "shell", "do the thing")
      .then(() => null, (error: unknown) => error as Error);
    expect(refused?.message).toContain("Initial prompts require");
    // Derived, not transcribed. The refusal hand-lists the prompt-capable
    // launchers beside PROMPT_LAUNCHERS itself, so the two drift apart
    // silently: adding Grok and Antigravity left this test asserting a
    // sentence naming only two of the four. Asserting that every launcher in
    // the list is named makes the next addition fail here instead.
    for (const launcher of PROMPT_LAUNCHERS) {
      expect(refused?.message).toContain(PROVIDER_LABELS[launcher]);
    }
    expect(get(requests)).toBeNull();
    expect(requests.take("/a")).toBeNull();
  });

  it("refuses a launch with no repository", async () => {
    const requests = createTerminalLaunchRequests();
    await expect(requests.request("   ", "shell")).rejects.toThrow("Open a repository");
    expect(get(requests)).toBeNull();
  });

  it("rejects duplicate pending launches and reports capacity refusal", async () => {
    const requests = createTerminalLaunchRequests();
    const launched = requests.request("/a", "claude", "Generate coverage");
    await expect(requests.request("/a", "codex", "Another")).rejects.toThrow("already pending");
    requests.take("/a")?.complete("All terminal sessions are in use");
    await expect(launched).rejects.toThrow("All terminal sessions");
  });

  it("expires undelivered requests without launching later", async () => {
    vi.useFakeTimers();
    const requests = createTerminalLaunchRequests();
    const launched = requests.request("/a", "codex", "Generate coverage");
    const result = expect(launched).rejects.toThrow("did not open");
    await vi.advanceTimersByTimeAsync(10000);
    await result;
    expect(requests.take("/a")).toBeNull();
  });

  it("cancels when the source page leaves before a terminal accepts", async () => {
    const requests = createTerminalLaunchRequests();
    const controller = new AbortController();
    const launched = requests.request("/a", "codex", "Generate coverage", controller.signal);
    controller.abort();
    await expect(launched).rejects.toThrow("cancelled");
    expect(requests.take("/a")).toBeNull();
  });

  it("passes hostile text as one literal CLI argument, with no shell or permission flags", () => {
    const prompt = "--bad\n$(touch /tmp/nope); `anything` \"quoted\" 世界";
    expect(agentPromptArgs("codex", prompt)).toEqual(["--", prompt]);
    expect(agentPromptArgs("claude", prompt)).toEqual(["--", prompt]);
    expect(agentPromptArgs("grok", prompt)).toEqual(["--", prompt]);
    expect(agentPromptArgs("agy", prompt)).toEqual(["--prompt-interactive", prompt]);
    expect(agentPromptArgs("shell")).toBeNull();
    expect(() => agentPromptArgs("shell", prompt)).toThrow("Claude Code, Codex, Grok, or Antigravity");
    expect(() => agentPromptArgs("manvi", prompt)).toThrow("Claude Code, Codex, Grok, or Antigravity");
  });

  it("refuses empty, NUL and oversized inputs before publishing a request", async () => {
    const requests = createTerminalLaunchRequests();
    for (const prompt of [" ", "a\0b", "界".repeat(6000)]) {
      await expect(requests.request("/a", "codex", prompt)).rejects.toThrow();
      expect(get(requests)).toBeNull();
    }
    await expect(requests.request("", "claude", "Generate")).rejects.toThrow("repository");
  });
});

it("retains the exact agent tab across page changes, updates its status, and forgets closed tabs", () => {
  const requests = createTerminalLaunchRequests();
  const revealA = vi.fn(), revealB = vi.fn();
  requests.remember({ id: "a", repoPath: "/a", launcher: "codex", status: "starting", reveal: revealA });
  requests.remember({ id: "b", repoPath: "/b", launcher: "claude", status: "starting", reveal: revealB });
  requests.update("a", "exited");
  const session = get(requests.sessions).find(row => row.repoPath === "/a");
  expect(session?.status).toBe("exited");
  session?.reveal();
  expect(revealA).toHaveBeenCalledTimes(1);
  expect(revealB).not.toHaveBeenCalled();
  requests.forget("a");
  expect(get(requests.sessions).map(row => row.id)).toEqual(["b"]);
  for (let i = 0; i < 30; i++) requests.remember({ id: `c${i}`, repoPath: "/c", launcher: "codex", status: "starting", reveal: revealA });
  expect(get(requests.sessions)).toHaveLength(16);
});
