import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { claimedFileOpen, createTerminalLaunchRequests, agentPromptArgs, resumeArgs, PROMPT_LAUNCHERS, TERMINAL_SESSION_CANCELED } from "./launchRequests";
import { TERMINAL_SESSION_PENDING } from "./sessionMessages";
import { PROVIDER_LABELS } from "../workbench/taskHandoff";

afterEach(() => vi.useRealTimers());

describe("resumeArgs", () => {
  const ID = "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c11";

  it("returns empty prompt args with no prompt, leaving --resume / --conversation to the Rust owner", () => {
    expect(resumeArgs(ID)).toEqual([]);
    expect(resumeArgs(ID, undefined, "agy")).toEqual([]);
    expect(resumeArgs("__agy_continue__", undefined, "agy")).toEqual([]);
  });

  it("puts the prompt after `--` for Claude Code and `--prompt-interactive` for Antigravity, so no flag or shell word in it is read as one", () => {
    // Prompt delivery only; typed `resume: { session_id, fork }` carries the
    // conversation id so the Rust backend owns `--resume` / `--conversation`.
    for (const text of ["fix it", "--settings {}", "-p", "$(touch /tmp/x) `id` ; rm -rf ~", "multi\nline\ttext ✓"]) {
      expect(resumeArgs(ID, text)).toEqual(["--", text]);
      expect(resumeArgs(ID, text, "agy")).toEqual(["--prompt-interactive", text]);
    }
  });

  it("refuses what the initial-prompt path refuses, and a session id that is not one", () => {
    for (const bad of ["", "  \n", "a\0b", "x".repeat(16001)]) expect(() => resumeArgs(ID, bad)).toThrow();
    for (const id of ["", "../etc", "6f1c2a7e", `${ID}x`]) expect(() => resumeArgs(id, "hi")).toThrow();
    expect(() => resumeArgs("__agy_continue__", "hi", "claude")).toThrow();
    expect(() => resumeArgs("--settings")).toThrow();
  });
});

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

  it("delivers to the panel whose checkout is the same one spelled differently", async () => {
    // Panels are bound to their tab's path; the palette asks with whatever
    // path it holds. One checkout spelled two ways must still be one.
    const requests = createTerminalLaunchRequests();
    const launched = requests.request("/Work/App/", "claude", "Look");
    expect(requests.take("/work/app", { caseInsensitive: false })).toBeNull();
    const request = requests.take("/work/app", { caseInsensitive: true });
    expect(request?.prompt).toBe("Look");
    request?.complete();
    await expect(launched).resolves.toBeUndefined();
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
    await expect(requests.request("/a", "codex", "Another")).rejects.toThrow(TERMINAL_SESSION_PENDING);
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
    await expect(launched).rejects.toThrow(TERMINAL_SESSION_CANCELED);
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

it("enqueues repository-contained file split requests and rejects traversal paths", async () => {
  const requests = createTerminalLaunchRequests();
  await expect(requests.requestFile("/repo", "../secret.txt")).rejects.toThrow("repository-relative");
  const pending = requests.requestFile("/repo", "./src/lib/tabs.ts", 25, 3);
  const claimed = requests.take("/repo");
  expect(claimed).toMatchObject({ launcher: "file", filePath: "src/lib/tabs.ts", line: 25, column: 3 });
  expect(claimed?.fileKeep).toBeUndefined();
  claimed?.complete();
  await expect(pending).resolves.toBeUndefined();
});

it("carries a double-click's keep on a file request", async () => {
  const requests = createTerminalLaunchRequests();
  const pending = requests.requestFile("/repo", "src/a.ts", null, null, undefined, { mode: "tab", keep: true });
  const claimed = requests.take("/repo");
  expect(claimed).toMatchObject({ launcher: "file", filePath: "src/a.ts", fileOpenMode: "tab", fileKeep: true });
  claimed?.complete();
  await expect(pending).resolves.toBeUndefined();
});

describe("claimedFileOpen: which checkout a sidebar file request opens and keeps in", () => {
  // The sidebar names a repository, and the panel claims it by checkout
  // identity (`take`), so the file belongs to the panel's own checkout. The
  // active tab may be an agent hosted in another checkout of the repository
  // (`TerminalTab.checkout`); the file must not follow it there.
  const panelSource = readFileSync(
    join(dirname(fileURLToPath(import.meta.url)), "../components/TerminalPanel.svelte"),
    "utf8",
  );
  const request = (extra: Record<string, unknown>) =>
    ({ launcher: "file", filePath: "src/a.ts", line: 3, column: null, ...extra }) as Parameters<typeof claimedFileOpen>[0];

  it("opens a tab and a split in the panel's own checkout, one value for opening and keeping", () => {
    for (const fileOpenMode of ["tab", "split"]) {
      const plan = claimedFileOpen(request({ fileOpenMode, fileKeep: true }));
      expect(plan).toMatchObject({ keep: true });
      expect(plan?.checkout).toBeUndefined();
    }
  });

  it("the panel's claim path takes the checkout from the plan, never from the active tab", () => {
    // The claim is a Svelte `$effect`, which vitest does not run, so the
    // wiring is pinned in the source: the active tab was where a tab-mode open
    // and a keep read their checkout before.
    const claimStart = panelSource.indexOf("const claimed = terminalLaunchRequests.take(here, pathOpts);");
    expect(claimStart).toBeGreaterThan(-1);
    const effect = panelSource.slice(claimStart, panelSource.indexOf("const createdId = newTab(", claimStart));
    expect(effect).toContain("claimedFileOpen(claimed)");
    expect(effect).not.toContain("activeTab");
    const tabOpener = panelSource.slice(
      panelSource.indexOf("function openFileTabFor("),
      panelSource.indexOf("function toggleExplorerSidebar()"),
    );
    expect(tabOpener).not.toContain("activeTab?.checkout");
  });

  it("carries the mode, the reveal position and the focus default of each mode", () => {
    expect(claimedFileOpen(request({ fileOpenMode: "tab" }))).toMatchObject({ mode: "tab", filePath: "src/a.ts", line: 3, column: null, focus: true, keep: false });
    expect(claimedFileOpen(request({}))).toMatchObject({ mode: "split", focus: false });
    expect(claimedFileOpen(request({ fileOpenMode: "split", fileFocus: true }))).toMatchObject({ focus: true });
    expect(claimedFileOpen(request({ fileOpenMode: "tab", fileFocus: false }))).toMatchObject({ focus: false });
  });

  it("is null for a request that is no file open", () => {
    expect(claimedFileOpen({ launcher: "shell" })).toBeNull();
    expect(claimedFileOpen({ launcher: "file" })).toBeNull();
  });
});

it("validates fork resume options fail-closed and delivers resume and custom name on take", async () => {
  const requests = createTerminalLaunchRequests();
  const ID = "6f1c2a7e-0d4b-4c1e-9a55-3b0e8f2d9c11";
  await expect(
    requests.request("/repo", "agy", undefined, undefined, { resume: { sessionId: ID, mode: "edit", fork: true } }),
  ).rejects.toThrow("Claude Code");
  await expect(
    requests.request("/repo", "claude", undefined, undefined, { resume: { sessionId: "__agy_continue__", mode: null, fork: true } }),
  ).rejects.toThrow("Claude Code");
  await expect(
    requests.request("/repo", "codex", undefined, undefined, { resume: { sessionId: ID, mode: null, fork: false } }),
  ).rejects.toThrow("Claude Code or Antigravity");

  const pending = requests.request("/repo-fork", "claude", undefined, undefined, {
    resume: { sessionId: ID, mode: "edit", fork: true },
    name: "  lane-1 (forked)  ",
  });
  const claimed = requests.take("/repo-fork");
  expect(claimed).toMatchObject({
    repoPath: "/repo-fork",
    launcher: "claude",
    resume: { sessionId: ID, mode: "edit", fork: true },
    name: "lane-1 (forked)",
  });
  claimed?.complete();
  await expect(pending).resolves.toBeUndefined();
});

it("carries a commit purpose to the panel and leaves an ordinary launch without one", async () => {
  const requests = createTerminalLaunchRequests();
  const commit = requests.request("/repo", "claude", "Commit", undefined, { purpose: "commit" });
  const claimed = requests.take("/repo");
  expect(claimed?.purpose).toBe("commit");
  claimed?.complete();
  await commit;

  const plain = requests.request("/repo", "claude", "Hello");
  const plainClaimed = requests.take("/repo");
  expect(plainClaimed).not.toBeNull();
  expect("purpose" in (plainClaimed ?? {})).toBe(false);
  plainClaimed?.complete();
  await plain;
});
