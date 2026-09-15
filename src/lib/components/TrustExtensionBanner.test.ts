import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * The offer to extend a pre-repository approval to a repository's worktrees.
 *
 * It began inside WorktreesPanel, beside the rows it is about. The sidebar
 * body is one scroller and the branch list above it has no height cap, so on
 * a repository with many branches the offer sat below the fold — while the
 * refusal that sends people looking for it names the button by label. It now
 * renders above the branch list, and these are the invariants that came with
 * it from `WorktreesPanel.test.ts`.
 */
const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "TrustExtensionBanner.svelte"), "utf8");
const sidebar = readFileSync(join(here, "Sidebar.svelte"), "utf8");

describe("TrustExtensionBanner", () => {
  it("offers the extension only when inspection said so", () => {
    // Gated on the backend's own scope answer, never on a row count or an
    // error string: an offer to fix a condition nobody established is a guess
    // wearing a button.
    expect(source).toContain("needsExtension(preview)");
    expect(source).toContain("{#if trustExtendable}");
    const fn = source.slice(
      source.indexOf("async function loadTrustScope"),
      source.indexOf("async function extendTrust"),
    );
    expect(fn).toContain('invoke<TrustPreview>("cmd_repository_trust"');
    // A failed inspection hides the offer rather than showing it.
    expect(fn).toMatch(/catch[\s\S]*?trustExtendable = false/);
  });

  it("drops a stale inspection", () => {
    const fn = source.slice(
      source.indexOf("async function loadTrustScope"),
      source.indexOf("async function extendTrust"),
    );
    expect(fn.match(/guard\.isLive\(\)/g)?.length).toBe(2);
  });

  it("re-guards the active repository across the extension dialog", () => {
    // The dialog is awaited, so the tab can change under it. Announcing then
    // would reload another repository's panel on this one's decision — the
    // same hazard as before the move, with a wider blast radius now that the
    // announcement is what reloads.
    const fn = source.slice(source.indexOf("async function extendTrust"));
    expect(fn).toMatch(
      /await repoStore\.trustRepo\(repo\)[\s\S]*?\$repoStore\.currentPath !== repo[\s\S]*?announceTrustExtended\(\)/,
    );
  });

  it("says what is currently unreadable, not just that trust is old", () => {
    const banner = source.slice(source.indexOf("{#if trustExtendable}"));
    expect(banner).toMatch(/before GitPulse covered worktrees/i);
    expect(banner).toMatch(/left out of comparisons and collision checks/i);
    expect(banner).toContain("Extend trust to every worktree");
  });

  it("re-inspects before announcing, so the offer retires itself", () => {
    // Without this the banner stays on screen after a successful grant: the
    // only other thing that clears it is a repository change.
    const fn = source.slice(source.indexOf("async function extendTrust"));
    expect(fn).toMatch(/await loadTrustScope\(repo\)[\s\S]*?announceTrustExtended\(\)/);
  });
});

describe("where the banner sits", () => {
  it("renders above the branch list", () => {
    // The whole point of the move. Below it, the notice explaining why the
    // rest of the sidebar is incomplete scrolled out of sight on exactly the
    // repositories that have enough worktrees to need it.
    const banner = sidebar.indexOf("<TrustExtensionBanner />");
    const branches = sidebar.indexOf("<BranchList />");
    const worktrees = sidebar.indexOf("<WorktreesPanel />");
    expect(banner, "Sidebar must mount the banner").toBeGreaterThan(-1);
    expect(branches, "Sidebar must still mount the branch list").toBeGreaterThan(-1);
    expect(worktrees, "Sidebar must still mount the worktrees panel").toBeGreaterThan(-1);
    expect(banner).toBeLessThan(branches);
    expect(branches).toBeLessThan(worktrees);
  });

  it("is mounted once", () => {
    expect(sidebar.match(/<TrustExtensionBanner\b/g)?.length).toBe(1);
  });
});
