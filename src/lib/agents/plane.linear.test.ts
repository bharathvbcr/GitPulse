import { beforeEach, describe, expect, it, vi } from "vitest";

/**
 * The projection's work, counted rather than timed: every path comparison
 * goes through `identityKey`, so the number of calls is the number of
 * comparisons. A linear scan per terminal and per task makes it grow with
 * the square of the workspace; a map keyed by path keeps it proportional.
 */
const calls = vi.hoisted(() => ({ count: 0 }));

vi.mock("../repos/paths", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../repos/paths")>();
  return {
    ...actual,
    identityKey: (...args: Parameters<typeof actual.identityKey>) => {
      calls.count += 1;
      return actual.identityKey(...args);
    },
  };
});

const { projectAgentPlane } = await import("./plane");
const { listing, PATHS, probe, snapshot, task, tasks, terminal, worktree } = await import("./testFixtures");

function workspace(size: number) {
  const items = Array.from({ length: size }, (_, index) =>
    worktree({ path: `/repo/.claude/worktrees/s${index}`, session_slug: `s${index}`, name: `s${index}` }));
  return {
    probes: [probe({ snapshot: snapshot({ worktrees: listing(items) }) })],
    terminals: Array.from({ length: size }, (_, index) =>
      terminal({ key: `t${index}`, sessionId: `p${index}`, cwd: `/repo/.claude/worktrees/s${index}/src` })),
    tasks: tasks({
      tasks: Array.from({ length: size }, (_, index) =>
        task({ runId: `run-${index}`, cwd: `/repo/.claude/worktrees/s${index}`, tone: "needs-you" })),
    }),
    paths: PATHS,
  };
}

function cost(size: number): number {
  const input = workspace(size);
  calls.count = 0;
  const plane = projectAgentPlane(input);
  expect(plane.total).toBeGreaterThanOrEqual(size);
  return calls.count;
}

describe("projectAgentPlane cost", () => {
  beforeEach(() => {
    calls.count = 0;
  });

  it("grows in proportion to the workspace, not with its square", () => {
    const small = cost(300);
    const large = cost(1200);
    expect(large / small).toBeLessThan(6);
    expect(large).toBeLessThan(1200 * 40);
  });
});
