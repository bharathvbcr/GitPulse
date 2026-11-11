import { isPromptLauncher } from "./launchRequests";
import type { LauncherKind } from "./tabs";

const SAFE_UNQUOTED_SHELL_PATH = /^[A-Za-z0-9_./:@+][A-Za-z0-9_./:@+-]*$/;

/** Strips embedded NUL bytes that cannot cross POSIX argv/PTY boundaries safely. */
function stripNul(value: string): string {
  return value.replace(/\0+/g, "");
}

/**
 * Formats one path for safe insertion into an interactive POSIX shell.
 *
 * Clean relative/absolute paths without spaces, shell metacharacters, or a
 * leading `-` stay unquoted for readability; everything else is wrapped in
 * POSIX single quotes with internal `'` escaped as `'\''`.
 */
export function posixShellQuote(rawPath: string): string {
  const clean = stripNul(rawPath);
  if (clean.length === 0) return "''";
  if (SAFE_UNQUOTED_SHELL_PATH.test(clean)) return clean;
  return `'${clean.replace(/'/g, "'\\''")}'`;
}

function normalizeSlashes(path: string): string {
  return stripNul(path).replace(/\\/g, "/");
}

function isAbsoluteAnyPlatform(path: string): boolean {
  return path.startsWith("/") || /^[A-Za-z]:\//.test(path);
}

function drivePrefixOf(path: string): string | null {
  return /^[A-Za-z]:\//.test(path) ? path[0].toLowerCase() : null;
}

function normalizePosixSegments(path: string): string[] | null {
  const out: string[] = [];
  const body = /^[A-Za-z]:\//.test(path) ? path.slice(2) : path;
  for (const part of body.split("/")) {
    if (!part || part === ".") continue;
    if (part === "..") {
      if (out.length === 0) return null;
      out.pop();
      continue;
    }
    out.push(part);
  }
  return out;
}

/**
 * Returns the clean slash-separated repository-relative path when `filePath`
 * resides inside `repoPath`, or `null` when it lies outside `repoPath`
 * (including `..` traversals that climb out of `repoPath`).
 */
export function relativeToRepo(filePath: string, repoPath: string): string | null {
  const normFile = normalizeSlashes(filePath).trim();
  if (!normFile) return null;

  if (!isAbsoluteAnyPlatform(normFile)) {
    if (normFile === "." || normFile === "./") return ".";
    const segs = normalizePosixSegments(normFile);
    if (!segs) return null;
    return segs.length > 0 ? segs.join("/") : ".";
  }

  const normRepo = normalizeSlashes(repoPath).trim();
  if (!normRepo || !isAbsoluteAnyPlatform(normRepo)) return null;
  if (drivePrefixOf(normFile) !== drivePrefixOf(normRepo)) return null;
  const repoSegs = normalizePosixSegments(normRepo);
  const fileSegs = normalizePosixSegments(normFile);
  if (!repoSegs || !fileSegs || repoSegs.length === 0) return null;
  if (fileSegs.length < repoSegs.length) return null;
  for (let i = 0; i < repoSegs.length; i++) {
    if (fileSegs[i] !== repoSegs[i]) return null;
  }
  const relSegs = fileSegs.slice(repoSegs.length);
  return relSegs.length > 0 ? relSegs.join("/") : ".";
}

/**
 * Formats a file path (and optional 1-based line range) as an `@path` token
 * understood by Claude Code, Antigravity (`agy`), Codex, and Grok CLI.
 */
export function formatAgentReference(
  targetPath: string,
  repoPath: string,
  range?: { startLine: number; endLine?: number },
): string {
  const cleanTarget = stripNul(targetPath).trim();
  if (!cleanTarget) return "";
  const display = relativeToRepo(cleanTarget, repoPath) ?? normalizeSlashes(cleanTarget);
  let suffix = "";
  if (range && Number.isFinite(range.startLine) && range.startLine >= 1) {
    const start = Math.floor(range.startLine);
    const end =
      range.endLine !== undefined && Number.isFinite(range.endLine)
        ? Math.floor(range.endLine)
        : start;
    suffix = end > start ? `#L${start}-L${end}` : `#L${start}`;
  }
  const combined = `${display}${suffix}`;
  if (/[\s"'`$\\]/.test(combined)) {
    const escaped = combined.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
    return `@"${escaped}"`;
  }
  return `@${combined}`;
}

/**
 * Formats one or more dropped/pasted file paths for the target terminal
 * session, trailing with a single space so multiple drops compose cleanly.
 */
export function formatDroppedPathsForSession(
  paths: readonly string[],
  options: { launcher: LauncherKind; repoPath: string },
): string {
  const valid = paths.map((p) => stripNul(p).trim()).filter((p) => p.length > 0);
  if (valid.length === 0) return "";

  if (isPromptLauncher(options.launcher)) {
    const refs = valid
      .map((p) => formatAgentReference(p, options.repoPath))
      .filter((r) => r.length > 0);
    return refs.length > 0 ? `${refs.join(" ")} ` : "";
  }

  const quoted = valid.map((p) => {
    const rel = relativeToRepo(p, options.repoPath);
    return posixShellQuote(rel ?? p);
  });
  return `${quoted.join(" ")} `;
}
