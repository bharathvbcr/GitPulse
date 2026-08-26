import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  createReporter,
  reportPanelError,
  withBackendLogSection,
} from "./report";
import { diagnostics } from "./diagnostics";

/** Recording sink shaped like the DiagnosticsStore slice the reporter uses. */
function makeSink() {
  const recorded: Array<{ severity: "error" | "warning"; source: string; message: string }> = [];
  return {
    recorded,
    warn: (source: string, detail: unknown) =>
      recorded.push({ severity: "warning" as const, source, message: String(detail) }),
    error: (source: string, detail: unknown) =>
      recorded.push({ severity: "error" as const, source, message: String(detail) }),
  };
}

describe("createReporter", () => {
  it("formats the error once and routes it as a source-tagged warning by default", () => {
    const sink = makeSink();
    const report = createReporter(sink);
    const banner = report("blame", new TypeError("nope"));
    expect(banner).toBe("nope");
    expect(sink.recorded).toEqual([
      { severity: "warning", source: "blame", message: "nope" },
    ]);
  });

  it("routes opts.severity 'error' through sink.error with the same banner text", () => {
    const sink = makeSink();
    const report = createReporter(sink);
    const banner = report("storage", "disk exploded", { severity: "error" });
    expect(banner).toBe("disk exploded");
    expect(sink.recorded).toEqual([
      { severity: "error", source: "storage", message: "disk exploded" },
    ]);
  });

  it("keeps IPC strings and objects on the formatError contract", () => {
    const sink = makeSink();
    const report = createReporter(sink);
    expect(report("github", "  trimmed  ")).toBe("trimmed");
    expect(report("ops", { code: 7 })).toBe('{"code":7}');
    expect(report("conflict", null)).toBe("Unknown error");
    expect(report("rebase", "")).toBe("Unknown error");
  });

  it("survives a hostile getter that throws before formatError's own guards run", () => {
    const sink = makeSink();
    const report = createReporter(sink);
    const hostile = {
      get message(): string {
        throw new Error("getter boom");
      },
    };
    expect(() => report("stack", hostile)).not.toThrow();
    expect(sink.recorded).toEqual([
      { severity: "warning", source: "stack", message: "Unknown error" },
    ]);
  });

  it("coalesces identical repeats through the singleton like any other entry", () => {
    try {
      diagnostics.clear();
      reportPanelError("coverage", "same failure");
      reportPanelError("coverage", "same failure");
      const entries = get(diagnostics);
      expect(entries).toHaveLength(1);
      expect(entries[0].count).toBe(2);
      expect(entries[0].severity).toBe("warning");
    } finally {
      diagnostics.clear();
    }
  });
});

describe("withBackendLogSection", () => {
  it("leaves the report byte-identical when there is no backend log", () => {
    expect(withBackendLogSection("GitPulse diagnostics — nothing recorded", [])).toBe(
      "GitPulse diagnostics — nothing recorded",
    );
  });

  it("appends the backend tail newest-last after a blank separator line", () => {
    const out = withBackendLogSection("report body", [
      "[2026-08-25T12:00:00Z] newest",
      "[2026-08-25T11:59:00Z] older",
    ]);
    expect(out).toBe(
      "report body\n\nBackend log (last 2)\n  [2026-08-25T12:00:00Z] newest\n  [2026-08-25T11:59:00Z] older",
    );
  });

  it("counts multi-line tails in the header N", () => {
    const out = withBackendLogSection("r", ["a", "b", "c"]);
    expect(out).toContain("Backend log (last 3)");
    expect(out.endsWith("\n  c")).toBe(true);
  });
});
