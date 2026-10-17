import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { createCommitDraftStore } from "./commitDraftStore";

describe("commitDraftStore", () => {
  it("edits only the active tab's draft and ignores edits with no tab", () => {
    const drafts = createCommitDraftStore();
    drafts.set("lost");
    expect(get(drafts)).toBe("");
    drafts.setActiveTab("a");
    drafts.set("for a");
    drafts.setActiveTab("b");
    expect(get(drafts)).toBe("");
    drafts.set("for b");
    expect(drafts.read("a")).toBe("for a");
    drafts.setActiveTab("a");
    expect(get(drafts)).toBe("for a");
    drafts.setActiveTab(null);
    expect(get(drafts)).toBe("");
  });

  it("notifies only when the visible draft changes", () => {
    const drafts = createCommitDraftStore();
    let notified = 0;
    const unsubscribe = drafts.subscribe(() => (notified += 1));
    notified = 0;
    // Two tabs with empty drafts: switching between them shows nothing new.
    drafts.setActiveTab("a");
    drafts.setActiveTab("b");
    drafts.set("");
    expect(notified).toBe(0);
    drafts.set("x");
    drafts.set("x");
    expect(notified).toBe(1);
    unsubscribe();
  });

  it("moves a draft to a successor without overwriting the successor's own", () => {
    const drafts = createCommitDraftStore();
    drafts.setActiveTab("alias");
    drafts.set("alias draft");
    drafts.move("alias", "canonical");
    expect(drafts.read("alias")).toBe("");
    expect(drafts.read("canonical")).toBe("alias draft");

    drafts.setActiveTab("other-alias");
    drafts.set("second alias draft");
    drafts.move("other-alias", "canonical");
    expect(drafts.read("canonical")).toBe("alias draft");
    expect(drafts.read("other-alias")).toBe("");
  });

  it("forgets the drafts of tabs that are no longer open", () => {
    const drafts = createCommitDraftStore();
    for (const id of ["a", "b", "c"]) {
      drafts.setActiveTab(id);
      drafts.set(`draft ${id}`);
    }
    drafts.retain(["a", "c"]);
    expect(drafts.read("a")).toBe("draft a");
    expect(drafts.read("b")).toBe("");
    expect(get(drafts)).toBe("draft c");
    drafts.retain([]);
    expect(get(drafts)).toBe("");
  });
});
