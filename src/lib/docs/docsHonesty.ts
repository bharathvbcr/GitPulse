/**
 * Honesty copy for the docs Map panel — vault caps, search limits, broken links.
 */

import type { BrokenLink, DocsSearchHit, DocsStatus } from "./client";
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
