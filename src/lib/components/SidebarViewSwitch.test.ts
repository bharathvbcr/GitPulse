import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import SidebarViewSwitch from "./SidebarViewSwitch.svelte";
import type { SidebarView } from "../sidebar/layoutStore";

function body(view: SidebarView): string {
  return render(SidebarViewSwitch, { props: { view, onSelect: () => {} } }).body;
}

/** Every opening `<button ...>` tag, in document order. */
function tabs(html: string): string[] {
  return [...html.matchAll(/<button\b[^>]*>/g)].map((m) => m[0]);
}

describe("SidebarViewSwitch", () => {
  it("is a real tablist: a named list of two tabs, each pointing at the panel it shows", () => {
    const html = body("git");
    expect(html).toMatch(/<div[^>]*role="tablist"[^>]*aria-label="Sidebar view"/);
    expect(html).toContain('data-testid="sidebar-view-switcher"');
    const [git, files] = tabs(html);
    expect(git).toContain('role="tab"');
    expect(git).toContain('id="sidebar-view-tab-git"');
    expect(git).toContain('aria-controls="sidebar-view-panel-git"');
    expect(git).toContain('data-testid="sidebar-view-git"');
    expect(files).toContain('role="tab"');
    expect(files).toContain('id="sidebar-view-tab-files"');
    expect(files).toContain('aria-controls="sidebar-view-panel-files"');
    expect(files).toContain('data-testid="sidebar-view-files"');
    // aria-selected is the tab's state; a toggle's aria-pressed would contradict it.
    expect(git).not.toContain("aria-pressed");
    expect(files).not.toContain("aria-pressed");
  });

  it("marks only the active view, and only the active tab is in the Tab order (arrows move between them)", () => {
    const [git, files] = tabs(body("git"));
    expect(git).toContain('aria-selected="true"');
    expect(git).toContain('data-active="true"');
    expect(git).toContain('tabindex="0"');
    expect(files).toContain('aria-selected="false"');
    expect(files).toContain('data-active="false"');
    expect(files).toContain('tabindex="-1"');

    const [git2, files2] = tabs(body("files"));
    expect(git2).toContain('aria-selected="false"');
    expect(git2).toContain('tabindex="-1"');
    expect(files2).toContain('aria-selected="true"');
    expect(files2).toContain('tabindex="0"');
  });

  it("centres icon and label on one line in a 28px target that fills its half of the track", () => {
    for (const tag of tabs(body("git"))) {
      // gp-seg-btn is not a flex box. Without these the icon sat on its own
      // baseline at the left and the label floated centred beside it.
      expect(tag).toMatch(/class="[^"]*\bgp-seg-btn\b/);
      expect(tag).toMatch(/class="[^"]*\binline-flex\b/);
      expect(tag).toMatch(/class="[^"]*\bitems-center\b/);
      expect(tag).toMatch(/class="[^"]*\bjustify-center\b/);
      expect(tag).toMatch(/class="[^"]*\bflex-1\b/);
      expect(tag).toMatch(/class="[^"]*\bh-7\b/);
    }
  });

  it("names both views in words, with decorative icons hidden from assistive tech", () => {
    const html = body("git");
    expect(html).toMatch(/<span[^>]*>Git<\/span>/);
    expect(html).toMatch(/<span[^>]*>Explorer<\/span>/);
    const svgs = [...html.matchAll(/<svg\b[^>]*>/g)].map((m) => m[0]);
    expect(svgs.length).toBe(2);
    for (const svg of svgs) expect(svg).toContain('aria-hidden="true"');
  });
});
