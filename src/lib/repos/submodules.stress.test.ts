import { describe, expect, it } from "vitest";
import {
  canDeinit,
  canInitialize,
  canSync,
  describeSubmodules,
  initializableSubmodules,
  isDestructiveSubmoduleChange,
  sortSubmodules,
  submoduleChangeConsequence,
  type SubmoduleChange,
  type SubmoduleInfo,
  type SubmoduleState,
} from "./submodules";

const STATES: SubmoduleState[] = ["Uninitialized", "UpToDate", "CommitDiffers", "Conflicted"];

function sub(path: string, extra: Partial<SubmoduleInfo> = {}): SubmoduleInfo {
  return {
    name: path,
    path,
    url: `https://example.test/${path}.git`,
    oid: "abc123",
    described: "heads/main",
    state: "UpToDate",
    orphaned: false,
    ...extra,
  };
}

describe("submodules stress", () => {
  it("never describes a blank consequence across every kind, including force variants", () => {
    const changes: SubmoduleChange[] = [
      { kind: "update", path: null, recursive: true },
      { kind: "update", path: "vendor/lib", recursive: false },
      { kind: "sync", path: null, recursive: true },
      { kind: "sync", path: "vendor/lib", recursive: false },
      { kind: "deinit", path: "vendor/lib", force: false },
      { kind: "deinit", path: "vendor/lib", force: true },
    ];
    for (const change of changes) {
      const text = submoduleChangeConsequence(change);
      expect(text.length, change.kind).toBeGreaterThan(10);
    }
  });

  it("deinit is the only destructive change, whether or not force is set", () => {
    expect(isDestructiveSubmoduleChange({ kind: "deinit", path: "x", force: false })).toBe(true);
    expect(isDestructiveSubmoduleChange({ kind: "deinit", path: "x", force: true })).toBe(true);
    expect(isDestructiveSubmoduleChange({ kind: "update", path: null, recursive: true })).toBe(false);
    expect(isDestructiveSubmoduleChange({ kind: "sync", path: null, recursive: true })).toBe(false);
  });

  it("sorts a large mixed set worst-first and stably by path", () => {
    const list: SubmoduleInfo[] = [];
    for (let i = 0; i < 400; i += 1) {
      list.push(sub(`mod-${String(i).padStart(3, "0")}`, { state: STATES[i % STATES.length] }));
    }
    const sorted = sortSubmodules(list);
    expect(sorted[0].path).toBe("mod-003"); // first Conflicted
    expect(sorted[0].state).toBe("Conflicted");
    const order = STATES.map((state) => sorted.filter((s) => s.state === state).map((s) => s.path));
    for (const group of order) {
      const copy = [...group].sort((a, b) => a.localeCompare(b));
      expect(group).toEqual(copy);
    }
  });

  it("bulk-initialize never includes orphans even in a large set", () => {
    const list = Array.from({ length: 80 }, (_, i) =>
      sub(`v/${i}`, {
        state: i % 2 === 0 ? "Uninitialized" : "UpToDate",
        orphaned: i % 10 === 0,
        url: i % 10 === 0 ? null : `https://example.test/${i}.git`,
      }),
    );
    const init = initializableSubmodules(list);
    expect(init.every((s) => canInitialize(s))).toBe(true);
    expect(init.some((s) => s.orphaned)).toBe(false);
    expect(init.length).toBeGreaterThan(0);
    expect(init.length).toBeLessThan(list.filter((s) => s.state === "Uninitialized").length);
  });

  it("canDeinit / canSync stay honest across every state, including orphans", () => {
    for (const state of STATES) {
      const ordinary = sub("lib", { state });
      const orphan = sub("lib", { state, orphaned: true, url: null });
      expect(canDeinit(ordinary)).toBe(state !== "Uninitialized");
      expect(canDeinit(orphan)).toBe(state !== "Uninitialized");
      expect(canSync(ordinary)).toBe(true);
      expect(canSync(orphan)).toBe(false);
    }
  });

  it("describeSubmodules still leads with the broken count on a large set", () => {
    const list = [
      ...Array.from({ length: 50 }, (_, i) => sub(`ok/${i}`)),
      ...Array.from({ length: 7 }, (_, i) => sub(`empty/${i}`, { state: "Uninitialized" })),
    ];
    expect(describeSubmodules(list)).toBe("7 of 57 submodules not initialized.");
  });
});
