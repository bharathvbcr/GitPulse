import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import HealthPanel, { dependabotFreshness } from "./HealthPanel.svelte";
import { HEALTH_SECTIONS } from "../health/sections";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "HealthPanel.svelte"),
  "utf8",
);

describe("HealthPanel rendering", () => {
  it("renders the Health header and initial scan state", () => {
    const { body } = render(HealthPanel);
    expect(body).toContain("Health");
    expect(body).toContain("Scan");
    expect(body).toContain("GitHub alerts are checked when GitPulse launches");
    // With no repository open there is nothing to have checked, so the
    // Dependabot state chip stays out of the header entirely.
    expect(body).not.toContain("Dependabot not checked");
    expect(body).not.toContain("Code scanning not checked");
    expect(body).toContain("Open a repository to scan dependency health");
  });

  it("gives a Dependabot result an unambiguous machine timestamp and a local label", () => {
    const freshness = dependabotFreshness(Date.UTC(2026, 8, 3, 12, 34, 56));
    expect(freshness.iso).toBe("2026-09-03T12:34:56.000Z");
    expect(freshness.label.length).toBeGreaterThan(0);
  });

  it("labels outdated results as npm-only, through the catalog heading", () => {
    expect(HEALTH_SECTIONS.find((s) => s.id === "outdated")?.heading).toBe(
      "Outdated npm packages",
    );
  });
});

/**
 * What used to follow here was ~170 `toContain`/`toMatch` assertions on this
 * component's source text. The behaviour lives in harness/health.html and
 * harness/healthChecks.js, which drive the mounted panel through fixture IPC
 * (copied and model-bound report, remediation steps and their journal, agent
 * launch and its cancellation, local-versus-GitHub separation, scan lifecycle,
 * failure separation, dead-code states, observed-total counts, ecosystem and
 * census disclosure, the failed-scanner wording), and each check was shown to
 * fail under a mutation of the source it covers.
 *
 * What remains cannot be seen from a rendered page. Each test says why.
 */
describe("HealthPanel source contracts", () => {
  it("reaches the backend only through the shared runner", () => {
    // The IPC payload and journal entry are pinned in terminal/manviAction.test.ts;
    // an inline copy of the wire shape or of the command is not visible.
    expect(source).not.toContain("invoke<{");
    expect(source).not.toContain('"cmd_manvi_run_action"');
  });

  it("owns no headings and hands every counted section a count object", () => {
    // The structural half of the "no hand-built count" rule: the number each
    // heading prints is checked in the harness against the shared formatter,
    // but a heading or a quoted string written here could not be.
    expect(source).not.toMatch(/<h3\b/);
    expect([...source.matchAll(/<HealthSection\b/g)].length).toBeGreaterThan(5);
    const counts = [...source.matchAll(/count=\{([^}]*)\}/g)].map((m) => m[1].trim());
    expect(counts.length, "no counted sections found").toBeGreaterThan(3);
    expect(counts.filter((expression) => /["'`]/.test(expression))).toEqual([]);
  });

  it("releases its pending work and feedback timers on teardown", () => {
    // A late completion writing into an unmounted panel, or a timer outliving
    // it, leaves nothing a page can show.
    const teardownStart = source.indexOf("return () => {");
    const teardown = source.slice(teardownStart, source.indexOf("};", teardownStart));
    const required = [
      "inflight?.cancel();", "dependabotInflight?.cancel();", "fixInflight?.cancel();", "stepsInflight?.cancel();",
      "copyFlash.dispose();", "planCopyFlash.dispose();", "agentCopyFlash.dispose();",
    ];
    expect(required.filter((line) => !teardown.includes(line))).toEqual([]);
  });
});
