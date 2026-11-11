import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import { functionBody } from "../stores/sourceScan";
import BranchNameSuggestion from "./BranchNameSuggestion.svelte";

const dir = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(dir, "BranchNameSuggestion.svelte"), "utf8");
const worktrees = readFileSync(join(dir, "WorktreesPanel.svelte"), "utf8");

describe("BranchNameSuggestion", () => {
  it("renders nothing until the model is ready for a repository", () => {
    // No model server and no repository in SSR: the control is absent rather
    // than a permanently disabled button.
    expect(render(BranchNameSuggestion).body).not.toContain("Suggest a name");
    expect(source).toContain("{#if ready && $repoStore.currentPath}");
    expect(source).toContain("Boolean($harnessStore.ai?.ready)");
  });

  it("creates the suggested branch in the repository it was suggested for, and keeps it on failure", () => {
    expect(functionBody(source, "suggestBranch")).toContain("branchTarget = repoStore.captureTarget();");
    const create = functionBody(source, "createSuggestedBranch");
    // createBranch resolves {ok:false}; it never throws, so no try can catch it.
    expect(create).not.toContain("try {");
    expect(create).toContain("await repoStore.createBranch(branchSuggestion, undefined, branchTarget)");
    expect(create).toContain("if (outcome.ok)");
    expect(create).toMatch(/branchError = outcome\.error/);
    expect(create.indexOf('branchSuggestion = ""')).toBeGreaterThan(create.indexOf("if (outcome.ok)"));
  });

  it("shows a failed suggestion and the model's warnings", () => {
    expect(functionBody(source, "suggestBranch")).toContain('branchError = failure("suggest", "a branch name", err);');
    expect(source).toContain("branchWarnings = result.warnings;");
    expect(source).toContain("{#each branchWarnings as warning}");
    expect(source).toContain('<div class="gp-notice" data-tone="danger" role="alert">{branchError}</div>');
  });

  it("is safe inside the worktree form: no button submits it and Enter does not", () => {
    for (const button of source.match(/<button\b[^>]*>/g) ?? []) {
      expect(button, button).toContain('type="button"');
    }
    expect(source).toContain('event.key === "Enter") event.preventDefault()');
  });

  it("lets the host field adopt the name through onUse", () => {
    expect(source).toContain("onclick={() => onUse(branchSuggestion)}");
  });
});

describe("WorktreesPanel hosts the suggestion beside the new-branch field", () => {
  it("places it after the branch and start-point inputs and fills the branch field", () => {
    const branchField = worktrees.indexOf('placeholder="new branch (optional)"');
    // Mounted through LazyView (its own chunk, out of the entry bundle), with
    // the same callback filling the branch field.
    const suggestion = worktrees.indexOf("load={loadBranchNameSuggestion}");
    expect(branchField).toBeGreaterThan(-1);
    expect(suggestion).toBeGreaterThan(branchField);
    expect(worktrees.slice(branchField, suggestion)).toContain('placeholder="start point"');
    expect(worktrees).toContain('const loadBranchNameSuggestion: ViewLoader = () => import("./BranchNameSuggestion.svelte");');
    expect(worktrees).toContain("onUse: (name: string) => (newBranch = name)");
  });
});
