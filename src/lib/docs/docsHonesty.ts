/**
 * Honesty copy for the docs Map panel — vault caps, search limits, broken links.
 */

import type { BrokenLink, DocRenameOutcome, DocsSearchHit, DocsStatus } from "./client";
import { DOCS_SEARCH_DEFAULT_LIMIT } from "./docGraphPayload";

export function docsStatusHonesty(status: DocsStatus | null | undefined): string | null {
  if (!status) return null;
  const parts: string[] = [];
  if (status.truncated) {
    parts.push(
      `Vault capped: indexed ${status.noteCount} notes (a resource limit was reached). This is not the whole docs set.`,
    );
  }
  if (status.skippedOversized > 0) {
    parts.push(`${status.skippedOversized} oversized note(s) skipped`);
  }
  if (status.skippedUnreadable > 0) {
    parts.push(`${status.skippedUnreadable} unreadable note(s) skipped`);
  }
  return parts.length > 0 ? parts.join(" · ") : null;
}

/**
 * What a rename did not do. A rename that reads "done" while some notes were
 * never searched, were changed under it, failed to write or still hold a
 * dead link is the failure this exists to prevent; every one is named.
 */
export function docsRenameHonesty(outcome: DocRenameOutcome | null | undefined): string | null {
  if (!outcome) return null;
  const parts: string[] = [];
  if (outcome.scan.notScanned > 0) {
    parts.push(
      `${outcome.scan.notScanned} notes were not scanned, so links in them were not updated`,
    );
  }
  if (outcome.skippedConflict.length > 0) {
    parts.push(
      `${outcome.skippedConflict.length} note(s) changed during the rename and were left as they were: ${outcome.skippedConflict.join(", ")}`,
    );
  }
  if (outcome.failed.length > 0) {
    parts.push(
      `${outcome.failed.length} note(s) could not be rewritten: ${outcome.failed.map((f) => `${f.path} (${f.error})`).join("; ")}`,
    );
  }
  if (outcome.leftBroken.length > 0) {
    parts.push(
      `${outcome.leftBroken.length} link(s) were left pointing at the old path: ${outcome.leftBroken.map((l) => `${l.source} -> ${l.target}`).join(", ")}`,
    );
  }
  if (parts.length === 0) return null;
  const lead = outcome.complete
    ? "Renamed, with gaps"
    : `Renamed, but incomplete (${outcome.rewrittenPaths.length} note(s) rewritten)`;
  return `${lead}: ${parts.join(" · ")}`;
}

export function docsSearchHonesty(
  hits: DocsSearchHit[],
  limit: number = DOCS_SEARCH_DEFAULT_LIMIT,
): string | null {
  if (hits.length === 0) return null;
  if (hits.length >= limit) {
    return `Showing ${hits.length} hits (limit ${limit}). More matches may exist.`;
  }
  return null;
}

export function brokenLinksHonesty(
  links: BrokenLink[],
  displayCap: number,
): { shown: BrokenLink[]; honesty: string | null } {
  if (links.length <= displayCap) {
    return { shown: links, honesty: null };
  }
  return {
    shown: links.slice(0, displayCap),
    honesty: `Showing ${displayCap} of ${links.length} broken links.`,
  };
}
