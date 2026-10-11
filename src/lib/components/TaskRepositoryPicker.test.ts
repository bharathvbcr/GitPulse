import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./TaskRepositoryPicker.svelte", import.meta.url), "utf8");

describe("TaskRepositoryPicker", () => {
  it("compiles", () => {
    const { warnings } = compile(source, { generate: "client", filename: "TaskRepositoryPicker.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });

  // Rendered in harness/tasksChecks.js: "linking and the primary choice are
  // one control", "the closed trigger names each linked repository and stars
  // the primary", "Escape in the repository dropdown closes the dropdown, not
  // the task", "Tab within the repository dropdown keeps it open...",
  // "Shift+Tab off the dropdown's first control closes it too", "the
  // repository dropdown escapes the sheet's own clip", "scrolling the sheet
  // dismisses the repository dropdown", "the picker marks which repositories
  // the home workspace already holds", "the sheet opens with nothing linked
  // and refuses to save until one is", "linking updates the trigger without
  // closing the dropdown". What stays below is what the two-repository
  // fixture cannot reach, or a structural rule a render cannot see.
  it("binds nothing to the draft, so a second control cannot disagree with the radio", () => {
    expect(source).not.toContain("primary_repository_id");
    expect([...source.matchAll(/bind:\w+/g)].map((m) => m[0])).toEqual(["bind:this", "bind:this", "bind:value"]);
  });
  it("counts a collapsed remainder of chips rather than dropping it", () => {
    // The fixture links two repositories, never enough to collapse any.
    expect(source).toContain("{#if overflow > 0}<span class=\"repo-more\">+{overflow}</span>{/if}");
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

  it("closes over a saving sheet rather than leaving dead checkboxes open", () => {
    expect(source).toMatch(/if \(disabled && open\) close\(\)/);
    expect(source).toContain("resize: true");
  });
  it("hands focus back only on Escape, never on a pointer dismissal", () => {
    expect(source).toContain('close({ restoreFocus: reason === "escape" })');
    expect(source).not.toContain("close({ restoreFocus: true })");
  });
  it("never renders an unread workspace membership as an empty one", () => {
    // `workspace` is null for "no home workspace". Membership is no longer a
    // per-row mark — it is the "In this workspace" group, and `groupRows` only
    // emits that group when some row carries `member`. With `members === null`
    // the sheet leaves `member` false everywhere, so an unread membership
    // produces no group at all rather than an empty one. `groupRows` owns that
    // rule and taskRepositories.test.ts holds it; what this file guards is that
    // the component asks that owner instead of splitting rows itself.
    expect(source).toContain("const groups = $derived(groupRows(rows))");
    expect(source).not.toMatch(/rows\.filter\(/);
    // Reading the membership can also fail outright, which is a different
    // thing from a workspace with nothing in it, and still says so.
    expect(source).toMatch(/\{#if workspace\.error\}[\s\S]*?\{:else if summary\.outsiders\.length\}/);
    expect(source).toContain("outsiderLine(summary.outsiders, workspace.name)");
  });

  it("groups the rows instead of listing the whole catalog flat", () => {
    expect(source).toContain("{#each groups as group (group.id)}");
    expect(source).toContain('<p class="repo-group">{group.label}</p>');
    // A row filtered out but kept because it is linked still says why it is
    // there, so the filter never looks like it is lying.
    expect(source).toContain("row.keptByLink");
  });

  it("says which empty it is", () => {
    // "No repositories are registered" and "nothing matches this filter" are
    // different problems with different recoveries.
    expect(source).toContain("No repositories are registered yet");
    expect(source).toContain("No repository matches this filter");
  });
});
