/**
 * Reading `cmd_create_file`'s and `cmd_write_file_content`'s typed failure.
 *
 * The host rejects with `{code, message, ...}` (`engine::git_cli::
 * FileWriteError`), where most commands reject with a string. A rejection that
 * is not exactly that shape is not trusted to be one: the caller keeps the
 * original and reports it as it always did.
 */

import type { FileWriteError } from "./types";

/** A create or save that did not happen, carrying the host's typed reason. */
export class FileWriteFailure extends Error {
  readonly failure: FileWriteError;

  constructor(failure: FileWriteError) {
    super(failure.message);
    this.name = "FileWriteFailure";
    this.failure = failure;
  }
}

const isText = (value: unknown): value is string => typeof value === "string";

/** The typed failure in `cause`, or null when it is anything else. */
export function readFileWriteError(cause: unknown): FileWriteError | null {
  if (cause instanceof FileWriteFailure) return cause.failure;
  if (cause === null || typeof cause !== "object") return null;
  const v = cause as Record<string, unknown>;
  const message = v.message;
  if (!isText(message) || message.trim() === "") return null;
  switch (v.code) {
    case "already_exists":
      return isText(v.path) ? { code: "already_exists", message, path: v.path } : null;
    case "changed_on_disk":
      return isText(v.path) && isText(v.expected) && isText(v.actual)
        ? { code: "changed_on_disk", message, path: v.path, expected: v.expected, actual: v.actual }
        : null;
    case "refused":
      return { code: "refused", message };
    default:
      return null;
  }
}

/**
 * A save of a draft that has no recorded disk version. Saving it would mean
 * guessing which version it was made from, and the blob on screen may already
 * be a newer one, so the save is refused instead; only an explicit Overwrite
 * writes it.
 */
export class UnbasedDraft extends Error {
  constructor(path: string) {
    super(`${path} has no recorded disk version, so a save cannot tell whether the file changed. Nothing was written.`);
    this.name = "UnbasedDraft";
  }
}

/**
 * The base a save sends, or why it must not. An explicit `override` wins (it is
 * Overwrite, naming the version it saw, or null for a confirmed unconditional
 * write). A draft is judged against the version it was made from and never
 * against the blob now on screen: falling back to that would accept a file an
 * agent rewrote after the draft began. Only with no draft is the blob's own
 * base the one to use.
 */
export function saveBase(
  draft: { base: string | null } | undefined,
  blobBase: string | null | undefined,
  override?: string | null,
): { base: string | null } | "unbased" {
  if (override !== undefined) return { base: override };
  if (draft) return draft.base === null ? "unbased" : { base: draft.base };
  return { base: blobBase ?? null };
}

/** Every path the listing implies: each path given and each folder above one. */
export function withAncestors(files: readonly string[]): Set<string> {
  const listed = new Set<string>();
  for (const file of files) {
    const parts = file.split("/");
    for (let end = 1; end <= parts.length; end += 1) listed.add(parts.slice(0, end).join("/"));
  }
  return listed;
}

/**
 * Why a New File / New Folder name cannot be used before asking the host:
 * empty segments, `.` and `..`, and a name the listing already holds. The host
 * is still the authority (a file the tree has not listed is refused there);
 * this keeps the dialog from closing on a mistake it can see.
 */
export function newPathProblem(
  name: string,
  parentDir: string,
  existing: ReadonlySet<string>,
): string | null {
  const trimmed = name.trim();
  if (trimmed === "") return "Enter a name.";
  const segments = trimmed.split("/");
  if (segments.some((segment) => segment.trim() === "")) {
    return "A path cannot contain empty segments.";
  }
  if (segments.some((segment) => segment === "." || segment === "..")) {
    return "A path cannot contain . or .. segments.";
  }
  const target = parentDir === "" ? trimmed : `${parentDir}/${trimmed}`;
  return existing.has(target) ? `"${target}" already exists.` : null;
}
