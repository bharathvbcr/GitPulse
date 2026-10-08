import { describe, expect, it } from "vitest";
import { describeCheckout, sessionRow } from "./checkoutLabel";

const opts = { caseInsensitive: true };
const TABS = [
  { path: "/work/app", label: "app", familyRoot: "/work/app" },
  { path: "/work/app/.claude/worktrees/fix-importer-8540d4", label: "fix-importer-8540d4", familyRoot: "/work/app" },
  { path: "/work/linked-checkout", label: "linked-checkout", familyRoot: "/work/app" },
  { path: "/a/api", label: "a/api", familyRoot: "/a/api" },
  { path: "/b/api.git", label: "b/api.git", familyRoot: "/b/api.git" },
];

describe("describeCheckout", () => {
  it("names the repository once for its primary checkout", () => {
    expect(describeCheckout("/work/app", TABS, opts)).toEqual({ repository: "app", checkout: null, primary: true, worktreeAgent: "" });
  });

  it("names an agent worktree by its session slug, under its repository, with the agent apart", () => {
    expect(describeCheckout("/Work/App/.claude/worktrees/fix-importer-8540d4/", TABS, opts)).toEqual({
      repository: "app", checkout: "fix-importer-8540d4", primary: false, worktreeAgent: "claude",
    });
  });

  it("names a hand-made linked worktree by its tab label, under its repository", () => {
    expect(describeCheckout("/work/linked-checkout", TABS, opts)).toMatchObject({ repository: "app", checkout: "linked-checkout", primary: false, worktreeAgent: "" });
  });

  it("trims a bare repository's .git the way the strip does", () => {
    expect(describeCheckout("/b/api.git", TABS, opts)).toMatchObject({ repository: "api", checkout: null, primary: true });
  });

  it("still finds the repository of an agent worktree with no open tab, from its layout", () => {
    expect(describeCheckout("/work/svc/.gitpulse/worktrees/fix-1-a1b2c3d4", [], opts)).toEqual({
      repository: "svc", checkout: "fix-1-a1b2c3d4", primary: false, worktreeAgent: "gitpulse",
    });
  });

  it("does not invent a repository for a plain directory with no open tab", () => {
    // Unknown is not "primary": nothing read says this is the repository root.
    expect(describeCheckout("/elsewhere/tool", [], opts)).toEqual({ repository: "tool", checkout: null, primary: false, worktreeAgent: "" });
  });

  it("keeps case apart on a case-sensitive volume", () => {
    expect(describeCheckout("/WORK/APP", TABS, { caseInsensitive: false })).toMatchObject({ repository: "APP", primary: false });
  });
});

describe("sessionRow", () => {
  it("shows the repository, the checkout and the agent, and knows its own panel by identity", () => {
    const row = sessionRow(
      { repoPath: "/work/app/.claude/worktrees/fix-importer-8540d4", launcher: "claude", taskRunId: "run-1" },
      "/WORK/app/.claude/worktrees/fix-importer-8540d4/",
      TABS,
      opts,
    );
    expect(row).toMatchObject({ repository: "app", checkout: "fix-importer-8540d4", agent: "Claude", here: true, taskRunId: "run-1" });
  });

  it("has no agent chip for a shell, and links a resumed conversation to the attempt it continues", () => {
    const row = sessionRow({ repoPath: "/work/app", launcher: "shell", continuesRunId: "run-9" }, "/a/api", TABS, opts);
    expect(row).toMatchObject({ agent: null, here: false, taskRunId: "run-9" });
    expect(sessionRow({ repoPath: "/work/app" }, null, TABS, opts)).toMatchObject({ agent: null, here: false, taskRunId: null });
  });

  it("names a hosted agent by the worktree it runs in, while its panel stays the host's", () => {
    // Hosted in the repository's own tab (taskLaunches.hostTabFor), the
    // record's repoPath is that tab. Named by it, every worktree agent of a
    // repository read as the repository itself and the worktree was lost.
    const row = sessionRow(
      { repoPath: "/work/app", checkout: "/work/app/.gitpulse/worktrees/fix-2-b2c3d4e5", launcher: "claude", taskRunId: "run-2" },
      "/work/app",
      TABS,
      opts,
    );
    expect(row).toMatchObject({ repository: "app", checkout: "fix-2-b2c3d4e5", worktreeAgent: "gitpulse", agent: "Claude", here: true, taskRunId: "run-2" });
  });
});
