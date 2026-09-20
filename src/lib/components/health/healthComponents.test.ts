import { readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import HealthSection from "./HealthSection.svelte";
import HealthTable from "./HealthTable.svelte";
import { HEALTH_CONTENT_WIDTH, HEALTH_SECTIONS } from "../../health/sections";

const here = dirname(fileURLToPath(import.meta.url));
const source = (file: string) => readFileSync(join(here, file), "utf8");
const componentFiles = readdirSync(here).filter((name) => name.endsWith(".svelte"));

/**
 * The branch-order contract for GitHub alerts, which used to be asserted
 * twice against two hand-written copies of the same five-state machine in
 * `HealthPanel.svelte`. There is one implementation now, so there is one
 * assertion — and unlike the old pair it cannot pass for Dependabot while
 * code scanning has drifted, because there is nothing left to drift from.
 */
describe("GithubAlertsSection — a failed fetch is not a missing CLI", () => {
  const text = source("GithubAlertsSection.svelte");

  it("tests the error branch before the missing-CLI branch", () => {
    const unchecked = text.indexOf("{#if !report}");
    const errorBranch = text.indexOf("{:else if report.error}");
    const missingCli = text.indexOf("{:else if !report.cli_present}");
    const empty = text.indexOf("{:else if empty}");
    expect(unchecked).toBeGreaterThan(-1);
    expect(errorBranch).toBeGreaterThan(unchecked);
    expect(missingCli).toBeGreaterThan(errorBranch);
    expect(empty).toBeGreaterThan(missingCli);
  });

  it("gates the install hint on the request having actually reached GitHub", () => {
    // A request that failed before it could ask anything also reports no CLI.
    // Telling the reader to install `gh` then sends them to fix a tool that is
    // already installed.
    const errorBranch = text.slice(
      text.indexOf("{:else if report.error}"),
      text.indexOf("{:else if !report.cli_present}"),
    );
    expect(errorBranch).toContain("!requestFailed");
    expect(errorBranch).toContain("report.is_github_remote");
  });

  it("states 'not checked' rather than rendering it as an empty result", () => {
    const unchecked = text.slice(text.indexOf("{#if !report}"), text.indexOf("{:else if report.error}"));
    expect(unchecked).toContain("Not checked");
    expect(unchecked).toContain("not the same as there being none");
  });

  it("takes the wire types rather than redeclaring their shape", () => {
    expect(text).toContain("DependabotReport | CodeScanningReport");
    expect(text).not.toMatch(/interface\s+AlertsReport\s*\{/);
  });
});

describe("HealthSection", () => {
  it("renders the catalog's heading, description and DOM ids", () => {
    const { body } = render(HealthSection, {
      props: { id: "outdated", children: (() => {}) as never },
    });
    expect(body).toContain('id="health-section-outdated"');
    expect(body).toContain('id="health-heading-outdated"');
    expect(body).toContain("Outdated npm packages");
    expect(body).toContain("npm packages behind their latest published release");
  });

  it("renders nothing for an id the catalog does not carry", () => {
    const { body } = render(HealthSection, {
      props: { id: "not-a-section", children: (() => {}) as never },
    });
    expect(body).not.toContain("health-section-not-a-section");
  });

  it("is focusable so a summary chip's jump moves keyboard focus too", () => {
    expect(source("HealthSection.svelte")).toContain('tabindex="-1"');
  });

  it("renders a bounded count with its shortfall, in amber", () => {
    const { body } = render(HealthSection, {
      props: {
        id: "issues",
        count: { shown: 3, total: 12 },
        children: (() => {}) as never,
      },
    });
    expect(body).toContain("(12; showing 3)");
    expect(body).toContain("text-amber-300");
  });

  it("renders a complete count plainly", () => {
    const { body } = render(HealthSection, {
      props: {
        id: "issues",
        count: { shown: 3, total: 3 },
        children: (() => {}) as never,
      },
    });
    expect(body).toContain("(3)");
  });

  it("puts the caveat above the content, not after it", () => {
    const text = source("HealthSection.svelte");
    expect(text.indexOf("{#if caveat}")).toBeLessThan(text.indexOf("{@render children()}"));
  });
});

describe("HealthTable", () => {
  it("gives every table an accessible name", () => {
    const { body } = render(HealthTable, {
      props: {
        caption: "Known vulnerabilities",
        columns: [{ label: "Severity" }],
        children: (() => {}) as never,
      },
    });
    expect(body).toContain("<caption");
    expect(body).toContain("Known vulnerabilities");
    expect(body).toContain("sr-only");
  });

  it("requires the caption rather than accepting an unnamed table", () => {
    // An optional accessible name is one the next table is written without.
    const text = source("HealthTable.svelte");
    expect(text).toMatch(/caption:\s*string;/);
    expect(text).not.toMatch(/caption\?\s*:/);
  });

  it("keeps a hidden column label in the accessibility tree", () => {
    const { body } = render(HealthTable, {
      props: {
        caption: "Alerts",
        columns: [{ label: "Open on GitHub", width: "w-8", hideLabel: true }],
        children: (() => {}) as never,
      },
    });
    expect(body).toContain("Open on GitHub");
    expect(body).toContain('scope="col"');
  });

  it("survives duplicate column labels without dropping a header cell", () => {
    const { body } = render(HealthTable, {
      props: {
        caption: "Alerts",
        columns: [{ label: "Fix" }, { label: "Fix" }],
        children: (() => {}) as never,
      },
    });
    // `<th\b`, not `<th` — the latter also counts the `<thead>` around them.
    expect([...body.matchAll(/<th\b/g)]).toHaveLength(2);
  });
});

describe("Health components share one measure and one link affordance", () => {
  /**
   * The panel used to mix `max-w-2xl`, `3xl`, `4xl` and `5xl` across sibling
   * sections, so the right edge stepped in and out down the page. The width
   * is now stated once; a component that hard-codes its own has reintroduced
   * the defect.
   */
  it("no health component hard-codes a content width of its own", () => {
    for (const file of componentFiles) {
      const text = source(file);
      const widths = [...text.matchAll(/max-w-(\d?xl|screen-\w+)/g)].map((m) => m[0]);
      for (const width of widths) {
        expect(
          width,
          `${file} hard-codes ${width}; use HEALTH_CONTENT_WIDTH / HEALTH_PROSE_WIDTH`,
        ).toBe(HEALTH_CONTENT_WIDTH);
      }
    }
  });

  it("names every icon-only control", () => {
    // `title` is a tooltip, not a reliable accessible name. A table of twelve
    // alerts used to offer twelve controls that all announced as "button".
    const text = source("AlertLinkButton.svelte");
    expect(text).toContain("aria-label={label}");
    expect(text).toMatch(/label:\s*string;/);
    expect(text).toContain('aria-hidden="true"');
  });

  it("keeps the catalog and the components agreed on section ids", () => {
    const panel = readFileSync(join(here, "..", "HealthPanel.svelte"), "utf8");
    const used = new Set(
      [...panel.matchAll(/\bid="([a-z-]+)"\s*$/gm)].map((m) => m[1]),
    );
    const known = new Set(HEALTH_SECTIONS.map((s) => s.id));
    for (const id of used) {
      if (!/^(summary|plan|vulnerabilities|dependabot|code-scanning|issues|outdated|packages|code-graph|dead-code)$/.test(id)) {
        continue;
      }
      expect(known, `HealthPanel renders section "${id}" with no catalog entry`).toContain(id);
    }
  });
});
