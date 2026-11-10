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
import {
  GITHUB_CONTEXT_FRESH_MS,
  githubContextDegraded,
  invalidateGitHubContext,
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
