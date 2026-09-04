import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import GitHubPanel from "./GitHubPanel.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "GitHubPanel.svelte"),
  "utf8",
);

describe("GitHubPanel", () => {
  it("renders header and action buttons", () => {
    const { body } = render(GitHubPanel);
    expect(body).toContain("GitHub");
    expect(body).toContain("Run CI locally");
    expect(body).toContain("Refresh");
  });
});

describe("GitHubPanel flicker contracts", () => {
  it("hydrates cached context and workflows before refetching on remount", () => {
    expect(source).toContain("createRepoPanelCache<GitHubContext>()");
    expect(source).toContain("createRepoPanelCache<WorkflowsReport>()");
    expect(source).toContain("ctxCache.set(repo, next);");
    expect(source).toContain("workflowsCache.set(repo, next);");
    expect(source).toContain("ctx = ctxCache.get(repo ?? \"\") ?? null;");
    expect(source).toContain("workflows = workflowsCache.get(repo ?? \"\") ?? null;");
  });

  it("never blanks fetched sections behind a refresh — spinner stays inline", () => {
    // The full-pane placeholder only fires when there is nothing to show.
    expect(source).toContain("{#if (loading && !ctx) || (workflowsLoading && !workflows)}");
    expect(source).not.toContain("{#if loading || workflowsLoading}");
    // Refresh feedback is an inline spinner next to the button.
    expect(source).toContain("<LoaderCircle size={13} class=\"animate-spin\" />");
  });
});

describe("GitHubPanel guarded-action contracts", () => {
  it("consumes Guarded<string> from every gated gh action — never the bare payload", () => {
    // The backend returns { policy, output }; typing the invoke as string
    // made every success banner render "[object Object]".
    for (const command of [
      "cmd_github_trigger_workflow",
      "cmd_github_rerun_run",
      "cmd_github_cancel_run",
      "cmd_github_checkout_pr",
    ]) {
      const callIdx = source.indexOf(`"${command}"`);
      expect(callIdx, `${command} invoked`).toBeGreaterThan(-1);
      const callSite = source.slice(Math.max(0, callIdx - 80), callIdx);
      expect(callSite, `${command} typed as Guarded<string>`).toMatch(
        /invoke<Guarded<string>>\($/,
      );
    }
  });

  it("files each gated action's policy verdict with the harness store", () => {
    // These actions bypass repoStore.runMutating; without an explicit
    // recordVerdict the gate's decision (including "no gate present") is
    // lost to the journal.
    const verdictCalls = source.match(/fileVerdict\(result, repo\);/g)?.length ?? 0;
    expect(verdictCalls).toBeGreaterThanOrEqual(4);
    expect(source).toContain(
      "harnessStore.recordVerdict(result?.policy ?? null, repoPath)",
    );
  });

  it("renders the action notice from result.output with a real fallback", () => {
    expect(source).toContain("function actionMessage(result: Guarded<string>, fallback: string)");
    expect(source).not.toMatch(/actionNotice\s*=\s*output\s*\|\|/);
  });

  it("guards CI:local so a superseded run's late report cannot win", () => {
    expect(source).toContain("let ciInflight: AsyncGuard | null = null;");
    const invokeIdx = source.indexOf('invoke<CiLocalReport>("cmd_ci_local"');
    expect(invokeIdx).toBeGreaterThan(-1);
    // Assignment happens only while this run is still the live one.
    const after = source.indexOf("guard.isLive()", invokeIdx);
    expect(after).toBeGreaterThan(-1);
  });

  it("surfaces backend degradation instead of clean-looking empty states", () => {
    expect(source).toContain("{#if ctx.runs_error}");
    expect(source).toContain("{#if ctx.runs_truncated}");
    expect(source).toContain("{#if ctx.prs_truncated}");
    expect(source).toContain("{#if ctx.issues_error}");
    expect(source).toContain("{#if ctx.issues_truncated}");
    expect(source).toContain("{#if (ctx.warnings?.length ?? 0) > 0}");
  });

  it("renders the issues the context already fetched", () => {
    // Issues were on the wire and never on the screen, so an open bug looked
    // like a repository with none. The list is the same shape as PRs.
    expect(source).toContain("ctx.issues");
    expect(source).toContain("Open issues");
  });

  it("offers a new-pull-request URL rather than silently omitting the write path", () => {
    expect(source).toContain("pullRequestCreateUrl");
    expect(source).toContain("New pull request");
    expect(source).not.toMatch(/window\.open\s*\(/);
  });

  it("opens external links through the canonical opener — no window.open fallback", () => {
    expect(source).toContain('from "../desktop/openExternal"');
    // The comment explains why; the *call* must not exist.
    expect(source).not.toMatch(/window\.open\s*\(/);
    expect(source).not.toContain('"@tauri-apps/plugin-opener"');
  });

  it("colors waiting/requested runs as in-flight, never muted or green", () => {
    const fn = source.slice(source.indexOf("function ciClass"), source.indexOf("function runLabel"));
    expect(fn).toContain("waiting");
    expect(fn).toContain("requested");
  });

  it("reloads the graph with the visible filter context after a PR checkout", () => {
    // A bare loadGraph(repo) reset the view to query=""/HEAD while FilterBar
    // still showed the selection, and the scheduler memo then blocked the
    // correction. The post-mutation reload must carry the filter context;
    // the backend applies every query term, so nothing is sanitized away.
    expect(source).toContain('from "../stores/filterStore"');
    expect(source).toMatch(
      /graphStore\.loadGraph\(\s*repo,\s*\$filterStore\.searchQuery,\s*\$filterStore\.selectedBranch,?\s*\)/,
    );
    expect(source).not.toMatch(/graphStore\.loadGraph\(\s*repo\s*\)/);
  });
});
