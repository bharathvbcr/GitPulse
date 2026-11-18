import { describe, expect, it } from "vitest";
import {
  NO_FOLLOW_TARGET,
  adoptFollowPane,
  decideFollow,
  followPaneOpen,
  type FollowPaneCandidate,
  type FollowTarget,
} from "./followAgent";

const agent: FollowPaneCandidate = { id: "agent", launcher: "claude" };
const filePane = (id: string, filePath?: string): FollowPaneCandidate => ({ id, launcher: "file", filePath });

describe("decideFollow", () => {
  it("opens one pane when follow owns none yet", () => {
    expect(decideFollow(NO_FOLLOW_TARGET, [agent])).toEqual({ kind: "open" });
  });

  it("retargets the pane follow owns while it still shows follow's file", () => {
    const target: FollowTarget = { paneId: "f1", filePath: "src/a.ts" };
    expect(decideFollow(target, [agent, filePane("f1", "src/a.ts")])).toEqual({
      kind: "retarget",
      paneId: "f1",
    });
  });

  it("retargets a preset's empty file pane that follow was handed", () => {
    const target = adoptFollowPane(filePane("f1"));
    expect(target).toEqual({ paneId: "f1", filePath: null });
    expect(decideFollow(target, [agent, filePane("f1")])).toEqual({ kind: "retarget", paneId: "f1" });
  });

  it("stops once the user keeps follow's pane on its file (a double-click)", () => {
    const target: FollowTarget = { paneId: "f1", filePath: "src/a.ts" };
    expect(decideFollow(target, [agent, { ...filePane("f1", "src/a.ts"), kept: true }])).toEqual({
      kind: "stop",
      reason: "taken",
    });
  });

  it("stops, and opens nothing, once the user closed follow's pane", () => {
    const target: FollowTarget = { paneId: "f1", filePath: "src/a.ts" };
    expect(decideFollow(target, [agent])).toEqual({ kind: "stop", reason: "closed" });
  });

  it("stops when the user pointed follow's pane at a file of their own", () => {
    const target: FollowTarget = { paneId: "f1", filePath: "src/a.ts" };
    expect(decideFollow(target, [agent, filePane("f1", "README.md")])).toEqual({
      kind: "stop",
      reason: "taken",
    });
  });

  it("stops when the user opened a file in a preset's still-empty pane", () => {
    const target = adoptFollowPane(filePane("f1"));
    expect(decideFollow(target, [agent, filePane("f1", "notes.md")])).toEqual({
      kind: "stop",
      reason: "taken",
    });
  });

  it("never drives another visible file pane the user opened", () => {
    // The user's pane f2 is visible beside the agent; follow owns f1 only.
    const target: FollowTarget = { paneId: "f1", filePath: "src/a.ts" };
    const decision = decideFollow(target, [agent, filePane("f1", "src/a.ts"), filePane("f2", "mine.ts")]);
    expect(decision).toEqual({ kind: "retarget", paneId: "f1" });
  });

  it("keeps a plan snippet pane follow opened, but not an id reused by another kind", () => {
    const target: FollowTarget = { paneId: "f1", filePath: null };
    expect(decideFollow(target, [{ id: "f1", launcher: "file", snippet: { title: "t", content: "c" } }])).toEqual({
      kind: "retarget",
      paneId: "f1",
    });
    expect(decideFollow(target, [{ id: "f1", launcher: "shell" }]).kind).toBe("stop");
  });

  it("compares paths through the panel's normalizer", () => {
    const target: FollowTarget = { paneId: "f1", filePath: "src/a.ts" };
    const normalize = (tab: FollowPaneCandidate) => tab.filePath?.replace(/^\.\//, "") ?? null;
    expect(decideFollow(target, [filePane("f1", "./src/a.ts")], normalize)).toEqual({
      kind: "retarget",
      paneId: "f1",
    });
  });
});

describe("the reported sequence: open a file beside a running agent, close it, agent keeps editing", () => {
  it("opens at most one pane on its own, and nothing after the user closes it", () => {
    let target: FollowTarget = NO_FOLLOW_TARGET;
    let tabs: FollowPaneCandidate[] = [agent];
    let opened = 0;
    let next = 0;
    let following = true;
    // Mirrors the panel: an edit does nothing once follow is off.
    const touch = (path: string) => {
      if (!following) return "ignored";
      const decision = decideFollow(target, tabs);
      if (decision.kind === "stop") {
        target = NO_FOLLOW_TARGET;
        following = false;
        return "stopped";
      }
      if (decision.kind === "open") {
        const pane = filePane(`f${++next}`, path);
        tabs = [...tabs, pane];
        opened += 1;
        target = adoptFollowPane(pane);
        return "opened";
      }
      tabs = tabs.map((tab) => (tab.id === decision.paneId ? { ...tab, filePath: path } : tab));
      target = { paneId: decision.paneId, filePath: path };
      return "retargeted";
    };

    expect(touch("src/a.ts")).toBe("opened");
    expect(touch("src/b.ts")).toBe("retargeted");
    // The user opens their own file in that pane from the sidebar…
    tabs = tabs.map((tab) => (tab.id === "f1" ? { ...tab, filePath: "README.md" } : tab));
    // (still open, so the close watcher leaves it to the next edit)
    expect(followPaneOpen(target, tabs)).toBe(true);
    // …and the agent's next edit must not replace it.
    expect(touch("src/c.ts")).toBe("stopped");
    expect(tabs.find((tab) => tab.id === "f1")?.filePath).toBe("README.md");
    // The user closes it; the agent keeps editing; nothing opens.
    tabs = tabs.filter((tab) => tab.id !== "f1");
    for (const path of ["src/d.ts", "src/e.ts", "src/f.ts"]) {
      expect(touch(path)).toBe("ignored");
    }
    expect(opened).toBe(1);
    expect(tabs.filter((tab) => tab.launcher === "file")).toEqual([]);
  });

  it("closing follow's pane while it still shows follow's file also stops, before any takeover", () => {
    // Follow on, one edit opened f1, the user closes f1 directly: the next
    // edit must stop, not take the "open" branch and pop a new split.
    const target: FollowTarget = { paneId: "f1", filePath: "src/a.ts" };
    expect(decideFollow(target, [agent])).toEqual({ kind: "stop", reason: "closed" });
  });

  it("closing follow's own pane ends follow rather than reopening it", () => {
    let target = adoptFollowPane(filePane("f1", "src/a.ts"));
    let tabs: FollowPaneCandidate[] = [agent, filePane("f1", "src/a.ts")];
    tabs = tabs.filter((tab) => tab.id !== "f1");
    expect(followPaneOpen(target, tabs)).toBe(false);
    expect(decideFollow(target, tabs)).toEqual({ kind: "stop", reason: "closed" });
    target = NO_FOLLOW_TARGET;
    expect(target.paneId).toBeNull();
  });
});

describe("adoptFollowPane", () => {
  it("adopts only a file pane", () => {
    expect(adoptFollowPane(undefined)).toEqual(NO_FOLLOW_TARGET);
    expect(adoptFollowPane(agent)).toEqual(NO_FOLLOW_TARGET);
    expect(adoptFollowPane({ id: "s", launcher: "file", snippet: {} })).toEqual({ paneId: "s", filePath: null });
    expect(adoptFollowPane(filePane("f1", "x.ts"))).toEqual({ paneId: "f1", filePath: "x.ts" });
  });

  it("an empty target is always open", () => {
    expect(followPaneOpen(NO_FOLLOW_TARGET, [])).toBe(true);
  });
});
