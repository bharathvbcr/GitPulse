import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import { compile } from "svelte/compiler";
import { get } from "svelte/store";
import RepoTabBar from "./RepoTabBar.svelte";

import { repoStore } from "../stores/repoStore";
import { interfaceStore } from "../stores/interfaceStore";
import { terminalSessions } from "../terminal/sessionRegistry";

// Server-rendered output and one compile contract only. Everything that needs a
// live page (menus and their focus, reorder, groups, drag, layout, the chips)
// is checked in harness/repo-tabs.html, on Chrome and WebKit.
const source = readFileSync(new URL("./RepoTabBar.svelte", import.meta.url), "utf8");

describe("RepoTabBar", () => {
  it("ignores the tab-changing shortcuts while a prompt is open, before any of them runs", () => {
    const handler = source.slice(source.indexOf("function handleKey"), source.indexOf("onMount("));
    const guard = handler.indexOf("isPromptOpen()");
    expect(guard).toBeGreaterThan(-1);
    // Escape still closes the tab menus; every switch happens after the guard.
    expect(guard).toBeGreaterThan(handler.indexOf('e.key === "Escape"'));
    expect(guard).toBeLessThan(handler.indexOf("switch (classifyShortcut(e))"));
    expect(guard).toBeLessThan(handler.indexOf("repoStore.activateTabByDigit"));
    expect(guard).toBeLessThan(handler.indexOf("repoStore.closeActiveTab"));
    expect(guard).toBeLessThan(handler.indexOf("onOpen?.()"));
  });

  it("keeps global Tasks and Fleet reachable when no repositories are open", () => {
    const { body } = render(RepoTabBar);
    expect(body).toContain('data-testid="tasks-tab-chip"');
    expect(body).toContain('data-testid="fleet-tab-chip"');
    expect(body).toContain('data-testid="open-repo-tab"');
    expect(body).toContain('title="Open repository"');
  });

  it("keeps Fleet before Agents before Tasks, each a tab named with its count", () => {
    const { body } = render(RepoTabBar);
    const fleet = body.indexOf('data-testid="fleet-tab-chip"');
    const agents = body.indexOf('data-testid="agents-tab-chip"');
    const tasks = body.indexOf('data-testid="tasks-tab-chip"');
    expect(fleet).toBeGreaterThan(-1);
    expect(agents).toBeGreaterThan(fleet);
    expect(tasks).toBeGreaterThan(agents);
    // Below 1100px the label gives way to icon plus badge, so the count must
    // live in the name: the badge is aria-hidden.
    expect(source).toContain("aria-label={surfaceCount > 0 ? `${spec.label}, ${surfaceCount} ${spec.countNoun ?? \"\"}`.trim() : spec.label}");
  });

  it("opens the recent-repositories menu aligned to the trigger's end edge", () => {
    // Open sits at the right edge now, so a start-aligned 320px menu would
    // spill off the window.
    const placement = source.slice(source.indexOf("const recentsPlacement"), source.indexOf("estimate: { width: 320"));
    expect(placement).toContain('align: "end"');
    expect(placement).not.toContain('align: "start"');
  });

  it("orders the title bar: the tablist, the drag zone, Open pinned right, then the workspace actions", () => {
    // Open is the first control of the right-hand cluster, so its position is
    // fixed rather than following the last tab. The free space before it is
    // the window-drag zone (E3): the strip scrolls before that space shrinks
    // below 48px.
    const tabs = source.indexOf('aria-label="Workspace"');
    const zone = source.indexOf("data-drag-zone");
    const open = source.indexOf('data-testid="open-repo-tab"');
    const menu = source.indexOf('data-testid="open-repo-menu"');
    const actions = source.indexOf("<WorkspaceActions");
    expect(tabs).toBeGreaterThan(-1);
    expect(zone).toBeGreaterThan(tabs);
    expect(open).toBeGreaterThan(zone);
    expect(menu).toBeGreaterThan(open);
    expect(actions).toBeGreaterThan(menu);
    const zoneTag = source.slice(source.lastIndexOf("<div", zone), source.indexOf("></div>", zone));
    expect(zoneTag).toContain("min-w-12");
    expect(zoneTag).toContain("flex-1");
    expect(zoneTag).toContain("data-tauri-drag-region");
    const openTag = source.slice(source.lastIndexOf("<button", open), source.indexOf("\n    >", open));
    expect(openTag).not.toContain("ml-auto");
    expect(openTag).toContain("shrink-0");
  });

  it("is one tablist, Workspace, holding the surfaces and the repositories with one roving stop", async () => {
    interfaceStore.setFleetOpen(false);
    await repoStore.openRepo("/repo/one-list", { allowBroken: true, activate: true });
    try {
      const { body } = render(RepoTabBar);
      expect(body.match(/role="tablist"/g)).toHaveLength(1);
      expect(body).toContain('aria-label="Workspace"');
      // The surfaces are tabs of that list, in Fleet, Agents, Tasks order.
      const tabs = [...body.matchAll(/<button\b[^>]*role="tab"[^>]*>/g)].map((match) => match[0]);
      const surfaces = tabs.filter((tag) => tag.includes("data-surface-tab"));
      expect(surfaces.map((tag) => /data-surface-tab="(\w+)"/.exec(tag)?.[1])).toEqual(["fleet", "agents", "tasks"]);
      // Exactly one tab is the roving stop: the active repository, with the
      // repository surface showing.
      expect(tabs.filter((tag) => tag.includes('tabindex="0"'))).toHaveLength(1);
      expect(tabs.filter((tag) => tag.includes('aria-selected="true"'))).toHaveLength(1);
      const stop = tabs.find((tag) => tag.includes('tabindex="0"')) ?? "";
      expect(stop).toContain("data-tab-id");
      expect(surfaces.every((tag) => tag.includes('aria-selected="false"'))).toBe(true);
    } finally {
      await repoStore.closeActiveTab();
    }
  });

  it("moves the one selected tab and the roving stop onto the open surface", async () => {
    await repoStore.openRepo("/repo/surface-stop", { allowBroken: true, activate: true });
    interfaceStore.setTasksOpen(true);
    try {
      const { body } = render(RepoTabBar);
      const tabs = [...body.matchAll(/<button\b[^>]*role="tab"[^>]*>/g)].map((match) => match[0]);
      expect(tabs.filter((tag) => tag.includes('aria-selected="true"'))).toHaveLength(1);
      const stops = tabs.filter((tag) => tag.includes('tabindex="0"'));
      expect(stops).toHaveLength(1);
      expect(stops[0]).toContain('data-surface-tab="tasks"');
    } finally {
      interfaceStore.setTasksOpen(false);
      await repoStore.closeActiveTab();
    }
  });

  it("makes Fleet the way in when nothing is selected", () => {
    const { body } = render(RepoTabBar);
    const tabs = [...body.matchAll(/<button\b[^>]*role="tab"[^>]*>/g)].map((match) => match[0]);
    const stops = tabs.filter((tag) => tag.includes('tabindex="0"'));
    expect(stops).toHaveLength(1);
    expect(stops[0]).toContain('data-surface-tab="fleet"');
  });

  it("replaces the native title tooltip with the hover card, opened after the intent delay", () => {
    // Native `title` on the tab shell is gone (the card says more and is
    // placed by the popover owner); the card is portaled and closes on drag.
    const tab = source.slice(source.indexOf("{#snippet repoTab"), source.indexOf("{#snippet stackHead"));
    expect(tab).not.toMatch(/\n    title=\{/);
    expect(tab).toContain("onpointerenter={(e) => onTabEnter(e, tab.id)}");
    expect(tab).toContain("closeCard();");
    expect(source).toContain("const CARD_INTENT_MS = 500;");
    expect(source).toContain("use:popover={cardPlacement}");
    expect(source).toContain("use:portal={\"body\"}");
  });

  it("jumps to a repository with Cmd/Ctrl+1-9 through the store, which makes 9 the last", () => {
    const handler = source.slice(source.indexOf("function handleKey"), source.indexOf("onMount("));
    // The digit-to-stop rule lives in `jumpTargetIndex`, shared with the native
    // menu's repository-tab items; this handler only forwards the digit.
    expect(handler).toContain("repoStore.activateTabByDigit(Number(digit[1]))");
    expect(handler).not.toContain("activationStops");
    // A key a terminal or editor already consumed is not a tab jump.
    const jump = handler.slice(handler.indexOf('case "jumpToTab"'), handler.indexOf('case "cycleTabs"'));
    expect(jump).toContain("if (e.defaultPrevented) return;");
  });

  it("hints the digit that reaches a tab, and none for a stop no digit reaches", () => {
    expect(source).toContain("jumpDigitFor(stops.indexOf(cardTab.id), stops.length)");
    expect(source).toContain("shortcutTextLabel(`⌘${jumpDigit}`, $hostPlatform.os)");
    expect(source).not.toContain("⌃⌥");
  });

  it("marks a background tab whose agent asks for the reader, and never marks an unread one", () => {
    const tab = source.slice(source.indexOf("{#snippet repoTab"), source.indexOf("{#snippet stackHead"));
    expect(tab).toContain("data-agent-mark={ask.reason}");
    expect(tab).toContain("{#if !(tab.isActive && viewingRepo)}");
    // Only an `attention` state draws a mark; unknown stays unknown.
    expect(source).toContain('return state?.state === "attention" ? state : null;');
  });

  it("opens the overflow menu only while the strip hides units", () => {
    // Nothing is hidden in a server render, so the trigger is absent.
    expect(render(RepoTabBar).body).not.toContain('data-testid="repo-overflow"');
    expect(source).toContain("new IntersectionObserver(");
    expect(source).toContain("{#if !hideTabStrip && overflowUnits.length > 0}");
  });

  it("gives the strip the diffuse edge", () => {
    expect(source).toContain('<ScrollCue target={scroller} axis="x" diffuse />');
  });

  it("dismisses Fleet with the same close control Agents and Tasks use", () => {
    interfaceStore.setFleetOpen(false);
    expect(render(RepoTabBar).body).not.toContain('data-testid="fleet-tab-close"');

    interfaceStore.setFleetOpen(true);
    try {
      const { body } = render(RepoTabBar);
      expect(body).toContain('data-testid="fleet-tab-close"');
      expect(body).toContain('aria-label="Close Fleet"');
      // The close is the icon-button dialect, danger-toned: no palette class.
      const close = /<button\b[^>]*data-testid="fleet-tab-close"[^>]*>/.exec(body)?.[0] ?? "";
      expect(close).toContain('class="gp-icon-btn"');
      expect(close).toContain('data-tone="danger"');
      expect(close).not.toContain("rose");
    } finally {
      interfaceStore.setFleetOpen(false);
    }

    // One snippet draws all three surfaces, so Fleet's wiring is read at its
    // call site and the close control's behaviour at the snippet.
    const fleet = source.slice(
      source.indexOf('id: "fleet"'),
      source.indexOf('id: "agents"'),
    );
    expect(fleet).toContain("onselect: () => interfaceStore.toggleFleet()");
    expect(fleet).toContain("onclose: () => interfaceStore.setFleetOpen(false)");
    const snippet = source.slice(source.indexOf("{#snippet surfaceTab"), source.indexOf("{#snippet groupHead"));
    expect(snippet.indexOf("onclick={spec.onselect}")).toBeLessThan(snippet.indexOf('data-testid="{spec.id}-tab-close"'));
    expect(snippet).toContain("{#if spec.open}");
    expect(snippet).toContain("event.stopPropagation()");
    expect(snippet).toContain("spec.onclose()");
  });

  it("renders tab bar when tabs are open", async () => {
    await repoStore.openRepo("/repo/my-project", { allowBroken: true, activate: true });

    const { body } = render(RepoTabBar);
    expect(body).toContain("my-project");
    expect(body).toContain('title="Open repository"');
    expect(body).toContain('title="Clone or open a recent repository"');

    await repoStore.closeActiveTab();
  });

  it("keeps Fleet, Tasks, Open, and Recents visible when autoHideRepoTabs hides the lone repository tab", async () => {
    interfaceStore.setAutoHideRepoTabs(true);
    await repoStore.openRepo("/repo/alone-project", { allowBroken: true, activate: true });

    const { body } = render(RepoTabBar);
    expect(body).toContain('data-testid="tasks-tab-chip"');
    expect(body).toContain('data-testid="fleet-tab-chip"');
    expect(body).toContain('data-testid="open-repo-tab"');
    expect(body).toContain('title="Clone or open a recent repository"');
    // The surfaces stay a tablist; only the repository strip goes away.
    expect(body).toContain('aria-label="Workspace"');
    expect(body).not.toContain("data-repo-strip-scroller");
    expect(body).not.toContain("alone-project");

    interfaceStore.setAutoHideRepoTabs(false);
    await repoStore.closeActiveTab();
  });

  it("has no accessibility compiler warnings", () => {
    const { warnings } = compile(source, { generate: "client" });
    expect(warnings.filter(({ code }) => code.startsWith("a11y_"))).toEqual([]);
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

    it("paints a tab's own color and inherits the group color when the tab has none", async () => {
      await repoStore.openRepo("/repo/color-own", { allowBroken: true, activate: true });
      await repoStore.openRepo("/repo/color-inherit", { allowBroken: true, activate: false });
      const own = get(repoStore).openTabs.find((tab) => tab.path.endsWith("color-own"));
      const inherited = get(repoStore).openTabs.find((tab) => tab.path.endsWith("color-inherit"));
      expect(own).toBeTruthy();
      expect(inherited).toBeTruthy();
      if (!own || !inherited) return;
      repoStore.setTabGroup(own.id, "marked");
      repoStore.setTabGroup(inherited.id, "marked");
      repoStore.setTabColor(own.id, "blue");
      repoStore.setGroupColor("marked", "teal");

      const { body } = render(RepoTabBar);
      expect(body).toContain('data-tab-color="blue"');
      expect(body).toContain('data-tab-color-source="own"');
      expect(body).toContain('data-tab-color="teal"');
      expect(body).toContain('data-tab-color-source="group"');
      expect(body).toContain('data-group-color="teal"');
      expect(body).toContain(">Blue<");
      expect(body).toContain("Teal group color");

      repoStore.ungroupTabs();
      while (get(repoStore).openTabs.length > 0) {
        await repoStore.closeActiveTab();
      }
    });

    it("collapses inactive tabs in tab groups to focus on active tabs", async () => {
      await repoStore.openRepo("/projects/tools/lint", { allowBroken: true, activate: true });
      await repoStore.openRepo("/projects/tools/format", { allowBroken: true, activate: false });
      await repoStore.openRepo("/projects/web/dashboard", { allowBroken: true, activate: false });
      await repoStore.openRepo("/projects/other/app", { allowBroken: true, activate: false });

      repoStore.setTabGroup(get(repoStore).openTabs[0].id, "tools");
      repoStore.setTabGroup(get(repoStore).openTabs[1].id, "tools");
      repoStore.setTabGroup(get(repoStore).openTabs[2].id, "web");

      interfaceStore.setCollapseInactiveGroupTabs(true);

      const { body } = render(RepoTabBar);

      // tools group has active tab lint -> header + lint visible, format hidden
      expect(body).toContain('data-group-head="tools"');
      expect(body).toContain("lint");
      expect(body).not.toContain("format");
      expect(body).toContain("1 inactive collapsed");

      // web group has no active tab -> header visible, dashboard hidden
      expect(body).toContain('data-group-head="web"');
      expect(body).not.toContain("dashboard");

      // ungrouped tab app remains visible
      expect(body).toContain("app");

      // Turn off collapseInactiveGroupTabs
      interfaceStore.setCollapseInactiveGroupTabs(false);
      const expandedBody = render(RepoTabBar).body;
      expect(expandedBody).toContain("lint");
      expect(expandedBody).toContain("format");
      expect(expandedBody).toContain("dashboard");
      expect(expandedBody).toContain("app");

      repoStore.ungroupTabs();
      while (get(repoStore).openTabs.length > 0) {
        await repoStore.closeActiveTab();
      }
    });

    it("collapses inactive tabs running active shells and indicates shells on the group header", async () => {
      await repoStore.openRepo("/projects/tools/lint", { allowBroken: true, activate: true });
      await repoStore.openRepo("/projects/tools/format", { allowBroken: true, activate: false });

      repoStore.setTabGroup(get(repoStore).openTabs[0].id, "tools");
      repoStore.setTabGroup(get(repoStore).openTabs[1].id, "tools");

      const shell = terminalSessions.reserve({
        key: "tools-format-shell",
        repoPath: "/projects/tools/format",
        label: "Build",
        status: "running",
        close: async () => {},
      });

      try {
        interfaceStore.setCollapseInactiveGroupTabs(true);
        const { body } = render(RepoTabBar);

        // Group head indicates the collapsed inactive tab and the active shell session running in tools
        expect(body).toContain('data-group-head="tools"');
        expect(body).toContain("1 inactive collapsed");
        expect(body).toContain("1 terminal session running in tools");
        expect(body).toContain(">lint</span>");
        expect(body).not.toContain(">format</span>");
      } finally {
        shell.release();
        interfaceStore.setCollapseInactiveGroupTabs(false);
        repoStore.ungroupTabs();
        while (get(repoStore).openTabs.length > 0) {
          await repoStore.closeActiveTab();
        }
      }
    });

    it("respects collapseInactiveIncludeShells when collapsing inactive tabs", async () => {
      await repoStore.openRepo("/projects/tools/lint", { allowBroken: true, activate: true });
      await repoStore.openRepo("/projects/tools/format", { allowBroken: true, activate: false });
      await repoStore.openRepo("/projects/tools/test", { allowBroken: true, activate: false });

      repoStore.setTabGroup(get(repoStore).openTabs[0].id, "tools");
      repoStore.setTabGroup(get(repoStore).openTabs[1].id, "tools");
      repoStore.setTabGroup(get(repoStore).openTabs[2].id, "tools");

      const shell = terminalSessions.reserve({
        key: "tools-format-shell-toggle",
        repoPath: "/projects/tools/format",
        label: "Build",
        status: "running",
        close: async () => {},
      });

      try {
        interfaceStore.setCollapseInactiveGroupTabs(true);
        // Default: collapseInactiveIncludeShells is true -> format is collapsed even with active shell
        let body = render(RepoTabBar).body;
        expect(body).toContain(">lint</span>");
        expect(body).not.toContain(">format</span>");
        expect(body).not.toContain(">test</span>");

        // When collapseInactiveIncludeShells is false -> format (active shell) remains visible
        interfaceStore.setCollapseInactiveIncludeShells(false);
        body = render(RepoTabBar).body;
        expect(body).toContain(">lint</span>");
        expect(body).toContain(">format</span>");
        expect(body).not.toContain(">test</span>");
      } finally {
        shell.release();
        interfaceStore.setCollapseInactiveGroupTabs(false);
        interfaceStore.setCollapseInactiveIncludeShells(true);
        repoStore.ungroupTabs();
        while (get(repoStore).openTabs.length > 0) {
          await repoStore.closeActiveTab();
        }
      }
    });

    it("includes collapse inactive options and shell controls in right-click menus", () => {
      // Tab right-click menu (data-repo-menu)
      expect(source).toContain("data-repo-menu");
      expect(source).toContain('Collapse inactive tabs in "{tab.group}"');
      expect(source).toContain("Collapse tabs with active shells");
      expect(source).toContain("Keep tabs with active shells visible");

      // Group head right-click menu (data-group-menu)
      expect(source).toContain("data-group-menu");
      expect(source).toContain("Collapse inactive tabs");
      expect(source).toContain("Expand all tabs in group");

      // Strip background right-click menu (data-strip-menu)
      expect(source).toContain("data-strip-menu");
      expect(source).toContain("Collapse inactive tabs in groups");
    });
  });
});
