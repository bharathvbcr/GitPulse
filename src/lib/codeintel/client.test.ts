import { describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import {
  getCodeintelStatus,
  searchSymbols,
  getImpact,
  getImpactAtRung,
  getImpactLayered,
  getImpactLayeredMany,
  getDeadSymbols,
  getDependencies,
  traceBetween,
  getNeighbors,
  exploreSymbol,
  getAffectedTests,
  getClones,
  cancelCodeintelQuery,
  newCodeintelCancelToken,
  buildDevmap,
  refreshDevmap,
  maybeRefreshDevmap,
  getDevmapCliStatus,
  getDevmapRepoMap,
  previewDevmapEdit,
  previewDevmapEdits,
  getCodeGraphViz,
  getMapPreviewViz,
  syncWorkspaceTabs,
  searchWorkspaceSymbols,
  listWorkspaceRepos,
  registerWorkspaceRepo,
  unregisterWorkspaceRepo,
  getWorkspaceLinkCandidates,
} from "./client";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

describe("codeintel client", () => {
  it("queries codeintel status", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      available: true,
      db_path: "/repo/.devcouncil/codeintel/devmap.sqlite",
      generation_id: 1,
      total_files: 42,
      total_symbols: 200,
      total_edges: 500,
      reason: null,
    });

    const status = await getCodeintelStatus("/repo");
    expect(invoke).toHaveBeenCalledWith("cmd_codeintel_status", { repoPath: "/repo" });
    expect(status.available).toBe(true);
    expect(status.generation_id).toBe(1);
  });

  it("searches symbols with budget", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      available: true,
      reason: null,
      items: [
        {
          symbol_name: "testFn",
          file_path: "src/test.rs",
          kind: "function",
          span_start_line: 10,
          span_end_line: 20,
          source_span: "fn testFn() {}",
          score: 1.0,
        },
      ],
      total: 1,
      shown: 1,
      truncated: false,
    });

    const res = await searchSymbols("/repo", "testFn", 500);
    expect(invoke).toHaveBeenCalledWith("cmd_codeintel_search", {
      repoPath: "/repo",
      query: "testFn",
      tokenBudget: 500,
    });
    expect(res.items).toHaveLength(1);
    expect(res.items[0].symbol_name).toBe("testFn");
  });

  it("computes impact and callers", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      available: true,
      reason: null,
      items: [
        {
          source_file: "src/caller.rs",
          target_file: "src/callee.rs",
          source_symbol: "caller_fn",
          target_symbol: "callee_fn",
          confidence: 0.95,
        },
      ],
      total: 1,
      shown: 1,
      truncated: false,
    });

    const res = await getImpact("/repo", "callee_fn");
    expect(invoke).toHaveBeenCalledWith("cmd_codeintel_impact", {
      repoPath: "/repo",
      target: "callee_fn",
      tokenBudget: undefined,
    });
    expect(res.items[0].confidence).toBe(0.95);
  });

  it("queries dead symbols", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      available: true,
      reason: null,
      items: [
        {
          symbol_name: "unused_helper",
          file_path: "src/legacy.rs",
          confidence: 0.9,
          is_exempt: false,
          exemption_reason: null,
        },
      ],
      total: 1,
      shown: 1,
      truncated: false,
    });

    const res = await getDeadSymbols("/repo", 1000);
    expect(invoke).toHaveBeenCalledWith("cmd_codeintel_dead_symbols", {
      repoPath: "/repo",
      tokenBudget: 1000,
    });
    expect(res.items[0].symbol_name).toBe("unused_helper");
  });

  it("queries impact at rung and layered impact for many targets", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      available: true,
      reason: null,
      items: [],
      total: 2,
      shown: 0,
      truncated: false,
      rungs: { deterministic: 1, high: 1, speculative: 0, filtered_out: 3 },
    });
    const atRung = await getImpactAtRung("/repo", "src/a.ts", 20, "high");
    expect(invoke).toHaveBeenCalledWith("cmd_codeintel_impact_at_rung", {
      repoPath: "/repo",
      target: "src/a.ts",
      tokenBudget: 20,
      minRung: "high",
    });
    expect(atRung.rungs?.filtered_out).toBe(3);

    vi.mocked(invoke).mockResolvedValueOnce([]);
    await getImpactLayeredMany("/repo", ["a.ts", "b.ts"], 800);
    expect(invoke).toHaveBeenCalledWith("cmd_codeintel_impact_layered_many", {
      repoPath: "/repo",
      targets: ["a.ts", "b.ts"],
      tokenBudget: 800,
    });
  });

  it("batches preview edits", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      available: true,
      files: [],
      cancelled: false,
    });
    await previewDevmapEdits("/repo", [["src/a.ts", "console.log(1)"]]);
    expect(invoke).toHaveBeenCalledWith("cmd_devmap_preview_many", {
      repoPath: "/repo",
      files: [["src/a.ts", "console.log(1)"]],
    });
  });

  it("loads ranked code-graph and map-preview canvas payloads", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      available: true,
      kind: "code_graph",
      payload: {
        nodes: [],
        links: [],
        counts: { nodes_shown: 0, nodes_total: 0, nodes_truncated: false },
      },
    });
    await getCodeGraphViz("/repo", true, 500);
    expect(invoke).toHaveBeenCalledWith("cmd_devmap_viz", {
      repoPath: "/repo",
      symbols: true,
      maxNodes: 500,
    });

    vi.mocked(invoke).mockResolvedValueOnce({
      available: true,
      kind: "map_preview",
      payload: { nodes: [], links: [] },
    });
    await getMapPreviewViz("/repo");
    expect(invoke).toHaveBeenCalledWith("cmd_devmap_map_preview", { repoPath: "/repo" });
  });

  it("syncs and searches the multi-repo workspace registry", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      version: 1,
      registry_root: "/a",
      registry_path: "/a/.devmap/workspace.json",
      repos: [{ name: "a", root: "/a", db: ".devmap/codeintel/devmap.sqlite", db_path: "/a/.devmap/codeintel/devmap.sqlite" }],
    });
    await syncWorkspaceTabs("/a", ["/a", "/b"]);
    expect(invoke).toHaveBeenCalledWith("cmd_workspace_sync", {
      registryRoot: "/a",
      repoPaths: ["/a", "/b"],
    });

    vi.mocked(invoke).mockResolvedValueOnce({
      items: [],
      repos_queried: 0,
      unavailable: [],
      total: 0,
      shown: 0,
      hidden: 0,
      truncated: false,
      semantic: true,
    });
    await searchWorkspaceSymbols("/a", "Widget", 400, true);
    expect(invoke).toHaveBeenCalledWith("cmd_workspace_search", {
      registryRoot: "/a",
      query: "Widget",
      tokenBudget: 400,
      semantic: true,
    });

    vi.mocked(invoke).mockResolvedValueOnce({ version: 1, registry_root: "/a", registry_path: "/a/.devmap/workspace.json", repos: [] });
    await listWorkspaceRepos("/a");
    expect(invoke).toHaveBeenCalledWith("cmd_workspace_list", { registryRoot: "/a" });

    vi.mocked(invoke).mockResolvedValueOnce({ name: "b", root: "/b", replaced: false, registry_path: "/a/.devmap/workspace.json" });
    await registerWorkspaceRepo("/a", "/b", "b");
    expect(invoke).toHaveBeenCalledWith("cmd_workspace_register", {
      registryRoot: "/a",
      repoPath: "/b",
      name: "b",
    });

    vi.mocked(invoke).mockResolvedValueOnce({ name: "b", removed: true, registry_path: "/a/.devmap/workspace.json" });
    await unregisterWorkspaceRepo("/a", "b");
    expect(invoke).toHaveBeenCalledWith("cmd_workspace_unregister", {
      registryRoot: "/a",
      name: "b",
    });

    vi.mocked(invoke).mockResolvedValueOnce({ links: [], count: 0, repos_considered: 1 });
    await getWorkspaceLinkCandidates("/a");
    expect(invoke).toHaveBeenCalledWith("cmd_workspace_link_candidates", {
      registryRoot: "/a",
    });
  });

  it("covers the remaining codeintel and digmap IPC wrappers", async () => {
    const envelope = {
      available: true,
      reason: null,
      items: [],
      total: 0,
      shown: 0,
      truncated: false,
    };
    vi.mocked(invoke).mockImplementation(async (command) => {
      if (command === "cmd_codeintel_neighbors") return [];
      if (command === "cmd_codeintel_impact_layered_many") return [];
      if (command === "cmd_codeintel_cancel") return true;
      if (String(command).startsWith("cmd_codeintel_")) return envelope;
      return {};
    });

    await getDependencies("/repo", "src/a.ts", 10);
    expect(invoke).toHaveBeenCalledWith("cmd_codeintel_dependencies", {
      repoPath: "/repo",
      filePath: "src/a.ts",
      tokenBudget: 10,
      minRung: undefined,
    });

    await traceBetween("/repo", "a", "b", 20);
    expect(invoke).toHaveBeenCalledWith("cmd_codeintel_trace", {
      repoPath: "/repo",
      from: "a",
      to: "b",
      tokenBudget: 20,
      minRung: undefined,
    });

    await getNeighbors("/repo", ["src/a.ts"], 30);
    expect(invoke).toHaveBeenCalledWith("cmd_codeintel_neighbors", {
      repoPath: "/repo",
      targets: ["src/a.ts"],
      tokenBudget: 30,
      minRung: undefined,
    });

    await exploreSymbol("/repo", "Foo", 40);
    expect(invoke).toHaveBeenCalledWith("cmd_codeintel_explore", {
      repoPath: "/repo",
      query: "Foo",
      tokenBudget: 40,
      limit: undefined,
    });

    await getAffectedTests("/repo", ["src/a.ts"], 50);
    expect(invoke).toHaveBeenCalledWith("cmd_codeintel_affected_tests", {
      repoPath: "/repo",
      targets: ["src/a.ts"],
      tokenBudget: 50,
      maxDepth: undefined,
    });

    await getClones("/repo", 60);
    expect(invoke).toHaveBeenCalledWith("cmd_codeintel_clones", {
      repoPath: "/repo",
      tokenBudget: 60,
    });

    await getImpactLayered("/repo", "src/a.ts", 70);
    expect(invoke).toHaveBeenCalledWith("cmd_codeintel_impact_layered", {
      repoPath: "/repo",
      target: "src/a.ts",
      tokenBudget: 70,
    });

    const token = newCodeintelCancelToken();
    expect(token.length).toBeGreaterThan(4);
    await cancelCodeintelQuery(token);
    expect(invoke).toHaveBeenCalledWith("cmd_codeintel_cancel", { cancelToken: token });

    await buildDevmap("/repo");
    expect(invoke).toHaveBeenCalledWith("cmd_devmap_build", { repoPath: "/repo" });
    await refreshDevmap("/repo");
    expect(invoke).toHaveBeenCalledWith("cmd_devmap_refresh", { repoPath: "/repo" });
    await maybeRefreshDevmap("/repo", true);
    expect(invoke).toHaveBeenCalledWith("cmd_devmap_maybe_refresh", {
      repoPath: "/repo",
      repoChanged: true,
    });
    await getDevmapCliStatus("/repo");
    expect(invoke).toHaveBeenCalledWith("cmd_devmap_status", { repoPath: "/repo" });
    await getDevmapRepoMap("/repo");
    expect(invoke).toHaveBeenCalledWith("cmd_devmap_repo_map", { repoPath: "/repo" });
    await previewDevmapEdit("/repo", "src/a.ts", "x");
    expect(invoke).toHaveBeenCalledWith("cmd_devmap_preview", {
      repoPath: "/repo",
      filePath: "src/a.ts",
      content: "x",
    });
  });

  it("rejects a null dead-symbol payload instead of returning it to Health", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(null);
    await expect(getDeadSymbols("/repo")).rejects.toThrow(/no payload/);
  });

  it("rejects an available dead-symbol payload that omitted items", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({ available: true, truncated: false });
    await expect(getDeadSymbols("/repo")).rejects.toThrow(/without items/);
  });

  it("rejects a null status payload instead of returning it to Health", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(null);
    await expect(getCodeintelStatus("/repo")).rejects.toThrow(/no payload/);
  });

  it("keeps an unavailable dead-symbol query as unavailable, not a crash", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      available: false,
      reason: "index missing",
    });
    const res = await getDeadSymbols("/repo");
    expect(res.available).toBe(false);
    expect(res.items).toEqual([]);
    expect(res.reason).toBe("index missing");
  });
});
