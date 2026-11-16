import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * Page layout contract (DESIGN_LANGUAGE section 7a): page content is fluid and
 * fills its pane. Width adapts through the shared `gp-page*` utilities, which
 * measure the PANE with a container query, never the window.
 *
 * Read from the stylesheet and the page sources that ship.
 */
const root = fileURLToPath(new URL("../../../", import.meta.url));
const read = (rel: string) => readFileSync(`${root}${rel}`, "utf8");
const css = read("src/app.css");

/** The pages design-lint rule 19 always reads: the survey pages plus Health. */
const PAGES = [
  "StoragePanel", "FirebasePanel", "GitHubPanel", "CodeStackViewer", "WorkView", "HygienePanel",
  "RepoPanel", "OutgoingCommitReview", "CoverageViewer", "GlobalCleaner", "AgentsView", "HealthPanel",
].map((n) => `src/lib/components/${n}.svelte`);

/** A width that is not a cap: the parent's own measure, or none. */
const NOT_A_CAP = /^max-w-(?:full|none|min|max|fit|0|\[\d*\.?\d+%\]|\[var\(.*\)\])$/;

/** Every `max-w-*` token on a tag that does not state a reason with `data-width-cap="..."`. */
function unmarkedCaps(source: string): string[] {
  const markup = source.replace(/<!--[\s\S]*?-->/g, "").replace(/<script[\s\S]*?<\/script>/g, "").replace(/<style[\s\S]*?<\/style>/g, "");
  const found: string[] = [];
  for (const tag of markup.matchAll(/<[A-Za-z][^<>]*>/g)) {
    if (/data-width-cap="[^"]+"/.test(tag[0])) continue;
    for (const token of tag[0].matchAll(/(?<![\w-])(?:[\w@-]+:)*(max-w-[^\s"'{}`]+)/g)) {
      if (!NOT_A_CAP.test(token[1])) found.push(token[1]);
    }
  }
  return found;
}

/** The body of `@utility <name> { ... }`, balanced, or throws. */
function utility(name: string): string {
  const start = css.indexOf(`@utility ${name} {`);
  if (start < 0) throw new Error(`app.css defines no @utility ${name}`);
  let depth = 0;
  for (let i = css.indexOf("{", start); i < css.length; i++) {
    if (css[i] === "{") depth++;
    if (css[i] === "}" && --depth === 0) return css.slice(start, i + 1);
  }
  throw new Error(`@utility ${name} is not closed`);
}

describe("page layout utilities", () => {
  it("defines the five utilities beside the other gp-* primitives", () => {
    for (const name of ["gp-page", "gp-page-body", "gp-page-section", "gp-page-grid", "gp-prose"]) {
      expect(() => utility(name), name).not.toThrow();
    }
  });

  it("gp-page is the pane's own width container, full width and the scroll area", () => {
    const body = utility("gp-page");
    expect(body).toMatch(/container:\s*gp-page\s*\/\s*inline-size/);
    expect(body).toMatch(/width:\s*100%/);
    expect(body).toMatch(/overflow:\s*auto/);
    expect(body).not.toMatch(/max-width|max-inline-size/);
  });

  it("gp-page owns the buffer under the last section, at least the widest section gap", () => {
    expect(utility("gp-page")).toMatch(/padding-block-end:\s*var\(--gp-space-6\)/);
    expect(utility("gp-page-body")).toMatch(/padding:\s*var\(--gp-page-pad\) var\(--gp-page-pad\) 0/);
  });

  it("gp-page-body steps padding and gap on the container, from the spacing tokens", () => {
    const body = utility("gp-page-body");
    const steps = [...body.matchAll(/@container gp-page \(min-width: ([\d.]+rem)\)/g)].map((m) => m[1]);
    expect(steps, "regular and wide steps").toEqual(["40rem", "72rem"]);
    expect(body).toContain("var(--gp-pad)");
    expect(body).toMatch(/--gp-page-pad:\s*var\(--gp-space-4\)/);
    expect(body).toMatch(/--gp-page-pad:\s*var\(--gp-space-6\)/);
    expect(body, "a window query would not follow the pane").not.toMatch(/@media|\d+vw/);
  });

  it("gp-page-section and gp-page-grid are uncapped; only gp-prose carries a measure", () => {
    for (const name of ["gp-page", "gp-page-body", "gp-page-section", "gp-page-grid"]) {
      expect(utility(name), name).not.toMatch(/max-width|max-inline-size/);
    }
    expect(utility("gp-page-grid")).toMatch(/repeat\(auto-fit,\s*minmax\(min\(100%,\s*var\(--gp-page-col\)\),\s*1fr\)\)/);
    expect(utility("gp-prose")).toMatch(/max-inline-size:\s*72ch/);
  });
});

describe("migrated pages carry no fixed content cap", () => {
  // The pages named in the survey, plus Health, whose two `max-w-*` constants were retired.
  // design-lint rule 19 is the authority (it also finds new pages by what they render).
  it("design-lint rule 19 always reads the same pages", () => {
    const lint = read("scripts/design-lint.mjs");
    for (const file of PAGES) expect(lint, file).toContain(file.replace("src/lib/components/", "").replace(".svelte", ""));
  });

  for (const file of PAGES) {
    it(`${file} has no max-w-* cap outside the marked exceptions`, () => {
      expect(unmarkedCaps(read(file))).toEqual([]);
    });
  }

  it("the cap scan itself sees a cap, a variant of one, and honours a reason", () => {
    expect(unmarkedCaps('<div class="gp-notice max-w-2xl"></div>')).toEqual(["max-w-2xl"]);
    expect(unmarkedCaps('<div class="@md:max-w-xl"></div>')).toEqual(["max-w-xl"]);
    expect(unmarkedCaps('<select class="max-w-40" data-width-cap="one select"></select>')).toEqual([]);
    expect(unmarkedCaps('<span class="max-w-full max-w-[40%]"></span>')).toEqual([]);
  });

  it("the full-pane pages sit in PageBody or gp-page", () => {
    for (const name of ["StoragePanel", "GitHubPanel", "HealthPanel", "CodeStackViewer"]) {
      expect(read(`src/lib/components/${name}.svelte`), name).toMatch(/<PageBody\b/);
    }
    expect(read("src/lib/components/AgentsView.svelte")).toMatch(/class="gp-page"/);
    expect(read("src/lib/components/pulse/PulseView.svelte")).toMatch(/class="gp-page /);
  });

  it("GitHubPanel's listings split on the pane's width, not the window's", () => {
    const text = read("src/lib/components/GitHubPanel.svelte");
    expect(text).toMatch(/grid-cols-1 @4xl:grid-cols-3/);
    expect(text).toMatch(/@4xl:col-span-2/);
    expect(text).not.toMatch(/\bxl:(?:grid-cols|col-span)/);
  });

  it("no page-layout class is keyed to a viewport breakpoint", () => {
    const files = [
      "StoragePanel", "GitHubPanel", "HealthPanel", "WorkView", "GlobalCleaner", "PolicyActivity",
      "ManviConnection", "BranchTagCleanup", "FleetPulse", "pulse/PulseView", "pulse/PulseDora",
      "pulse/PulseHygiene", "pulse/PulseCodeAge", "pulse/PulseRhythm", "pulse/PulseKnowledgeMap",
      "pulse/PulseExtensionChurn", "health/HostLoadSection",
    ];
    for (const name of files) {
      const text = read(`src/lib/components/${name}.svelte`);
      expect(text.match(/(?<![\w@:-])(?:sm|md|lg|xl|2xl):(?:grid-cols|col-span|flex-row|flex-col)/g) ?? [], name).toEqual([]);
    }
  });
});
