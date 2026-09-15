import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./TaskDuePicker.svelte", import.meta.url), "utf8");
/** Everything the component renders, with the doc comment and logic stripped. */
const markup = source.slice(source.indexOf("</script>") + "</script>".length);

describe("TaskDuePicker", () => {
  it("compiles", () => {
    const { warnings } = compile(source, { generate: "client", filename: "TaskDuePicker.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });

  it("asks the shared grammar for every date rather than parsing words itself", () => {
    // A shortcut chip, a typed phrase and `due:friday` on the board have to
    // land on the same second, and they can only do that by going through one
    // parser. `parseQuickAddDue` is that parser; `taskDue` is the only other
    // thing allowed to produce a timestamp here.
    expect(source).toContain('from "../workbench/taskQuickAdd"');
    expect(source).toContain('from "../workbench/taskDue"');
    expect(source).toContain("parseQuickAddDue(word, now())");
    expect(source).toContain("readDuePhrase(phrase, value, now())");
    // No second weekday table, offset regex or hour constant.
    expect(source).not.toMatch(/\bmonday\b|\btomorrow\b|\bnext-week\b/i);
    expect(source).not.toMatch(/setHours|getDay\(\)|86_?400/);
    // Shortcut labels come from the list that owns them, never retyped here.
    expect(source).toContain("DUE_SHORTCUTS as shortcut");
  });

  it("keeps the platform clock and replaces only the calendar", () => {
    // Same reasoning as `gp-select` in app.css: the native widget brings the
    // locale, the keyboard handling and the mobile picker for free.
    expect(markup).toContain('type="time"');
    // The prose above names the control this replaces, so the guard is on what
    // the component actually renders.
    expect(markup).not.toContain("datetime-local");
    expect(markup).not.toContain('role="listbox"');
  });

  it("can be cleared, which the native control it replaces could not be", () => {
    expect(source).toContain('data-testid="task-due-clear"');
    expect(source).toContain("onChange(null)");
    // Nothing to clear is a disabled button, not a button that writes null
    // over null and marks the draft dirty.
    expect(source).toContain("disabled={value === null}");
  });

  it("distinguishes today from the chosen day", () => {
    // Outline means "you are here", fill means "this is chosen". Painting both
    // the same way makes the grid ambiguous on the day a task is due.
    expect(source).toContain("data-today={cell.isToday}");
    expect(source).toContain("data-selected={cell.isSelected}");
    expect(source).toMatch(/\.day\[data-today="true"\]\{[^}]*border-color/);
    expect(source).toMatch(/\.day\[data-selected="true"\]\{[^}]*background/);
  });

  it("says how far away the deadline is, and reports the board's own urgency", () => {
    // The chip beside the date and the lane the board files the task under
    // must agree; `relativeDue` is what makes them ask the same function.
    expect(source).toContain("relativeDue(value, Math.floor(now() / 1000))");
    expect(source).toContain('relative.state === "overdue"');
  });

  it("closes without also closing the task sheet behind it", () => {
    expect(source).toContain('event.key === "Escape"');
    expect(source).toContain("event.stopPropagation()");
    expect(source).toContain("shouldDismissOverlay");
    expect(source).toContain("[data-task-due-picker], [data-task-due-popup]");
  });

  it("is portaled, because the sheet body it lives in is a scroller", () => {
    expect(source).toContain('use:portal={"body"}');
    expect(source).toContain("clampMenuPosition");
    expect(source).toContain("LAYERS.MENU");
  });

  it("opens on the month the task is due in, not the month last browsed", () => {
    expect(source).toContain("browsed = $state<{ year: number; month: number } | null>(null)");
    expect(source).toContain("const view = $derived(browsed ?? viewFor(value, now()))");
    // Closing forgets the browsing, so the next open follows the value again.
    expect(source).toMatch(/function close\([\s\S]*?browsed = null;/);
  });

  it("takes its clock by injection so a test can pin the calendar", () => {
    expect(source).toContain("now = () => Date.now()");
    expect(source).not.toMatch(/\bnew Date\(\)/);
  });
});
