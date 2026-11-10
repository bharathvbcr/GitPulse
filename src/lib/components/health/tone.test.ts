import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import HealthSummaryCard from "./HealthSummaryCard.svelte";
import { issueNoticeTone, roleTextClass, roleTone, severityTone, updateKindTextClass } from "./tone";
import type { HealthTone } from "../../health/summary";

const TONES: readonly HealthTone[] = ["clear", "note", "unknown", "warn", "critical"];

describe("Health tone roles", () => {
  it("maps every verdict tone to a design role, keeping unknown apart", () => {
    expect(TONES.map(roleTone)).toEqual(["ok", "info", "unknown", "warn", "danger"]);
    // A check that could not run must not wear the clear or the warn role.
    expect(roleTone("unknown")).not.toBe(roleTone("clear"));
    expect(roleTone("unknown")).not.toBe(roleTone("warn"));
  });

  it("gives each role a text utility from the role tokens, never a palette color", () => {
    for (const tone of TONES) {
      expect(roleTextClass(roleTone(tone))).toMatch(/^text-(ok|info|unknown|warn|danger)$/);
    }
  });

  it("reads severity through the shared normalizer", () => {
    expect(severityTone("HIGH")).toBe("danger");
    expect(severityTone("Critical")).toBe("danger");
    expect(severityTone("medium")).toBe("warn");
    expect(severityTone("moderate")).toBe("warn");
    expect(severityTone("low")).toBe("info");
    expect(severityTone("info")).toBe("neutral");
    expect(severityTone("")).toBe("neutral");
    // CodeQL's `error` and `warning` ride the same path.
    expect(severityTone("error")).toBe("danger");
    expect(severityTone("warning")).toBe("warn");
  });

  it("maps configuration-issue severity and update kind to roles", () => {
    expect(issueNoticeTone("error")).toBe("danger");
    expect(issueNoticeTone(" Warning ")).toBe("warn");
    expect(issueNoticeTone("note")).toBe("info");
    expect(updateKindTextClass("major")).toBe("text-danger");
    expect(updateKindTextClass("minor")).toBe("text-warn");
    expect(updateKindTextClass("patch")).toBe("text-info");
    expect(updateKindTextClass("same")).toBe("text-textMuted");
  });
});

describe("HealthSummaryCard tone", () => {
  const summary = (tone: HealthTone) => ({
    tone,
    headline: "Headline",
    facets: [{ id: "issues", label: "Issues", value: "0", tone, caveat: null }],
    caveats: [],
    clearClaimable: tone === "clear",
  });

  it("draws an unknown verdict as the unknown notice, not a clear one", () => {
    const unknown = render(HealthSummaryCard, {
      props: { summary: summary("unknown") as never, rendered: new Set(["issues"]) },
    }).body;
    expect(unknown).toContain('data-tone="unknown"');
    expect(unknown).toContain("gp-notice");
    expect(unknown).not.toContain('data-tone="ok"');
    const clear = render(HealthSummaryCard, {
      props: { summary: summary("clear") as never, rendered: new Set(["issues"]) },
    }).body;
    expect(clear).toContain('data-tone="ok"');
  });

  it("uses no Tailwind palette color", () => {
    const body = render(HealthSummaryCard, {
      props: { summary: summary("critical") as never, rendered: new Set<string>() },
    }).body;
    expect(body).not.toMatch(/(?:text|bg|border)-(?:amber|emerald|rose|red|sky)-\d/);
  });
});
