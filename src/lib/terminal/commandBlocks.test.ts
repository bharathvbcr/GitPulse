import { describe, expect, it } from "vitest";
import {
  createCommandBlockTracker,
  createCommandDecorationRing,
  extractBlockCommand,
  extractBlockOutput,
  extractLoopbackServerUrl,
  formatCommandDuration,
  formatFailedCommandForAgent,
  parseOsc133,
  parseOsc7501,
  parseOsc7Path,
  resolveOsc7Subdir,
  type BufferMarker,
} from "./commandBlocks";

function mockMarker(initialLine: number): BufferMarker & { shift(delta: number): void } {
  let current = initialLine;
  let disposed = false;
  return {
    get line() {
      return disposed ? -1 : current;
    },
    get isDisposed() {
      return disposed;
    },
    shift(delta: number) {
      current += delta;
      if (current < 0) disposed = true;
    },
    dispose() {
      disposed = true;
    },
  };
}

describe("commandBlocks", () => {
  it("parses OSC 7 file URIs with percent-encoded UTF-8 and refuses sibling prefix escapes", () => {
    expect(parseOsc7Path("file://macbook.local/Users/dev/Code/GitPulse/src%20dir/sub/")).toBe(
      "/Users/dev/Code/GitPulse/src dir/sub",
    );
    expect(parseOsc7Path("file:///C:/Users/dev/repo/src")).toBe("C:/Users/dev/repo/src");
    expect(parseOsc7Path("https://evil.example/Users/dev")).toBeNull();
    expect(parseOsc7Path("file://localhost/bad%00path")).toBeNull();

    expect(resolveOsc7Subdir("/Users/dev/repo", "/Users/dev/repo")).toBe("");
    expect(resolveOsc7Subdir("/Users/dev/repo", "/Users/dev/repo/packages/ui")).toBe("packages/ui");
    expect(resolveOsc7Subdir("/Users/dev/repo", "/Users/dev/repo-evil/packages/ui")).toBeNull();
    expect(resolveOsc7Subdir("/Users/dev/repo", "/etc")).toBeNull();
  });

  it("parses OSC 133 prompt/command boundaries and exit statuses", () => {
    expect(parseOsc133("A")).toEqual({ kind: "prompt_start" });
    expect(parseOsc133("B")).toEqual({ kind: "command_start" });
    expect(parseOsc133("C")).toEqual({ kind: "command_executed" });
    expect(parseOsc133("D;0")).toEqual({ kind: "command_finished", exitCode: 0 });
    expect(parseOsc133("D;127")).toEqual({ kind: "command_finished", exitCode: 127 });
    expect(parseOsc133("D")).toEqual({ kind: "command_finished", exitCode: null });
    expect(parseOsc133("Z")).toBeNull();
  });

  it("parses Herdr OSC 7501 positional and key-value status updates", () => {
    expect(parseOsc7501("working;Running cargo test")).toEqual({
      state: "working",
      detail: "Running cargo test",
    });
    expect(parseOsc7501("state=blocked;message=Approve schema migration?")).toEqual({
      state: "blocked",
      detail: "Approve schema migration?",
    });
    expect(parseOsc7501("done")).toEqual({ state: "done", detail: null });
    expect(parseOsc7501("status;working;Compiling crates")).toEqual({
      state: "working",
      detail: "Compiling crates",
    });
    expect(parseOsc7501("status:blocked:message_b64=QXBwbHkgcGF0Y2g/")).toEqual({
      state: "blocked",
      detail: "Apply patch?",
    });
    expect(parseOsc7501("clear")).toEqual({ state: "clear", detail: null });
    expect(parseOsc7501("unknown;x")).toBeNull();
  });

  it("extracts loopback dev-server URLs across ANSI color codes and normalizes 0.0.0.0", () => {
    expect(extractLoopbackServerUrl("\x1b[32m  ➜  Local:   \x1b[36mhttp://localhost:5173/\x1b[0m\r\n")).toBe(
      "http://localhost:5173/",
    );
    expect(extractLoopbackServerUrl("Listening on http://0.0.0.0:3000/app.")).toBe(
      "http://localhost:3000/app",
    );
    expect(extractLoopbackServerUrl("Fetched https://api.github.com:443/repos")).toBeNull();
    expect(extractLoopbackServerUrl("Invalid port http://localhost:99999/")).toBeNull();
  });

  it("tracks semantic command blocks, survives scrollback trims, jumps prompts, and extracts output", () => {
    const tracker = createCommandBlockTracker(3);
    const created: Array<ReturnType<typeof mockMarker>> = [];
    const makeAt = (line: number) => () => {
      const m = mockMarker(line);
      created.push(m);
      return m;
    };

    tracker.onMark({ kind: "prompt_start" }, makeAt(2));
    tracker.onMark({ kind: "command_executed" }, makeAt(2));
    tracker.onMark({ kind: "command_finished", exitCode: 0 }, makeAt(5));

    tracker.onMark({ kind: "prompt_start" }, makeAt(5));
    tracker.onMark({ kind: "command_executed" }, makeAt(5));
    tracker.onMark({ kind: "command_finished", exitCode: 1 }, makeAt(9));

    expect(tracker.jumpTarget(9, "prev")).toBe(5);
    expect(tracker.jumpTarget(5, "prev")).toBe(2);
    expect(tracker.jumpTarget(2, "next")).toBe(5);

    const last = tracker.lastCompletedBlock();
    expect(last).toEqual({
      promptLine: 5,
      outputStartLine: 5,
      outputEndLine: 9,
      exitCode: 1,
      running: false,
    });

    const rows = [
      { text: "" },
      { text: "" },
      { text: "$ echo hi" },
      { text: "hi" },
      { text: "" },
      { text: "$ cargo test" },
      { text: "error[E0308]: mismatched " },
      { text: "types in src/main.rs:14", isWrapped: true },
      { text: "test result: FAILED" },
      { text: "$ " },
    ];
    const buffer = {
      length: rows.length,
      getLine(y: number) {
        const r = rows[y];
        if (!r) return undefined;
        return {
          isWrapped: r.isWrapped,
          translateToString: () => r.text,
        };
      },
    };
    expect(extractBlockOutput(buffer, last!)).toBe(
      "error[E0308]: mismatched types in src/main.rs:14\ntest result: FAILED",
    );

    // Simulate scrollback trim of 4 rows: first block at line 2 scrolls off (`line = -1`).
    for (const m of created) m.shift(-4);
    expect(tracker.blocks()).toEqual([
      {
        promptLine: 1,
        outputStartLine: 1,
        outputEndLine: 5,
        exitCode: 1,
        running: false,
      },
    ]);
  });

  it("records command duration, extracts prompt command text, bounds decoration ring, and formats failed command payloads", () => {
    const tracker = createCommandBlockTracker(4);
    const makeAt = (line: number) => () => mockMarker(line);

    tracker.onMark({ kind: "prompt_start" }, makeAt(1), 1000);
    tracker.onMark({ kind: "command_executed" }, makeAt(1), 1100);
    tracker.onMark({ kind: "command_finished", exitCode: 101 }, makeAt(4), 2450);

    const detail = tracker.lastFinishedDetail();
    expect(detail?.exitCode).toBe(101);
    expect(detail?.durationMs).toBe(1350);
    expect(formatCommandDuration(1350)).toBe("1.4s");
    expect(formatCommandDuration(42)).toBe("42ms");
    expect(formatCommandDuration(65_000)).toBe("1m 5s");

    const rows = [
      { text: "" },
      { text: "user@mac repo % cargo test --lib" },
      { text: "error[E0425]: cannot find value `x`" },
      { text: "error: test failed" },
      { text: "user@mac repo % " },
    ];
    const buffer = {
      length: rows.length,
      getLine(y: number) {
        const r = rows[y];
        if (!r) return undefined;
        return { translateToString: () => r.text };
      },
    };
    const cmd = extractBlockCommand(buffer, {
      promptLine: detail!.promptMarker.line,
      outputStartLine: detail!.outputStartMarker!.line,
    });
    expect(cmd).toBe("cargo test --lib");

    const prompt = formatFailedCommandForAgent({
      command: cmd,
      exitCode: 101,
      durationMs: 1350,
      cwd: "src-tauri",
      output: extractBlockOutput(buffer, {
        outputStartLine: detail!.outputStartMarker!.line,
        outputEndLine: detail!.outputEndMarker!.line,
      }),
    });
    expect(prompt).toContain("Command failed (exit 101, 1.4s) in `src-tauri`:");
    expect(prompt).toContain("$ cargo test --lib");
    expect(prompt).toContain("error[E0425]: cannot find value `x`");

    // Bounded decoration ring disposes oldest entry past capacity and clears all on reset.
    const disposed: string[] = [];
    const ring = createCommandDecorationRing(2);
    const makeEntry = (label: string) => ({
      marker: { line: 1, isDisposed: false, dispose: () => disposed.push(`m:${label}`) },
      decoration: { isDisposed: false, dispose: () => disposed.push(`d:${label}`) },
    });
    ring.push(makeEntry("one"));
    ring.push(makeEntry("two"));
    ring.push(makeEntry("three"));
    expect(ring.size()).toBe(2);
    expect(disposed).toEqual(["d:one", "m:one"]);
    ring.clear();
    expect(ring.size()).toBe(0);
    expect(disposed).toEqual(["d:one", "m:one", "d:three", "m:three", "d:two", "m:two"]);
  });
});
