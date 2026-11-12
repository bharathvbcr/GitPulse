import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import RebaseModal from "./RebaseModal.svelte";

const source = readFileSync(new URL("./RebaseModal.svelte", import.meta.url), "utf8");

/**
 * Execute is disabled for the plans the backend refuses ([Drop, Squash],
 * [Drop, Fixup], [Drop, Drop, Squash]). The rule itself is `rebasePlanProblem`
 * (planner.test.ts); this pins that the dialog consumes it for the button, the
 * message and the handler, so the rule cannot be bypassed from the UI.
 */
describe("RebaseModal plan validation", () => {
  it("derives the plan problem from the shared validator", () => {
    expect(source).toContain('import { rebasePlanProblem');
    expect(source).toContain("$derived(rebasePlanProblem(items))");
  });

  it("disables Execute and refuses to run while the plan has a problem", () => {
    expect(source).toMatch(/disabled=\{[^}]*planProblem !== null[^}]*\}/);
    expect(source).toMatch(/async function executeRebase\(\)[\s\S]*?planProblem !== null\) return;/);
  });

  it("says why, in an alert", () => {
    expect(source).toMatch(/\{#if planProblem\}\s*<p role="alert"[^>]*>\{planProblem\}<\/p>/);
  });

  it("renders closed without a plan problem", () => {
    const { body } = render(RebaseModal, { props: { isOpen: false } });
    expect(body).not.toContain("no kept commit");
  });
});

/**
 * The plan list is the dialog's one scroller (max-h-80). A hard clip slices the
 * row at its edge in half; the shared edge fade softens it and the scroll
 * padding keeps a focused row out from under the fade band. Rendered proof of
 * the fade is the palette harness; this dialog has no harness page.
 */
describe("RebaseModal scroll edge", () => {
  it("fades the plan list's clipped edge and pads its scroll by the fade band", () => {
    expect(source).toContain('import { scrollEdgeFade } from "../ui/scrollEdgeFade";');
    expect(source).toMatch(/<div class="[^"]*overflow-y-auto[^"]*scroll-py-8[^"]*" data-dialog-body use:scrollEdgeFade>/);
    expect(source).toMatch(/overflow-y-auto scroll-py-8" data-dialog-body use:scrollEdgeFade>\s*\{#each items as commit\}/);
  });
});
