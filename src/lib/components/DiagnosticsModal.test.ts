import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import DiagnosticsModal from "./DiagnosticsModal.svelte";
import { diagnostics } from "../diagnostics/diagnostics";

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
