import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import GitHubPanel from "./GitHubPanel.svelte";
import { VIEW_REGISTRY } from "../views/viewRegistry";

// What the panel does with a live page — truncation and error states, the
// narrowing filters, previews and expanders, guarded actions and their
// verdicts, CI:local, the live run poll, cache hydration, layout — is checked in
// harness/github-panel.html on Chrome and WebKit. The three source contracts
// left here pin timer lifecycles that a rendered check cannot reach within a
// harness deadline.
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
    const heading = /<h2[^>]*>([\s\S]*?)<\/h2>/.exec(render(GitHubPanel).body)?.[1] ?? "";
    expect(heading.replace(/<[^>]+>/g, "").trim()).toBe(section?.label);
  });
});

describe("GitHubPanel confirmation and refresh contracts", () => {
  // Rendered behaviour (the notice outliving the row, the failed refresh keeping
  // the listings) is checked in harness/github-panel.html; effects do not run
  // here. These pin the structure that makes it true.
  it("sets an action's confirmation after the reload that clears it, from both callers", () => {
    expect(source).toContain("async function reloadThenNotice(message: string)");
    const body = source.slice(source.indexOf("async function reloadThenNotice"));
    expect(body.indexOf("await loadAll()")).toBeLessThan(body.indexOf("actionNotice = message"));
    expect(source).toContain("onChanged={(message) => void reloadThenNotice(message)}");
    expect(source).toContain("void reloadThenNotice(message); }}");
    // The old shape: the notice set first, then wiped by loadAll's own reset.
    expect(source).not.toMatch(/actionNotice = message;\s*void loadAll\(\)/);
  });

  it("hands the confirmation up instead of keeping a copy on the row that may unmount", () => {
    const row = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "github", "PullRequestDetail.svelte"), "utf8");
    expect(row).toContain("onChanged: (message: string) => void;");
    expect(row).not.toContain("let notice");
    expect(row).toContain(">Try again</button>");
  });

  it("hands the run timeline the panel's own refresh as its Rescan, not a new fetch path", () => {
    // Behavior is proven in harness/github-panel.html (one click, one context read).
    const timeline = source.slice(source.indexOf("<DeliveryTimeline"), source.indexOf("/>", source.indexOf("<DeliveryTimeline")));
    expect(timeline).toContain("onRescan={() => void loadAll()}");
    expect(source).toContain('onclick={loadAll} class="gp-btn"');
  });

  it("keeps the last good context when a refresh fails, and disables Refresh while one is out", () => {
    expect(source).toContain("if (ctx && !ctx.error) reportRefreshFailure(formatError(err));");
    expect(source).toContain("if (next.error && ctx && !ctx.error) {");
    expect(source).toContain("disabled={loading || workflowsLoading}");
    // The stamp is only moved by a read that landed.
    const failure = source.slice(source.indexOf("function reportRefreshFailure"), source.indexOf("async function loadWorkflowsFor"));
    expect(failure).not.toContain("fetchedAt");
  });
});

describe("GitHubPanel silent background refresh", () => {
  // Ported from ManviOpsPanel's issue monitor, which refreshed once a minute
  // while visible. Remote now owns that single read; no second poller exists.
  it("refreshes once a minute through the adaptive timer, keyed on the repository", () => {
    expect(source).toContain("const BACKGROUND_REFRESH_MS = 60_000;");
    expect(source).toContain("createAdaptiveTimer(() => void refreshQuietly(repo), BACKGROUND_REFRESH_MS)");
  });

  it("runs no timer while a global surface hides the repository", () => {
    // Tasks, Agents and Fleet hide the repository surface without unmounting
    // it; a minute's gh read for a pane nobody can see is pure cost.
    const effect = source.slice(source.indexOf("const repo = $currentRepoPath;\n    if (!repo ||"));
    const guard = effect.slice(0, effect.indexOf("createAdaptiveTimer("));
    expect(guard).toContain('$interfaceStore.globalSurface !== "repository") return;');
  });

  it("claims neither loading nor the action banner, and yields to a read a person asked for", () => {
    const body = source.slice(source.indexOf("async function refreshQuietly"), source.indexOf("/** A failed refresh over data already on screen"));
    expect(body).toContain("if (quietRefreshing || loading || $repoStore.currentPath !== repo) return;");
    expect(body).not.toContain("loading = true");
    expect(body).not.toContain("actionNotice");
    expect(body).not.toContain("actionError");
    expect(body).toContain("readGitHubContext<GitHubContext>(repo, { force: false })");
  });

  it("keeps what is on screen when the background read fails, and says so beside the stamp", () => {
    const body = source.slice(source.indexOf("async function refreshQuietly"), source.indexOf("/** A failed refresh over data already on screen"));
    expect(body).toContain("if (next.error) {");
    expect(body).toContain("Couldn't refresh GitHub in the background.");
    expect(source).toContain("{#if backgroundError}");
    // Only an explicit Refresh may revive a run poll that gave up.
    expect(body).not.toContain("livePoll");
  });

  it("drops a background answer for a repository that is no longer open", () => {
    const body = source.slice(source.indexOf("async function refreshQuietly"), source.indexOf("/** A failed refresh over data already on screen"));
    expect(body).toContain("if ($repoStore.currentPath !== repo || loading) return;");
  });
});

describe("GitHubPanel's issue, outgoing and release additions", () => {
  it("adds no whole-store array read of its own", () => {
    // repoStoreEffects.contract budgets GitHubPanel at two; these were already there.
    expect(source.match(/\$repoStore\.branches\.find/g)).toHaveLength(2);
  });

  it("mounts the three cards where the design puts them", () => {
    expect(source).toContain("<OutgoingCommitReview />");
    expect(source).toContain("<IssueTaskActions issues={ctx.issues} onReported={() => void loadAll()}>");
    expect(source).toContain("runs={runs}");
  });
});

describe("GitHubPanel live-poll lifecycle contracts", () => {
  it("drops a poll whose repository is no longer the open one", () => {
    // A resolve can land after the path moved; applying it would show one
    // repository's runs under another's name.
    expect(source).toContain("if ($repoStore.currentPath !== repo) return false;");
  });

  it("creates the driver before the effect that feeds it, and tears it down per repository", () => {
    // Svelte runs effects in declaration order. If the sync effect ran first,
    // its one call would hit a null driver and no-op — and on a repository
    // whose runs arrive already in flight from the panel cache, `runs` never
    // changes again, so nothing would ever start the poll.
    const driverAt = source.indexOf("createLivePoll({");
    const syncAt = source.indexOf("livePoll?.sync(moving)");
    expect(driverAt).toBeGreaterThan(0);
    expect(syncAt).toBeGreaterThan(0);
    expect(driverAt, "the driver effect must be declared first").toBeLessThan(syncAt);
    expect(source).toContain("driver.dispose()");
  });
});
