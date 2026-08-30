import { describe, expect, it } from "vitest";
import {
  formatRunDetail,
  formatRunSummary,
  runPassed,
  type TerminalRunResult,
} from "./runResult";

function result(overrides: Partial<TerminalRunResult> = {}): TerminalRunResult {
  return {
    command: "npm run coverage",
    gated: true,
    policy: null,
    timed_out: false,
    exit_code: 1,
    stdout_tail: "",
    stderr_tail: "",
    truncated: false,
    duration_ms: 1234,
    ...overrides,
  };
}

/**
 * The defect these tests exist for: three call sites built a command's failure
 * detail as `res.stderr_tail || res.stdout_tail`, so a non-empty stderr
 * discarded stdout entirely.
 *
 * On a real repository that threw away the answer. pytest wrote one
 * near-content-free line to stderr and 348 lines to stdout ending in the file
 * and line that caused the abort; only the useless line survived.
 */
describe("formatRunDetail never discards a captured stream", () => {
  it("keeps stdout when stderr is also present", () => {
    const detail = formatRunDetail(
      result({
        stderr_tail: "mainloop: caught unexpected SystemExit!",
        stdout_tail: 'INTERNALERROR> File "bench/stress_test.py", line 944, in <module>\nSystemExit: 0',
      }),
    );
    expect(detail).toContain("mainloop: caught unexpected SystemExit!");
    expect(detail).toContain("bench/stress_test.py");
    expect(detail).toContain("SystemExit: 0");
    // Labelled, because an unlabelled splice of two streams reads as one log.
    expect(detail).toContain("stderr:");
    expect(detail).toContain("stdout:");
  });

  it("keeps output produced before a timeout", () => {
    // The longest-running failure used to yield the least information: the
    // timeout branch replaced everything with a single sentence.
    const detail = formatRunDetail(
      result({ timed_out: true, stdout_tail: "compiling crate 412/900", duration_ms: 900_000 }),
    );
    expect(detail).toContain("compiling crate 412/900");
    expect(detail).toContain("Timed out after 15m0s");
  });

  it("says so plainly when a timeout captured nothing", () => {
    const detail = formatRunDetail(result({ timed_out: true, duration_ms: 5000 }));
    expect(detail).toContain("Timed out after 5.0s");
    expect(detail).toContain("No output was captured before the timeout.");
  });

  it("marks a clipped tail as clipped in every branch", () => {
    for (const res of [
      result({ truncated: true, stdout_tail: "a" }),
      result({ truncated: true, stderr_tail: "b" }),
      result({ truncated: true, stdout_tail: "a", stderr_tail: "b" }),
      result({ truncated: true, timed_out: true }),
      result({ truncated: true, exit_code: 0 }),
    ]) {
      expect(formatRunDetail(res), JSON.stringify(res)).toContain("(output clipped)");
    }
    // …and never claims clipping that did not happen.
    expect(formatRunDetail(result({ stdout_tail: "a" }))).not.toContain("(output clipped)");
  });

  it("reports a silent failure as a failure, not as success", () => {
    const detail = formatRunDetail(result({ exit_code: 7 }));
    expect(detail).toContain("exit 7");
    expect(detail).not.toContain("successfully");
  });

  it("distinguishes a silent success", () => {
    expect(formatRunDetail(result({ exit_code: 0 }))).toContain(
      "Command completed successfully (exit 0)",
    );
  });
});

describe("runPassed", () => {
  it("requires both a clean exit and no timeout", () => {
    expect(runPassed(result({ exit_code: 0 }))).toBe(true);
    expect(runPassed(result({ exit_code: 1 }))).toBe(false);
    expect(runPassed(result({ exit_code: null }))).toBe(false);
    // A killed process can still report exit 0 on some platforms; the timeout
    // flag is authoritative.
    expect(runPassed(result({ exit_code: 0, timed_out: true }))).toBe(false);
  });
});

describe("formatRunSummary stays a single usable line", () => {
  it("never returns empty, whatever the payload", () => {
    for (const res of [
      result(),
      result({ exit_code: 0 }),
      result({ exit_code: null }),
      result({ stdout_tail: "   \n\n  " }),
      result({ stderr_tail: "\t\n" }),
      result({ timed_out: true }),
    ]) {
      expect(formatRunSummary(res).trim(), JSON.stringify(res)).not.toBe("");
    }
  });

  it("never spans multiple lines, so the status row cannot be broken by output", () => {
    const hostile = "first line\nsecond line\nthird line";
    for (const res of [
      result({ stderr_tail: hostile }),
      result({ stdout_tail: hostile }),
      result({ stdout_tail: hostile, stderr_tail: hostile }),
      result({ timed_out: true, stdout_tail: hostile }),
    ]) {
      expect(formatRunSummary(res)).not.toContain("\n");
    }
  });

  it("skips leading blank lines rather than summarizing a command as nothing", () => {
    expect(formatRunSummary(result({ stderr_tail: "\n\n   \nreal error here" }))).toBe(
      "real error here",
    );
  });

  it("falls back through stderr, then stdout, then the exit status", () => {
    expect(formatRunSummary(result({ stderr_tail: "E", stdout_tail: "O" }))).toBe("E");
    expect(formatRunSummary(result({ stdout_tail: "O" }))).toBe("O");
    expect(formatRunSummary(result({ exit_code: 3 }))).toBe("Command failed (exit 3)");
    expect(formatRunSummary(result({ exit_code: null }))).toBe("Command failed (exit ?)");
  });
});

/**
 * The payload crosses an IPC boundary. A field that arrives as the wrong type
 * must degrade to "no information", never to a thrown render.
 */
describe("hostile and malformed payloads", () => {
  it("survives non-string streams", () => {
    for (const bad of [null, undefined, 42, {}, [], true, NaN]) {
      const res = result({
        stdout_tail: bad as unknown as string,
        stderr_tail: bad as unknown as string,
      });
      expect(() => formatRunDetail(res)).not.toThrow();
      expect(() => formatRunSummary(res)).not.toThrow();
      expect(formatRunSummary(res)).not.toContain("[object");
      expect(formatRunDetail(res)).not.toContain("[object");
    }
  });

  it("never renders a nonsense duration", () => {
    for (const bad of [null, undefined, NaN, Infinity, -Infinity, -1, "long" as unknown as number]) {
      const summary = formatRunSummary(result({ timed_out: true, duration_ms: bad as number }));
      expect(summary).toContain("Timed out after");
      expect(summary).not.toMatch(/NaN|Infinity|-\d/);
    }
  });

  it("formats durations across every magnitude", () => {
    const at = (ms: number) => formatRunSummary(result({ timed_out: true, duration_ms: ms }));
    expect(at(0)).toContain("0ms");
    expect(at(999)).toContain("999ms");
    expect(at(1000)).toContain("1.0s");
    expect(at(59_900)).toContain("59.9s");
    expect(at(60_000)).toContain("1m0s");
    expect(at(3_600_000)).toContain("60m0s");
  });

  it("cannot be tricked into forging a stream label", () => {
    // Output that contains the labels must not be mistakable for the real
    // structure: when both streams are present the labels are line-anchored
    // and each stream's body is present verbatim underneath its own label.
    const detail = formatRunDetail(
      result({
        stderr_tail: "stdout:\nnot really stdout",
        stdout_tail: "genuine stdout",
      }),
    );
    const lines = detail.split("\n");
    expect(lines[0]).toBe("stderr:");
    expect(lines.indexOf("stdout:")).toBeGreaterThan(0);
    // The real stdout section is the last one, and holds the real content.
    expect(lines[lines.lastIndexOf("stdout:") + 1]).toBe("genuine stdout");
  });

  it("handles very large tails without truncating them itself", () => {
    const big = "x".repeat(64 * 1024);
    const detail = formatRunDetail(result({ stdout_tail: big, stderr_tail: "e" }));
    expect(detail).toContain(big);
    expect(detail.length).toBeGreaterThan(64 * 1024);
  });

  it("treats a whitespace-only stream as no output at all", () => {
    const detail = formatRunDetail(result({ stdout_tail: "   \n\t\n", stderr_tail: "" }));
    expect(detail).toContain("exit 1");
    expect(detail).not.toMatch(/stdout:/);
  });
});
