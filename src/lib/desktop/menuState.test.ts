import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { createRepoStore, type RepoState } from "../stores/repoStore";
import { interfaceStore } from "../stores/interfaceStore";
import { buildMenuState, menuActionEnabled } from "./menuState";

const empty = () => get(createRepoStore({ storage: null }));
const prefs = () => get(interfaceStore);
const loaded = (): RepoState => ({ ...empty(), currentPath: "/r/a", currentBranch: "main", isLoading: false });
const model = (repo = empty(), activity: Record<string, string[]> = {}) =>
  buildMenuState(repo, prefs(), "system", activity, false);

describe("native menu projection", () => {
  it("keeps the first status menu compact even for long repository names and branches", () => {
    const state = model({ ...loaded(), currentPath: `/r/${"long-path".repeat(40)}`,
      currentBranch: "feature/".repeat(40) }, { "/r/a": ["fetch"] });
    expect(Array.from(state.trayDetail).length).toBeLessThanOrEqual(72);
    expect(state.traySummary.text).toBe("Clean · 1 running elsewhere");
  });
  it("disables repository work at startup while Help and Open remain available", () => {
    const state = model();
    for (const id of ["fetch", "stage-all", "stash-pop", "terminal-dock", "tab-work", "section:history:graph", "copy-repo-path"]) {
      expect(menuActionEnabled(state, id), id).toBe(false);
    }
    expect(menuActionEnabled(state, "open")).toBe(true);
    expect(menuActionEnabled(state, "documentation")).toBe(true);
  });
  it("distinguishes empty, failed, bare, and busy states", () => {
    const repo = loaded();
    expect(menuActionEnabled(model(repo), "stash-pop")).toBe(false);
    for (const patch of [{ error: "unreadable" }, { isLoading: true }, { isBare: true },
      { operation: { operation: null, probeFailed: true } }]) {
      expect(menuActionEnabled(model({ ...repo, ...patch }), "quick-commit")).toBe(false);
    }
    const state = model(repo, { "/r/a": ["fetch"] });
    expect(menuActionEnabled(state, "fetch")).toBe(false);
    expect(state.labels.find((item) => item.id === "fetch")?.text).toBe("Fetching…");
    expect(menuActionEnabled(model(repo, { "/r/b": ["fetch"] }), "fetch")).toBe(true);
  });
  it("shows selection and terminal state, including an explicit theme equal to the system", () => {
    const repo = { ...loaded(), activeTab: "history" as const, viewSections: { history: "reflog" } };
    const state = buildMenuState(repo, { ...prefs(), terminalDockOpen: true }, "light", {}, false);
    expect(state.checked).toEqual(expect.arrayContaining(["theme-light", "tab-history", "section:history:reflog", "terminal-dock"]));
    expect(state.checked).not.toContain("theme-system");
    expect(state.labels).toContainEqual({ id: "terminal-dock", text: "Hide Terminal" });
  });
  it("only exposes supported operation actions and permits staging resolutions", () => {
    const repo: RepoState = { ...loaded(), statuses: [{ path: "a", is_staged: false, is_conflicted: true,
      status_code: "UU", additions: 0, deletions: 0 }], operation: { probeFailed: false,
      operation: { kind: "Merge", current_step: null, total_steps: null, head_ref: null, incoming_ref: null,
        conflicted_paths: ["a"], conflicted_total: 1, available: ["abort", "continue"] } } };
    const state = model(repo);
    expect(menuActionEnabled(state, "stage-all")).toBe(true);
    expect(menuActionEnabled(state, "operation-abort")).toBe(true);
    expect(menuActionEnabled(state, "operation-continue")).toBe(false);
    expect(menuActionEnabled(state, "operation-skip")).toBe(false);
    expect(menuActionEnabled(state, "pull")).toBe(false);
    expect(state.traySummary.text).toContain("1 conflict");
  });
  it("never calls an unreadable repository clean and counts files uniquely", () => {
    expect(model({ ...loaded(), error: "denied" }).traySummary.text).toContain("unavailable");
    const file = { path: "same", is_conflicted: false, status_code: "M", additions: 0, deletions: 0 };
    const state = model({ ...loaded(), statuses: [{ ...file, is_staged: true }, { ...file, is_staged: false }] });
    expect(state.traySummary.text).toBe("1 changed · 1 staged");
    expect(state.trayDetails).toContain("1 changed · 1 staged · 0 conflicts");
  });
  it("links status to review, recovery or history and distinguishes other repositories' work", () => {
    expect(model().traySummary.id).toBe("open");
    expect(model(loaded()).traySummary.id).toBe("section:history:graph");
    expect(model({ ...loaded(), error: "denied" }).traySummary.id).toBe("refresh");
    const busy = model(loaded(), { "/r/a": ["fetch"], "/r/b": ["pull"] });
    expect(busy.traySummary.text).toBe("Fetching… · 1 running elsewhere");
    expect(busy.traySummary.id).toBe("section:work:overview");
    const degraded = model({ ...loaded(), watch: { status: "degraded", reason: "watch lost" } });
    expect(degraded.traySummary.text).toContain("Not live");
    expect(degraded.trayDetails).toContain("Polling · live updates unavailable");
  });
  it("labels upstream counts as the last fetch and never infers remote freshness", () => {
    const repo = { ...loaded(), branches: [{ name: "main", is_current: true, is_remote: false,
      tip_commit_id: "a".repeat(40), ahead_count: 2, behind_count: 3, upstream: "origin/main",
      is_default: true, is_gone: false, last_commit_timestamp: 0, last_author: "", last_summary: "",
      commits_ahead_of_base: 0, commits_behind_base: 0, additions: 0, deletions: 0, files_changed: 0 }] };
    expect(model(repo).traySummary.text).toContain("↑2 ↓3");
    expect(model(repo).trayDetails).toContain("2 ahead · 3 behind origin/main (last fetch)");
    repo.branches[0].is_gone = true;
    expect(model(repo).traySummary.text).not.toContain("↑");
    expect(model(repo).trayDetails).toContain("Upstream no longer exists");
  });
  it("does not label a prompt or clipboard read as running Git work", () => {
    const state = buildMenuState(loaded(), prefs(), "system", { "/r/a": ["copy-commit"] }, false, {});
    expect(state.traySummary.text).toBe("Clean");
    expect(state.trayDetails.join(" ")).not.toContain("running");
    expect(menuActionEnabled(state, "fetch")).toBe(false);
    expect(model(loaded(), { "/r/a": ["unstash"] }).traySummary.text).toBe("Popping Stash…");
  });
});
