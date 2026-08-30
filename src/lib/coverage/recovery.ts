import { classifyCoverageFailure, type CoverageLimitation } from "./report";

/**
 * A second, bounded attempt at a coverage command that failed for a cause
 * GitPulse can name and act on.
 *
 * Recovery is deliberately narrow. Most coverage failures are the repository's
 * to fix — a missing binary, a failing test suite — and retrying them just
 * burns the suite again. Only causes where GitPulse knows a *different*
 * command that would have worked are recoverable.
 *
 * A recovery is never free: excluding a file to get past a collection abort
 * measures a smaller codebase than the user asked about. `excludes` carries
 * exactly what was given up so no caller can render the resulting percentage
 * as though it came from a clean run.
 */
export interface CoverageRecoveryStep {
  /**
   * The argv to run. Authoritative: built by editing the argv that already
   * ran, never by re-parsing a display string. A path with a space in it
   * cannot become two arguments this way.
   */
  argv: string[];
  /** Display form of {@link argv}, for the panel and the copied report. */
  command: string;
}

export type { CoverageLimitation };

export interface CoverageRecovery {
  /** Commands to run, in order. */
  steps: CoverageRecoveryStep[];
  /**
   * `all` requires every step to succeed: Go coverage is cumulative across
   * modules, and a subset presented as the result would be a whole-repository
   * percentage over part of the repository.
   */
  mode: "all" | "first_success";
  limitation: CoverageLimitation;
  /** One sentence stating what was retried and what it cost. */
  note: string;
}

/**
 * Shell-ish rendering of an argv for display only. Nothing is ever executed
 * from this string — {@link CoverageRecovery.argv} is what runs.
 */
export function formatArgv(argv: readonly string[]): string {
  return argv
    .map((arg) => (/[\s"'\\]/.test(arg) ? `'${arg.replace(/'/g, "'\\''")}'` : arg))
    .join(" ");
}

/**
 * Resolves a path reported by a tool to one inside the open repository.
 *
 * Returns null when the path is outside the repository or escapes it, so a
 * traceback frame pointing into site-packages or `/etc` can never become an
 * argument. The backend gate refuses those too; refusing here as well means
 * GitPulse does not offer a command it knows will be rejected.
 */
export function repoRelativePath(repoPath: string, candidate: string): string | null {
  const repo = repoPath.replace(/\\/g, "/").replace(/\/+$/, "");
  const raw = candidate.replace(/\\/g, "/").trim();
  if (!repo || !raw) return null;
  let rel = raw;
  if (raw.startsWith("/")) {
    if (raw !== repo && !raw.startsWith(`${repo}/`)) return null;
    rel = raw.slice(repo.length + 1);
  }
  rel = rel.replace(/^\.\//, "");
  if (!rel || rel.startsWith("/")) return null;
  if (rel.split("/").some((segment) => segment === "..")) return null;
  return rel;
}

/**
 * What the recovery planner is allowed to know about the repository.
 *
 * Everything here comes from the coverage scan, never from parsing the
 * failure text: a traceback can name any path on the machine, but the module
 * directories were found by walking this repository.
 */
export interface RecoveryContext {
  repoPath: string;
  /** Go module directories the scan published; empty when it found none. */
  goModules?: readonly string[];
  /** Whether the scan's module search was cut short by a bound. */
  goModulesPartial?: boolean;
}

/** Strips a trailing `:<line>` from a `path:line` locator. */
function withoutLineNumber(locator: string): string {
  return locator.replace(/:\d+$/, "");
}

function runsPytest(argv: readonly string[]): boolean {
  return argv.some((arg) => arg === "pytest" || arg.endsWith("/pytest"));
}

function runsGo(argv: readonly string[]): boolean {
  const program = argv[0] ?? "";
  return program === "go" || program.endsWith("/go");
}

function alreadyIgnores(argv: readonly string[], rel: string): boolean {
  return argv.some(
    (arg, index) =>
      arg === `--ignore=${rel}` || (arg === "--ignore" && argv[index + 1] === rel),
  );
}

/**
 * Plans one retry for a failed coverage command, or null when nothing better
 * is known.
 *
 * Callers must run the result at most once per failure. An unbounded retry
 * loop is the failure mode here: a repository with two scripts that both abort
 * collection would otherwise re-run its suite once per script, and a repeating
 * cause would never terminate.
 */
export function planCoverageRecovery(
  argv: readonly string[],
  detail: unknown,
  context: RecoveryContext,
): CoverageRecovery | null {
  if (!Array.isArray(argv) || argv.length === 0) return null;
  const repoPath = context?.repoPath;
  if (typeof repoPath !== "string" || !repoPath) return null;
  const cause = classifyCoverageFailure(formatArgv(argv), detail);
  if (!cause) return null;

  if (cause.kind === "pytest_collection_abort") {
    // Without the aborting module there is nothing to exclude, and which file
    // to drop from measurement is not something to guess at.
    if (!cause.module || !runsPytest(argv)) return null;
    const rel = repoRelativePath(repoPath, withoutLineNumber(cause.module));
    if (!rel) return null;
    // The command already ran with this exclusion and aborted on the same
    // file — repeating it cannot change the outcome.
    if (alreadyIgnores(argv, rel)) return null;
    const next = [...argv, `--ignore=${rel}`];
    return {
      steps: [{ argv: next, command: formatArgv(next) }],
      mode: "all",
      limitation: { kind: "excluded_paths", paths: [rel] },
      note: `Retried without ${rel}: importing it called sys.exit(), which aborted pytest before any test ran. This coverage excludes that file.`,
    };
  }

  if (cause.kind === "go_missing_module") {
    if (!runsGo(argv)) return null;
    // A command that already names a directory failed *for that directory*.
    // Retrying with a different one is a different plan, not a retry, and
    // retrying with the same one cannot terminate.
    if (argv.includes("-C")) return null;
    const modules = uniqueModules(repoPath, context.goModules);
    if (modules.length === 0) return null;
    const steps = modules.map((dir) => {
      // `go -C <dir>` must precede the subcommand, so this inserts rather
      // than appends.
      const next = [argv[0], "-C", dir, ...argv.slice(1)];
      return { argv: next, command: formatArgv(next) };
    });
    const partial = context.goModulesPartial === true;
    const capped = partial
      ? " The module list was capped, so there may be modules this did not measure either."
      : "";
    return {
      steps,
      mode: "all",
      limitation: { kind: "scoped_to_modules", modules, partial },
      note: `Retried per Go module (${modules.join(", ")}): the repository root is not a module, so \`go test ./...\` could not run there. This coverage covers only those modules.${capped}`,
    };
  }

  return null;
}

/**
 * Module directories that are safe to hand to `go -C`.
 *
 * The workspace root is dropped: it is the directory the failing command
 * already ran in, so retrying there reproduces the failure.
 */
function uniqueModules(
  repoPath: string,
  modules: readonly string[] | undefined,
): string[] {
  if (!Array.isArray(modules)) return [];
  const seen: string[] = [];
  for (const dir of modules) {
    if (typeof dir !== "string") continue;
    const rel = repoRelativePath(repoPath, dir);
    if (!rel || seen.includes(rel)) continue;
    seen.push(rel);
  }
  return seen;
}
