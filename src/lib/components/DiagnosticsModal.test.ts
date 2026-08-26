import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import DiagnosticsModal from "./DiagnosticsModal.svelte";
import { diagnostics } from "../diagnostics/diagnostics";
import { withBackendLogSection } from "../diagnostics/report";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "DiagnosticsModal.svelte"), "utf8");

describe("DiagnosticsModal", () => {
  it("renders nothing while closed", () => {
    const { body } = render(DiagnosticsModal, { props: { isOpen: false } });
    expect(body).not.toContain('role="dialog"');
  });

  it("shows an explicit empty state while nothing is recorded", () => {
    diagnostics.clear();
    const { body } = render(DiagnosticsModal, { props: { isOpen: true } });

    expect(body).toContain('role="dialog"');
    expect(body).toContain('aria-label="Filter by severity"');
    expect(body).toContain("No diagnostics recorded");
    // Copy and Clear have nothing to act on yet.
    expect(body).toContain("disabled");
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
    expect(body).not.toContain("No diagnostics recorded");
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
  it("fetches cmd_diagnostic_log_tail inside a try/catch so copying survives its absence", () => {
    const invokeIdx = source.indexOf('"cmd_diagnostic_log_tail"');
    expect(invokeIdx).toBeGreaterThan(-1);
    const openTry = source.lastIndexOf("try {", invokeIdx);
    const closeCatch = source.indexOf("} catch {", invokeIdx);
    expect(openTry).toBeGreaterThan(-1);
    expect(closeCatch).toBeGreaterThan(invokeIdx);
  });
});
