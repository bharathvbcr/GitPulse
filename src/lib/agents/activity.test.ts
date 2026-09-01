import { describe, expect, it } from "vitest";
import {
  MAX_AGENT_ACTIONS,
  actionKindForCommand,
  appendAction,
  makeAgentAction,
  type AgentActionEntry,
} from "./activity";

function entry(i: number): AgentActionEntry {
  return {
    id: i,
    ts: 1000 + i,
    kind: "commit",
    label: `c${i}`,
    ok: true,
    verdict: null,
  };
}

describe("appendAction", () => {
  it("appends and preserves order", () => {
    let list: AgentActionEntry[] = [];
    list = appendAction(list, entry(1));
    list = appendAction(list, entry(2));
    expect(list.map((e) => e.id)).toEqual([1, 2]);
  });

  it("drops the oldest entry past the cap and never grows past it", () => {
    let list: AgentActionEntry[] = [];
    for (let i = 0; i < MAX_AGENT_ACTIONS + 25; i++) {
      list = appendAction(list, entry(i));
    }
    expect(list).toHaveLength(MAX_AGENT_ACTIONS);
    expect(list[0].id).toBe(25);
    expect(list[list.length - 1].id).toBe(MAX_AGENT_ACTIONS + 24);
  });

  it("does not mutate the input list", () => {
    const original = [entry(1)];
    const next = appendAction(original, entry(2));
    expect(original).toHaveLength(1);
    expect(next).toHaveLength(2);
  });
});

describe("makeAgentAction", () => {
  it("assigns monotonically increasing ids and a timestamp", () => {
    const a = makeAgentAction({ kind: "push", label: "origin/main", ok: true }, 42);
    const b = makeAgentAction({ kind: "pull", label: "x", ok: false }, 43);
    expect(a.ts).toBe(42);
    expect(b.id).toBeGreaterThan(a.id);
  });

  it("defaults verdict to null when absent", () => {
    const a = makeAgentAction({ kind: "stage", label: "f.rs", ok: true });
    expect(a.verdict).toBeNull();
  });
});

describe("actionKindForCommand", () => {
  it("maps guarded commands to coarse verbs", () => {
    expect(actionKindForCommand("cmd_commit")).toBe("commit");
    expect(actionKindForCommand("cmd_quick_commit")).toBe("commit");
    expect(actionKindForCommand("cmd_push")).toBe("push");
    expect(actionKindForCommand("cmd_rebase_interactive")).toBe("rebase");
    expect(actionKindForCommand("cmd_stage_selective_patch")).toBe("stage");
    expect(actionKindForCommand("cmd_add_worktree")).toBe("worktree");
    expect(actionKindForCommand("cmd_write_file_content")).toBe("edit");
  });

  it("maps every mutating surface to a human-readable verb", () => {
    // The journal is the record of what an unattended agent did; a raw
    // command name there is legible but inconsistent with its neighbours.
    expect(actionKindForCommand("cmd_cherry_pick")).toBe("cherry-pick");
    expect(actionKindForCommand("cmd_revert")).toBe("revert");
    expect(actionKindForCommand("cmd_reset")).toBe("reset");
    expect(actionKindForCommand("cmd_stash_action")).toBe("unstash");
    expect(actionKindForCommand("cmd_repo_operation_action")).toBe("operation");
    expect(actionKindForCommand("cmd_remote_change")).toBe("remote");
    expect(actionKindForCommand("cmd_submodule_change")).toBe("submodule");
    // The ones whose raw name is not already a verb must not leak it.
    // `cmd_revert` and `cmd_reset` are excluded on purpose: their command
    // name IS the verb, so mapping them to themselves is correct.
    for (const cmd of [
      "cmd_cherry_pick",
      "cmd_stash_action",
      "cmd_repo_operation_action",
      "cmd_remote_change",
      "cmd_submodule_change",
    ]) {
      expect(actionKindForCommand(cmd), cmd).not.toBe(cmd.slice(4));
    }
    expect(actionKindForCommand("cmd_terminal_run")).toBe("terminal");
  });

  it("passes unknown names through rather than guessing", () => {
    expect(actionKindForCommand("cmd_something_new")).toBe("something_new");
  });
});
