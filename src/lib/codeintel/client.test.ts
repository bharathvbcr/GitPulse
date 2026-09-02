import { describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import {
  getCodeintelStatus,
  searchSymbols,
  getImpact,
  getDeadSymbols,
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
});
