import type { PolicyVerdict } from "../stores/harnessStore";

/**
 * Wire shape of `crate::terminal::TerminalRunResult`.
 *
 * Declared once. This interface previously existed three times — twice as a
 * named interface (`TerminalRunResult` in CoverageViewer, `TerminalRunResponse`
 * in TerminalPanel) and once inlined anonymously at HealthPanel's `invoke`
 * call — so a backend field rename could reach the UI as `undefined` in one
 * panel and be caught in none of them. `scripts/check-coverage-types.mjs`
 * checks this declaration against the Rust struct in both directions.
 */
export interface TerminalRunResult {
  command: string;
  gated: boolean;
  policy?: PolicyVerdict | null;
  timed_out: boolean;
  exit_code: number | null;
  stdout_tail: string;
  stderr_tail: string;
  truncated: boolean;
  /**
   * Why the tails are a prefix, when they are. Rendered instead of a guess:
   * "we stopped at the display budget" and "we never finished reading the
   * stream" are different facts, and one sentence for both asserts a cause
   * the UI cannot know.
   */
  truncation_reason: string | null;
  duration_ms: number;
}

/** A command succeeded only if it ran to completion and exited 0. */
export function runPassed(res: TerminalRunResult): boolean {
  return !res.timed_out && res.exit_code === 0;
}

function stream(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function formatDuration(ms: unknown): string {
  if (typeof ms !== "number" || !Number.isFinite(ms) || ms < 0) return "an unknown time";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds.toFixed(1)}s`;
  const minutes = Math.floor(seconds / 60);
  return `${minutes}m${Math.round(seconds - minutes * 60)}s`;
}

/**
 * A single line naming what happened, for the status row.
 *
 * Deliberately separate from {@link formatRunDetail}: one field was being
 * asked to be both a one-line teaser and a complete diagnostic, and could not
 * be both — labelling the streams in the detail would have reduced the row to
 * the word "stderr:".
 */
export function formatRunSummary(res: TerminalRunResult): string {
  if (res.timed_out) return `Timed out after ${formatDuration(res.duration_ms)} and was killed.`;
  const err = stream(res.stderr_tail);
  const out = stream(res.stdout_tail);
  if (!runPassed(res)) {
    const cause = failureLine(err, out);
    if (cause) return cause;
  }
  const firstLine = (text: string) => lines(text).find((line) => line.trim())?.trim() ?? "";
  const first = firstLine(err) || firstLine(out);
  if (first) return clampLine(first);
  return runPassed(res)
    ? "Command completed successfully (exit 0)"
    : `Command failed (exit ${res.exit_code ?? "?"})`;
}

const MAX_SUMMARY_CHARS = 400;
const ANSI_ESCAPE = /\u001b\[[0-9;?]*[ -/]*[@-~]/g;

function lines(text: string): string[] {
  return text.replace(ANSI_ESCAPE, "").split(/\r?\n/);
}

function clampLine(line: string): string {
  return line.length <= MAX_SUMMARY_CHARS ? line : `${line.slice(0, MAX_SUMMARY_CHARS)}…`;
}

/**
 * Lines a failing build prints that name nothing: progress, advisories, and
 * the package header Go prints above its real error. The first line of a
 * failed run's output is very often one of these — `cargo llvm-cov` opens
 * with `info: … setting cfg(coverage)`, `go test` with `# <package>` — and
 * the status row used to show exactly that line in place of the cause.
 */
const NOISE_LINE = [
  /^(info|warning|warn|note|help|hint)(\[[^\]]*\])?:/i,
  /^#\s/,
  /^(Compiling|Checking|Running|Finished|Downloading|Downloaded|Updating|Locking|Blocking|Fresh|Documenting|Installing|Installed|Resolving|Fetching|Building)\b/,
  /^\d+\s*\|/,
];

/**
 * rustc (and similar) diagnostic chrome: a file pointer, a gutter bar, or a
 * `= note:` continuation. These are prefix checks rather than one regular
 * expression, because a pattern written as `-->` is an HTML-comment closer
 * to a scanner and this filter is only discarding compiler chrome.
 */
function isDiagnosticChrome(line: string): boolean {
  if (line.startsWith("-->") || line.startsWith("|")) return true;
  return line.startsWith("=") && (line.length === 1 || /\s/.test(line.charAt(1)));
}

/** A line stating the cause: compiler, linker, runtime or test-runner error. */
const CAUSE_LINE = [
  /^error(\[[A-Z]*\d+\])?:/i,
  /^fatal( error)?:/i,
  /^Undefined symbols\b/,
  /^ld(\.lld)?: /,
  /^[\w./-]+:\d+(:\d+)?: (fatal )?error\b/i,
  /^([A-Z][A-Za-z]*)?(Error|Exception): /,
  /^E\s{2,}\S/,
  /\bpanicked at\b/,
  /: command not found$/,
  /\bcannot find (module|package)\b/i,
];

/** A line stating that something failed without saying why. */
const VERDICT_LINE = [/^FAIL\b/, /^--- FAIL\b/, /\[build failed\]/, /\bfailed\b/i, /\berror\b/i];

/**
 * The line of a failed run's output that says why it failed.
 *
 * A cause outranks a verdict ("ld: symbol(s) not found" over "FAIL … [build
 * failed]"), and a cause that is a complete sentence outranks one that only
 * introduces a list ("Undefined symbols for architecture arm64:"). Falls back
 * to the first line that is not progress noise, then to nothing, so the
 * caller's own fallback still applies.
 */
function failureLine(err: string, out: string): string {
  const candidates = [...lines(err), ...lines(out)].map((line) => line.trim()).filter(Boolean);
  const signal = candidates.filter(
    (line) => !isDiagnosticChrome(line) && !NOISE_LINE.some((pattern) => pattern.test(line)),
  );
  const causes = signal.filter((line) => CAUSE_LINE.some((pattern) => pattern.test(line)));
  const pick =
    causes.find((line) => !line.endsWith(":")) ??
    causes[0] ??
    signal.find((line) => VERDICT_LINE.some((pattern) => pattern.test(line))) ??
    signal[0] ??
    "";
  return pick ? clampLine(pick) : "";
}

/**
 * Everything the run told us, for the copied diagnostics.
 *
 * The rule this enforces is that **no captured stream is ever discarded**.
 * Three call sites previously built this detail as
 * `res.stderr_tail || res.stdout_tail`, so a non-empty stderr shadowed stdout
 * entirely. On the user's own Manvi repository that threw away the answer:
 * pytest wrote one near-content-free line to stderr ("mainloop: caught
 * unexpected SystemExit!") and 348 lines to stdout, ending in the file and
 * line number that caused it (`bench/stress_test.py:944`, `SystemExit: 0`,
 * "no tests ran"). Only the useless line survived, three times over.
 *
 * A timeout kept nothing at all — the longest-running failure yielded the
 * least information, when a partial log is exactly what a hung build needs.
 * Both streams are now kept in that case too.
 */
export function formatRunDetail(res: TerminalRunResult): string {
  const out = stream(res.stdout_tail);
  const err = stream(res.stderr_tail);
  const parts: string[] = [];

  if (res.timed_out) parts.push(formatRunSummary(res));

  if (out && err) {
    // Both present: label them, because an unlabelled concatenation of two
    // streams reads as one confusing log.
    parts.push(`stderr:\n${err}`, `stdout:\n${out}`);
  } else if (err) {
    parts.push(err);
  } else if (out) {
    parts.push(out);
  } else if (!res.timed_out) {
    parts.push(
      runPassed(res)
        ? "Command completed successfully (exit 0)"
        : `Command failed (exit ${res.exit_code ?? "?"}) and produced no output.`,
    );
  } else {
    parts.push("No output was captured before the timeout.");
  }

  // A clipped tail presented as the whole log is the same lie as a capped scan
  // presented as full coverage.
  if (res.truncated) parts.push("(output clipped)");
  return parts.join("\n");
}

/** The spawn acknowledgement: which session was started, and where. */
export interface TerminalSpawned {
  id: string;
  shell: string;
  cwd: string;
}

/**
 * Streamed terminal output. Consumed as an anonymous `{ id; data_b64 }` at the
 * listen() call until it was named — events are a wire surface too, and
 * check:types could not see this one at all.
 */
export interface TerminalOutputPayload {
  id: string;
  data_b64: string;
  /**
   * Bytes the reader reserved for this chunk. Absent on events from a host
   * that predates the field. A present value outside one read is ignored.
   */
  bytes?: number;
}

/**
 * What a live session is running and where, read once on request
 * (`cmd_terminal_context`). Every field can be unknown: the OS may decline to
 * describe a process, and unknown is never read as idle or as the root.
 */
export interface TerminalContext {
  process: string | null;
  /** A job other than the session's own process holds the foreground. */
  busy: boolean | null;
  cwd: string | null;
  /** `cwd` relative to the repository root (`""` is the root), or null outside it. */
  repo_dir: string | null;
}

/** Sent once when a session ends. Unknown exit status is null; signals and transport failures are separate. */
export interface TerminalExitPayload {
  id: string;
  exit_code: number | null;
  signal: string;
  error: string | null;
  /** False when the native host could not confirm process termination. */
  reaped: boolean;
}
