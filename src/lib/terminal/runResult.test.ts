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
    truncation_reason: null,
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
 * Captured from two real coverage runs on 2026-10-01 (paths shortened). The
 * status row showed each one's first line — `info: … cfg(coverage)` and
 * `# <package>` — which name nothing; the cause was further down both times.
 */
const CARGO_LLVM_COV_MANIFEST_ERROR = [
  "info: cargo-llvm-cov currently setting cfg(coverage); you can opt-out it by passing --no-cfg-coverage",
  "error: ojas-metal/Cargo.toml: can't find `metal_bench` example at `examples/metal_bench.rs` or `examples/metal_bench/main.rs`. Please specify example.path if you want to use a non-default path.",
  "error: could not parse `ojas-metal` (manifest) due to 1 previous error",
  "error: process didn't exit successfully: `cargo test --tests --manifest-path /w/ojas/Cargo.toml --target-dir /w/ojas/target/llvm-cov-target --workspace` (exit status: 101)",
].join("\n");

const GO_TEST_LINK_FAILURE = [
  "# github.com/bharathvbcr/ojas/go.test",
  "/opt/homebrew/Cellar/go/1.27.1/libexec/pkg/tool/darwin_arm64/link: running cc failed: exit status 1",
  "/usr/bin/cc -arch arm64 -Wl,-S -Wl,-x -o $WORK/b001/go.test -Qunused-arguments /tmp/go-link/go.o /tmp/go-link/000000.o -O2 -g -lgusset -lpthread -lm -ldl",
  "Undefined symbols for architecture arm64:",
  '  "_ojas_engine_init", referenced from:',
  "      __cgo_80c61d759ee3_Cfunc_ojas_engine_init in 000001.o",
  "ld: symbol(s) not found for architecture arm64",
  "clang: error: linker command failed with exit code 1 (use -v to see invocation)",
  "",
  "FAIL\tgithub.com/bharathvbcr/ojas/go [build failed]",
  "FAIL",
].join("\n");

describe("formatRunSummary names the cause of a failed run", () => {
  it("skips cargo-llvm-cov's info banner for the manifest error beneath it", () => {
    const summary = formatRunSummary(result({ exit_code: 101, stderr_tail: CARGO_LLVM_COV_MANIFEST_ERROR }));
    expect(summary).toMatch(/^error: ojas-metal\/Cargo\.toml: can't find `metal_bench` example/);
  });

  it("skips rustc's file pointer and gutter instead of summarizing them", () => {
    const summary = formatRunSummary(
      result({
        stderr_tail: [" --> src/lib.rs:10:5", "  |", "10 |     missing", "  |     ^^^^^^^", "error: boom"].join(
          "\n",
        ),
      }),
    );
    expect(summary).toBe("error: boom");
  });

  it("skips go's package header and prefers the self-contained linker line", () => {
    const summary = formatRunSummary(result({ exit_code: 1, stdout_tail: GO_TEST_LINK_FAILURE }));
    expect(summary).toBe("ld: symbol(s) not found for architecture arm64");
  });

  it("finds the cause in stdout when stderr holds only noise", () => {
    expect(
      formatRunSummary(
        result({
          stderr_tail: "warning: unused import\n   Compiling demo v0.1.0",
          stdout_tail: "running 3 tests\nthread 'main' panicked at src/lib.rs:4:5:\nboom",
        }),
      ),
    ).toBe("thread 'main' panicked at src/lib.rs:4:5:");
  });

  it("falls back to a verdict line when no cause is printed", () => {
    expect(formatRunSummary(result({ stdout_tail: "ok  \tpkg/a\nFAIL\tpkg/b\nFAIL" }))).toBe("FAIL\tpkg/b");
  });

  it("strips terminal colour codes rather than showing them", () => {
    const summary = formatRunSummary(result({ stderr_tail: "\u001b[1m\u001b[31merror\u001b[0m: boom" }));
    expect(summary).toBe("error: boom");
  });

  it("keeps a passed run's first line, so success summaries do not change", () => {
    expect(
      formatRunSummary(result({ exit_code: 0, stderr_tail: "info: cargo-llvm-cov currently setting cfg(coverage)" })),
    ).toBe("info: cargo-llvm-cov currently setting cfg(coverage)");
  });

  it("stays one bounded line for hostile output", () => {
    const summary = formatRunSummary(result({ stderr_tail: `error: ${"x".repeat(50_000)}\r\nsecond` }));
    expect(summary).not.toContain("\n");
    expect(summary.length).toBeLessThanOrEqual(401);
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
