/**
 * Shared A1 preview state: one `previewDevmapEdits` call feeds the commit
 * composer summary and the diff-rail markers. A second query path would drift.
 */

import { writable, derived, get } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import { previewDevmapEdits } from "./client";
import { normalizePreviewFileResult } from "./previewNormalize";
import {
  markersByPath,
  summarizePreview,
  type PreviewCommitSummary,
  type PreviewMarker,
} from "./previewSummary";
import type { DevmapPreviewFileResult, DevmapPreviewOutcome } from "./types";
import { beginGeneration } from "../async/guard";

export interface PreviewStoreState {
  repoPath: string | null;
  /** Paths that seeded the last successful (or in-flight) request. */
  pathsKey: string;
  loading: boolean;
  outcome: DevmapPreviewOutcome | null;
  files: DevmapPreviewFileResult[];
  error: string | null;
}

const initial: PreviewStoreState = {
  repoPath: null,
  pathsKey: "",
  loading: false,
  outcome: null,
  files: [],
  error: null,
};

const { subscribe, update, set } = writable<PreviewStoreState>(initial);
const generation = beginGeneration();

function pathsKey(paths: string[]): string {
  return [...paths].sort().join("\0");
}

/**
 * Read working-tree bytes the same way ConflictEditor / CoverageViewer do —
 * `cmd_get_file_content` with `commitId: null`. Do not invent a second reader.
 */
export async function readWorkingTreeContent(
  repoPath: string,
  filePath: string,
): Promise<{ ok: true; content: string } | { ok: false; reason: string }> {
  try {
    const content = await invoke<string>("cmd_get_file_content", {
      repoPath,
      filePath,
      commitId: null,
    });
    return { ok: true, content };
  } catch (err: unknown) {
    const reason =
      typeof err === "string"
        ? err
        : err instanceof Error
          ? err.message
          : "could not read working-tree content";
    return { ok: false, reason };
  }
}

async function collectFilePairs(
  repoPath: string,
  paths: string[],
): Promise<{
  pairs: Array<[string, string]>;
  unread: DevmapPreviewFileResult[];
}> {
  const pairs: Array<[string, string]> = [];
  const unread: DevmapPreviewFileResult[] = [];
  for (const path of paths) {
    const read = await readWorkingTreeContent(repoPath, path);
    if (!read.ok) {
      unread.push({
        file_path: path,
        available: false,
        reason: read.reason,
        report: null,
      });
      continue;
    }
    pairs.push([path, read.content]);
  }
  return { pairs, unread };
}

export const previewStore = {
  subscribe,

  reset() {
    generation.next();
    set(initial);
  },

  /**
   * Refresh preview for the given paths (typically staged files). Same results
   * are published for CommitComposer and DiffFileRail.
   */
  async refresh(repoPath: string | null, paths: string[]): Promise<void> {
    const key = pathsKey(paths);
    if (!repoPath || paths.length === 0) {
      generation.next();
      set({
        repoPath,
        pathsKey: key,
        loading: false,
        outcome: null,
        files: [],
        error: null,
      });
      return;
    }

    const token = generation.next();
    update((s) => ({
      ...s,
      repoPath,
      pathsKey: key,
      loading: true,
      error: null,
    }));

    try {
      const { pairs, unread } = await collectFilePairs(repoPath, paths);
      if (!generation.isCurrent(token)) return;

      let outcome: DevmapPreviewOutcome | null = null;
      let fromPreview: DevmapPreviewFileResult[] = [];
      if (pairs.length > 0) {
        outcome = await previewDevmapEdits(repoPath, pairs);
        if (!generation.isCurrent(token)) return;
        fromPreview = (outcome.files ?? []).map(normalizePreviewFileResult);
      } else {
        outcome = {
          available: false,
          binary: null,
          lookup: null,
          reason: "could not read any staged file from the working tree",
          files: [],
          cancelled: false,
        };
      }

      const files = [...fromPreview, ...unread];
      update(() => ({
        repoPath,
        pathsKey: key,
        loading: false,
        outcome,
        files,
        error: null,
      }));
    } catch (err: unknown) {
      if (!generation.isCurrent(token)) return;
      const message =
        typeof err === "string"
          ? err
          : err instanceof Error
            ? err.message
            : "preview failed";
      update((s) => ({
        ...s,
        loading: false,
        outcome: null,
        files: [],
        error: message,
      }));
    }
  },
};

export const previewSummary = derived(previewStore, ($s): PreviewCommitSummary | null => {
  if ($s.error) {
    return summarizePreview([], { reason: $s.error });
  }
  if ($s.files.length === 0 && !$s.loading) {
    if (!$s.repoPath) return null;
    return summarizePreview([], {
      cancelled: $s.outcome?.cancelled,
      reason: $s.outcome?.reason ?? null,
    });
  }
  return summarizePreview($s.files, {
    cancelled: $s.outcome?.cancelled,
    reason: $s.outcome?.reason ?? null,
  });
});

export const previewMarkers = derived(
  previewStore,
  ($s): Map<string, PreviewMarker> => markersByPath($s.files),
);

/** Snapshot helper for non-Svelte callers / tests. */
export function getPreviewMarkers(): Map<string, PreviewMarker> {
  return get(previewMarkers);
}
