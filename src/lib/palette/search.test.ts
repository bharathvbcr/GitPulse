import { afterEach, describe, expect, it, vi } from "vitest";
import { emptySearch, scheduleSearch, SEARCH_DELAY_MS, SEARCH_TIMEOUT_MS, workspaceRoot, type SearchRequest, type SearchResult, searchDependencies } from "./search";
import type { CodeintelResponse, CodeintelSymbolHit, WorkspaceSearchResult } from "../codeintel/types";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const symbols = (name: string): CodeintelResponse<CodeintelSymbolHit> => ({ available: true, items: [{ symbol_name: name, file_path: "test.ts", kind: "Function", span_start_line: 1, span_end_line: 2, source_span: "function test() {}", score: 1 }], shown: 1, total: 1, truncated: false });
const request: SearchRequest = { mode: "symbols", repoPath: "/repo", text: "test", semantic: false };
const workspace: WorkspaceSearchResult = { items: [], repos_queried: 1, unavailable: [], total: 0, shown: 0, hidden: 0, truncated: false, semantic: false };
function dependencies() {
  return {
    symbols: vi.fn<typeof searchDependencies.symbols>().mockResolvedValue(symbols("test")),
    files: vi.fn<typeof searchDependencies.files>().mockResolvedValue(["README.md"]),
    workspace: vi.fn<typeof searchDependencies.workspace>().mockResolvedValue(workspace),
    repos: vi.fn<typeof searchDependencies.repos>().mockResolvedValue({ version: 1, registry_root: "/repo", registry_path: "/repo/workspace.json", repos: [] }),
  };
}
afterEach(() => vi.useRealTimers());

describe("palette search lifecycle", () => {
  it("debounces and cancels before any IPC is started", async () => {
    vi.useFakeTimers(); const deps = dependencies(); const publish = vi.fn();
    const stop = scheduleSearch(request, publish, deps);
    expect(publish).toHaveBeenCalledWith({ ...emptySearch(), loading: true });
    stop(); await vi.runAllTimersAsync();
    expect(deps.symbols).not.toHaveBeenCalled();
    expect(publish).toHaveBeenCalledTimes(1);
  });
  it.each(["resolve", "reject"] as const)("ignores a stale %s after query, mode, repository change or close", async settle => {
    vi.useFakeTimers(); const deps = dependencies(); const pending = deferred<CodeintelResponse<CodeintelSymbolHit>>();
    deps.symbols.mockReturnValueOnce(pending.promise);
    const publish = vi.fn(); const stop = scheduleSearch(request, publish, deps);
    await vi.advanceTimersByTimeAsync(SEARCH_DELAY_MS);
    stop(); scheduleSearch({ ...request, text: "new" }, publish, deps);
    await vi.advanceTimersByTimeAsync(SEARCH_DELAY_MS);
    if (settle === "resolve") pending.resolve(symbols("stale")); else pending.reject(Error("stale failure"));
    await vi.runAllTimersAsync();
    expect(publish.mock.lastCall?.[0].symbols[0].symbol_name).toBe("test");
    expect(publish).toHaveBeenCalledTimes(3);
  });
  it("times out once, ignores late completion and supports a separate retry", async () => {
    vi.useFakeTimers(); const deps = dependencies(); const pending = deferred<CodeintelResponse<CodeintelSymbolHit>>();
    deps.symbols.mockReturnValueOnce(pending.promise);
    const values: SearchResult[] = [];
    scheduleSearch(request, result => values.push(result), deps);
    await vi.advanceTimersByTimeAsync(SEARCH_DELAY_MS + SEARCH_TIMEOUT_MS);
    expect(values.at(-1)?.note).toContain("timed out");
    expect(values.at(-1)?.failed).toBe(true);
    pending.resolve(symbols("late")); await vi.runAllTimersAsync();
    expect(values).toHaveLength(2);
    scheduleSearch(request, result => values.push(result), deps); await vi.runAllTimersAsync();
    expect(values.at(-1)?.symbols[0].symbol_name).toBe("test");
  });
  it("distinguishes unavailable, error, partial and empty responses", async () => {
    vi.useFakeTimers(); const deps = dependencies(); const publish = vi.fn();
    deps.symbols.mockResolvedValueOnce({ ...symbols("no"), available: false, reason: "No map" });
    scheduleSearch(request, publish, deps); await vi.runAllTimersAsync();
    expect(publish.mock.lastCall?.[0]).toMatchObject({ failed: true, note: "No map", symbols: [] });
    deps.symbols.mockRejectedValueOnce(Error("Offline"));
    scheduleSearch(request, publish, deps); await vi.runAllTimersAsync();
    expect(publish.mock.lastCall?.[0].note).toContain("Offline");
    deps.symbols.mockResolvedValueOnce({ ...symbols("part"), truncated: true, total: 200 });
    scheduleSearch(request, publish, deps); await vi.runAllTimersAsync();
    expect(publish.mock.lastCall?.[0]).toMatchObject({ failed: false, note: "1 of 200 symbol matches returned. Refine your search for more." });
    deps.symbols.mockResolvedValueOnce({ available: true, items: [], total: 0, shown: 0, truncated: false });
    scheduleSearch(request, publish, deps); await vi.runAllTimersAsync();
    expect(publish.mock.lastCall?.[0]).toEqual(emptySearch());
  });
  it("loads files through their existing bounded IPC owner and reports errors", async () => {
    vi.useFakeTimers(); const deps = dependencies(); const publish = vi.fn();
    scheduleSearch({ ...request, mode: "files" }, publish, deps); await vi.runAllTimersAsync();
    expect(deps.files).toHaveBeenCalledWith("/repo");
    expect(publish.mock.lastCall?.[0].files).toEqual(["README.md"]);
    deps.files.mockRejectedValueOnce(Error("File count limit"));
    scheduleSearch({ ...request, mode: "files" }, publish, deps); await vi.runAllTimersAsync();
    expect(publish.mock.lastCall?.[0]).toMatchObject({ failed: true, files: [], note: "Search failed: File count limit" });
  });
  it("retains workspace partial coverage and TF-IDF semantics even with no hits", async () => {
    vi.useFakeTimers(); const deps = dependencies(); const publish = vi.fn();
    deps.workspace.mockResolvedValueOnce({ ...workspace, unavailable: [{ repo: "other", reason: "Missing index" }], truncated: true, total: 10 });
    scheduleSearch({ ...request, mode: "workspace", semantic: true }, publish, deps); await vi.runAllTimersAsync();
    const result = publish.mock.lastCall?.[0];
    expect(result.failed).toBe(true);
    expect(result.note).toContain("other: Missing index");
    expect(result.note).toContain("0 of 10");
    expect(result.note).toContain("TF-IDF");
    expect(deps.workspace).toHaveBeenCalledWith("/repo", "test", 8000, true);
  });
  it("reports an empty or unreadable workspace registry", async () => {
    vi.useFakeTimers(); const deps = dependencies(); const publish = vi.fn();
    deps.workspace.mockResolvedValueOnce({ ...workspace, repos_queried: 0 });
    scheduleSearch({ ...request, mode: "workspace" }, publish, deps); await vi.runAllTimersAsync();
    expect(publish.mock.lastCall?.[0].note).toContain("No registered repositories");
    deps.repos.mockRejectedValueOnce(Error("Registry unreadable"));
    scheduleSearch({ ...request, mode: "workspace" }, publish, deps); await vi.runAllTimersAsync();
    expect(publish.mock.lastCall?.[0]).toMatchObject({ failed: true, workspace: [], note: "Search failed: Registry unreadable" });
  });
  it("requires a unique exact registry name instead of guessing a matching tab", () => {
    const repo = { name: "apps/GitPulse", root: "/source/one", db: "", db_path: "" };
    expect(workspaceRoot("apps/GitPulse", [repo])).toBe("/source/one");
    expect(workspaceRoot("GitPulse", [repo])).toBeNull();
    expect(workspaceRoot("apps/gitpulse", [repo])).toBeNull();
    expect(workspaceRoot(repo.name, [repo, { ...repo, root: "/source/two" }])).toBeNull();
  });
});
