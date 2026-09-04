import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import DiagnosticsModal, {
  classifyBackendDiagnostics,
  isCurrentBackendLoad,
} from "./DiagnosticsModal.svelte";
import { diagnostics } from "../diagnostics/diagnostics";
import { withBackendLogSection } from "../diagnostics/report";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "DiagnosticsModal.svelte"), "utf8");

describe("DiagnosticsModal", () => {
  it("renders nothing while closed", () => {
    const { body } = render(DiagnosticsModal, { props: { isOpen: false } });
    expect(body).not.toContain('role="dialog"');
  });

  it("keeps crash-only backend evidence reachable when the frontend ring is empty", () => {
    diagnostics.clear();
    const { body } = render(DiagnosticsModal, { props: { isOpen: true } });

    expect(body).toContain('role="dialog"');
    expect(body).toContain('aria-label="Filter by severity"');
    expect(body).toContain("No frontend diagnostics recorded");
    expect(body).toContain("Loading backend diagnostics");
    expect(body).toContain("Review local paths and command output before sharing");
    expect(body).toContain("Clear frontend");
    // The frontend can be empty immediately after a backend crash. Copy must
    // remain available so the durable log can still be exported after relaunch.
    expect(source).not.toContain(
      'onclick={copyReport} disabled={$diagnostics.length === 0}',
    );
    const copyLabel = body.indexOf("<span>Copy Report</span>");
    const copyButton = body.lastIndexOf("<button", copyLabel);
    expect(copyLabel).toBeGreaterThan(copyButton);
    expect(body.slice(copyButton, copyLabel)).not.toContain("disabled");
  });

  it("lists recorded errors and warnings with source, time and repeat count", () => {
    diagnostics.clear();
    diagnostics.error("pane-crash", "graph blew up");
    diagnostics.error("repo", "clone failed");
    diagnostics.warn("console", "watch out");

    const { body } = render(DiagnosticsModal, { props: { isOpen: true } });

    expect(body).toContain("graph blew up");
    expect(body).toContain("clone failed");
    expect(body).toContain("watch out");
    expect(body).toContain("pane-crash");
    // Severity chips render lowercase text styled uppercase via CSS.
    expect(body).toContain('tracking-wider text-rose-400">error</span>');
    expect(body).toContain('tracking-wider text-amber-400">warning</span>');
    // Header filter counts reflect occurrences per severity.
    expect(body).toContain('aria-label="Filter by severity"');
    expect(body).not.toContain("No frontend diagnostics recorded");
    diagnostics.clear();
  });

  it("qualifies a repeat count whose occurrences were not identical", () => {
    // Repeats group by fingerprint, so a bare count would present N
    // occurrences as N verbatim copies of the one message shown.
    diagnostics.clear();
    diagnostics.error("coverage", "no tests ran in 17.30s");
    diagnostics.error("coverage", "no tests ran in 17.00s");

    const { body } = render(DiagnosticsModal, { props: { isOpen: true } });

    expect(body).toContain("differing");
    expect(body).toContain("17.00s");
    expect(body).not.toContain("17.30s");
    diagnostics.clear();
  });

  it("leaves an identical repeat count unqualified", () => {
    diagnostics.clear();
    diagnostics.error("repo", "clone failed");
    diagnostics.error("repo", "clone failed");

    const { body } = render(DiagnosticsModal, { props: { isOpen: true } });

    expect(body).not.toContain("differing");
    diagnostics.clear();
  });
});

describe("withBackendLogSection", () => {
  it("returns the report unchanged when there is no backend log", () => {
    // Missing command / IPC failure must degrade to byte-identical output.
    expect(withBackendLogSection("GitPulse diagnostics — nothing recorded", [])).toBe(
      "GitPulse diagnostics — nothing recorded",
    );
  });

  it("appends the backend tail newest-last under a counted header", () => {
    const out = withBackendLogSection("report body", ["newest line", "older line"]);
    expect(out).toBe("report body\n\nBackend log (last 2)\n  newest line\n  older line");
    expect(out.indexOf("newest line")).toBeLessThan(out.indexOf("older line"));
  });
});

describe("DiagnosticsModal backend log context", () => {
  const persisted = (
    path: string,
    lines: string[] = [],
    degraded: string | null = null,
  ) => ({ path, lines, degraded });

  it("loads both backend sources together while the modal is open", () => {
    const load = source.indexOf("async function loadBackendContext");
    const memory = source.indexOf('"cmd_diagnostic_log_tail"', load);
    const durable = source.indexOf('"cmd_diagnostic_persisted_log"', load);
    const effect = source.indexOf("$effect(() =>", load);
    const copy = source.indexOf("async function copyReport", load);

    expect(load).toBeGreaterThan(-1);
    expect(source.slice(load, effect)).toContain("Promise.allSettled");
    expect(memory).toBeGreaterThan(load);
    expect(durable).toBeGreaterThan(memory);
    expect(effect).toBeGreaterThan(durable);
    expect(source.slice(effect, copy)).toContain("void beginBackendLoad()");
  });

  it("classifies healthy, empty, degraded and wholly unavailable reads distinctly", () => {
    expect(classifyBackendDiagnostics(null, ["current"], persisted("/logs/app.log"))).toBe(
      "healthy",
    );
    expect(classifyBackendDiagnostics(null, [], persisted("/logs/app.log"))).toBe("empty");
    expect(
      classifyBackendDiagnostics(null, [], persisted("/logs/app.log", [], "write failed")),
    ).toBe("degraded");
    expect(
      classifyBackendDiagnostics("memory IPC failed", [], persisted("", [], "read failed")),
    ).toBe("unavailable");
  });

  it("rejects a response from an earlier opening or a closed dialog", () => {
    expect(isCurrentBackendLoad(4, 4, true)).toBe(true);
    expect(isCurrentBackendLoad(3, 4, true)).toBe(false);
    expect(isCurrentBackendLoad(4, 4, false)).toBe(false);
    expect(source).toContain(
      "isCurrentBackendLoad(generation, backendLoadGeneration, isOpen)",
    );
  });

  it("states every backend condition and keeps the durable section in copied reports", () => {
    for (const label of [
      "Loading backend diagnostics",
      "Backend diagnostics healthy",
      "Backend diagnostics degraded",
      "Backend diagnostics unavailable",
      "Backend diagnostics empty",
    ]) {
      expect(source).toContain(label);
    }
    expect(source).toContain("withPersistedLogSection");
    expect(source).toContain("Backend memory log — unavailable");
  });

  it("bounds the whole dialog and lets only its content region scroll", () => {
    expect(source).toContain("max-h-[calc(100vh-2rem)] min-h-0");
    expect(source).toContain("min-h-0 flex-1 overflow-y-auto");
  });

  it("announces only the concise backend status instead of every log update", () => {
    const backendSection = source.slice(
      source.indexOf('aria-label="Backend diagnostics"'),
      source.indexOf("Frontend diagnostics"),
    );
    const sectionOpeningTag = backendSection.slice(0, backendSection.indexOf(">"));

    expect(sectionOpeningTag).not.toContain("aria-live=");
    expect(backendSection).toContain('role="status"');
    expect(backendSection).toContain('aria-live="polite"');
    expect(backendSection.indexOf('aria-live="polite"')).toBeLessThan(
      backendSection.indexOf("backendStatusLabel(backendStatus)"),
    );
  });
});
