import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import { compile } from "svelte/compiler";
import { get } from "svelte/store";
import RepoTabBar from "./RepoTabBar.svelte";

import { repoStore } from "../stores/repoStore";
import { interfaceStore } from "../stores/interfaceStore";
import { terminalSessions } from "../terminal/sessionRegistry";

const source = readFileSync(new URL("./RepoTabBar.svelte", import.meta.url), "utf8");

describe("RepoTabBar", () => {
  it("keeps global Tasks and Fleet reachable when no repositories are open", () => {
    const { body } = render(RepoTabBar);
    expect(body).toContain('data-testid="tasks-tab-chip"');
    expect(body).toContain('data-testid="fleet-tab-chip"');
    expect(body).toContain('data-testid="open-repo-tab"');
    expect(body).toContain('title="Open repository"');
  });

  it("renders tab bar when tabs are open", async () => {
    await repoStore.openRepo("/repo/my-project", { allowBroken: true, activate: true });

    const { body } = render(RepoTabBar);
    expect(body).toContain("my-project");
    expect(body).toContain('title="Open repository"');
    expect(body).toContain('title="Recent repositories"');

    await repoStore.closeActiveTab();
  });

  it("hides the repository tab strip while a single repository is open without unmounting chrome", () => {
    expect(source).toContain("hideTabStrip");
    expect(source).toContain("$interfaceStore.autoHideRepoTabs && $repoStore.openTabs.length <= 1");
    expect(source).toContain("{#if !hideTabStrip}");
  });

  it("keeps Fleet, Tasks, Open, and Recents visible when autoHideRepoTabs hides the lone repository tab", async () => {
    interfaceStore.setAutoHideRepoTabs(true);
    await repoStore.openRepo("/repo/alone-project", { allowBroken: true, activate: true });

    const { body } = render(RepoTabBar);
    expect(body).toContain('data-testid="tasks-tab-chip"');
    expect(body).toContain('data-testid="fleet-tab-chip"');
    expect(body).toContain('data-testid="open-repo-tab"');
    expect(body).toContain('title="Recent repositories"');
    expect(body).not.toContain('role="tablist"');
    expect(body).not.toContain("alone-project");

    interfaceStore.setAutoHideRepoTabs(false);
    await repoStore.closeActiveTab();
  });

  it("reports copy-path failure through the shared clipboard seam", () => {
    expect(source).toContain('from "../desktop/clipboard"');
    expect(source).toContain("if (!(await copyText(path)))");
    expect(source).toContain('repoStore.setError("Could not copy path to clipboard")');
  });

  it("gives both popup menus complete keyboard and assistive semantics", () => {
    expect(source).toContain('role="menu"');
    expect((source.match(/role="menuitem"/g) ?? []).length).toBeGreaterThanOrEqual(12);
    expect(source).toContain('aria-haspopup="menu"');
    expect(source).toContain("aria-expanded={recentsOpen}");
    expect(source).toContain("function handlePopupKeydown");
    for (const key of ['"Escape"', '"Tab"', '"ArrowDown"', '"ArrowUp"', '"Home"', '"End"']) {
      expect(source).toContain(key);
    }
  });

  it("moves focus into an opened menu and restores its opener on Escape dismissal", () => {
    expect(source).toContain("menuOpener = e.currentTarget instanceof HTMLElement");
    expect(source).toContain("recentsTriggerEl");
    expect(source).toContain("focusPopup(menuEl)");
    expect(source).toContain("focusPopup(recentsEl)");
    expect(source).toContain("closeMenu({ restoreFocus: true })");
  });

  it("closes on Tab and continues from the opener in document tab order", () => {
    const handler = source.slice(
      source.indexOf("function handlePopupKeydown"),
      source.indexOf("function isTypingTarget"),
    );
    const tabBranch = handler.slice(
      handler.indexOf('e.key === "Tab"'),
      handler.indexOf('e.key === "Escape"'),
    );

    expect(tabBranch).toContain("focusAdjacentToMenuOpener(");
    expect(tabBranch).toContain("e.shiftKey");
    expect(tabBranch).not.toContain("restoreFocus");
    expect(source).toContain("candidate.tabIndex >= 0");
  });

  it("keeps close controls outside tab elements and exposes one roving tab stop", () => {
    expect(source).toContain('<button\n            type="button"\n            role="tab"');
    expect(source).not.toContain('<div\n          role="tab"');
    const tablist = source.slice(source.indexOf('role="tablist"'), source.indexOf("{#each $repoStore.openTabs"));
    expect(tablist).not.toContain('tabindex="0"');
    expect(source).toContain("[data-tab-shell-index]");
    expect(source).toContain('aria-keyshortcuts="Enter p Delete Control+Shift+ArrowLeft Control+Shift+ArrowRight"');
    expect(source).toContain('if (e.key === "Delete")');

    const closeButton = source.slice(
      source.indexOf('title="Close"') - 100,
      source.indexOf('title="Close"') + 250,
    );
    expect(closeButton).toContain('tabindex="-1"');
    expect(closeButton).not.toContain("tab.isActive ? 0 : -1");
  });

  it("has no accessibility compiler warnings", () => {
    const { warnings } = compile(source, { generate: "client" });
    expect(warnings.filter(({ code }) => code.startsWith("a11y_"))).toEqual([]);
  });

  it("elevates the tab bar stacking context so dropdowns are not clipped beneath workspace panes", () => {
    expect(source).toMatch(/class="[^"]*gp-repo-tabs[^"]*relative[^"]*z-20/);
  });

  it("lets the repo label and current branch render in full instead of clipping both inside 14rem", () => {
    const each = source.slice(
      source.indexOf("{#each $repoStore.openTabs"),
      source.indexOf("{#if tab.pinned}"),
    );
    expect(each).not.toContain("max-w-56");
    expect(each).not.toContain("min-w-0 flex-1");

    const label = source.split("\n").find((line) => line.includes("{tab.label}") && line.includes("<span"));
    expect(label).toBeDefined();
    expect(label).not.toContain("truncate");
    expect(label).toContain("whitespace-nowrap");

    const branch = source
      .split("\n")
      .find((line) => line.includes("{tab.currentBranch}") && line.includes("<span"));
    expect(branch).toBeDefined();
    expect(branch).not.toContain("truncate");
    expect(branch).toContain("whitespace-nowrap");
  });

  it("reorders tabs by drag, keyboard, and the context menu", () => {
    expect(source).toContain("dropReorderIndex");
    expect(source).toContain('e.dataTransfer.setData("text/plain", id)');
    expect(source).toContain("application/x-gitpulse-repo-tab");
    expect(source).toContain("Control+Shift+ArrowLeft");
    expect(source).toContain("Move left");
    expect(source).toContain("Move right");
    expect(source).toContain("Move to start");
    expect(source).toContain("Move to end");
    expect(source).toContain("repoStore.moveTab");
    expect(source).toContain("repoStore.moveTabBy");
    expect(source).toContain('aria-live="polite"');
    expect(source).toContain("Drag to reorder");
  });

  it("keeps Open beside the repository tabs instead of stranded after a growing spacer", () => {
    const scroller = source.slice(
      source.indexOf('aria-label="Open repositories"') - 220,
      source.indexOf('role="tablist"'),
    );
    expect(scroller).toContain("min-w-0");
    expect(scroller).toContain("shrink");
    expect(scroller).not.toContain("flex-1");
  });

  it("paints open repository tabs as plates, not ghost text on glass", () => {
    expect(source).toContain("function repoTabChrome");
    expect(source).toContain("border-accent/60 bg-accent/10 text-accent");
    expect(source).toContain("border-border/70 bg-background/50 text-textPrimary");
    expect(source).not.toContain(
      "border-transparent text-textMuted hover:text-textPrimary hover:bg-surfaceHover/60",
    );
  });

  it("styles Open repository as a labelled pill, not a ghost icon", () => {
    const openStart = source.indexOf('data-testid="open-repo-tab"');
    const recents = source.indexOf('<div class="relative shrink-0" data-recents-menu>');
    const open = source.slice(openStart - 80, recents);
    expect(open).toContain("gp-btn");
    expect(open).toContain('title="Open repository"');
    expect(open).toContain("<span>Open</span>");
    expect(open).not.toContain("gp-icon-btn");
  });

  it("opens Tasks from the chip and closes it with a sibling control", () => {
    expect(source).toContain("setTasksOpen(true)");
    expect(source).toContain("setTasksOpen(false)");
    expect(source).toContain('data-testid="tasks-tab-close"');
    expect(source).toContain("taskChrome");
    expect(source).toContain("ListChecks");
    expect(source).toContain("border-accent/60 bg-accent/10 text-accent");
    expect(source).not.toContain('globalSurface === "tasks" ? "repository" : "tasks"');
  });

  it("reveals the repository surface when a repository tab is chosen", () => {
    expect(source).toContain("function selectRepoTab");
    expect(source).toContain('interfaceStore.setGlobalSurface("repository")');
    expect(source).toContain("onclick={() => selectRepoTab(tab.id)}");
  });

  describe("live terminal sessions", () => {
    it("marks a repository holding shells, and counts only past the first", async () => {
      // The dock is per repository tab now, so a shell can be running where
      // the user is not looking — and the PTY budget is process-global, so
      // "which of my repositories hold shells" is also the question to answer
      // when a new one is refused.
      await repoStore.openRepo("/repo/shelled", { allowBroken: true, activate: true });
      const first = terminalSessions.reserve({
        key: "badge-1", repoPath: "/repo/shelled", label: "Shell", status: "running", close: async () => {},
      });
      try {
        let body = render(RepoTabBar).body;
        expect(body).toContain("1 terminal session running in");
        // A single session shows the glyph alone; a bare "1" beside it is noise.
        expect(body).not.toContain("2 terminal sessions running in");

        const second = terminalSessions.reserve({
          key: "badge-2", repoPath: "/repo/shelled", label: "Claude", status: "running", close: async () => {},
        });
        try {
          body = render(RepoTabBar).body;
          expect(body).toContain("2 terminal sessions running in");
        } finally {
          second.release();
        }
      } finally {
        first.release();
      }

      // Released, so the badge goes: an exited shell must not leave a repository
      // looking busy.
      expect(render(RepoTabBar).body).not.toContain("terminal session running in");
      // Nothing is running now, so this close asks nothing and cannot hang.
      await repoStore.closeActiveTab();
    });

    it("does not mark a repository whose sessions belong to another one", async () => {
      await repoStore.openRepo("/repo/quiet", { allowBroken: true, activate: true });
      const elsewhere = terminalSessions.reserve({
        key: "badge-3", repoPath: "/repo/somewhere-else", label: "Shell", status: "running", close: async () => {},
      });
      try {
        expect(render(RepoTabBar).body).not.toContain("terminal session running in");
      } finally {
        elsewhere.release();
      }
      await repoStore.closeActiveTab();
    });
  });

  it("bounds recent repositories dropdown height and enables scrolling to prevent viewport clipping", () => {
    const recentsMenu = source.slice(
      source.indexOf('id="recent-repositories-menu"'),
      source.indexOf("<!-- Workspace-wide actions"),
    );
    expect(recentsMenu).toContain("max-h-");
    expect(recentsMenu).toContain("overflow-y-auto");
    expect(recentsMenu).toContain("shrink-0");
  });

  describe("tab grouping and collapsed group heads", () => {
    it("renders group header with label, repository count, and chevron when tabs are grouped", async () => {
      await repoStore.openRepo("/projects/backend/core", { allowBroken: true, activate: true });
      await repoStore.openRepo("/projects/backend/api", { allowBroken: true, activate: false });
      await repoStore.openRepo("/projects/frontend/web", { allowBroken: true, activate: false });

      repoStore.setTabGroup(get(repoStore).openTabs[0].id, "backend");
      repoStore.setTabGroup(get(repoStore).openTabs[1].id, "backend");

      const { body } = render(RepoTabBar);
      expect(body).toContain('data-group-header="backend"');
      expect(body).toContain('data-group-head="backend"');
      expect(body).toContain("backend");
      expect(body).toContain("(2)");
      expect(body).toContain('aria-expanded="true"');
      expect(body).toContain("core");
      expect(body).toContain("api");
      expect(body).toContain("web");

      repoStore.ungroupTabs();
      while (get(repoStore).openTabs.length > 0) {
        await repoStore.closeActiveTab();
      }
    });

    it("collapses member tabs to a single tab head when group is collapsed", async () => {
      await repoStore.openRepo("/projects/tools/lint", { allowBroken: true, activate: false });
      await repoStore.openRepo("/projects/tools/format", { allowBroken: true, activate: false });
      await repoStore.openRepo("/projects/other/app", { allowBroken: true, activate: true });

      repoStore.setTabGroup(get(repoStore).openTabs[0].id, "tools");
      repoStore.setTabGroup(get(repoStore).openTabs[1].id, "tools");

      repoStore.setGroupCollapsed("tools", true);

      const { body } = render(RepoTabBar);
      expect(body).toContain('data-group-head="tools"');
      expect(body).toContain('aria-expanded="false"');
      expect(body).toContain("(2)");
      // Member tabs of collapsed group are hidden from tab strip
      expect(body).not.toContain("lint");
      expect(body).not.toContain("format");
      // Other tabs remain visible
      expect(body).toContain("app");

      // Expand group again
      repoStore.setGroupCollapsed("tools", false);
      const expandedBody = render(RepoTabBar).body;
      expect(expandedBody).toContain('aria-expanded="true"');
      expect(expandedBody).toContain("lint");
      expect(expandedBody).toContain("format");

      repoStore.ungroupTabs();
      while (get(repoStore).openTabs.length > 0) {
        await repoStore.closeActiveTab();
      }
    });

    it("scales down 15+ repositories across multiple groups so they fit compactly", async () => {
      for (let i = 1; i <= 5; i++) {
        await repoStore.openRepo(`/repos/srv/api-${i}`, { allowBroken: true, activate: false });
      }
      for (let i = 1; i <= 5; i++) {
        await repoStore.openRepo(`/repos/ui/web-${i}`, { allowBroken: true, activate: false });
      }
      for (let i = 1; i <= 5; i++) {
        await repoStore.openRepo(`/repos/infra/k8s-${i}`, { allowBroken: true, activate: false });
      }

      // Group all by parent folder
      repoStore.groupByParentFolder();

      // Collapse all 3 groups
      repoStore.setGroupCollapsed("srv", true);
      repoStore.setGroupCollapsed("ui", true);
      repoStore.setGroupCollapsed("infra", true);

      const { body } = render(RepoTabBar);
      const tablist = body.slice(
        body.indexOf('role="tablist"'),
        body.indexOf('data-testid="open-repo-tab"'),
      );
      // All 3 group heads are visible in tablist with counts of 5
      expect(tablist).toContain('data-group-head="srv"');
      expect(tablist).toContain('data-group-head="ui"');
      expect(tablist).toContain('data-group-head="infra"');
      expect(tablist).toContain("(5)");

      // Member tabs are hidden from tablist to keep tab strip clean and compact
      expect(tablist).not.toContain("api-1");
      expect(tablist).not.toContain("web-1");
      expect(tablist).not.toContain("k8s-1");

      repoStore.ungroupTabs();
      while (get(repoStore).openTabs.length > 0) {
        await repoStore.closeActiveTab();
      }
    });

    it("exposes tab group and group management menus with complete accessible semantics", () => {
      expect(source).toContain("data-group-menu");
      expect(source).toContain("data-group-head");
      expect(source).toContain("groupChrome");
      expect(source).toContain("promptSetGroup");
      expect(source).toContain("promptRenameGroup");
      expect(source).toContain("confirmCloseGroup");
      expect(source).toContain("Change group…");
      expect(source).toContain("Remove from group");
      expect(source).toContain("Add to group…");
      expect(source).toContain("Group all by parent folder");
      expect(source).toContain("Ungroup all repositories");
      expect(source).toContain("Close group repositories…");
      expect(source).toContain("Rename group…");
      expect(source).toContain("[data-tab-index], [data-group-head]");
    });

    it("supports spring-loaded drag auto-expansion and direct drop on group heads", () => {
      expect(source).toContain("function onGroupDragOver");
      expect(source).toContain("function onGroupDragLeave");
      expect(source).toContain("function onGroupDrop");
      expect(source).toContain("dragHoverGroup");
      expect(source).toContain("dragHoverGroupTimer");
      expect(source).toContain("400");
      expect(source).toContain("repoStore.setGroupCollapsed(group, false)");
      expect(source).toContain("repoStore.setTabGroup(id, group)");
      expect(source).toContain("ondragover={(e) => onGroupDragOver(e, info.group, info.isCollapsed)}");
      expect(source).toContain("ondragleave={(e) => onGroupDragLeave(e, info.group)}");
      expect(source).toContain("ondrop={(e) => onGroupDrop(e, info.group)}");
    });

    it("supports keyboard navigation shortcuts and middle-click on group heads", () => {
      expect(source).toContain("function onGroupKeydown");
      expect(source).toContain('e.key === "ArrowRight" && info.isCollapsed');
      expect(source).toContain('e.key === "ArrowLeft" && !info.isCollapsed');
      expect(source).toContain('e.key === "F2"');
      expect(source).toContain('promptRenameGroup(info.group)');
      expect(source).toContain('e.key === "Delete"');
      expect(source).toContain('confirmCloseGroup(info.group, info.tabCount)');
      expect(source).toContain("onauxclick={(e) => {");
      expect(source).toContain("e.button === 1");
    });

    it("provides rich context menus for tabs, group heads, and strip background", () => {
      // Tab menu additions
      expect(source).toContain('Move to group');
      expect(source).toContain('Expand group');
      expect(source).toContain('Collapse group');
      expect(source).toContain('Collapse all groups');
      expect(source).toContain('Expand all groups');

      // Group head menu additions
      expect(source).toContain('Add open ungrouped repositories');
      expect(source).toContain('Close other groups…');

      // Strip background menu
      expect(source).toContain('data-strip-menu');
      expect(source).toContain('oncontextmenu={onStripContext}');
      expect(source).toContain('Reopen closed repository');
      expect(source).toContain('Close all repositories…');
    });
  });
});
