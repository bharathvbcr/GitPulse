import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./TaskRepositoryPicker.svelte", import.meta.url), "utf8");

describe("TaskRepositoryPicker", () => {
  it("compiles", () => {
    const { warnings } = compile(source, { generate: "client", filename: "TaskRepositoryPicker.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });

  it("links and picks the primary in one control instead of two that can disagree", () => {
    // Moved here verbatim from the sheet's contract when the rows moved into
    // this component. A checkbox says whether the task links a repository; a
    // radio in one group says which of the linked ones is primary. Two
    // separate controls could report different answers.
    expect(source).toContain('name="task-primary-{name}"');
    expect(source).toContain("onPrimary(row.id)");
    expect(source).toContain('aria-label="Make {row.name} the primary repository"');
    expect(source).toContain("onToggle(row.id, e.currentTarget.checked)");
    // The sheet's negative, restated where a second, disagreeing control would
    // most plausibly reappear. The primary is reported through `onPrimary`, so
    // this component binds nothing to the draft; `filter` is its own view state.
    expect(source).not.toContain("primary_repository_id");
    expect([...source.matchAll(/bind:\w+/g)].map((m) => m[0])).toEqual(["bind:this", "bind:this", "bind:value"]);
  });

  it("keeps the answer on the closed trigger", () => {
    // A dropdown is only an improvement if collapsing it costs no information.
    // `summaryLine` is the single owner of "how many are linked, and which is
    // primary", and it is the trigger's own label.
    expect(source).toContain("summaryLine(summary)");
    expect(source).toContain("const label = $derived(summaryLine(summary))");
    expect(source).toMatch(/class="gp-btn repo-trigger"[\s\S]*?repo-trigger-label">\{label\}/);
    expect(source).toContain('aria-expanded={open}');
    expect(source).toContain('aria-haspopup="dialog"');
  });

  it("uses native checkboxes and radios rather than rebuilding a listbox", () => {
    // Same reasoning as `gp-select` in app.css: keep the platform widget and
    // replace only its painting. A listbox option also cannot carry a second,
    // independent choice, which is exactly what the primary radio is.
    expect(source).toContain('type="checkbox"');
    expect(source).toContain('type="radio"');
    expect(source).not.toContain('role="listbox"');
    expect(source).not.toContain('role="option"');
    expect(source).toContain('role="radiogroup"');
  });

  it("filters without hiding a linked repository, and without dirtying the draft", () => {
    expect(source).toContain("offerFilter");
    expect(source).toContain('aria-label="Filter repositories"');
    expect(source).toContain("row.keptByLink");
    // The filter is this component's own state and reaches nothing but the
    // rows the sheet derives from it.
    expect(source).toContain("filter = $bindable(\"\")");
    // Only the rows scroll. The summary and the filter above them are how the
    // picker stays readable at any catalog size.
    expect(source).toMatch(/\.repo-list\{[^}]*max-height/);
  });

  it("escapes the sheet's scroller instead of being clipped by it", () => {
    // `.sheet-body` is `overflow:auto` and carries `container-type`, which
    // makes it a containing block for fixed descendants. Either one alone
    // would clip an in-place popover.
    expect(source).toContain('use:portal={"body"}');
    expect(source).toContain("clampMenuPosition");
    expect(source).toContain("LAYERS.MENU");
  });

  it("dismisses for every reason the anchor stops being where it was", () => {
    expect(source).toContain('shouldDismissOverlay(event.target, "[data-task-repo-picker], [data-task-repo-popup]")');
    // Scroll does not bubble, so only a capture listener on window sees the
    // sheet's own scroller move the trigger out from under the popup.
    expect(source).toMatch(/addEventListener\("scroll", onScroll, true\)/);
    expect(source).toMatch(/addEventListener\("resize", onResize\)/);
    // A popover left open over a saving sheet is a panel of dead checkboxes.
    expect(source).toMatch(/if \(disabled && open\) close\(\)/);
  });

  it("closes itself on Escape without closing the task behind it", () => {
    // The sheet also closes on Escape. This listener is on the capture phase
    // and stops propagation, so one Escape dismisses one thing.
    expect(source).toMatch(/addEventListener\("keydown", onKey, true\)/);
    expect(source).toMatch(/event\.key === "Escape"[\s\S]*?event\.stopPropagation\(\)/);
    expect(source).toContain("close({ restoreFocus: true })");
  });

  it("never renders an unread workspace membership as an empty one", () => {
    // `workspace` is null for "no home workspace"; `member` marks come from the
    // rows, which the sheet derives with `members === null` meaning unread.
    expect(source).toContain("row.member");
    expect(source).toMatch(/\{#if workspace\.error\}[\s\S]*?\{:else if summary\.outsiders\.length\}/);
    expect(source).toContain("outsiderLine(summary.outsiders, workspace.name)");
  });

  it("says which empty it is", () => {
    // "No repositories are registered" and "nothing matches this filter" are
    // different problems with different recoveries.
    expect(source).toContain("No repositories are registered yet");
    expect(source).toContain("No repository matches this filter");
  });
});
