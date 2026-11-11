import { readFileSync } from "node:fs";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { compile } from "svelte/compiler";
import { render } from "svelte/server";
import type { GitHubContext } from "../github/types";

// The glance's wiring: which store fact lands in which chip. The chip
// arithmetic itself is ui/viewBarGlance.test.ts; the live row (placement, the
// switcher still whole beside the chips) is harness/tab-switch.html.
interface RepoFacts {
  currentPath: string | null;
  currentBranch: string | null;
  stashEntries: unknown[];
  stashTruncated: boolean;
  stashFailed: boolean;
  fetchedAt: number | null;
}
const { repo, plane } = await vi.hoisted(async () => {
  const { writable } = await import("svelte/store");
  const facts: RepoFacts = {
    currentPath: "/r/glance", currentBranch: "feat/cache", stashEntries: [], stashTruncated: false, stashFailed: false, fetchedAt: null,
  };
  const probes: { probes: unknown[] } = { probes: [] };
  return { repo: writable(facts), plane: writable(probes) };
});

vi.mock("../stores/repoStore", () => ({
  repoStore: { subscribe: repo.subscribe, setActiveTab: vi.fn(), fetch: vi.fn() },
}));
vi.mock("../agents/headerPlane", () => ({ headerAgentPlane: { subscribe: plane.subscribe } }));

const { default: ViewBarGlance } = await import("./ViewBarGlance.svelte");
const { notePullRequests } = await import("../github/pullRequestReads");

const source = readFileSync(new URL("./ViewBarGlance.svelte", import.meta.url), "utf8");
const glance = () => render(ViewBarGlance, { props: { onOpenAgents: () => {} } }).body;
const chip = (body: string, name: string) => new RegExp(`<(button|span)\\b[^>]*data-glance="${name}"[^>]*>`).exec(body)?.[0] ?? "";

function context(prs: GitHubContext["pull_requests"]): GitHubContext {
  return {
    available: true, owner: "o", repo: "r", workflow_runs: [], releases: [], cli_present: true, host: "github.com",
    html_url: "", pull_requests: prs, issues: [], issues_truncated: false,
  };
}

beforeEach(() => {
  repo.update((s) => ({ ...s, currentPath: "/r/glance", currentBranch: "feat/cache", stashEntries: [], stashTruncated: false, stashFailed: false, fetchedAt: null }));
  plane.set({ probes: [] });
});

describe("ViewBarGlance", () => {
  it("draws nothing when nothing has been read: no chip says zero, and the empty strip is not displayed", () => {
    const body = glance();
    expect(body).not.toContain('data-glance="');
    expect(/<div\b[^>]*data-glance-strip[^>]*>/.exec(body)?.[0]).toMatch(/class="[^"]*\bhidden\b/);
  });

  it("orders the chips by priority, so the ones the strip hides first are last", () => {
    const at = (name: string) => source.indexOf(`data-glance="${name}"`);
    expect(at("overlap")).toBeLessThan(at("pull-request"));
    expect(at("pull-request")).toBeLessThan(at("fetch"));
    expect(at("fetch")).toBeLessThan(at("stash"));
    // Hidden whole and out of the tab order, never clipped mid-chip.
    expect(source).toContain("chip.inert = out;");
    expect(source).toContain("shrink-[1000]");
  });

  it("shows the stash count with its sentence for screen readers, and the fetch age as a button that fetches", () => {
    repo.update((s) => ({ ...s, stashEntries: [{}, {}], fetchedAt: Date.now() - 3 * 3_600_000 }));
    const body = glance();
    expect(chip(body, "stash")).toMatch(/^<span/);
    expect(body).toContain("2 stash entries");
    expect(chip(body, "fetch")).toMatch(/^<button/);
    expect(chip(body, "fetch")).toContain('aria-label="Last fetched 3h ago. Fetch now"');
    expect(source).toContain("onclick={() => void repoStore.fetch()}");
  });

  it("shows the branch's pull request from a panel's earlier read, and none for another branch or repository", () => {
    notePullRequests("/r/glance", context([{
      number: 9, title: "Cache", state: "OPEN", head_ref: "feat/cache", base_ref: "main", url: "u", is_draft: false,
      ci_status: "failure", created_at: "", updated_at: "", review_decision: "", first_review_at: "",
    }]));
    const tag = chip(glance(), "pull-request");
    expect(tag).toContain('data-tone="danger"');
    expect(tag).toContain("checks failing");
    expect(glance()).toContain("#9");
    repo.update((s) => ({ ...s, currentBranch: "main" }));
    expect(glance()).not.toContain('data-glance="pull-request"');
    repo.update((s) => ({ ...s, currentBranch: "feat/cache", currentPath: "/r/elsewhere" }));
    expect(glance()).not.toContain('data-glance="pull-request"');
    expect(source).toContain('repoStore.setActiveTab("work", "remote")');
  });

  it("shows overlapping files from the snapshot the Agents sweep already took, opening Agents", () => {
    plane.set({
      probes: [{
        path: "/r/glance", label: "glance", error: "", skipped: false,
        snapshot: {
          worktrees: { items: [] },
          collisions: {
            ok: true, error: "", overlapping_files: 3, worktrees_involved: 2, scanned_worktrees: 2, unscanned_worktrees: 0,
            failed_worktrees: 0, truncated: false, items: [], shared_worktree_files: 0, shared_worktrees: [], sessions_ok: true, sessions_error: "",
          },
        },
      }],
    });
    const tag = chip(glance(), "overlap");
    expect(tag).toContain('data-tone="warn"');
    expect(tag).toContain("3 files changed in more than one worktree");
    expect(source).toContain("onclick={onOpenAgents}");
  });

  it("ticks its clock once a minute and stops it on unmount", () => {
    expect(source).toContain("setInterval(() => (now = Date.now()), 60_000)");
    expect(source).toContain("clearInterval(timer)");
  });

  it("has no accessibility compiler warnings", () => {
    const { warnings } = compile(source, { generate: "client" });
    expect(warnings.filter(({ code }) => code.startsWith("a11y_"))).toEqual([]);
  });
});
