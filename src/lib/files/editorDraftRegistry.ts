export interface RepositoryDrafts {
  repo: string;
  paths: string[];
}

// FileViewer is remounted on view and repository switches. This module-owned
// registry is the small, synchronous truth the app-close handler can inspect
// even while no editor instance is mounted.
const draftsByRepository = new Map<string, Map<string, string[]>>();

export function recordEditorDrafts(repo: string, paths: readonly string[], owner = "files"): void {
  const normalized = [...new Set(paths.filter((path) => path.trim().length > 0))].sort();
  const owners = draftsByRepository.get(repo) ?? new Map<string, string[]>();
  if (normalized.length === 0) {
    owners.delete(owner);
    if (owners.size === 0) draftsByRepository.delete(repo);
    return;
  }
  owners.set(owner, normalized);
  draftsByRepository.set(repo, owners);
}

export function hasUnsavedEditorDrafts(): boolean {
  return draftsByRepository.size > 0;
}

export function unsavedEditorDrafts(): RepositoryDrafts[] {
  return [...draftsByRepository.entries()].map(([repo, owners]) => ({
    repo,
    paths: [...new Set([...owners.values()].flat())].sort(),
  }));
}

/** Test isolation for the intentionally process-lifetime registry. */
export function clearEditorDraftRegistryForTests(): void {
  draftsByRepository.clear();
}
