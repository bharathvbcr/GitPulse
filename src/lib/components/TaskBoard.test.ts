import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./TaskBoard.svelte", import.meta.url), "utf8");

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
    expect(source).toContain('aria-keyshortcuts="ArrowLeft ArrowRight"');
  });

  it("keeps the board chrome short and skips generic marketing labels", () => {
    expect(source).not.toContain("GLOBAL BOARD");
    expect(source).not.toContain("WORKSPACE BOARD");
    expect(source).not.toContain("REPOSITORY BOARD");
    expect(source).not.toContain("Drop a Git repository");
    expect(source).not.toContain("Drop a task here");
    expect(source).not.toContain("Activity inbox");
    expect(source).not.toContain("Add repositories to start creating linked tasks");
    expect(source).toContain("New task</button>");
    expect(source).toContain('aria-label="Task notifications"');
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
    expect(source).toContain("PRIORITY_LABELS[card.priority]");
    expect(source).toContain("Change status of ${card.title}");
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
});
