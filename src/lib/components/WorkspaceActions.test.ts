import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { showsWorkspaceControls } from "./WorkspaceActions.svelte";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "WorkspaceActions.svelte"), "utf8");

describe("showsWorkspaceControls", () => {
  it("hides the controls until a second repository is open", () => {
    // With one tab, "fetch all" is just "fetch" and the roll-up repeats what
    // the tab already says; the controls would be pure noise.
    expect(showsWorkspaceControls(0)).toBe(false);
    expect(showsWorkspaceControls(1)).toBe(false);
  });

  it("shows them from the second repository onward", () => {
    expect(showsWorkspaceControls(2)).toBe(true);
    expect(showsWorkspaceControls(24)).toBe(true);
  });
});

describe("the work-in-progress roll-up panel", () => {
  it("makes each listed repository a control, not a caption", () => {
    // The defect: the panel named the repositories holding work and then left
    // the reader to find them in the tab strip by hand. Every row is a button
    // that activates its repository now, so the list is the door to what it
    // reports.
    expect(source).toContain("function reveal(path: string)");
    expect(source).toContain("onclick={() => reveal(repo.path)}");
    // Rows are open tabs: activate rather than reopen, and fall back to
    // openRepo only for a tab closed between render and click.
    expect(source).toContain("repoStore.activateTab(open.id)");
    expect(source).toContain("repoStore.openRepo(path)");
  });

  it("lands on the pane that answers the row's worst reason", () => {
    // The destination comes from the tested model, not from a second copy of
    // the mapping written into the markup.
    expect(source).toContain("wipDestination(wip.repos.find((repo) => repo.path === path)?.severity");
    expect(source).toContain("repoStore.setActiveTab(tab, section)");
    // …and the tooltip names that pane from the registry, so a renamed
    // section renames the promise too.
    expect(source).toContain("describeDestination(to.tab, to.section)");
  });

  it("makes the sweep report's repositories reachable too", () => {
    // Same defect, same panel: a row saying a fetch was skipped here names
    // exactly the repository the reader now wants to open. Both lists route
    // through one function, so neither can drift from the other.
    expect(source).toContain("onclick={() => reveal(result.path)}");
    expect(source).toContain("destinationFor(result.path)");
    // Live state decides the pane, not which list drew the row: a repository
    // the sweep skipped for conflicts opens the resolver.
    expect(source).toContain("function destinationFor(path: string)");
  });

  it("leaves Fleet before activating a tab underneath it", () => {
    // The tab strip stays on screen over Fleet, so activating a repository
    // without closing Fleet looks exactly like the inert row this replaced.
    expect(source).toContain("interfaceStore.setFleetOpen(false)");
  });

  it("dismisses the way every other overlay in the app does", () => {
    expect(source).toContain("shouldDismissOverlay");
    expect(source).toContain('event.key === "Escape"');
    // The trigger has to count as inside, or its own pointerdown would close
    // the panel a beat before its click reopened it.
    expect(source).toContain("[data-workspace-wip], [data-workspace-wip-trigger]");
    expect(source).toContain("data-workspace-wip-trigger");
    expect(source).toContain('aria-haspopup="dialog"');
    // Registered and torn down together — a window listener that outlives the
    // component keeps closing a panel that no longer exists.
    expect(source).toContain('window.addEventListener("pointerdown", handlePointerDown, true)');
    expect(source).toContain('window.removeEventListener("pointerdown", handlePointerDown, true)');
    expect(source).toContain('window.addEventListener("keydown", handleKey)');
    expect(source).toContain('window.removeEventListener("keydown", handleKey)');
  });

  it("closes from one red control at the top, not a button below the content", () => {
    // The panel grows with the sweep report, so a full-width Close at the
    // bottom was the first thing to scroll out of reach.
    expect(source).toContain('aria-label="Close workspace status"');
    expect(source).toContain("border-rose-500/30 bg-rose-500/10");
    expect(source).toContain('title="Close (Esc)"');
    // Replaced, not accumulated: the old bottom button is gone.
    expect(source).not.toContain('class="gp-btn mt-2 !py-1 !px-2 !text-[11px] w-full"');
  });

  it("styles the trigger as a defined pill button, not a bare transparent element", () => {
    // The trigger sits beside 'Fetch all' in the header/tab bar. Leaving it
    // with un-bordered, transparent styling made it read as ghost text or
    // too transparent over the glass titlebar. It must wear the gp-btn pill
    // geometry, a border, and non-transparent background in both clear and work states.
    expect(source).toMatch(/data-workspace-wip-trigger[\s\S]*?class="[^"]*\bgp-btn\b/);
    expect(source).not.toContain(
      'class="inline-flex items-center gap-1 rounded px-1.5 py-1 text-[11px] transition-colors',
    );
  });
});
