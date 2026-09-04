export interface RepositoryDrafts {
  repo: string;
  paths: string[];
}

// FileViewer is remounted on view and repository switches. This module-owned
// registry is the small, synchronous truth the app-close handler can inspect
// even while no editor instance is mounted.
const draftsByRepository = new Map<string, string[]>();

export function recordEditorDrafts(repo: string, paths: readonly string[]): void {
  const normalized = [...new Set(paths.filter((path) => path.trim().length > 0))].sort();
  if (normalized.length === 0) {
    draftsByRepository.delete(repo);
    return;
  }
  draftsByRepository.set(repo, normalized);
}

export function hasUnsavedEditorDrafts(): boolean {
  return draftsByRepository.size > 0;
}

export function unsavedEditorDrafts(): RepositoryDrafts[] {
  return [...draftsByRepository.entries()].map(([repo, paths]) => ({
    repo,
    paths: [...paths],
  }));
}

/** Test isolation for the intentionally process-lifetime registry. */
export function clearEditorDraftRegistryForTests(): void {
  draftsByRepository.clear();
}
