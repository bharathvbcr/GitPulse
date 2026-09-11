import { invoke } from "@tauri-apps/api/core";
import {
  parseCodeintelResponse,
  parseCodeintelStatus,
  type CodeintelAffectedTests,
  type CodeintelClones,
  type CodeintelDeadSymbol,
  type CodeintelEdge,
  type CodeintelExplore,
  type CodeintelLayeredImpact,
  type CodeintelNeighbors,
  type CodeintelResponse,
  type CodeintelRung,
  type CodeintelStatus,
  type CodeintelSymbolHit,
  type DevmapBuildOutcome,
  type DevmapCliStatus,
  type DevmapPreviewFileResult,
  type DevmapPreviewOutcome,
  type GraphVizLoad,
  type LiveRefreshOutcome,
  type RepoMapLoad,
  type WorkspaceLinksResult,
  type WorkspaceRegisterResult,
  type WorkspaceSearchResult,
  type WorkspaceSnapshot,
  type WorkspaceUnregisterResult,
} from "./types";

function asResponse<T>(command: string) {
  return (raw: unknown) => parseCodeintelResponse<T>(raw, command);
}

export async function getCodeintelStatus(repoPath: string): Promise<CodeintelStatus> {
  return parseCodeintelStatus(
    await invoke("cmd_codeintel_status", { repoPath }),
    "cmd_codeintel_status",
  );
}

export async function searchSymbols(
  repoPath: string,
  query: string,
  tokenBudget?: number,
): Promise<CodeintelResponse<CodeintelSymbolHit>> {
  return invoke("cmd_codeintel_search", {
    repoPath,
    query,
    tokenBudget,
  }).then(asResponse("cmd_codeintel_search"));
}

export async function getImpact(
  repoPath: string,
  target: string,
  tokenBudget?: number,
  cancelToken?: string,
): Promise<CodeintelResponse<CodeintelEdge>> {
  return invoke("cmd_codeintel_impact", {
    repoPath,
    target,
    tokenBudget,
    ...(cancelToken ? { cancelToken } : {}),
  }).then(asResponse("cmd_codeintel_impact"));
}

export async function getImpactAtRung(
  repoPath: string,
  target: string,
  tokenBudget?: number,
  minRung?: CodeintelRung,
  cancelToken?: string,
): Promise<CodeintelResponse<CodeintelEdge>> {
  return invoke("cmd_codeintel_impact_at_rung", {
    repoPath,
    target,
    tokenBudget,
    minRung,
    ...(cancelToken ? { cancelToken } : {}),
  }).then(asResponse("cmd_codeintel_impact_at_rung"));
}

export async function getDeadSymbols(
  repoPath: string,
  tokenBudget?: number,
): Promise<CodeintelResponse<CodeintelDeadSymbol>> {
  return invoke("cmd_codeintel_dead_symbols", {
    repoPath,
    tokenBudget,
  }).then(asResponse("cmd_codeintel_dead_symbols"));
}

export async function getDependencies(
  repoPath: string,
  filePath: string,
  tokenBudget?: number,
  minRung?: CodeintelRung,
): Promise<CodeintelResponse<CodeintelEdge>> {
  return invoke("cmd_codeintel_dependencies", {
    repoPath,
    filePath,
    tokenBudget,
    minRung,
  }).then(asResponse("cmd_codeintel_dependencies"));
}

export async function traceBetween(
  repoPath: string,
  from: string,
  to: string,
  tokenBudget?: number,
  minRung?: CodeintelRung,
): Promise<CodeintelResponse<CodeintelEdge>> {
  return invoke("cmd_codeintel_trace", {
    repoPath,
    from,
    to,
    tokenBudget,
    minRung,
  }).then(asResponse("cmd_codeintel_trace"));
}

export async function getNeighbors(
  repoPath: string,
  targets: string[],
  tokenBudget?: number,
  minRung?: CodeintelRung,
): Promise<CodeintelNeighbors[]> {
  return invoke<CodeintelNeighbors[]>("cmd_codeintel_neighbors", {
    repoPath,
    targets,
    tokenBudget,
    minRung,
  });
}

export async function exploreSymbol(
  repoPath: string,
  query: string,
  tokenBudget?: number,
  limit?: number,
): Promise<CodeintelExplore> {
  return invoke<CodeintelExplore>("cmd_codeintel_explore", {
    repoPath,
    query,
    tokenBudget,
    limit,
  });
}

export async function getAffectedTests(
  repoPath: string,
  targets: string[],
  tokenBudget?: number,
  maxDepth?: number,
): Promise<CodeintelAffectedTests> {
  return invoke<CodeintelAffectedTests>("cmd_codeintel_affected_tests", {
    repoPath,
    targets,
    tokenBudget,
    maxDepth,
  });
}

export async function getClones(
  repoPath: string,
  tokenBudget?: number,
): Promise<CodeintelClones> {
  return invoke<CodeintelClones>("cmd_codeintel_clones", {
    repoPath,
    tokenBudget,
  });
}

export async function getImpactLayered(
  repoPath: string,
  target: string,
  tokenBudget?: number,
  cancelToken?: string,
): Promise<CodeintelLayeredImpact> {
  return invoke<CodeintelLayeredImpact>("cmd_codeintel_impact_layered", {
    repoPath,
    target,
    tokenBudget,
    ...(cancelToken ? { cancelToken } : {}),
  });
}

export async function getImpactLayeredMany(
  repoPath: string,
  targets: string[],
  tokenBudget?: number,
  cancelToken?: string,
): Promise<CodeintelLayeredImpact[]> {
  return invoke<CodeintelLayeredImpact[]>("cmd_codeintel_impact_layered_many", {
    repoPath,
    targets,
    tokenBudget,
    ...(cancelToken ? { cancelToken } : {}),
  });
}

export async function cancelCodeintelQuery(cancelToken: string): Promise<boolean> {
  return invoke<boolean>("cmd_codeintel_cancel", { cancelToken });
}

/** Mint a cancel token for one long codeintel walk (timeout + dismiss). */
export function newCodeintelCancelToken(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return `ci-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

export async function buildDevmap(repoPath: string): Promise<DevmapBuildOutcome> {
  return invoke<DevmapBuildOutcome>("cmd_devmap_build", { repoPath });
}

export async function refreshDevmap(repoPath: string): Promise<DevmapBuildOutcome> {
  return invoke<DevmapBuildOutcome>("cmd_devmap_refresh", { repoPath });
}

export async function maybeRefreshDevmap(
  repoPath: string,
  repoChanged = true,
): Promise<LiveRefreshOutcome> {
  return invoke<LiveRefreshOutcome>("cmd_devmap_maybe_refresh", {
    repoPath,
    repoChanged,
  });
}

export async function getDevmapCliStatus(repoPath: string): Promise<DevmapCliStatus> {
  return invoke<DevmapCliStatus>("cmd_devmap_status", { repoPath });
}

export async function getDevmapRepoMap(repoPath: string): Promise<RepoMapLoad> {
  return invoke<RepoMapLoad>("cmd_devmap_repo_map", { repoPath });
}

export async function getCodeGraphViz(
  repoPath: string,
  symbols?: boolean,
  maxNodes?: number,
): Promise<GraphVizLoad> {
  return invoke<GraphVizLoad>("cmd_devmap_viz", {
    repoPath,
    symbols,
    maxNodes,
  });
}

export async function getMapPreviewViz(repoPath: string): Promise<GraphVizLoad> {
  return invoke<GraphVizLoad>("cmd_devmap_map_preview", { repoPath });
}

export async function previewDevmapEdit(
  repoPath: string,
  filePath: string,
  content: string,
): Promise<DevmapPreviewFileResult> {
  return invoke<DevmapPreviewFileResult>("cmd_devmap_preview", {
    repoPath,
    filePath,
    content,
  });
}

export async function previewDevmapEdits(
  repoPath: string,
  files: Array<[string, string]>,
): Promise<DevmapPreviewOutcome> {
  return invoke<DevmapPreviewOutcome>("cmd_devmap_preview_many", {
    repoPath,
    files,
  });
}

/* ── Multi-repo workspace registry ─────────────────────────────────────── */

export async function registerWorkspaceRepo(
  registryRoot: string,
  repoPath: string,
  name?: string,
): Promise<WorkspaceRegisterResult> {
  return invoke<WorkspaceRegisterResult>("cmd_workspace_register", {
    registryRoot,
    repoPath,
    name,
  });
}

export async function unregisterWorkspaceRepo(
  registryRoot: string,
  name: string,
): Promise<WorkspaceUnregisterResult> {
  return invoke<WorkspaceUnregisterResult>("cmd_workspace_unregister", {
    registryRoot,
    name,
  });
}

export async function listWorkspaceRepos(registryRoot: string): Promise<WorkspaceSnapshot> {
  return invoke<WorkspaceSnapshot>("cmd_workspace_list", { registryRoot });
}

/** Sync open tabs into the registry rooted at `registryRoot`. */
export async function syncWorkspaceTabs(
  registryRoot: string,
  repoPaths: string[],
): Promise<WorkspaceSnapshot> {
  return invoke<WorkspaceSnapshot>("cmd_workspace_sync", {
    registryRoot,
    repoPaths,
  });
}

/**
 * Cross-repo symbol search. Pass `semantic: true` for TF-IDF name ranking
 * (not AI / embedding search).
 */
export async function searchWorkspaceSymbols(
  registryRoot: string,
  query: string,
  tokenBudget?: number,
  semantic?: boolean,
): Promise<WorkspaceSearchResult> {
  return invoke<WorkspaceSearchResult>("cmd_workspace_search", {
    registryRoot,
    query,
    tokenBudget,
    semantic,
  });
}

export async function getWorkspaceLinkCandidates(
  registryRoot: string,
): Promise<WorkspaceLinksResult> {
  return invoke<WorkspaceLinksResult>("cmd_workspace_link_candidates", {
    registryRoot,
  });
}
