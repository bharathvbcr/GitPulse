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
