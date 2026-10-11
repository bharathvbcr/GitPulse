import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import CoverageViewer from "./CoverageViewer.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "CoverageViewer.svelte"),
  "utf8",
);

describe("CoverageViewer", () => {
  it("renders empty state when no repo is open", () => {
    const { body } = render(CoverageViewer);
    expect(body).toContain("Test coverage");
    expect(body).toContain("Rescan coverage artifacts");
    expect(body).toContain("No coverage report");
    expect(body).toContain("Pick a file");
  });

  it("always renders the MANVI toggle affordance in the header", () => {
    const { body } = render(CoverageViewer);
    expect(body).toContain("MANVI: analyze coverage with the local model");
  });

  it("always renders a copy-report affordance in the header", () => {
    const { body } = render(CoverageViewer);
    expect(body).toContain("Copy coverage report");
    expect(body).toContain("disabled");
  });

  it("offers agent prompting independently of scan and MANVI availability", () => {
    const { body } = render(CoverageViewer);
    expect(body).toContain("Copy agent prompt");
    expect(body).toContain("Run in Claude Code");
    expect(body).toContain("Coverage coding agent");
    expect(body).toContain("Codex");
    expect(body).toContain("Grok");
    expect(body).toContain("Antigravity");
    expect(body).toContain("Preview prompt");
  });
});
/**
 * What used to follow here was ~200 `toContain`/`toMatch` assertions on this
 * component's source text, so deleting a handler left every one of them green.
 * The behaviour lives where a page can be rendered: harness/coverage.html and
 * harness/coverageChecks.js drive the panel through the fixture IPC (copy,
 * MANVI plan steps, the setup/alternatives/verification pipeline, automatic
 * recovery, opt-in auto-run, scan lifecycle, selection, capped scans, honesty
 * strings, the issue flow) and each check was shown to fail under a mutation
 * of the source it covers.
 *
 * What remains cannot be seen from a rendered page. Each test says why.
 */
describe("CoverageViewer source contracts", () => {
  it("holds the file's source lines in raw state: replaced wholesale, never written in place", () => {
    // A $state proxy over a 100k-line file is a cost, not a visible behaviour.
    expect(source).toContain("let sourceLines: string[] = $state.raw([]);");
    expect(source).not.toMatch(/sourceLines\.(?:push|pop|shift|unshift|splice|sort|reverse|fill)\(/);
    expect(source).not.toMatch(/sourceLines\[[^\]]+\]\s*=[^=]/);
  });

  it("reaches the backend only through the shared runner and the shared wire type", () => {
    // The IPC payload and journal entry are pinned in terminal/manviAction.test.ts;
    // a second spelling of the command or of the result shape is not visible.
    expect(source).not.toContain('"cmd_manvi_run_action"');
    expect(source).not.toContain("interface TerminalRunResult {");
    expect(source).toContain('from "../terminal/runResult"');
  });

  it("pairs every feedback flash and copy timer with a reset and a dispose", () => {
    // Timers firing into an unmounted panel, or surviving a repository switch,
    // leave no trace a page can show; the countdown itself is createCopyFlash's
    // (desktop/clipboard.test.ts).
    const required = [
      "reportFlash.reset();", "reportFlash.dispose();", "aiFlash.reset();", "aiFlash.dispose();",
      "scanErrorFlash.reset();", "scanErrorFlash.dispose();",
      "window.clearTimeout(copiedScriptTimer);", "window.clearTimeout(copiedAllTimer);", "window.clearTimeout(copiedStepTimer);",
      "generationInflight?.cancel();", "opsInflight?.cancel();", "scanInflight?.cancel();",
    ];
    expect(required.filter((line) => !source.includes(line))).toEqual([]);
  });

  it("resets MANVI state from both branches of the repository effect", () => {
    // The harness host remounts the panel per repository, as the app does, so
    // the in-component reset is reachable only through the effect itself.
    expect(source.match(/resetManvi\(\);/g)?.length).toBe(2);
  });

  it("latches auto-run for a repository before starting it, not after", () => {
    // Redundant while the panel is remounted per repository (the Set is per
    // instance and the trigger is called from the mount scan only), which is
    // why the mutation that drops it survives the harness.
    const trigger = source.slice(
      source.indexOf("function maybeAutoRunCoverage"),
      source.indexOf("// Repo-scoped scan lifecycle"),
    );
    expect(trigger).toContain("if (autoRunAttempted.has(repo)) return;");
    const latch = trigger.indexOf("autoRunAttempted.add(repo);");
    expect(latch).toBeGreaterThan(-1);
    expect(latch).toBeLessThan(trigger.indexOf("void runMissingCoverage();"));
  });

  it("ignores publications of its own scan before they are adopted", () => {
    // Unobservable since adoption stopped starting a scan: dropping the guard
    // survives the harness, and the shared metric publishes before scan() resumes.
    expect(source).toContain("if (isScanning) return;");
  });

  it("says a capped scan's absent source may be incomplete rather than empty", () => {
    // Unreachable from a rendered page today: the empty-state branch that shows
    // this hint needs `!contentError`, and a failed content read always sets it.
    expect(source).toContain("this file's absence may be incomplete rather than a real zero");
  });
});
