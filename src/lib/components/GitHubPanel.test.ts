import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import GitHubPanel from "./GitHubPanel.svelte";
import { VIEW_REGISTRY } from "../views/viewRegistry";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "GitHubPanel.svelte"),
  "utf8",
);

describe("GitHubPanel", () => {
  it("renders header and action buttons", () => {
    const { body } = render(GitHubPanel);
    // The heading names the section the reader clicked. Which forge this is
    // stays legible from the mark and the owner/repo link beside it, both of
    // which need a context this render has not got.
    expect(body).toContain("Remote");
    expect(body).toContain("lucide-folder-git-2");
    expect(body).toContain("Run CI locally");
    expect(body).toContain("Refresh");
  });

  it("titles the pane the way the section that opens it is labelled", () => {
    // Header and tab disagreeing about a pane's name is how a reader ends up
    // unsure whether they arrived where they clicked.
    const section = VIEW_REGISTRY.work.sections?.find((s) => s.id === "remote");
    expect(section?.label).toBe("Remote");
    expect(source).toContain(`\n        ${section?.label}\n`);
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

  it("surfaces fail-closed full-suite reasons from test_scope", () => {
    expect(source).toContain("ciReport.test_scope");
    expect(source).toContain("fail_closed");
    expect(source).toContain("full suite —");
  });

  it("surfaces backend degradation instead of clean-looking empty states", () => {
    // Runs now come from either the full context or the live poll, so the
    // error and truncation states are surfaced through the derivations that
    // cover both rather than off `ctx` directly — see the live-poll contracts
    // below for what those derivations must consider.
    expect(source).toContain("ctx.runs_error");
    expect(source).toContain("{#if runsTruncated}");
    expect(source).toContain("{#if ctx.prs_truncated}");
    expect(source).toContain("{#if ctx.issues_error}");
    expect(source).toContain("{#if ctx.issues_truncated}");
    expect(source).toContain("{#if (ctx.warnings?.length ?? 0) > 0}");
  });

  describe("live run polling", () => {
    it("polls the narrow runs command, not the four-call context", () => {
      // `cmd_github_context` is four `gh` round trips at up to 45s each. On a
      // repeating timer that is the difference between a live view and a
      // subprocess storm against the user's rate limit.
      expect(source).toContain("cmd_github_runs");
      expect(source).toMatch(/poll:\s*\(\)\s*=>\s*pollRunsOnce\(repo\)/);
    });

    it("only lets a poll that actually ran supersede the context's runs", () => {
      // `checked: false` means the poll could not run. Letting it through
      // would replace real rows with a confident empty list.
      expect(source).toContain("liveRuns?.checked === true");
      expect(source).toContain("runsFromPoll ? (liveRuns?.runs ?? []) : (ctx?.workflow_runs ?? [])");
    });

    it("reports the listing the rows came from, not the last poll attempt", () => {
      // A failed poll while the context's rows are still on screen is a stale
      // live view, not an absent listing. Wiring the timeline's `checked` to
      // the poll would hide rows that were genuinely fetched behind "could not
      // read runs" — the opposite dishonesty from the one the flag prevents.
      expect(source).toContain("const runsChecked = $derived(runsFromPoll ? true : !ctx?.runs_error)");
      expect(source).toContain("checked={runsChecked}");
      expect(source).toContain("error={runsError}");
      // And the poll's own trouble stays visible through the badge instead.
      expect(source).toContain("live={liveState}");
    });

    it("drives one decision, so the four run figures cannot disagree", () => {
      // Rows, truncation, checked and error must all come from the same
      // source. Four independent ternaries is how a panel ends up showing one
      // source's rows beside another's truncation note.
      for (const derived of ["const runs = ", "const runsTruncated = ", "const runsChecked = ", "const runsError = "]) {
        const at = source.indexOf(derived);
        expect(at, derived).toBeGreaterThan(0);
        expect(source.slice(at, at + 220)).toContain("runsFromPoll");
      }
    });

    it("drops a poll whose repository is no longer the open one", () => {
      expect(source).toContain("if ($repoStore.currentPath !== repo) return false;");
    });

    it("clears the poll's snapshot when a newer full context arrives", () => {
      // Otherwise Refresh shows fresh pull requests beside runs from whenever
      // the poll last managed a call: one panel, two ages.
      expect(source).toContain("liveRuns = null;");
      expect(source).toContain("livePoll?.reset();");
    });

    it("gates the poll on every run, not on the branch-filtered view", () => {
      // The branch filter is a view. Gating on it would stop watching a
      // running job the moment someone filtered it off screen.
      expect(source).toContain("anyInFlight(runTimelineRows(runs))");
    });

    it("creates the driver before the effect that feeds it", () => {
      // Svelte runs effects in declaration order. If the sync effect ran
      // first, its one call would hit a null driver and no-op — and on a
      // repository whose runs arrive already in flight from the panel cache,
      // `runs` never changes again, so nothing would ever start the poll.
      const driverAt = source.indexOf("createLivePoll({");
      const syncAt = source.indexOf("livePoll?.sync(moving)");
      expect(driverAt).toBeGreaterThan(0);
      expect(syncAt).toBeGreaterThan(0);
      expect(driverAt, "the driver effect must be declared first").toBeLessThan(syncAt);
    });

    it("tears the driver down per repository", () => {
      expect(source).toContain("driver.dispose()");
    });

    it("reports a run that went red while it was watching", () => {
      expect(source).toContain("failuresSince(");
      expect(source).toContain("Failed while watching:");
    });
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

  it("gives every CI verdict both shades, so neither theme reads it as grey", () => {
    // A bare `-400` is tuned for the dark theme and sits near 2:1 on the
    // light theme's card — on the labels a reader came here to check.
    const fn = source.slice(source.indexOf("function ciClass"), source.indexOf("function runLabel"));
    for (const hue of ["green", "red", "amber"]) {
      expect(fn, `${hue} has no light shade`).toContain(`text-${hue}-700 dark:text-${hue}-400`);
    }
    expect(fn).not.toMatch(/return "text-(green|red|amber)-400"/);
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

describe("GitHubPanel narrowing contracts", () => {
  it("counts the facets with the same predicate the list filters by", () => {
    // A chip reading "4 failing" over a list that shows three is the one
    // failure a filter built from two implementations always eventually has.
    expect(source).toContain("prFacetCounts(ctx?.pull_requests ?? [])");
    expect(source).toContain("filterPullRequests(ctx?.pull_requests ?? [], prFacet, prQuery)");
    expect(source).toContain("{#each visiblePrs as pr (pr.number)}");
    expect(source).toContain("{#each visibleIssues as issue (issue.number)}");
    // Runs render through a preview slice, which is itself derived from the
    // narrowed list: the filter still decides scope, the preview only decides
    // how many cards get drawn. Both links are pinned so the preview can never
    // be re-pointed at the unfiltered runs.
    expect(source).toContain("{#each previewedRuns as run (run.id)}");
    expect(source).toContain("previewSlice(visibleRuns, runsExpanded, RUN_PREVIEW_COUNT)");
    expect(source).toContain("{#each visibleReleases as release");
  });

  it("never dresses a filter that matches nothing as an empty repository", () => {
    expect(source).toContain("prsNarrowedToNothing");
    expect(source).toContain("issuesNarrowedToNothing");
    expect(source).toContain("releasesNarrowedToNothing");
    expect(source).toContain("No pull request matches this filter");
    expect(source).toContain("No issue matches this filter");
    expect(source).toContain("No run on this branch");
    expect(source).toContain("No latest release found");
    // The reader can always get back to the full list from the empty state.
    expect(source).toContain("Clear filter");
    expect(source).toContain("Show all runs");
    expect(source).toContain("Show all releases");
  });

  it("keeps the run timeline's gate and its measured list the same list", () => {
    // The contract above, one layer up. `DeliveryTimeline` documents an empty
    // `rows` with `checked: true` as a real, measured absence and says so:
    // "No runs recorded for this repository." Gating it on the unfiltered
    // `runs` while measuring the narrowed list made it claim nothing was
    // recorded whenever the branch filter matched none of them — directly
    // above the "No run on this branch" that counts the ones it had.
    //
    // Resolved through the declaration rather than matched as text, so the
    // contract still binds if either name changes.
    const measured = /const runRows = \$derived\(runTimelineRows\((\w+)\)\)/.exec(source)?.[1];
    expect(
      measured,
      "runRows is no longer a plain runTimelineRows(<list>) — re-point this contract",
    ).toBeTruthy();
    const gate = /\{#if ([^}]*)\}\s*<div class="mb-2 min-w-0">\s*<DeliveryTimeline/.exec(source)?.[1];
    expect(gate, "could not find the {#if} wrapping the run timeline").toBeTruthy();
    expect(gate).toContain(`${measured}.length`);
  });

  it("drops the previous repository's narrowing on a switch", () => {
    const effect = source.slice(source.indexOf("ctx = ctxCache.get("));
    expect(effect).toContain("clearPrFilter();");
    expect(effect).toContain('issueQuery = "";');
    expect(effect).toContain("runsThisBranch = false;");
    // An expanded workflow list is narrowing in reverse: carried across a
    // switch, it reopens fifty rows for a repository nobody asked to expand.
    expect(effect).toContain("workflowsExpanded = false;");
  });

  it("stamps when the context was fetched, and clears the stamp on hydration", () => {
    // A cached listing is from whenever it was fetched. Carrying the previous
    // repository's stamp onto it would date it to a fetch that never happened.
    expect(source).toContain("fetchedAt = Date.now();");
    expect(source).toContain("fetchedAt = null;");
    expect(source).toContain("fetched {fetchedAgo}");
  });

  it("keeps the CI:local report reachable after it is folded away", () => {
    // Whether the run became a durable git-native claim is the part a reader
    // who has collapsed the steps still needs.
    const report = source.slice(source.indexOf("{:else if ciReport}"));
    const fold = report.indexOf("{#if ciReportOpen}");
    const recorded = report.indexOf("recorded on {ciReport.recorded_commit");
    const notRecorded = report.indexOf("not recorded —");
    expect(fold).toBeGreaterThan(-1);
    expect(recorded).toBeGreaterThan(report.indexOf("{/if}", fold));
    expect(notRecorded).toBeGreaterThan(-1);
  });

  it("lays the listings out in columns rather than one ragged grid", () => {
    // Grid rows are as tall as their tallest cell: twenty pull requests beside
    // a three-line releases card left a screen of white space, and Workflows
    // sat a row away from the runs they produce.
    expect(source).not.toContain("lg:grid-cols-2 xl:grid-cols-3");
    expect(source).toContain('class="xl:col-span-2 space-y-5 min-w-0"');
  });
});

describe("GitHubPanel materials", () => {
  it("uses liquid tabs on the pull-request filter so the page matches the header", () => {
    expect(source).toContain('role="tablist" aria-label="Pull request filter"');
    expect(source).toContain("class:gp-liquid-tabs={macos}");
    expect(source).toContain("gp-liquid-selection");
  });
});

describe("GitHubPanel deploy-section reachability", () => {
  it("puts the deploy section above the rail's two long listings", () => {
    // The whole point: Firebase App Hosting used to sit last, under twenty
    // run rows and ten release rows, so a section that loads fine was
    // unreachable without scrolling past everything that does not need
    // reading. Ordering is the fix that does not depend on list lengths.
    const firebase = source.indexOf("<FirebasePanel repoPath=");
    const workflows = source.indexOf(">Workflows</h3>");
    const runs = source.indexOf(">Workflow runs</h3>");
    const releases = source.indexOf(">Releases</h3>");
    for (const [name, idx] of Object.entries({ firebase, workflows, runs, releases })) {
      expect(idx, `${name} present`).toBeGreaterThan(-1);
    }
    expect(firebase).toBeGreaterThan(workflows);
    expect(firebase).toBeLessThan(runs);
    expect(firebase).toBeLessThan(releases);
  });

  it("mounts the deploy section exactly once", () => {
    // Moving a block is how it ends up rendered in both places.
    expect(source.match(/<FirebasePanel\b/g)).toHaveLength(1);
  });

  it("renders collapsed slices, not the whole fetched lists", () => {
    expect(source).toContain("{#each visibleWfs as wf (wf.id)}");
    expect(source).toContain("{#each previewedRuns as run (run.id)}");
    expect(source).not.toContain("{#each workflows.workflows as wf (wf.id)}");
    expect(source).not.toContain("{#each visibleRuns as run (run.id)}");
    expect(source).toContain(
      "previewSlice(workflows?.workflows ?? [], workflowsExpanded, WORKFLOW_PREVIEW_COUNT)",
    );
    expect(source).toContain(
      "previewSlice(visibleRuns, runsExpanded, RUN_PREVIEW_COUNT)",
    );
  });

  it("defaults releases to the latest one", () => {
    expect(source).toContain("let latestReleaseOnly = $state(true);");
    expect(source).not.toContain("let latestReleaseOnly = $state(false);");
  });

  it("offers two-way expanders whose state is exposed to assistive tech", () => {
    expect(source).toContain(
      "overflowsPreview(workflows.workflows.length, WORKFLOW_PREVIEW_COUNT)",
    );
    expect(source).toContain("overflowsPreview(visibleRuns.length, RUN_PREVIEW_COUNT)");
    expect(source).toContain("aria-expanded={workflowsExpanded}");
    expect(source).toContain("aria-expanded={runsExpanded}");
    expect(source).toContain("workflowsExpanded = !workflowsExpanded");
    expect(source).toContain("runsExpanded = !runsExpanded");
    expect(source).toContain(
      'expandLabel(workflows.workflows.length, workflowsExpanded, "workflows")',
    );
    expect(source).toContain('expandLabel(visibleRuns.length, runsExpanded, "runs")');
  });

  it("never lets a preview decide scope", () => {
    // A preview slices what is drawn. Feeding it to the poll gate would stop
    // watching a running job because its card was collapsed away, and feeding
    // it to the timeline would make the visualization disagree with the
    // branch filter it is supposed to be measuring.
    expect(source).toContain("anyInFlight(runTimelineRows(runs))");
    expect(source).toContain("runTimelineRows(visibleRuns)");
    expect(source).not.toContain("runTimelineRows(previewedRuns)");
    expect(source).not.toContain("anyInFlight(runTimelineRows(previewedRuns))");
    expect(source).toContain("rows={runRows}");
    expect(source).not.toContain("rows={previewedRuns}");
  });

  it("keeps the backend truncation notices counting what was fetched", () => {
    // Two different facts: rows a control hides, and rows the backend never
    // sent. Sourcing a notice from the visible slice would let a collapsed
    // list read as complete coverage of the full listing.
    expect(source).toContain(
      "Showing {workflows.workflows.length} workflows; more exist.",
    );
    expect(source).toContain("Showing the {runs.length} most recent runs");
    expect(source).not.toContain("Showing {visibleWfs.length} workflows");
    expect(source).not.toContain("Showing the {previewedRuns.length} most recent runs");
  });

  it("derives row density from the rows on screen", () => {
    expect(source).toContain("useCompactRows(visibleWfs.length)");
    expect(source).toContain("useCompactRows(previewedRuns.length)");
    expect(source).toContain('compactWfRows ? "space-y-1" : "space-y-2"');
    expect(source).toContain('compactRunRows ? "space-y-1" : "space-y-2"');
  });

  it("keeps the dispatch selector visible at both densities", () => {
    // `path` is what tells two workflows sharing a `name:` apart and is the
    // selector the dispatch call sends; compact tightens spacing, it does not
    // drop the line.
    const rowBlock = source.slice(
      source.indexOf("{#each visibleWfs as wf (wf.id)}"),
      source.indexOf("{#if overflowsPreview(workflows.workflows.length"),
    );
    expect(rowBlock).toContain("{wf.path}");
    expect(rowBlock).not.toContain("{#if !compactWfRows}");
  });

  it("drops both expansions on a repository switch", () => {
    const effect = source.slice(source.indexOf("ctx = ctxCache.get("));
    expect(effect).toContain("workflowsExpanded = false;");
    expect(effect).toContain("runsExpanded = false;");
    expect(effect).toContain("latestReleaseOnly = true;");
  });
});
