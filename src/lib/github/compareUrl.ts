/**
 * The GitHub "open a pull request" URL for a pair of branches.
 *
 * GitPulse does not create the pull request itself — that is a mutation the
 * browser form owns, with its own review and draft controls. What this app
 * can do honestly is land the user on that form with the branches filled in,
 * which is the same bridge GitHub Desktop uses when `gh` is not the writer.
 */

const REPO_URL = /^https:\/\/[^/\s]+\/[^/\s]+\/[^/\s]+$/i;

/**
 * True when a ref is safe to place on one side of GitHub's `base...head`
 * compare delimiter. Empty, whitespace, or a ref that itself contains `...`
 * would build a URL that names the wrong comparison or none at all.
 */
export function isCompareRef(value: string): boolean {
  if (!value) return false;
  if (/\s/.test(value)) return false;
  if (value.includes("...")) return false;
  if (value.includes("?")) return false;
  if (value.includes("#")) return false;
  return true;
}

/**
 * `https://github.com/owner/repo/compare/base...head?expand=1`, or null when
 * the URL would be a lie: missing repo, identical sides, or a ref that would
 * break the delimiter.
 */
export function pullRequestCreateUrl(
  htmlUrl: string,
  base: string,
  head: string,
): string | null {
  const repo = htmlUrl.trim().replace(/\/+$/, "");
  if (!REPO_URL.test(repo)) return null;
  if (!isCompareRef(base) || !isCompareRef(head)) return null;
  if (base === head) return null;
  return `${repo}/compare/${encodeURIComponent(base)}...${encodeURIComponent(head)}?expand=1`;
}
