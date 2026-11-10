import { readFileSync } from "node:fs";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";
import { render } from "svelte/server";
import ViewBar from "./ViewBar.svelte";
import { interfaceStore } from "../stores/interfaceStore";
import { repoStore } from "../stores/repoStore";

// Server-rendered output and source contracts only. What needs a live page (the
// row's height under a global surface, the container-query tiers, the context
// content moving into the slot) is checked in harness/tab-switch.html.
const source = readFileSync(new URL("./ViewBar.svelte", import.meta.url), "utf8");

const bar = () => render(ViewBar, { props: { conflictedCount: 0 } }).body;

beforeEach(async () => {
  interfaceStore.setGlobalSurface("repository");
  await repoStore.openRepo("/repo/view-bar", { allowBroken: true, activate: true });
});

afterEach(async () => {
  interfaceStore.setGlobalSurface("repository");
  await repoStore.closeActiveTab();
});

describe("ViewBar", () => {
  it("is the Views landmark: the switcher, the Terminal pill and Refresh, in that order", () => {
    const body = bar();
    expect(body).toContain('<nav');
    expect(body).toContain('aria-label="Views"');
    const switcher = body.indexOf("data-view-group=");
    const terminal = body.indexOf('data-testid="terminal-pill"');
    const refresh = body.indexOf('data-testid="viewbar-refresh"');
    expect(switcher).toBeGreaterThan(-1);
    expect(terminal).toBeGreaterThan(switcher);
    expect(refresh).toBeGreaterThan(terminal);
  });

  it("puts the Agents pill nearest the content, before the Terminal pill", () => {
    // Nothing has swept the plane in a test, so the repository reads as
    // unknown: the pill shows "?" and says it was not read, never zero.
    const body = bar();
    const agents = body.indexOf('data-testid="viewbar-agents"');
    expect(agents).toBeGreaterThan(-1);
    expect(agents).toBeLessThan(body.indexOf('data-testid="terminal-pill"'));
    const tag = /<button\b[^>]*data-testid="viewbar-agents"[^>]*>/.exec(body)?.[0] ?? "";
    expect(tag).toContain('data-agents-state="unknown"');
    expect(tag).toContain("not read yet");
    expect(tag).toContain('data-tone="unknown"');
  });

  it("opens the Agents surface narrowed to this repository", () => {
    expect(source).toContain("setAgentsRepositoryScope(repoPath || null);");
    expect(source).toContain("interfaceStore.setAgentsOpen(true);");
  });

  it("keeps every control live on the repository surface, with no surface name", () => {
    const body = bar();
    expect(body).not.toContain("data-viewbar-surface");
    expect(body).not.toMatch(/data-viewbar-controls[^>]*\binert\b/);
    expect(body).not.toContain("invisible");
  });

  it("under a global surface stays mounted, its controls inert and hidden, the surface named", () => {
    for (const [surface, label] of [["fleet", "Fleet"], ["agents", "Agents"], ["tasks", "Tasks"]] as const) {
      interfaceStore.setGlobalSurface(surface);
      const body = bar();
      // Mounted at the same height: the nav and its controls are still there.
      expect(body).toContain('aria-label="Views"');
      expect(body).toContain("data-viewbar-controls");
      expect(body).toMatch(new RegExp(`data-viewbar-surface="${surface}"[^>]*>${label}<`));
      expect(/<div\b[^>]*data-viewbar-controls[^>]*>/.exec(body)?.[0]).toMatch(/\binert\b/);
      expect(/<div\b[^>]*data-viewbar-controls[^>]*>/.exec(body)?.[0]).toContain("invisible");
    }
  });

  it("is not rendered by App on the welcome screen", () => {
    const app = readFileSync(new URL("../../App.svelte", import.meta.url), "utf8");
    expect(app).toMatch(/\{#if \$repoStore\.currentPath\}\s*<ViewBar \{conflictedCount\} \/>\s*\{\/if\}/);
  });

  it("moves the uncommitted chip out of the switcher and into the trailing cluster", () => {
    expect(source).toContain('data-testid="viewbar-uncommitted"');
    expect(source).toContain('aria-label="Preview {dirty} uncommitted changes"');
    expect(source).toContain("void repoStore.previewUncommitted()");
    const switcher = readFileSync(new URL("./ViewTabBar.svelte", import.meta.url), "utf8");
    expect(switcher).not.toContain("previewUncommitted");
  });

  it("collapses its labels by container width, not window width", () => {
    // Container queries on the row itself (HEADER_SPEC section 4).
    expect(source).toContain("@container");
    expect(source).toContain("@min-[1100px]:inline");
    expect(source).not.toMatch(/\b(sm|md|lg|xl):inline/);
  });

  it("offers the row's width to the context bar and lets go on unmount", () => {
    expect(source).toContain("registerContextSlot(slot)");
    expect(source).toContain("reportViewBarWidth(width)");
    expect(source).toContain("observer?.disconnect();");
  });

  it("gives its scroller the diffuse edge and keeps the drag spacer outside the scroller", () => {
    expect(source).toContain('<ScrollCue target={scroller} axis="x" diffuse />');
    const scroll = source.indexOf("bind:this={scroller}");
    const spacer = source.indexOf('class="h-full min-w-4 flex-1"');
    expect(spacer).toBeGreaterThan(source.indexOf("<ScrollCue"));
    expect(scroll).toBeGreaterThan(-1);
  });

  it("has no accessibility compiler warnings", () => {
    const { warnings } = compile(source, { generate: "client" });
    expect(warnings.filter(({ code }) => code.startsWith("a11y_"))).toEqual([]);
  });
});
