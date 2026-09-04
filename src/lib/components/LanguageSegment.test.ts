import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import LanguageSegment from "./LanguageSegment.svelte";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "LanguageSegment.svelte"), "utf8");

describe("LanguageSegment", () => {
  it("draws nothing with no repository open", () => {
    // A status-bar segment that draws a placeholder is worse than one that
    // draws nothing: the bar is one row tall and every pixel is a claim.
    // Svelte's SSR hydration markers are not content, so they are stripped
    // rather than asserted away with a substring check that would also pass
    // on a rendered element.
    const visible = render(LanguageSegment)
      .body.replace(/<!--[\s\S]*?-->/g, "")
      .trim();
    expect(visible).toBe("");
  });

  it("keeps the click-to-filter jump into Code → Explorer", () => {
    expect(source).toContain("handleLanguageClick");
    expect(source).toContain('repoStore.setActiveTab("code", "explorer")');
    expect(source).toContain("gitpulse:filter-lang");
  });

  it("reads the shared LOC metric instead of fetching the scan again", () => {
    // The strip this replaced ran its own `cmd_get_language_stats` invoke and
    // its own cache beside `locMetric`, which already owns that command —
    // two fetches for one scan, and the bar's copy swallowed every failure.
    expect(source).toContain("locMetric.subscribe");
    // The command name still appears in the comment explaining who owns it;
    // what must not come back is a call to it, or a second cache beside the
    // metric's own.
    expect(source).not.toMatch(/invoke[<(]/);
    expect(source).not.toContain("createRepoPanelCache");
  });

  it("derives what it draws from the tested mix function", () => {
    // The honesty rule (capped or stale never reads as complete) lives in
    // describeLanguageMix, where barStats.test.ts can hold it to account.
    expect(source).toContain("describeLanguageMix");
  });

  it("dismisses the breakdown the way every other overlay does", () => {
    expect(source).toContain("shouldDismissOverlay");
    expect(source).toContain('event.key === "Escape"');
    expect(source).toContain('aria-expanded={open}');
  });
});
