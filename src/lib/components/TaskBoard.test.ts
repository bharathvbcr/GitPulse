import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./TaskBoard.svelte", import.meta.url), "utf8");

describe("TaskBoard materials", () => {
  it("lets the shared material system paint the board instead of covering it with opaque local fills", () => {
    for (const selector of ["workbench", "search", "column-title", "card", "ghost"]) {
      const rule = source.match(new RegExp(`\\.${selector}\\{([^}]*)\\}`))?.[1];
      expect(rule, `missing .${selector} rule`).toBeDefined();
      expect(rule, `.${selector} covers the shared material`).not.toMatch(/(?:^|;)background(?:-color)?:/);
    }
    expect(source).toContain('class="workbench bg-background"');
    expect(source).toContain('class="card bg-surface"');
  });

  it("uses a shared floating material for the drag preview without filtering every card", () => {
    expect(source).toContain('class="ghost gp-glass bg-surface shadow-float"');
    expect(source).not.toContain("backdrop-filter:");
    expect(source).not.toContain("backdrop-blur-");
  });
});

describe("TaskBoard", () => {
  it("compiles", () => {
    const { warnings } = compile(source, { generate: "client", filename: "TaskBoard.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });

  it("moves cards with pointer capture so Tauri file-drop never sees an HTML5 drag", () => {
    expect(source).toContain("setPointerCapture");
    expect(source).toContain("onpointerdown");
    expect(source).toContain("onpointerup");
    expect(source).toContain("skipClick");
    expect(source).toContain("onCardClick");
    expect(source).toContain("data-task-column");
    expect(source).toContain('draggable="false"');
    expect(source).not.toContain("ondragstart");
    expect(source).not.toContain("ondrop");
    expect(source).not.toContain("dataTransfer");
  });

  it("drops onto the column under the pointer, not only the column title", () => {
    expect(source).toContain("statusAtPoint");
    expect(source).toContain("elementFromPoint");
    expect(source).toContain("drop-target");
    expect(source).toContain("task-drag-ghost");
  });

  it("measures insert slots from the same attribute the card buttons carry", () => {
    const query = source.match(/querySelectorAll\("(\[[^\]]+\])"\)/);
    expect(query?.[1]).toBe("[data-task-card]");
    const card = source.slice(source.indexOf('data-testid="task-card"'), source.indexOf("onpointerdown"));
    expect(card).toContain("data-task-card");
    expect(card).toContain("data-card-id={card.id}");
    expect(source).toContain("insertIndexFromY");
  });

  it("moves a focused card with arrow keys", () => {
    expect(source).toContain("ArrowLeft");
    expect(source).toContain("ArrowRight");
    expect(source).toContain("neighborStatus");
    expect(source).toContain("ArrowLeft");
    expect(source).toContain("ContextMenu");
    expect(source).toContain('aria-keyshortcuts="ArrowLeft ArrowRight Delete ContextMenu"');
  });

  it("keeps the board chrome short and skips generic marketing labels", () => {
    expect(source).not.toContain("GLOBAL BOARD");
    expect(source).not.toContain("WORKSPACE BOARD");
    expect(source).not.toContain("REPOSITORY BOARD");
    expect(source).not.toContain("Drop a Git repository");
    expect(source).not.toContain("Drop a task here");
    expect(source).not.toContain("Activity inbox");
    expect(source).not.toContain("Add repositories to start creating linked tasks");
    expect(source).toContain(">New task<");
    expect(source).toContain('aria-label="Inbox"');
  });

  it("does not wipe columns before fetch and keeps previous cards while busy", () => {
    expect(source).not.toMatch(/columns\s*=\s*\{\s*\}/);
    expect(source).toContain("loading = true");
    expect(source).toContain("generation !== revision");
  });

  it("uses GitPulse pill chrome, a compact automation chip, and explains a disabled New", () => {
    expect(source).toContain("gp-btn");
    expect(source).toContain("gp-btn-primary");
    expect(source).toContain("compact");
    expect(source).toContain("Add a repository to create tasks");
    expect(source).toContain("unread");
    expect(source).toContain("listAttention");
    expect(source).not.toContain('class="primary"');
  });

  it("renders scannable cards and collapses empty columns while exposing empty drop targets", () => {
    expect(source).toContain("cardFace");
    expect(source).toContain("visibleStatuses");
    expect(source).toContain("insertionPosition");
    expect(source).toContain("shouldCommitMove");
    expect(source).not.toContain("PRIORITY_LABELS[card.priority]");
    expect(source).not.toContain("repoNames(");
  });

  it("adds already-open repositories from a menu and attaches them to the current workspace", () => {
    expect(source).toContain("openMembershipCandidates");
    expect(source).toContain("menuTabs");
    expect(source).toContain("Add all open");
    expect(source).toContain("Choose folder…");
    expect(source).toContain("attachRegistered");
    expect(source).toContain("putWorkspace");
    expect(source).toContain("openTabs={openTabRefs}");
    expect(source).toContain('aria-haspopup="menu"');
    expect(source).toContain("data-add-repo");
    expect(source).toContain('shouldDismissOverlay(event.target, "[data-add-repo]")');
    expect(source).toContain("emptyAddLabel");
    expect(source).toContain("pickerSelectionIds");
    expect(source).toContain('aria-controls="task-add-repo-menu"');
    expect(source).toContain("onAddMenuKey");
    expect(source).toContain("class=\"add-path\"");
  });

  it("uses in-app confirms, a task context menu, liquid glass, and Quick Enhance", () => {
    expect(source).toContain("canLeave");
    expect(source).not.toContain("window.confirm");
    expect(source).toContain("oncontextmenu");
    expect(source).toContain("TaskContextMenu");
    expect(source).toContain("QuickEnhanceSheet");
    expect(source).toContain("gp-glass");
    expect(source).toContain("gp-btn-danger");
    expect(source).toContain("TaskActionDialog");
    expect(source).toContain("removeSelected");
    expect(source).toContain("isContextMenuKey");
    expect(source).toContain("aria-label=\"Task layout\"");
    expect(source).toContain("gp-liquid-tabs");
    expect(source).toContain("duplicateTitle");
    expect(source).toContain("copyCardsForAgent");
    expect(source).toContain("Copy for agent");
    expect(source).toContain("copyAgent");
    expect(source).toContain("id=\"task-search\"");
    expect(source).toContain("Unassigned");
    expect(source).toContain('e.key.toLowerCase() === "a"');
    expect(source).toContain("Open Quick Enhance?");
    expect(source).toContain("Start a duplicate");
  });

  it("does not add a second backdrop-filter on the board scrim path", () => {
    expect(source).not.toMatch(/\bbackdrop-blur-/);
  });
});
