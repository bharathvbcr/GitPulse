/**
 * What has to be true before git can write anything: a git new enough to
 * drive, and an identity to record commits under. Both used to surface as
 * git's own failure from whichever command ran first; these are the shapes
 * the backend now reports them in, and the checks the UI reads them with.
 */

/** Mirrors the Rust `GitPreflightStatus`. */
export type GitPreflightStatus = "ok" | "missing" | "outdated" | "broken" | "unchecked";

/** Mirrors the Rust `GitPreflight`. */
export interface GitPreflight {
  status: GitPreflightStatus;
  version: string | null;
  message: string | null;
}

const STATUSES: readonly GitPreflightStatus[] = ["ok", "missing", "outdated", "broken", "unchecked"];

/** Refuses malformed IPC rather than reading it as "git is fine". */
export function parseGitPreflight(value: unknown): GitPreflight {
  if (!value || typeof value !== "object") throw new Error("Invalid git preflight");
  const record = value as Record<string, unknown>;
  const status = record.status;
  if (typeof status !== "string" || !STATUSES.includes(status as GitPreflightStatus)) {
    throw new Error("Invalid git preflight status");
  }
  const text = (field: unknown) => (typeof field === "string" ? field : null);
  return { status: status as GitPreflightStatus, version: text(record.version), message: text(record.message) };
}

/**
 * The sentence to show, or null when there is nothing to tell the user.
 * `unchecked` is the probe being shed under load: it says nothing about git,
 * so it is not presented as a problem with git.
 */
export function preflightProblem(preflight: GitPreflight): string | null {
  if (preflight.status === "ok" || preflight.status === "unchecked") return null;
  return preflight.message ?? "Git could not be used.";
}

/** Mirrors the Rust `GitIdentity`. */
export interface GitIdentity {
  name: string | null;
  email: string | null;
}

/** Mirrors the Rust `IdentityScope`. */
export type IdentityScope = "repo" | "global";

/** Opening phrase of a commit refused for want of an identity (Rust `IDENTITY_MISSING`). */
export const IDENTITY_MISSING = "Git does not know who you are";

export function isIdentityMissing(error: string | null | undefined): boolean {
  return typeof error === "string" && error.startsWith(IDENTITY_MISSING);
}

/** The phrase a user-cancelled hook-running command carries (Rust `CANCELLED_MARKER`). */
const HOOK_CANCELLED_MARKER = " was cancelled while running ";

export function isHookCancelled(error: string | null | undefined): boolean {
  return typeof error === "string" && error.includes(HOOK_CANCELLED_MARKER);
}

/** Client-side mirror of `GitWriter::validate_identity`; the backend still decides. */
export function identityProblem(name: string, email: string): string | null {
  const n = name.trim();
  const e = email.trim();
  if (!n || n.length > 256 || /[\u0000-\u001f\u007f]/.test(n)) return "Enter a name of 1 to 256 printable characters.";
  if (/[<>]/.test(n)) return "A name cannot contain < or >.";
  const at = e.lastIndexOf("@");
  if (e.length > 320 || /[\s<>\u0000-\u001f\u007f]/.test(e) || at <= 0 || at === e.length - 1) {
    return "Enter an email address such as you@example.org.";
  }
  return null;
}
