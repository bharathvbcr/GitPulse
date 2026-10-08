import { afterEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  awaitedTabIds, consumeTaskTerminal, consumeTaskTerminalRequest, enqueueTaskTerminal, heldElsewhere, holdTaskTerminal, hostTabFor, launchFor,
  pruneTaskTerminals, releaseTaskTerminal, requestFor, taskTerminalHolds, taskTerminalRequests, type HostCandidate, type TaskTerminalRequest,
} from "./taskLaunches";

const opts = { caseInsensitive: false };
const FAMILY = "/repo/.git";
const tab = (id: string, path: string, extra: Partial<HostCandidate> = {}): HostCandidate => ({ id, path, family: FAMILY, familyRoot: "/repo", ...extra });
const worktree = (name: string) => `/repo/.gitpulse/worktrees/${name}`;
const request = (runId: string, repoPath: string, family?: string): TaskTerminalRequest => ({
  runId, repoPath, provider: "claude", title: runId, ...(family ? { family } : {}),
});

describe("hostTabFor: which open tab's dock takes a task terminal", () => {
  it("is the checkout's own tab whenever that is open", () => {
    const tabs = [tab("main", "/repo"), tab("wt", worktree("a"))];
    expect(hostTabFor(request("r", worktree("a"), FAMILY), tabs, opts)?.id).toBe("wt");
  });

  it("keeps an untrusted own tab rather than slipping into a trusted sibling", () => {
    // Waiting for trust is the answer; another checkout must not run it.
    const tabs = [tab("main", "/repo"), tab("wt", worktree("a"), { trustRequired: true })];
    expect(hostTabFor(request("r", worktree("a"), FAMILY), tabs, opts)?.id).toBe("wt");
    expect(awaitedTabIds(tabs, [request("r", worktree("a"), FAMILY)], opts).size).toBe(0);
  });

  it("is the repository's own checkout for a worktree with no tab of its own", () => {
    const tabs = [tab("other", worktree("b")), tab("main", "/repo"), tab("x", "/elsewhere", { family: "/elsewhere/.git", familyRoot: "/elsewhere" })];
    expect(hostTabFor(request("r", worktree("a"), FAMILY), tabs, opts)?.id).toBe("main");
  });

  it("falls back to any trusted sibling still on disk when the repository's own checkout is not open", () => {
    const tabs = [
      tab("gone", worktree("b"), { missing: true }),
      tab("untrusted", worktree("c"), { trustRequired: true }),
      tab("sibling", worktree("d")),
    ];
    expect(hostTabFor(request("r", worktree("a"), FAMILY), tabs, opts)?.id).toBe("sibling");
  });

  it("is nothing without a family, or with no open checkout of it", () => {
    const tabs = [tab("main", "/repo")];
    expect(hostTabFor(request("r", worktree("a")), tabs, opts)).toBeUndefined();
    expect(hostTabFor(request("r", worktree("a"), "/other/.git"), tabs, opts)).toBeUndefined();
    expect(hostTabFor(request("r", "", FAMILY), tabs, opts)).toBeUndefined();
    expect(hostTabFor(request("r", worktree("a"), FAMILY), [], opts)).toBeUndefined();
  });

  it("never joins a tab whose family is unknown", () => {
    const tabs = [tab("main", "/repo", { family: null, familyRoot: null })];
    expect(hostTabFor(request("r", worktree("a"), FAMILY), tabs, opts)).toBeUndefined();
  });
});

describe("the dock and the panel agree on the host", () => {
  it("hosts exactly the tab the panel will take the request in, for many worktrees at once", () => {
    // Forty agents in forty worktrees of one repository with only its own
    // checkout open: one panel hosts all of them, and no other tab is asked.
    const tabs = [tab("main", "/repo"), tab("x", "/elsewhere", { family: "/elsewhere/.git", familyRoot: "/elsewhere" })];
    const requests = Array.from({ length: 40 }, (_, i) => request(`r${i}`, worktree(`w${i}`), FAMILY));
    expect([...awaitedTabIds(tabs, requests, opts)]).toEqual(["main"]);
    expect(requestFor(requests, "/repo", opts, tabs)?.runId).toBe("r0");
    expect(requestFor(requests, "/elsewhere", opts, tabs)).toBeUndefined();
    // Every request is taken by exactly one panel.
    for (const each of requests) {
      const takers = tabs.filter((candidate) => requestFor([each], candidate.path, opts, tabs));
      expect(takers.map((candidate) => candidate.id)).toEqual(["main"]);
    }
  });

  it("matches only by the request's own checkout when the panel is not told the tabs", () => {
    const requests = [request("r", worktree("a"), FAMILY)];
    expect(requestFor(requests, "/repo", opts)).toBeUndefined();
    expect(requestFor(requests, worktree("a"), opts)?.runId).toBe("r");
  });

  it("is case-blind on a case-insensitive volume, as tabs are", () => {
    const ci = { caseInsensitive: true };
    const tabs = [tab("main", "/Repo")];
    expect(requestFor([request("r", "/repo")], "/REPO", ci, tabs)?.runId).toBe("r");
  });
});

describe("a request held by the panel that opened a tab for it", () => {
  const a = Symbol("panel-a"), b = Symbol("panel-b");
  afterEach(() => {
    for (const each of get(taskTerminalRequests)) consumeTaskTerminalRequest(each);
    releaseTaskTerminal(a); releaseTaskTerminal(b);
  });

  it("is not taken by another panel while it waits for its slot, even when the host moves", () => {
    // The host is recomputed from the open tabs, so it can move mid-start;
    // without the hold the new host opened a second tab and launched twice.
    const req = request("r", worktree("a"), FAMILY);
    enqueueTaskTerminal(req);
    expect(holdTaskTerminal(req, a)).toBe(true);
    expect(holdTaskTerminal(req, a)).toBe(true);
    expect(holdTaskTerminal(req, b)).toBe(false);
    expect(heldElsewhere(req, b)).toBe(true);
    expect(heldElsewhere(req, a)).toBe(false);
  });

  it("is free again once released, consumed, withdrawn or pruned", () => {
    const req = request("r", worktree("a"), FAMILY);
    enqueueTaskTerminal(req);
    holdTaskTerminal(req, a);
    releaseTaskTerminal(a, req);
    expect(heldElsewhere(req, b)).toBe(false);

    holdTaskTerminal(req, a);
    consumeTaskTerminalRequest(req);
    expect(get(taskTerminalHolds).size).toBe(0);

    enqueueTaskTerminal(req); holdTaskTerminal(req, a);
    consumeTaskTerminal("r");
    expect(get(taskTerminalHolds).size).toBe(0);

    enqueueTaskTerminal(req); holdTaskTerminal(req, a);
    expect(pruneTaskTerminals([{ id: "r", state: "cancelled", expires_at: 0 }], Date.now())).toEqual(["r"]);
    expect(get(taskTerminalHolds).size).toBe(0);
  });

  it("is released wholesale when its panel goes, leaving other panels' holds alone", () => {
    const one = request("r1", worktree("a"), FAMILY), two = request("r2", worktree("b"), FAMILY);
    enqueueTaskTerminal(one); enqueueTaskTerminal(two);
    holdTaskTerminal(one, a); holdTaskTerminal(two, b);
    releaseTaskTerminal(a);
    expect(heldElsewhere(one, b)).toBe(false);
    expect(heldElsewhere(two, a)).toBe(true);
  });
});

describe("launchFor: where a hosted process runs", () => {
  it("names the checkout when the panel belongs to another", () => {
    expect(launchFor(request("r", worktree("a"), FAMILY), "/repo", opts)).toMatchObject({ checkout: worktree("a") });
  });
  it("leaves its own checkout's request untouched", () => {
    const own = request("r", "/repo", FAMILY);
    expect(launchFor(own, "/repo/", opts)).toBe(own);
  });
});
