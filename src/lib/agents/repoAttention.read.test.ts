import { readFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";
import { ensurePlaneRead, probeFor, repoAgentPill, resetPlaneReadRequests } from "./repoAttention";
import { PATHS, probe, project } from "./__tests__/fixtures";

const source = (relative: string) => readFileSync(new URL(relative, import.meta.url), "utf8");

describe("the unknown pill says why", () => {
  const unknown = (probes: ReturnType<typeof probe>[], scanning = false) =>
    repoAgentPill({ plane: project({ probes }), probes, repoPath: "/repo", paths: PATHS, scanning });

  it("names each cause, so Not read is never a shrug", () => {
    expect(unknown([]).why).toMatch(/has not covered this repository/);
    expect(unknown([], true)).toMatchObject({ state: "unknown", phrase: "Reading", why: expect.stringMatching(/running now/) });
    expect(unknown([probe({ skipped: true, skipReason: "deadline" })]).why).toMatch(/ran out of time/);
    expect(unknown([probe({ skipped: true, skipReason: "cap" })]).why).toMatch(/more are open than one read covers/);
    expect(unknown([probe({ error: "boom", snapshot: null })]).why).toBe("The agents read failed: boom");
  });

  it("is empty for every state that is known", () => {
    const probes = [probe()];
    const known = repoAgentPill({ plane: project({ probes }), probes, repoPath: "/repo", paths: PATHS });
    expect(known.state).toBe("none");
    expect(known.why).toBe("");
  });

  it("shows the cause in the pill's own tooltip", () => {
    expect(source("../components/ViewBar.svelte")).toContain("${agents.why}");
  });
});

describe("probeFor", () => {
  it("finds a repository's read by one of its worktrees, because a sweep reads each repository once", () => {
    const probes = [probe()];
    expect(probeFor(probes, "/repo", PATHS)).toBe(probes[0]);
    expect(probeFor(probes, "/repo/.claude/worktrees/alpha", PATHS)).toBe(probes[0]);
    expect(probeFor(probes, "/elsewhere", PATHS)).toBeUndefined();
  });
});

describe("ensurePlaneRead", () => {
  const targets = [{ path: "/repo", label: "Repo" }];
  const ask = (repoPath: string, state: { probes: ReturnType<typeof probe>[]; scanning: boolean }, store = { refresh: vi.fn(async () => {}) }) => ({
    started: ensurePlaneRead({ store, state, repoPath, targets, paths: PATHS }),
    store,
  });

  it("asks the shared sweep once for a repository no read has covered, and never again", () => {
    resetPlaneReadRequests();
    const first = ask("/repo", { probes: [], scanning: false });
    expect(first.started).toBe(true);
    expect(first.store.refresh).toHaveBeenCalledWith(targets);
    expect(ask("/repo", { probes: [], scanning: false }).started).toBe(false);
  });

  it("does nothing while a read is running, when one already covers the repository, or with nothing to read", () => {
    resetPlaneReadRequests();
    expect(ask("/repo", { probes: [], scanning: true }).started).toBe(false);
    expect(ask("/repo", { probes: [probe()], scanning: false }).started).toBe(false);
    expect(ask("", { probes: [], scanning: false }).started).toBe(false);
    expect(ensurePlaneRead({ store: { refresh: vi.fn(async () => {}) }, state: { probes: [], scanning: false }, repoPath: "/repo", targets: [], paths: PATHS })).toBe(false);
  });
});

describe("one mapping for terminals, one sweep", () => {
  it("has the Agents view call the header's mapping and row 2 ask the shared sweep", () => {
    const view = source("../components/AgentsView.svelte");
    expect(view).toContain("planeTerminals($terminalSessions, $sessionActivity, $agentDirectories)");
    expect(view).not.toContain("attention: activity?.attention?.kind");
    const bar = source("../components/ViewBar.svelte");
    expect(bar).toContain("ensurePlaneRead");
    // No poller of its own: the bar reads through the shared store.
    expect(bar).not.toMatch(/setInterval|createAdaptiveTimer/);
  });
});

describe("CloneModal copy", () => {
  it("says Clone repository in sentence case", () => {
    const modal = source("../components/CloneModal.svelte");
    expect(modal).toContain('"Clone repository"');
    expect(modal).not.toContain("Clone Repository");
  });
});
