import { readFileSync } from "node:fs";
import { beforeEach, describe, expect, it, vi } from "vitest";

const calls: string[] = [];
let answer: () => unknown = () => ({ available: true, owner: "o", repo: "r", workflow_runs: [], releases: [] });

vi.mock("./invoke", () => ({
  invoke: async (command: string) => {
    calls.push(command);
    if (command === "cmd_github_context") return answer();
    throw new Error(`no ${command} in this test`);
  },
}));

import { loadWork } from "../work/load";
import { createWorkRefresh } from "../work/refresh";
import {
  GITHUB_CONTEXT_FRESH_MS,
  githubContextDegraded,
  invalidateGitHubContext,
  readGitHubContext,
  sharedGitHubContext,
} from "./githubContext";

const clean = { available: true, owner: "o", repo: "r", workflow_runs: [], releases: [] };

beforeEach(() => {
  calls.length = 0;
  invalidateGitHubContext();
  answer = () => clean;
});

describe("shared GitHub context read", () => {
  it("three consumers asking together reach the backend once and get the same payload", async () => {
    let reads = 0;
    const read = async () => (reads += 1, clean);
    const [a, b, c] = await Promise.all([
      sharedGitHubContext("/r", read),
      sharedGitHubContext("/r", read),
      sharedGitHubContext("/r", read),
    ]);
    expect(reads).toBe(1);
    expect(a).toBe(b);
    expect(b).toBe(c);
  });

  it("serves a clean answer inside the freshness window and reads again after it", async () => {
    let clock = 1_000;
    let reads = 0;
    const read = async () => (reads += 1, clean);
    await sharedGitHubContext("/r", read, { now: () => clock });
    clock += GITHUB_CONTEXT_FRESH_MS - 1;
    await sharedGitHubContext("/r", read, { now: () => clock });
    expect(reads).toBe(1);
    clock += 2;
    await sharedGitHubContext("/r", read, { now: () => clock });
    expect(reads).toBe(2);
  });

  it("keeps repositories apart, and force or invalidate reads again", async () => {
    let reads = 0;
    const read = async () => (reads += 1, clean);
    await sharedGitHubContext("/a", read);
    await sharedGitHubContext("/b", read);
    expect(reads).toBe(2);
    await sharedGitHubContext("/a", read, { force: true });
    expect(reads).toBe(3);
    invalidateGitHubContext("/a");
    await sharedGitHubContext("/a", read);
    expect(reads).toBe(4);
  });

  it("a failed read rejects every joiner and is not kept", async () => {
    let reads = 0;
    const read = async () => {
      reads += 1;
      throw new Error("gh not authenticated");
    };
    const results = await Promise.allSettled([sharedGitHubContext("/r", read), sharedGitHubContext("/r", read)]);
    expect(results.map((r) => r.status)).toEqual(["rejected", "rejected"]);
    await expect(sharedGitHubContext("/r", read)).rejects.toThrow("gh not authenticated");
    expect(reads).toBe(2);
  });

  it("a partial payload reaches every joiner with its degradation intact and is not kept", async () => {
    const partial = { ...clean, releases_error: "rate limited", warnings: ["PR listing failed to parse"] };
    let reads = 0;
    const read = async () => (reads += 1, partial);
    const [a, b] = await Promise.all([sharedGitHubContext("/r", read), sharedGitHubContext("/r", read)]);
    expect(a).toBe(partial);
    expect(b.releases_error).toBe("rate limited");
    await sharedGitHubContext("/r", read);
    expect(reads).toBe(2);
  });

  it("names what counts as degraded", () => {
    expect(githubContextDegraded(clean)).toBe(false);
    expect(githubContextDegraded({ ...clean, error: "boom" })).toBe(true);
    expect(githubContextDegraded({ ...clean, runs_error: "x" })).toBe(true);
    expect(githubContextDegraded({ ...clean, warnings: ["w"] })).toBe(true);
    expect(githubContextDegraded({ ...clean, releases_error: null, warnings: [] })).toBe(false);
    expect(githubContextDegraded(null)).toBe(true);
  });
});

describe("Work and GitHubPanel share one read", () => {
  // The panels cannot be mounted here (no DOM in vitest), so each consumer is
  // exercised the way it calls the owner: Work through `loadWork`, GitHubPanel
  // through `readGitHubContext` with the `force` its read sites pass (mount and
  // the once-a-minute background read: false; Refresh and post-action reloads:
  // true). GitHubPanel also carries the issue monitor ManviOpsPanel used to be.
  const githubCalls = () => calls.filter((c) => c === "cmd_github_context").length;

  it("all three asking inside the window invoke the command once", async () => {
    await Promise.all([
      loadWork("/repo"),
      readGitHubContext("/repo", { force: false }),
      readGitHubContext("/repo", { force: false }),
    ]);
    expect(githubCalls()).toBe(1);
    await loadWork("/repo");
    await readGitHubContext("/repo", { force: false });
    expect(githubCalls()).toBe(1);
  });

  it("an explicit refresh in a panel forces a new read, and the next reuse sees it", async () => {
    await readGitHubContext("/repo", { force: false });
    await readGitHubContext("/repo", { force: true });
    expect(githubCalls()).toBe(2);
    await loadWork("/repo");
    expect(githubCalls()).toBe(2);
  });

  it("a failed read fails for each of the three (Work, the mount read, the background read), and none of them gets an empty success", async () => {
    answer = () => {
      throw new Error("gh not authenticated");
    };
    const [work, panel, background] = await Promise.allSettled([
      loadWork("/repo"),
      readGitHubContext("/repo", { force: false }),
      readGitHubContext("/repo", { force: false }),
    ]);
    expect(work.status).toBe("fulfilled");
    if (work.status === "fulfilled") {
      expect(work.value.sources.github.ok).toBe(false);
      expect(work.value.sources.github.detail).toContain("gh not authenticated");
    }
    expect(panel.status).toBe("rejected");
    expect(background.status).toBe("rejected");
    expect(githubCalls()).toBe(1);
  });

  it("a partial read reaches each consumer with its degradation intact, and the next ask reads again", async () => {
    const partial = { ...clean, releases_error: "rate limited", warnings: ["PR listing failed to parse"] };
    answer = () => partial;
    const [work, panel, background] = await Promise.all([
      loadWork("/repo"),
      readGitHubContext<typeof partial>("/repo", { force: false }),
      readGitHubContext<typeof partial>("/repo", { force: false }),
    ]);
    expect(panel).toBe(partial);
    expect(background.releases_error).toBe("rate limited");
    expect(background.warnings).toEqual(["PR listing failed to parse"]);
    expect(work.sources.github.detail).not.toBe("");
    expect(githubCalls()).toBe(1);
    await readGitHubContext("/repo", { force: false });
    expect(githubCalls()).toBe(2);
  });
});

describe("Work's Refresh button", () => {
  const githubCalls = () => calls.filter((c) => c === "cmd_github_context").length;

  function refresher() {
    const refresh = createWorkRefresh({
      load: loadWork,
      collisions: async () => {
        throw new Error("collision check not part of this test");
      },
    });
    return (options?: { force?: boolean }) =>
      new Promise<void>((resolve) => {
        refresh.request("/repo", { projection() {}, collisions() {}, finished: () => resolve() }, options);
      });
  }

  it("a refresh the person asked for reads GitHub again inside the freshness window", async () => {
    const request = refresher();
    await request();
    expect(githubCalls()).toBe(1);
    await request(); // a mount, a return to the view, a status-driven reload
    expect(githubCalls()).toBe(1);
    await request({ force: true }); // the Refresh button
    expect(githubCalls()).toBe(2);
  });

  it("WorkView's button asks for a forced refresh and nothing else does", () => {
    const view = readFileSync(new URL("../components/WorkView.svelte", import.meta.url), "utf8");
    expect(view.match(/refresh\(\s*\$repoStore\.currentPath\s*,\s*true\s*\)/g)).toHaveLength(1);
    expect(view).toMatch(/function refresh\(\s*repo: string,\s*force = false\s*\)/);
  });

  it("a forced request replaced by an ordinary one for the same repository stays forced", async () => {
    const request = refresher();
    await request();
    const first = request({ force: true });
    const second = request();
    await Promise.all([first, second]);
    expect(githubCalls()).toBe(2);
  });
});

describe("the panels read through the owner", () => {
  const panel = readFileSync(new URL("../components/GitHubPanel.svelte", import.meta.url), "utf8");
  const issues = readFileSync(new URL("../components/IssueTaskActions.svelte", import.meta.url), "utf8");

  it("neither asks the backend for the context itself", () => {
    expect(panel).not.toContain('"cmd_github_context"');
    // The issue block's task wiring takes the pane's list; it never reads GitHub.
    expect(issues).not.toContain('"cmd_github_context"');
    expect(issues).not.toContain("readGitHubContext");
    expect(panel).toMatch(/readGitHubContext<GitHubContext>\(\s*repo\s*,\s*\{\s*force\s*\}\s*\)/);
  });

  it("only the mount read and the once-a-minute background read may reuse; every refresh forces", () => {
    expect(panel).toMatch(/async function loadFor\(\s*repo: string,\s*force = true\s*\)/);
    expect(panel.match(/loadFor\(\s*repo\s*,\s*false\s*\)/g)).toHaveLength(1);
    // The background read is the issue monitor ManviOpsPanel used to run, folded into this one path.
    expect(panel.match(/readGitHubContext<GitHubContext>\(\s*repo\s*,\s*\{\s*force:\s*false\s*\}\s*\)/g)).toHaveLength(1);
  });
});

describe("Work's GitHub read goes through the shared owner", () => {
  it("returning to Work within the window reaches the backend zero more times", async () => {
    await loadWork("/repo");
    await loadWork("/repo");
    expect(calls.filter((c) => c === "cmd_github_context")).toHaveLength(1);
  });

  it("two Work loads at once reach the backend once", async () => {
    await Promise.all([loadWork("/repo"), loadWork("/repo")]);
    expect(calls.filter((c) => c === "cmd_github_context")).toHaveLength(1);
  });

  it("a failed GitHub read still shows as a failed source, never an empty one", async () => {
    answer = () => {
      throw new Error("gh exploded");
    };
    const first = await loadWork("/repo");
    const second = await loadWork("/repo");
    for (const projection of [first, second]) {
      expect(projection.sources.github.ok).toBe(false);
      expect(projection.sources.github.detail).toContain("gh exploded");
    }
    expect(calls.filter((c) => c === "cmd_github_context")).toHaveLength(2);
  });
});
