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
  const firstLine = (text: string) => text.split("\n").find((line) => line.trim())?.trim() ?? "";
  const err = firstLine(stream(res.stderr_tail));
  if (err) return err;
  const out = firstLine(stream(res.stdout_tail));
  if (out) return out;
  return runPassed(res)
    ? "Command completed successfully (exit 0)"
    : `Command failed (exit ${res.exit_code ?? "?"})`;
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
