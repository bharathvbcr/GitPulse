import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import LanguageSegment from "./LanguageSegment.svelte";
import { stripMarkupComments } from "../dom/markupText";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "LanguageSegment.svelte"), "utf8");

describe("LanguageSegment", () => {
  it("draws nothing with no repository open", () => {
    // A status-bar segment that draws a placeholder is worse than one that
    // draws nothing: the bar is one row tall and every pixel is a claim.
    // Svelte's SSR hydration markers are not content, so they are stripped
    // rather than asserted away with a substring check that would also pass
    // on a rendered element.
    const visible = stripMarkupComments(render(LanguageSegment).body).trim();
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
    // Literally the same way now: the shared popover owner registers the
    // listeners and `popover.test.ts` holds their phases to account. What
    // stays this component's own contract is which surfaces count as inside
    // — the trigger among them, or its own pointerdown would close the panel
    // a beat before its click reopened it — and that Escape closes it.
    expect(source).toContain("use:popover={dismissal}");
    expect(source).toContain("[data-language-panel], [data-language-trigger]");
    expect(source).toContain('escape: "bubble"');
    expect(source).toContain("resize: true");
    expect(source).toContain('aria-expanded={open}');
  });

  it("opens upward from the status bar, by its own measured height", () => {
    // The status bar is the last row on screen: "below" is off the window.
    // The owner derives a top from the measured height, so the panel still
    // grows upward but can no longer grow off the top of the screen.
    expect(source).toContain('place: "above"');
    expect(source).toContain("element: triggerEl");
    // Replaced, not accumulated: the hand-rolled bottom anchor is gone.
    expect(source).not.toContain("window.innerHeight - rect.top");
    expect(source).not.toContain("bottom: {anchor.bottom}px");
  });

  it("has a rescan button that force-refreshes the LOC metric", () => {
    // The popover must let the user trigger an explicit language rescan.
    // Force bypasses the cost floor: an explicit click must always do something.
    expect(source).toContain("data-language-rescan");
    expect(source).toContain('locMetric.refresh(repoPath, { force: true })');
    expect(source).toContain("RefreshCw");
  });

  it("offers two options for code percentage: code-only and include notes with explanatory note", () => {
    // The panel must present both options clearly
    expect(source).toContain('data-testid="mode-code-only"');
    expect(source).toContain('data-testid="mode-all-files"');
    expect(source).toContain("Only code files");
    expect(source).toContain("Include notes");
    expect(source).toContain('interfaceStore.setCodePercentageMode("code-only")');
    expect(source).toContain('interfaceStore.setCodePercentageMode("all")');
    expect(source).toContain('data-testid="language-mix-note"');
    expect(source).toContain('data-testid="empty-code-files"');
  });
});
