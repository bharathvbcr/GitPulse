import {
  identityKey,
  normalizeRepoPath,
  sameRepo,
  type PathIdentityOptions,
} from "../repos/paths";

const LOCAL_PREFIX = "local:";

export interface OpenTabRef {
  path: string;
  label: string;
}

export interface RegisteredRef {
  id: string;
  identity_key: string;
}

export interface OpenMembershipCandidate {
  path: string;
  label: string;
  registeredId: string | null;
  alreadyMember: boolean;
}

/** Git common dir stored as `local:{common}` by native registration. */
export function identityCommonDir(identityKeyValue: string): string | null {
  if (!identityKeyValue.startsWith(LOCAL_PREFIX)) return null;
  return identityKeyValue.slice(LOCAL_PREFIX.length) || null;
}

export function tabMatchesRegistered(
  path: string,
  identityKeyValue: string,
  options: PathIdentityOptions,
): boolean {
  const common = identityCommonDir(identityKeyValue);
  const normalized = normalizeRepoPath(path);
  if (!common || !normalized) return false;
  return sameRepo(common, normalized, options) || sameRepo(common, `${normalized}/.git`, options);
}

export function registeredIdForPath(
  path: string,
  repositories: readonly RegisteredRef[],
  options: PathIdentityOptions,
): string | null {
  return repositories.find((repo) => tabMatchesRegistered(path, repo.identity_key, options))?.id ?? null;
}

export function withRepositoryId(ids: readonly string[], id: string): string[] {
  return ids.includes(id) ? [...ids] : [...ids, id];
}

export function membershipAfterAttach(current: readonly string[], incoming: readonly string[]): string[] {
  let ids = [...current];
  for (const id of incoming) ids = withRepositoryId(ids, id);
  return ids;
}

export function addableOpenTabs(
  candidates: readonly OpenMembershipCandidate[],
): OpenMembershipCandidate[] {
  return candidates.filter((candidate) => !candidate.alreadyMember);
}

/** Workspace boards hide members; the catalog hides anything already registered. */
export function pickerSelectionIds(
  kind: "workspace" | "global" | "repository",
  workspaceMemberIds: readonly string[] | null,
  catalogIds: readonly string[],
): readonly string[] {
  return kind === "workspace" ? (workspaceMemberIds ?? []) : catalogIds;
}

export function openAddActionLabel(tabs: readonly { label: string }[]): string | null {
  if (tabs.length === 0) return null;
  if (tabs.length === 1) return `Add ${tabs[0].label}`;
  return `Add ${tabs.length} open repositories`;
}

/**
 * Unique open tabs, labelled for a picker. `alreadyMember` is a display hint
 * from identity_key; registration itself is always the native idempotent path.
 */
export function openMembershipCandidates(
  tabs: readonly OpenTabRef[],
  repositories: readonly RegisteredRef[],
  selectedIds: readonly string[],
  options: PathIdentityOptions,
): OpenMembershipCandidate[] {
  const selected = new Set(selectedIds);
  const seen = new Set<string>();
  const out: OpenMembershipCandidate[] = [];
  for (const tab of tabs) {
    const key = identityKey(tab.path, options);
    if (!key || seen.has(key)) continue;
    seen.add(key);
    const registeredId = registeredIdForPath(tab.path, repositories, options);
    out.push({
      path: tab.path,
      label: tab.label,
      registeredId,
      alreadyMember: registeredId !== null && selected.has(registeredId),
    });
  }
  return out;
}
