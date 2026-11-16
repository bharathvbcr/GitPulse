import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createRawSnippet } from "svelte";
import { render } from "svelte/server";
import { describe, expect, it } from "vitest";
import PageHeader from "./PageHeader.svelte";

const here = dirname(fileURLToPath(import.meta.url));

const snippet = (html: string) => createRawSnippet(() => ({ render: () => html }));

function bar(props: Record<string, unknown>): string {
  return render(PageHeader, { props: { title: "Overview", ...props } }).body;
}

describe("PageHeader", () => {
  it("renders the title as an h2 by default and as an h1 for a full-pane surface", () => {
    const two = bar({});
    expect(two).toMatch(/<h2 class="gp-surface-title[^"]*">Overview<\/h2>/);
    expect(two).not.toContain("<h1");
    const one = bar({ level: 1 });
    expect(one).toMatch(/<h1 class="gp-surface-title[^"]*">Overview<\/h1>/);
    expect(one).not.toContain("<h2");
  });

  it("is a gp-header bar carrying the shared hook", () => {
    const body = bar({});
    expect(body).toMatch(/<header class="gp-header[^"]*" data-page-header/);
  });

  it("draws no rule under the bar: a tonal step, never a border", () => {
    const body = bar({ description: "d", actions: snippet("<button>Go</button>") });
    expect(body).not.toMatch(/\bborder(-[a-z]+)?\b/);
    expect(body).not.toContain("gp-section-edge");
  });

  it("puts the description on the same row, truncated, with its full text in title=", () => {
    const text = "A long description that the bar must truncate rather than wrap onto a second row";
    const body = bar({ description: text, descriptionTestId: "hint" });
    const line = /<p [^>]*data-page-header-description[^>]*>/.exec(body)?.[0] ?? "";
    expect(line).toContain(`title="${text}"`);
    expect(line).toContain('data-testid="hint"');
    expect(line).toMatch(/\btruncate\b/);
    expect(line).toMatch(/\bmin-w-0\b/);
    expect(body).toContain(`>${text}</p>`);
  });

  it("renders no description element when there is no description", () => {
    expect(bar({})).not.toContain("data-page-header-description");
  });

  it("renders the actions slot right-aligned in gp-header-actions, and nothing when absent", () => {
    const body = bar({ actions: snippet('<button aria-label="Rescan" data-x="1">Rescan</button>') });
    const plain = body.replace(/<!--.*?-->/g, "");
    expect(plain).toMatch(/<div class="gp-header-actions"[^>]*>\s*<button aria-label="Rescan" data-x="1">Rescan<\/button>\s*<\/div>/);
    expect(bar({})).not.toContain("gp-header-actions");
  });

  it("orders leading, title, meta, description, actions on one row", () => {
    const body = bar({
      description: "desc",
      leading: snippet("<i>L</i>"),
      meta: snippet("<b>M</b>"),
      actions: snippet("<u>A</u>"),
    });
    const at = (needle: string) => body.indexOf(needle);
    expect(at("<i>L</i>")).toBeGreaterThan(-1);
    expect(at("<i>L</i>")).toBeLessThan(at("<h2"));
    expect(at("<h2")).toBeLessThan(at("<b>M</b>"));
    expect(at("<b>M</b>")).toBeLessThan(at("desc</p>"));
    expect(at("desc</p>")).toBeLessThan(at("<u>A</u>"));
  });

  it("renders a toolbar as a full-width second row inside the same header, and nothing when absent", () => {
    const body = bar({ actions: snippet("<u>A</u>"), toolbar: snippet("<s>T</s>") });
    const header = /<header[\s\S]*<\/header>/.exec(body)?.[0] ?? "";
    expect(header).toContain("<s>T</s>");
    expect(body.indexOf("<u>A</u>")).toBeLessThan(body.indexOf("<s>T</s>"));
    const row = /<div [^>]*data-page-header-toolbar[^>]*>/.exec(body)?.[0] ?? "";
    expect(row).toContain("flex-basis: 100%");
    expect(row).toContain("flex-wrap: wrap");
    expect(/<header[^>]*>/.exec(body)?.[0]).toContain("padding-top: var(--gp-space-2)");
    const plain = bar({ actions: snippet("<u>A</u>") });
    expect(plain).not.toContain("data-page-header-toolbar");
    expect(/<header[^>]*>/.exec(plain)?.[0]).not.toContain("padding-top");
  });

  it("keeps the bar's minimum height from density and lets the row wrap instead of clipping", () => {
    const body = bar({});
    expect(body).toContain("min-height: var(--gp-header-h)");
    expect(body).toContain("flex-wrap: wrap");
  });

  it("passes extra classes through to the bar", () => {
    expect(bar({ class: "extra-token" })).toMatch(/<header class="gp-header extra-token"/);
  });
});

describe("every page takes its header from PageHeader", () => {
  const read = (path: string) => readFileSync(join(here, path), "utf8");
  const pages: ReadonlyArray<readonly [string, string]> = [
    ["WorkView.svelte", 'title="Overview"'],
    ["GitHubPanel.svelte", 'title="Remote"'],
    ["TaskBoard.svelte", "<PageHeader {title}"],
    ["pulse/PulseView.svelte", 'title="Repository pulse"'],
    ["CoverageViewer.svelte", 'title="Coverage"'],
    ["HealthPanel.svelte", 'title="Health"'],
    ["StoragePanel.svelte", 'title="Storage"'],
    ["AgentsView.svelte", 'title="Agents"'],
  ];

  it.each(pages)("%s renders <PageHeader> and draws no header of its own", (file, usage) => {
    const source = read(file);
    expect(source).toContain('import PageHeader from');
    expect(source).toContain(usage);
    expect(source).not.toMatch(/<h[12][^>]*class="[^"]*\b(text-lg|text-sm)\b[^"]*font-semibold/);
    expect(source).not.toContain('class="gp-header');
    expect(source).not.toContain("gp-surface-header");
  });
});
