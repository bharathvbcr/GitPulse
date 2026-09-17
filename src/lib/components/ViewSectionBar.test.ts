import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import ViewSectionBar from "./ViewSectionBar.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "ViewSectionBar.svelte"),
  "utf8",
);

describe("ViewSectionBar responsive overflow and scroll cues", () => {
  it("mounts a horizontal scroller with hidden scrollbar styling", () => {
    expect(source).toContain("gp-header-scroll");
    expect(source).toContain("bind:this={scroller}");
  });

  it("mounts ScrollCue for horizontal overflow cues", () => {
    expect(source).toContain('from "./ScrollCue.svelte"');
    expect(source).toContain('<ScrollCue target={scroller} axis="x"');
  });

  it("scrolls the active section pill into view on section change", () => {
    expect(source).toContain("scrollIntoView({ block: \"nearest\", inline: \"nearest\" })");
  });

  it("protects the outer bar with min-w-0 for narrow viewports", () => {
    expect(source).toMatch(/class="[^"]*h-9[^"]*min-w-0/);
  });

  it("renders section buttons and tablist inside the scroller", () => {
    const { body } = render(ViewSectionBar, { props: { view: "work" } });
    expect(body).toContain('role="tablist"');
    expect(body).toContain("Overview");
    expect(body).toContain("Policy");
  });
});
