import { describe, expect, it } from "vitest";
import {
  buildRestoredStrip,
  buildTabStrip,
  composeWorkspaceWithStrip,
  parsePersistedTabStrip,
  planReloadRestore,
  stripWorthWaitingFor,
  type AdoptedFact,
  type PersistedTabStrip,
} from "./reloadRestore";
import { parsePersistedSplitWorkspace, serializeSplitWorkspace, leaf, splitPane } from "./splitTree";
import { createTab, type TerminalTab } from "./tabs";

const REPO = "/work/repo";
const belongsHere = (fact: AdoptedFact, meta: { checkout?: string }) =>
  fact.repoPath === REPO || (meta.checkout !== undefined && fact.repoPath === meta.checkout);

function fact(sessionId: string, over: Partial<AdoptedFact> = {}): AdoptedFact {
  return { sessionId, repoPath: REPO, launcher: "shell", ...over };
}

function strip(entries: Array<{ id: string; session: string; launcher?: "shell" | "claude"; checkout?: string; parent?: string }>, activeId: string | null = null): PersistedTabStrip {
  const leaves: PersistedTabStrip["leaves"] = {};
  entries.forEach((e, i) => {
    leaves[e.id] = {
      launcher: e.launcher ?? "shell",
      slotIndex: i,
      backendSessionId: e.session,
      ...(e.checkout ? { checkout: e.checkout } : {}),
      ...(e.parent ? { parentSessionId: e.parent } : {}),
    };
  });
  return { activeId, order: entries.map((e) => e.id), leaves };
}

const plan = (s: PersistedTabStrip | null, available: AdoptedFact[], splitLeafIds: string[] = []) =>
  planReloadRestore({ strip: s, available, belongsHere, splitLeafIds: new Set(splitLeafIds), maxTabs: 32 });

describe("planReloadRestore", () => {
  it("restores every saved tab in order with the saved active tab", () => {
    const s = strip([{ id: "tab-1", session: "a" }, { id: "tab-2", session: "b", launcher: "claude" }, { id: "tab-3", session: "c" }], "tab-2");
    const p = plan(s, [fact("a"), fact("b", { launcher: "claude" }), fact("c")]);
    expect(p.tabs.map((t) => t.sessionId)).toEqual(["a", "b", "c"]);
    expect(p.activeOldId).toBe("tab-2");
    expect(p.tabs.map((t) => t.eager)).toEqual([false, true, false]);
  });

  it("drops a saved id the host no longer has, silently, and keeps the rest", () => {
    const p = plan(strip([{ id: "tab-1", session: "gone" }, { id: "tab-2", session: "b" }], "tab-1"), [fact("b")]);
    expect(p.tabs.map((t) => t.sessionId)).toEqual(["b"]);
    expect(p.dropped).toEqual(["tab-1"]);
    expect(p.activeOldId).toBe("tab-2");
  });

  it("restores nothing when every id is gone, so the caller opens its fresh shell", () => {
    const p = plan(strip([{ id: "tab-1", session: "x" }]), []);
    expect(p.tabs).toEqual([]);
    expect(p.activeOldId).toBeNull();
  });

  it("restores nothing without a strip", () => {
    expect(plan(null, [fact("a")]).tabs).toEqual([]);
  });

  it("never restores a session nobody had a tab for", () => {
    const p = plan(strip([{ id: "tab-1", session: "a" }]), [fact("a"), fact("background-task"), fact("agent-child")]);
    expect(p.tabs.map((t) => t.sessionId)).toEqual(["a"]);
  });

  it("leaves a session another dock showed to Sessions: wrong repository, no recorded checkout", () => {
    const p = plan(strip([{ id: "tab-1", session: "a" }, { id: "tab-2", session: "sib" }]), [fact("a"), fact("sib", { repoPath: "/work/sibling" })]);
    expect(p.tabs.map((t) => t.sessionId)).toEqual(["a"]);
    expect(p.dropped).toEqual(["tab-2"]);
  });

  it("restores a session this dock hosted in a recorded sibling checkout", () => {
    const p = plan(strip([{ id: "tab-1", session: "h", checkout: "/work/wt" }]), [fact("h", { repoPath: "/work/wt" })]);
    expect(p.tabs.map((t) => t.sessionId)).toEqual(["h"]);
  });

  it("restores a session once when the strip names it twice", () => {
    const s = strip([{ id: "tab-1", session: "a" }, { id: "tab-2", session: "a" }]);
    expect(plan(s, [fact("a")]).tabs).toHaveLength(1);
  });

  it("focuses the first restored tab when the saved active tab was dropped", () => {
    const p = plan(strip([{ id: "tab-1", session: "gone" }, { id: "tab-2", session: "b" }, { id: "tab-3", session: "c" }], "tab-1"), [fact("b"), fact("c")]);
    expect(p.activeOldId).toBe("tab-2");
  });

  it("attaches split leaves eagerly along with the active tab; the rest wait", () => {
    const s = strip([{ id: "tab-1", session: "a" }, { id: "tab-2", session: "b" }, { id: "tab-3", session: "c" }, { id: "tab-4", session: "d" }], "tab-1");
    const p = plan(s, [fact("a"), fact("b"), fact("c"), fact("d")], ["tab-3", "tab-4"]);
    expect(p.tabs.map((t) => [t.oldId, t.eager])).toEqual([
      ["tab-1", true],
      ["tab-2", false],
      ["tab-3", true],
      ["tab-4", true],
    ]);
  });

  it("bounds the restored strip", () => {
    const entries = Array.from({ length: 10 }, (_, i) => ({ id: `tab-${i}`, session: `s${i}` }));
    const p = planReloadRestore({
      strip: strip(entries),
      available: entries.map((e) => fact(e.session)),
      belongsHere,
      splitLeafIds: new Set(),
      maxTabs: 4,
    });
    expect(p.tabs).toHaveLength(4);
    expect(p.dropped).toHaveLength(6);
  });

  it("drops entries with no backend id or a non-terminal launcher", () => {
    const s = strip([{ id: "tab-1", session: "a" }]);
    (s.leaves as Record<string, unknown>)["tab-2"] = { launcher: "browser", slotIndex: 1, backendSessionId: "b" };
    (s.leaves as Record<string, unknown>)["tab-3"] = { launcher: "shell", slotIndex: 2 };
    const p = plan({ ...s, order: ["tab-1", "tab-2", "tab-3"] }, [fact("a"), fact("b")]);
    expect(p.tabs.map((t) => t.sessionId)).toEqual(["a"]);
  });
});

describe("buildRestoredStrip", () => {
  it("builds attach tabs in order, focuses the saved tab, and defers background ones", () => {
    const s = strip([{ id: "tab-1", session: "a" }, { id: "tab-2", session: "b", launcher: "claude" }, { id: "tab-3", session: "c" }], "tab-2");
    const built = buildRestoredStrip(plan(s, [fact("a", { title: "build" }), fact("b", { launcher: "claude" }), fact("c")]));
    expect(built.state.tabs.map((t) => t.attachSessionId)).toEqual(["a", "b", "c"]);
    expect(built.state.tabs.map((t) => t.launcher)).toEqual(["shell", "claude", "shell"]);
    expect(built.state.activeId).toBe(built.idMap.get("tab-2"));
    expect([...built.deferred].sort()).toEqual([built.idMap.get("tab-1"), built.idMap.get("tab-3")].sort() as string[]);
    expect(built.state.tabs[0].title).toBe("build");
  });

  it("an empty plan builds no tabs", () => {
    const built = buildRestoredStrip(plan(null, []));
    expect(built.state.tabs).toEqual([]);
    expect(built.state.activeId).toBeNull();
  });
});

describe("strip persistence", () => {
  function tab(launcher: TerminalTab["launcher"]): TerminalTab {
    return createTab(launcher);
  }

  it("saves terminal tabs with a live backend id, in order, and skips ended, id-less and browser tabs", () => {
    const [a, b, c, d, e] = [tab("shell"), tab("claude"), tab("shell"), tab("browser"), tab("shell")];
    const ids: Record<string, string> = { [a.id]: "sa", [b.id]: "sb", [c.id]: "sc", [d.id]: "sd" };
    const saved = buildTabStrip([a, b, c, d, e], b.id, (t) => ids[t.id], (t) => t.id === c.id);
    expect(saved?.order).toEqual([a.id, b.id]);
    expect(saved?.activeId).toBe(b.id);
    expect(saved?.leaves[b.id].backendSessionId).toBe("sb");
  });

  it("saves nothing, not an empty strip, when no tab has a session", () => {
    expect(buildTabStrip([tab("shell")], null, () => null, () => false)).toBeNull();
  });

  it("round-trips through the split storage value without inventing a split", () => {
    const a = tab("shell");
    const saved = buildTabStrip([a], a.id, () => "sa", () => false)!;
    const raw = composeWorkspaceWithStrip(null, saved)!;
    expect(parsePersistedTabStrip(raw)).toEqual(saved);
    expect(parsePersistedSplitWorkspace(raw)).toBeNull();
  });

  it("keeps split groups and the strip side by side", () => {
    const groups = [{ id: "pair-1", tree: splitPane(leaf("tab-1"), "tab-1", "tab-2", "row", "after", 0.5) as never }];
    const split = serializeSplitWorkspace(groups as never, { "tab-1": { launcher: "shell", slotIndex: 0 }, "tab-2": { launcher: "shell", slotIndex: 1 } }, "pair-1");
    const s = strip([{ id: "tab-1", session: "a" }, { id: "tab-2", session: "b" }, { id: "tab-3", session: "c" }]);
    const raw = composeWorkspaceWithStrip(split, s)!;
    expect(parsePersistedSplitWorkspace(raw)?.groups).toHaveLength(1);
    expect(parsePersistedTabStrip(raw)?.order).toEqual(["tab-1", "tab-2", "tab-3"]);
  });

  it("an old value with no strip parses as no strip", () => {
    const split = serializeSplitWorkspace([{ id: "pair-1", tree: splitPane(leaf("a"), "a", "b", "row", "after", 0.5) as never }] as never, {}, null);
    expect(parsePersistedTabStrip(split)).toBeNull();
    expect(parsePersistedTabStrip("not json")).toBeNull();
    expect(parsePersistedTabStrip(null)).toBeNull();
  });

  it("drops malformed strip entries instead of trusting them", () => {
    const raw = JSON.stringify({
      version: 3,
      groups: [],
      strip: {
        activeId: "ghost",
        order: ["tab-1", "tab-1", 7, "", "tab-9", "tab-2"],
        leaves: { "tab-1": { launcher: "shell", slotIndex: 0, backendSessionId: "a" }, "tab-2": { launcher: "claude" } },
      },
    });
    const parsed = parsePersistedTabStrip(raw)!;
    expect(parsed.order).toEqual(["tab-1", "tab-2"]);
    expect(parsed.activeId).toBeNull();
  });

  it("only holds the dock open for a strip that names a session", () => {
    expect(stripWorthWaitingFor(null)).toBe(false);
    expect(stripWorthWaitingFor(strip([{ id: "tab-1", session: "a" }]))).toBe(true);
  });
});
