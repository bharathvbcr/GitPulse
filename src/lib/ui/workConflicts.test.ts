import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { get } from "svelte/store";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { repoStore } from "../stores/repoStore";
import {
  conflictsFocusRequest,
  openConflicts,
  requestConflictsFocus,
  takeConflictsFocus,
} from "./workConflicts";

const lib = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (path: string) => readFileSync(join(lib, path), "utf8");

describe("the conflict editor's door", () => {
  beforeEach(() => {
    takeConflictsFocus();
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("lands on Overview and leaves a request for the pane that mounts after", () => {
    const navigate = vi.spyOn(repoStore, "setActiveTab").mockImplementation(() => {});
    openConflicts();
    expect(navigate).toHaveBeenCalledWith("work", "overview");
    expect(get(conflictsFocusRequest)).toBe(true);
  });

  it("clears on read, so a later visit to Overview does not jump to the editor", () => {
    requestConflictsFocus();
    expect(takeConflictsFocus()).toBe(true);
    expect(takeConflictsFocus()).toBe(false);
    expect(get(conflictsFocusRequest)).toBe(false);
  });

  it("is the only way the conflict doors navigate", () => {
    // A door that only switched sections did nothing for a reader already on
    // Overview, scrolled into the rows, and from another view the editor did
    // not exist yet when the switch returned.
    const doors = [
      "components/StatusBar.svelte",
      "components/WorkspaceView.svelte",
      "components/MergeBranchDialog.svelte",
      "components/WorkView.svelte",
      "palette/catalog.ts",
    ];
    for (const door of doors) {
      const source = read(door);
      expect(source, door).toMatch(/\bopenConflicts(\(\)|\})/);
      expect(source, door).not.toMatch(/set(ActiveTab|ViewSection)\("work", "overview"\)/);
    }
    // The native tray, status popover and its `3` key reach Overview through
    // App's setTab, which asks for the editor while a merge is stopped.
    expect(read("../App.svelte")).toMatch(/section === "overview" &&[\s\S]{0,200}requestConflictsFocus\(\);/);
  });

  it("is consumed by the pane that renders the editor, which takes it even when nothing is parked", () => {
    const pane = read("components/WorkOverviewPane.svelte");
    const effect = pane.slice(pane.indexOf("if (!$conflictsFocusRequest) return;"));
    expect(effect.indexOf("takeConflictsFocus();")).toBeGreaterThan(-1);
    expect(effect.indexOf("takeConflictsFocus();")).toBeLessThan(effect.indexOf("if (!showEditor) return;"));
    expect(pane).toMatch(/<section id=\{WORK_CONFLICTS_ID\} tabindex="-1"/);
  });
});
