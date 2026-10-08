import { describe, expect, it, vi } from "vitest";
import { createActivityLog, MAX_ACTIVITY_EVENTS, MAX_NAMED_COMMANDS, observeSettled } from "./activity";

function log() {
  let time = 0;
  const activity = createActivityLog(() => time);
  return { activity, at(t: number) { time = t; } };
}

describe("stall activity log", () => {
  it("counts only what was delivered inside the late interval", () => {
    const l = log();
    l.at(5); l.activity.noteCommandSettled("cmd_before");
    l.at(10); l.activity.noteCommandSettled("cmd_get_status");
    l.at(20); l.activity.noteCommandSettled("cmd_get_status");
    l.at(30); l.activity.noteCommandSettled("cmd_list_branches");
    l.at(50); l.activity.noteCommandSettled("cmd_after");
    const a = l.activity.attribute(10, 40, "repository/history");
    expect(a.cause).toBe("command:cmd_get_status");
    expect(a.commands).toEqual([["cmd_get_status", 2], ["cmd_list_branches", 1]]);
    expect(a.watcherEvents).toBe(0);
    expect(a.view).toBe("repository/history");
    expect(a.partial).toBe(false);
  });

  it("names a watcher burst when change events outnumber answers", () => {
    const l = log();
    l.at(1); l.activity.noteCommandSettled("cmd_get_status");
    for (let t = 2; t < 6; t++) { l.at(t); l.activity.noteWatcherEvent(); }
    const a = l.activity.attribute(0, 10, "repository/work");
    expect(a.cause).toBe("watcher-burst");
    expect(a.watcherEvents).toBe(4);
  });

  it("falls back to the visible view when nothing was delivered", () => {
    const l = log();
    expect(l.activity.attribute(0, 10, "fleet").cause).toBe("view:fleet");
    expect(l.activity.attribute(0, 10, "").cause).toBe("view:unknown");
  });

  it("bounds named commands and marks a wrapped interval as partial", () => {
    const l = log();
    for (let i = 0; i < MAX_ACTIVITY_EVENTS + 10; i++) {
      l.at(100 + i);
      l.activity.noteCommandSettled(`cmd_${i % (MAX_NAMED_COMMANDS + 2)}`);
    }
    const a = l.activity.attribute(0, 10_000, "repository/code");
    expect(a.commands).toHaveLength(MAX_NAMED_COMMANDS);
    expect(a.otherCommands).toBe(2);
    expect(a.partial).toBe(true);
    const total = a.commands.reduce((sum, [, n]) => sum + n, 0);
    expect(total).toBeLessThanOrEqual(MAX_ACTIVITY_EVENTS);
    // An interval that starts after the oldest retained event is complete.
    expect(l.activity.attribute(200, 10_000, "x").partial).toBe(false);
  });
});

describe("observeSettled", () => {
  it("notes answers, failures and synchronous throws, and forwards each unchanged", async () => {
    const noted: string[] = [];
    const error = new Error("denied");
    const raw = vi.fn((cmd: string) => {
      if (cmd === "cmd_throw") throw error;
      return cmd === "cmd_fail" ? Promise.reject(error) : Promise.resolve(`${cmd}:ok`);
    });
    const invoke = observeSettled(
      <T,>(cmd: string) => raw(cmd) as Promise<T>,
      (cmd) => noted.push(cmd),
    );
    await expect(invoke<string>("cmd_ok", { a: 1 })).resolves.toBe("cmd_ok:ok");
    await expect(invoke("cmd_fail")).rejects.toBe(error);
    await expect(invoke("cmd_throw")).rejects.toBe(error);
    expect(noted).toEqual(["cmd_ok", "cmd_fail", "cmd_throw"]);
  });

  it("costs a caller exactly one microtask", async () => {
    const invoke = observeSettled(<T,>() => Promise.resolve("v" as T), () => {});
    let rawSeen = false;
    let seen = false;
    void Promise.resolve("v").then(() => { rawSeen = true; });
    void invoke("cmd").then(() => { seen = true; });
    await Promise.resolve();
    expect(rawSeen).toBe(true);
    expect(seen).toBe(false);
    await Promise.resolve();
    expect(seen).toBe(true);
  });

  it("does not note a call before it settles", async () => {
    const noted: string[] = [];
    let release!: (value: string) => void;
    const invoke = observeSettled(
      <T,>() => new Promise<T>((resolve) => { release = resolve as (value: string) => void; }),
      (cmd) => noted.push(cmd),
    );
    const call = invoke<string>("cmd_slow");
    await Promise.resolve();
    expect(noted).toEqual([]);
    release("done");
    await expect(call).resolves.toBe("done");
    expect(noted).toEqual(["cmd_slow"]);
  });
});
