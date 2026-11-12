import { describe, expect, it } from "vitest";
import type { CodeintelEdge, CodeintelFileSymbol, CodeintelNeighbors } from "../codeintel/types";
import {
  extractDirectCallers,
  formatSymbolLensForAgent,
  pickEnclosingSymbol,
} from "./symbolQuickLens";

function edgeResponse(items: CodeintelEdge[]) {
  return {
    source_freshness: { fresh: true },
    available: true,
    items,
    total: items.length,
    shown: items.length,
    truncated: false,
  };
}

describe("symbolQuickLens", () => {
  const symbols: CodeintelFileSymbol[] = [
    {
      symbol_name: "TerminalOrchestrator",
      qualified_name: "terminal::orchestrate::TerminalOrchestrator",
      kind: "struct",
      span_start_line: 10,
      span_end_line: 200,
    },
    {
      symbol_name: "execute_close",
      qualified_name: "terminal::orchestrate::TerminalOrchestrator::execute_close",
      kind: "function",
      span_start_line: 50,
      span_end_line: 85,
    },
    {
      symbol_name: "one_liner",
      qualified_name: "terminal::orchestrate::one_liner",
      kind: "function",
      span_start_line: 210,
      span_end_line: 210,
    },
  ];

  it("picks the innermost enclosing symbol and rejects invalid or uncovered lines", () => {
    expect(pickEnclosingSymbol(symbols, 64)?.symbol_name).toBe("execute_close");
    expect(pickEnclosingSymbol(symbols, 15)?.symbol_name).toBe("TerminalOrchestrator");
    expect(pickEnclosingSymbol(symbols, 210)?.symbol_name).toBe("one_liner");
    expect(pickEnclosingSymbol(symbols, 0)).toBeNull();
    expect(pickEnclosingSymbol(symbols, -3)).toBeNull();
    expect(pickEnclosingSymbol(symbols, 12.5)).toBeNull();
    expect(pickEnclosingSymbol(symbols, 999)).toBeNull();
    expect(pickEnclosingSymbol([], 64)).toBeNull();
  });

  it("deduplicates direct callers while preserving both capped list and true total", () => {
    const neighbors: CodeintelNeighbors[] = [
      {
        target: "terminal::orchestrate::TerminalOrchestrator::execute_close",
        callers: edgeResponse([
          {
            source_file: "src-tauri/src/mcp/mod.rs",
            target_file: "src-tauri/src/terminal/orchestrate.rs",
            source_symbol: "mcp::dispatch",
            target_symbol: "execute_close",
            confidence: 0.95,
          },
          {
            source_file: "src-tauri/src/mcp/mod.rs",
            target_file: "src-tauri/src/terminal/orchestrate.rs",
            source_symbol: "mcp::dispatch",
            target_symbol: "execute_close",
            confidence: 0.95,
          },
          {
            source_file: "src-tauri/src/hooks/mod.rs",
            target_file: "src-tauri/src/terminal/orchestrate.rs",
            source_symbol: "hooks::handle_orchestrate",
            target_symbol: "execute_close",
            confidence: 0.9,
          },
          {
            source_file: "src-tauri/src/bin/gitpulse-hook.rs",
            target_file: "src-tauri/src/terminal/orchestrate.rs",
            source_symbol: "main",
            target_symbol: "execute_close",
            confidence: 0.9,
          },
        ]),
        callees: edgeResponse([]),
      },
    ];

    const res = extractDirectCallers(neighbors, 2);
    expect(res.total).toBe(3);
    expect(res.callers).toEqual([
      {
        symbol: "dispatch",
        qualifiedSymbol: "mcp::dispatch",
        filePath: "src-tauri/src/mcp/mod.rs",
      },
      {
        symbol: "handle_orchestrate",
        qualifiedSymbol: "hooks::handle_orchestrate",
        filePath: "src-tauri/src/hooks/mod.rs",
      },
    ]);
  });

  it("formats symbol lens context for agent handoff with exact span and overflow count", () => {
    const text = formatSymbolLensForAgent({
      filePath: "src-tauri/src/terminal/orchestrate.rs",
      symbol: symbols[1],
      callers: [
        {
          symbol: "dispatch",
          qualifiedSymbol: "mcp::dispatch",
          filePath: "src-tauri/src/mcp/mod.rs",
        },
      ],
      totalCallers: 3,
    });
    expect(text).toBe(
      "@src-tauri/src/terminal/orchestrate.rs#L50-L85 (execute_close; direct callers: dispatch (src-tauri/src/mcp/mod.rs) +2 more) ",
    );

    const singleLine = formatSymbolLensForAgent({
      filePath: "src-tauri/src/terminal/orchestrate.rs",
      symbol: symbols[2],
      callers: [],
      totalCallers: 0,
    });
    expect(singleLine).toBe("@src-tauri/src/terminal/orchestrate.rs#L210 (one_liner) ");
  });
});
