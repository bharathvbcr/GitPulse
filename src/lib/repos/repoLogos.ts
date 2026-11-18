/**
 * The app's one repository-logo store, wired to the backend.
 *
 * Reads go through the existing owners: the file listing through
 * `files/repoListing.ts` (shared with the Code view and the palette, so the
 * repository on screen is listed once), and the bytes through the
 * trust-gated `cmd_get_file_blob`. Nothing here can read outside a checkout
 * the backend already accepts.
 */

import { invoke } from "../ipc/invoke";
import { listRepoFiles } from "../files/repoListing";
import type { FileBlob } from "../files/types";
import { familyName } from "./repoFamily";
import { displayName, identityKey, normalizeRepoPath, sameRepo, type PathIdentityOptions } from "./paths";
import { createRepoLogoStore, type LogoTarget, type StorageLike } from "./repoLogoStore";
import type { TabColor } from "./tabColors";
import { normalizeTabColor } from "./tabColors";

function browserStorage(): StorageLike | null {
  try {
    return typeof window !== "undefined" && window.localStorage ? window.localStorage : null;
  } catch {
    return null;
  }
}

export const repoLogos = createRepoLogoStore({
  list: (repoPath) => listRepoFiles(repoPath),
  blob: (repoPath, filePath) =>
    invoke<FileBlob>("cmd_get_file_blob", { repoPath, filePath, commitId: null, withDigest: false }),
  storage: browserStorage(),
});

/** The fields of an open repository tab a logo is drawn from. */
export interface LogoTab {
  path: string;
  label?: string;
  family?: string | null;
  familyRoot?: string | null;
  color?: TabColor | null;
  group?: string | null;
  trustRequired?: boolean;
  missing?: boolean;
  /** Read at least once: its family, if it has one, is known by now. */
  countsKnown?: boolean;
  error?: string | null;
  watch?: string;
}

/** What one logo needs: where to read, what to key it by, and what to call it. */
export interface LogoSubject {
  target: LogoTarget;
  /** The repository's name, for the initials. */
  name: string;
  color: TabColor | null;
  /**
   * Whether the key is final. A tab just opened has no family until its
   * checkout is resolved; asking before then looks it up under its own path,
   * and again under the family a moment later, once per checkout.
   */
  settled: boolean;
}

/**
 * The logo subject for a checkout. With its open tab, the logo is keyed by
 * the repository family, so every checkout of a repository shares one, and
 * the initials come from the repository's own directory rather than a
 * worktree's. Without one (a session in a checkout no tab shows), it stands
 * alone under its own path.
 */
export function logoSubject(
  path: string,
  tab: LogoTab | null | undefined,
  options: PathIdentityOptions,
  name?: string,
): LogoSubject {
  const normalized = normalizeRepoPath(path) ?? path;
  const family = tab?.family ?? null;
  const key = family ? `family:${family}` : `path:${identityKey(normalized, options)}`;
  const display = name?.trim()
    || (tab?.familyRoot ? familyName(tab.familyRoot) : "")
    || tab?.label?.trim()
    || displayName(normalized);
  return {
    target: { key, repoPath: tab?.path ?? normalized, blocked: tab?.trustRequired === true || tab?.missing === true },
    name: display,
    color: normalizeTabColor(tab?.color ?? null),
    settled:
      !tab || family !== null || tab.countsKnown === true || !!tab.error || tab.watch === "parked" || tab.missing === true,
  };
}

/** The open tab showing this checkout, if one does. */
export function tabForLogo<T extends LogoTab>(path: string, tabs: readonly T[], options: PathIdentityOptions): T | null {
  return tabs.find((tab) => sameRepo(tab.path, path, options)) ?? null;
}
