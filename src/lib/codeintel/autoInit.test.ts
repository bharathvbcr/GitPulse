import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { blockedOnMissingTool, createAutoInit } from "./autoInit";
import type { InitReport } from "./types";

function report(overrides: Partial<InitReport> = {}): InitReport {
  return {
    repo: "/a",
    state_dir: "/a/.devmap",
    exclude: { status: "added", file: "/a/.git/info/exclude", pattern: "/.devmap/" },
    workspace_registry: "/a/.devmap/workspace.json",
    workspace_reason: null,
    devmap_available: true,
    ...overrides,
  };
}

function scope(activeKey: string | null, retainedKeys: string[], visible = true) {
  return { activeKey, retainedKeys, visible };
}

describe("autoInit", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("initializes the active repository with the whole open-tab set", async () => {
    const initialize = vi.fn(async () => report());
    const index = createAutoInit({ debounceMs: 0, initialize });
    index.setScope(scope("/a", ["/a", "/b"]));
    await vi.advanceTimersByTimeAsync(0);
    expect(initialize).toHaveBeenCalledWith("/a", ["/a", "/b"]);
    expect(index.get("/a").report?.workspace_registry).toBe("/a/.devmap/workspace.json");
    index.reset();
  });

  it("does not re-ask while the open-tab set is unchanged", async () => {
    const initialize = vi.fn(async () => report());
    const index = createAutoInit({ debounceMs: 0, initialize });
    index.setScope(scope("/a", ["/a", "/b"]));
    await vi.advanceTimersByTimeAsync(0);
    // Scope objects churn on every visibility change and tab render; only a
    // real change to the set may cost a filesystem write and a registry lock.
    index.setScope(scope("/a", ["/b", "/a"]));
    index.setScope(scope("/a", ["/a", "/b"], false));
    await vi.advanceTimersByTimeAsync(10);
    expect(initialize).toHaveBeenCalledTimes(1);
    index.reset();
  });

  it("re-syncs when a repository is opened or closed", async () => {
    const initialize = vi.fn(async () => report());
    const index = createAutoInit({ debounceMs: 0, initialize });
    index.setScope(scope("/a", ["/a"]));
    await vi.advanceTimersByTimeAsync(0);
    index.setScope(scope("/a", ["/a", "/b"]));
    await vi.advanceTimersByTimeAsync(0);
    expect(initialize).toHaveBeenNthCalledWith(2, "/a", ["/a", "/b"]);
    index.reset();
  });

  it("coalesces a burst of scope changes into one call", async () => {
    const initialize = vi.fn(async () => report());
    const index = createAutoInit({ debounceMs: 50, initialize });
    index.setScope(scope("/a", ["/a"]));
    index.setScope(scope("/a", ["/a", "/b"]));
    index.setScope(scope("/a", ["/a", "/b", "/c"]));
    await vi.advanceTimersByTimeAsync(50);
    expect(initialize).toHaveBeenCalledTimes(1);
    expect(initialize).toHaveBeenCalledWith("/a", ["/a", "/b", "/c"]);
    index.reset();
  });

  it("retries after a failure instead of remembering it as done", async () => {
    const initialize = vi
      .fn<(repo: string, open: string[]) => Promise<InitReport>>()
      .mockRejectedValueOnce(new Error("locked"))
      .mockResolvedValue(report());
    const warn = vi.fn();
    const index = createAutoInit({ debounceMs: 0, initialize, warn });
    index.setScope(scope("/a", ["/a"]));
    await vi.advanceTimersByTimeAsync(0);
    expect(index.get("/a").error).toBe("locked");
    expect(warn).toHaveBeenCalledWith("devcouncil-init", "/a: locked");

    // The same scope again must retry, because nothing was applied.
    index.setScope(scope("/a", ["/a"]));
    await vi.advanceTimersByTimeAsync(0);
    expect(initialize).toHaveBeenCalledTimes(2);
    expect(index.get("/a").error).toBeNull();
    index.reset();
  });

  it("reports a refusal instead of letting it pass silently", async () => {
    const warn = vi.fn();
    const index = createAutoInit({
      debounceMs: 0,
      warn,
      initialize: async () =>
        report({
          exclude: { status: "refused", reason: "a later rule re-includes it" },
          workspace_registry: null,
          workspace_reason: "the DevMap state directory is not ignored",
        }),
    });
    index.setScope(scope("/a", ["/a"]));
    await vi.advanceTimersByTimeAsync(0);
    expect(warn).toHaveBeenCalledWith(
      "devcouncil-init",
      expect.stringContaining("could not be hidden from git status"),
    );
    expect(warn).toHaveBeenCalledWith(
      "devcouncil-init",
      expect.stringContaining("cross-repository search has no registry"),
    );
    // A refusal is still a completed answer: it must not spin.
    index.setScope(scope("/a", ["/a"]));
    await vi.advanceTimersByTimeAsync(10);
    expect(warn).toHaveBeenCalledTimes(2);
    index.reset();
  });

  it("forgets a repository once its tab closes", async () => {
    const initialize = vi.fn(async () => report());
    const index = createAutoInit({ debounceMs: 0, initialize });
    index.setScope(scope("/a", ["/a"]));
    await vi.advanceTimersByTimeAsync(0);
    index.setScope(scope("/b", ["/b"]));
    await vi.advanceTimersByTimeAsync(0);
    expect(index.get("/a").report).toBeNull();

    // Re-opening it is a fresh repository as far as this controller knows, so
    // a clone that was re-created on disk is initialized again.
    index.setScope(scope("/a", ["/a"]));
    await vi.advanceTimersByTimeAsync(0);
    expect(initialize).toHaveBeenCalledTimes(3);
    index.reset();
  });

  it("does nothing without an active repository", async () => {
    const initialize = vi.fn(async () => report());
    const index = createAutoInit({ debounceMs: 0, initialize });
    index.setScope(scope(null, []));
    await vi.advanceTimersByTimeAsync(10);
    expect(initialize).not.toHaveBeenCalled();
    index.reset();
  });

  it("never runs two initializations at once", async () => {
    let active = 0;
    let peak = 0;
    const initialize = vi.fn(async () => {
      active += 1;
      peak = Math.max(peak, active);
      await Promise.resolve();
      active -= 1;
      return report();
    });
    const index = createAutoInit({ debounceMs: 0, initialize });
    index.setScope(scope("/a", ["/a"]));
    await vi.advanceTimersByTimeAsync(0);
    index.setScope(scope("/a", ["/a", "/b"]));
    index.setScope(scope("/a", ["/a", "/b", "/c"]));
    await vi.advanceTimersByTimeAsync(0);
    expect(peak).toBe(1);
    index.reset();
  });
  it("retries a repository that was initialized before devmap existed", async () => {
    // The gap this closes: with no devmap on PATH the registry step is skipped
    // and the run still succeeds, so the old guard remembered it as done.
    // Installing devmap then changed nothing for any repository already open —
    // cross-repository search kept answering "nothing" for the rest of the
    // session, for a reason that had just been fixed.
    const initialize = vi
      .fn<(repo: string, open: string[]) => Promise<InitReport>>()
      .mockResolvedValueOnce(
        report({
          devmap_available: false,
          workspace_registry: null,
          workspace_reason: "devmap is not installed and this repository has no DevMap state yet",
        }),
      )
      .mockResolvedValue(report());
    const index = createAutoInit({ debounceMs: 0, initialize });
    index.setScope(scope("/a", ["/a"]));
    await vi.advanceTimersByTimeAsync(0);
    expect(index.get("/a").report?.workspace_registry).toBeNull();

    index.onToolsChanged();
    await vi.advanceTimersByTimeAsync(0);
    expect(initialize).toHaveBeenCalledTimes(2);
    expect(index.get("/a").report?.workspace_registry).toBe("/a/.devmap/workspace.json");
    index.reset();
  });

  it("does not re-initialize a finished repository when the tools are probed", async () => {
    // Probes are frequent — three surfaces call the tool status on mount — so
    // a blanket invalidation would cost a filesystem write and a registry lock
    // every time a panel opened.
    const initialize = vi.fn(async () => report());
    const index = createAutoInit({ debounceMs: 0, initialize });
    index.setScope(scope("/a", ["/a"]));
    await vi.advanceTimersByTimeAsync(0);
    index.onToolsChanged();
    index.onToolsChanged();
    await vi.advanceTimersByTimeAsync(10);
    expect(initialize).toHaveBeenCalledTimes(1);
    index.reset();
  });

  it("stops redoing a repository that is short for a reason no install fixes", async () => {
    // A refused exclude is not repaired by installing anything, and a probe
    // that kept clearing it would re-run initialization — and re-emit its
    // warning — on every Settings open, forever.
    const initialize = vi.fn(async () =>
      report({
        exclude: { status: "refused", reason: "a later rule re-includes it" },
        workspace_registry: null,
        workspace_reason: "the DevMap state directory is not ignored",
      }),
    );
    const index = createAutoInit({ debounceMs: 0, initialize, warn: vi.fn() });
    index.setScope(scope("/a", ["/a"]));
    await vi.advanceTimersByTimeAsync(0);
    index.onToolsChanged();
    await vi.advanceTimersByTimeAsync(10);
    expect(initialize).toHaveBeenCalledTimes(1);
    index.reset();
  });

  it("clears a blocked repository only once, then leaves it alone", async () => {
    // Self-limiting: the second run reports devmap present but fails for
    // another reason, so it is no longer blocked and further probes are free.
    const initialize = vi
      .fn<(repo: string, open: string[]) => Promise<InitReport>>()
      .mockResolvedValueOnce(report({ devmap_available: false, workspace_registry: null }))
      .mockResolvedValue(
        report({ workspace_registry: null, workspace_reason: "registry lock is held" }),
      );
    const index = createAutoInit({ debounceMs: 0, initialize, warn: vi.fn() });
    index.setScope(scope("/a", ["/a"]));
    await vi.advanceTimersByTimeAsync(0);
    index.onToolsChanged();
    await vi.advanceTimersByTimeAsync(0);
    expect(initialize).toHaveBeenCalledTimes(2);
    index.onToolsChanged();
    index.onToolsChanged();
    await vi.advanceTimersByTimeAsync(10);
    expect(initialize).toHaveBeenCalledTimes(2);
    index.reset();
  });

  it("has nothing to redo before any repository is open", async () => {
    const initialize = vi.fn(async () => report());
    const index = createAutoInit({ debounceMs: 0, initialize });
    index.onToolsChanged();
    await vi.advanceTimersByTimeAsync(10);
    expect(initialize).not.toHaveBeenCalled();
    index.reset();
  });

  it("re-runs the whole open-tab set, not just the repository that was blocked", async () => {
    const initialize = vi
      .fn<(repo: string, open: string[]) => Promise<InitReport>>()
      .mockResolvedValueOnce(report({ devmap_available: false, workspace_registry: null }))
      .mockResolvedValue(report());
    const index = createAutoInit({ debounceMs: 0, initialize });
    index.setScope(scope("/a", ["/a", "/b"]));
    await vi.advanceTimersByTimeAsync(0);
    index.onToolsChanged();
    await vi.advanceTimersByTimeAsync(0);
    expect(initialize).toHaveBeenNthCalledWith(2, "/a", ["/a", "/b"]);
    index.reset();
  });
});

describe("blockedOnMissingTool", () => {
  it("is the absence of devmap and nothing else", () => {
    expect(blockedOnMissingTool(report({ devmap_available: false }))).toBe(true);
    expect(blockedOnMissingTool(report())).toBe(false);
  });

  it("does not call a refused exclude a tool problem", () => {
    const refused = report({
      exclude: { status: "refused", reason: "a later rule re-includes it" },
      workspace_registry: null,
    });
    expect(blockedOnMissingTool(refused)).toBe(false);
  });

  it("does not call an unwritten registry a tool problem when devmap is there", () => {
    expect(blockedOnMissingTool(report({ workspace_registry: null }))).toBe(false);
  });
});
