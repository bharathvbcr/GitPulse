import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  createReporter,
  formatDiagnosticFailure,
  reportPanelError,
  withBackendLogSection,
  withPersistedLogSection,
} from "./report";
import { unreadablePersistedLog, type PersistedLog } from "./types";
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
  it("redacts a secret-bearing IPC rejection before it can reach UI state", () => {
    const detail = formatDiagnosticFailure(
      new Error(
        "Authorization: Bearer opaque-rejection https://me:password@example.test/r",
      ),
    );
    expect(detail).toContain("Authorization: Bearer <redacted>");
    expect(detail).toContain("https://me:<redacted>@example.test/r");
    expect(detail).not.toContain("opaque-rejection");
    expect(detail).not.toContain("password");
  });

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

  it("redacts credentials from legacy backend lines before export", () => {
    const key = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";
    const out = withBackendLogSection("r", [`Authorization failed for ${key}`]);
    expect(out).not.toContain(key);
    expect(out).toContain("ghp_");
  });
});

describe("withPersistedLogSection", () => {
  const healthy = (lines: string[]): PersistedLog => ({
    path: "/tmp/gitpulse.log",
    lines,
    degraded: null,
  });

  it("renders the durable tail oldest-first under a header naming the file", () => {
    const out = withPersistedLogSection("report body", healthy(["older", "newer"]));
    expect(out).toBe(
      "report body\n\nDurable backend log (2 line(s) from /tmp/gitpulse.log)\n  older\n  newer",
    );
    expect(out.indexOf("older")).toBeLessThan(out.indexOf("newer"));
  });

  it("still writes a section when the durable log is empty", () => {
    // Silence here would be read as "the backend said nothing went wrong",
    // which is a different fact from "the backend recorded nothing".
    const out = withPersistedLogSection("report body", healthy([]));
    expect(out).toContain("Durable backend log (0 line(s) from /tmp/gitpulse.log)");
  });

  it("says the log is unavailable rather than omitting the section", () => {
    const out = withPersistedLogSection("report body", {
      path: "",
      lines: [],
      degraded: "no durable log for this binary",
    });
    expect(out).toContain("Durable backend log — unavailable");
    expect(out).toContain("! incomplete: no durable log for this binary");
  });

  it("marks a truncated log as incomplete beside the lines it did keep", () => {
    // The dangerous case: lines are present, so the section looks whole.
    const out = withPersistedLogSection("report body", {
      path: "/tmp/gitpulse.log",
      lines: ["kept"],
      degraded: "rotate failed, truncated instead: EACCES",
    });
    expect(out).toContain("! incomplete: rotate failed, truncated instead: EACCES");
    expect(out).toContain("  kept");
    expect(out.indexOf("! incomplete")).toBeLessThan(out.indexOf("  kept"));
  });

  it("turns a failed read into a stated reason, not an empty log", () => {
    const out = withPersistedLogSection("r", unreadablePersistedLog("IPC rejected"));
    expect(out).toContain("Durable backend log — unavailable");
    expect(out).toContain("could not be read: IPC rejected");
  });

  it("redacts credentials from legacy durable metadata and lines before export", () => {
    const key = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";
    const out = withPersistedLogSection("r", {
      path: `/tmp/${key}/gitpulse.log`,
      lines: [`failed with ${key}`],
      degraded: `write rejected for ${key}`,
    });
    expect(out).not.toContain(key);
    expect(out).toContain("ghp_");
  });
});
