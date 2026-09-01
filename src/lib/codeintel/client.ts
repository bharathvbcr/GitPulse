import { invoke } from "@tauri-apps/api/core";
import type {
  CodeintelDeadSymbol,
  CodeintelEdge,
  CodeintelResponse,
  CodeintelStatus,
  CodeintelSymbolHit,
} from "./types";

export async function getCodeintelStatus(repoPath: string): Promise<CodeintelStatus> {
  return invoke<CodeintelStatus>("cmd_codeintel_status", { repoPath });
}

export async function searchSymbols(
  repoPath: string,
  query: string,
  tokenBudget?: number,
): Promise<CodeintelResponse<CodeintelSymbolHit>> {
  return invoke<CodeintelResponse<CodeintelSymbolHit>>("cmd_codeintel_search", {
    repoPath,
    query,
    tokenBudget,
  });
}

export async function getImpact(
  repoPath: string,
  target: string,
  tokenBudget?: number,
): Promise<CodeintelResponse<CodeintelEdge>> {
  return invoke<CodeintelResponse<CodeintelEdge>>("cmd_codeintel_impact", {
    repoPath,
    target,
    tokenBudget,
  });
}

export async function getDependencies(
  repoPath: string,
  filePath: string,
  tokenBudget?: number,
): Promise<CodeintelResponse<CodeintelEdge>> {
  return invoke<CodeintelResponse<CodeintelEdge>>("cmd_codeintel_dependencies", {
    repoPath,
    filePath,
    tokenBudget,
  });
}

export async function getDeadSymbols(
  repoPath: string,
  tokenBudget?: number,
): Promise<CodeintelResponse<CodeintelDeadSymbol>> {
  return invoke<CodeintelResponse<CodeintelDeadSymbol>>("cmd_codeintel_dead_symbols", {
    repoPath,
    tokenBudget,
  });
}

export async function traceBetween(
  repoPath: string,
  from: string,
  to: string,
  tokenBudget?: number,
): Promise<CodeintelResponse<CodeintelEdge>> {
  return invoke<CodeintelResponse<CodeintelEdge>>("cmd_codeintel_trace_between", {
    repoPath,
    from,
    to,
    tokenBudget,
  });
}
