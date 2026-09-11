import { invoke } from "@tauri-apps/api/core";
import { createAsyncGuard } from "../async/guard";
import { searchSymbols, searchWorkspaceSymbols, listWorkspaceRepos } from "../codeintel/client";
import type { CodeintelSymbolHit, WorkspaceFederatedHit, WorkspaceRepoEntry } from "../codeintel/types";
import { boundText, boundedJoin, tooltipWalkIncomplete } from "../codeintel/walkIncomplete";
import type { PaletteMode } from "./model";

export interface SearchResult {
  files: string[];
  symbols: CodeintelSymbolHit[];
  workspace: WorkspaceFederatedHit[];
  repos: WorkspaceRepoEntry[];
  note: string | null;
  failed: boolean;
  loading: boolean;
}
export const emptySearch = (): SearchResult => ({ files: [], symbols: [], workspace: [], repos: [], note: null, failed: false, loading: false });
export interface SearchRequest { mode: PaletteMode; repoPath: string; text: string; semantic: boolean }
export const SEARCH_DELAY_MS = 180;
export const SEARCH_TIMEOUT_MS = 12_000;
export const searchDependencies = {
  symbols: searchSymbols,
  workspace: searchWorkspaceSymbols,
  repos: listWorkspaceRepos,
  files: (repoPath: string) => invoke<string[]>("cmd_list_repo_files", { repoPath }),
};

/** One lifecycle for all remote providers: debounce, clear stale rows, deadline, cleanup.
 * These IPC commands have no cancellation parameter. Cleanup invalidates their
 * results; it does not claim to terminate an already running backend query.
 */
export function scheduleSearch(request: SearchRequest, publish: (result: SearchResult) => void, deps = searchDependencies): () => void {
  const guard = createAsyncGuard();
  let deadline: ReturnType<typeof setTimeout> | undefined;
  const finish = (result: SearchResult) => {
    if (!guard.isLive()) return;
    clearTimeout(deadline);
    guard.cancel();
    publish(result);
  };
  publish({ ...emptySearch(), loading: true });
  const debounce = setTimeout(async () => {
    deadline = setTimeout(() => finish({ ...emptySearch(), failed: true, note: "Search timed out. Retry when the repository is ready." }), SEARCH_TIMEOUT_MS);
    try {
      const result = emptySearch();
      if (request.mode === "files") result.files = await deps.files(request.repoPath);
      else if (request.mode === "symbols") {
        const response = await deps.symbols(request.repoPath, request.text, 6000);
        if (!response.available) {
          result.failed = true;
          result.note = response.reason || "Symbol search unavailable — not the same as zero matches. Install or build the code map.";
        } else {
          result.symbols = response.items;
          const notes: string[] = [];
          if (response.truncated) notes.push(`${response.shown} of ${response.total} symbol matches returned. Refine your search for more.`);
          if (response.walk_incomplete) {
            notes.push(tooltipWalkIncomplete([response.walk_incomplete]) ?? response.walk_incomplete);
          }
          result.note = boundText(notes.join(" · ")) || null;
        }
      } else if (request.mode === "workspace") {
        const [response, registry] = await Promise.all([deps.workspace(request.repoPath, request.text, 8000, request.semantic), deps.repos(request.repoPath)]);
        result.workspace = response.items;
        result.repos = registry.repos;
        const notes: string[] = [];
        if (request.semantic) notes.push("TF-IDF name ranking");
        if (response.unavailable.length) {
          notes.push(
            boundedJoin(
              response.unavailable.map((repo) => `${repo.repo}: ${repo.reason}`),
              8,
            ),
          );
        }
        if (response.truncated) notes.push(`${response.shown} of ${response.total} matches returned. Refine your search for more.`);
        if (response.repos_queried === 0) notes.push("No registered repositories were searched. Open Map to manage the workspace.");
        result.note = boundText(notes.join(" · ")) || null;
        result.failed = response.unavailable.length > 0 || response.repos_queried === 0;
      }
      finish(result);
    } catch (error) {
      finish({ ...emptySearch(), failed: true, note: `Search failed: ${error instanceof Error ? error.message : String(error)}` });
    }
  }, SEARCH_DELAY_MS);
  return () => { guard.cancel(); clearTimeout(debounce); clearTimeout(deadline); };
}

/** Registry names are identities. Labels and basename/suffix guesses are not. */
export function workspaceRoot(name: string, repos: readonly WorkspaceRepoEntry[]): string | null {
  const matches = repos.filter(repo => repo.name === name);
  return matches.length === 1 && matches[0].root ? matches[0].root : null;
}
