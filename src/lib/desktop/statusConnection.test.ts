import { describe, expect, it, vi } from "vitest";
import { createStatusConnection } from "./statusConnection";
import type { MenuState } from "./menuState";
import { statusFixture } from "../../../harness/statusFixtures";

describe("status window connection", () => {
  it("retries a failed subscription before loading the snapshot", async () => {
    const stop = vi.fn();
    const read = vi.fn().mockResolvedValue(statusFixture("changes"));
    const subscribe = vi.fn().mockRejectedValueOnce(new Error("bridge starting"))
      .mockResolvedValue(stop);
    const apply = vi.fn();
    const failed = vi.fn();
    const connection = createStatusConnection({ read, subscribe, apply, failed });
    await connection.connect();
    expect(read).not.toHaveBeenCalled();
    expect(failed).toHaveBeenCalledTimes(1);
    await connection.connect();
    expect(subscribe).toHaveBeenCalledTimes(2);
    expect(apply).toHaveBeenCalledWith(statusFixture("changes"));
    connection.dispose();
    expect(stop).toHaveBeenCalledTimes(1);
  });

  it.each([false, true])("live updates supersede a pending snapshot (read fails: %s)", async (fails) => {
    let receive: ((snapshot: MenuState) => void) | undefined;
    let resolve: ((snapshot: MenuState) => void) | undefined;
    let reject: ((error: Error) => void) | undefined;
    const read = vi.fn(() => new Promise<MenuState>((yes, no) => { resolve = yes; reject = no; }));
    const subscribe = vi.fn(async (callback: (snapshot: MenuState) => void) => { receive = callback; return vi.fn(); });
    const apply = vi.fn();
    const failed = vi.fn();
    const connection = createStatusConnection({ read, subscribe, apply, failed });
    const loading = connection.connect();
    expect(connection.connect()).toBe(loading);
    await Promise.resolve();
    receive!(statusFixture("conflicts"));
    if (fails) reject!(new Error("stale request failed"));
    else resolve!(statusFixture("clean"));
    await loading;
    expect(apply.mock.calls).toEqual([[statusFixture("conflicts")]]);
    expect(failed).not.toHaveBeenCalled();
    connection.dispose();
  });

  it("removes a listener that finishes attaching after disposal", async () => {
    let attach: ((stop: () => void) => void) | undefined;
    const stop = vi.fn();
    const read = vi.fn();
    const apply = vi.fn();
    const connection = createStatusConnection({
      read, apply, failed: vi.fn(),
      subscribe: () => new Promise((resolve) => { attach = resolve; }),
    });
    const connecting = connection.connect();
    connection.dispose();
    attach!(stop);
    await connecting;
    await connection.connect();
    expect(stop).toHaveBeenCalledTimes(1);
    expect(read).not.toHaveBeenCalled();
    expect(apply).not.toHaveBeenCalled();
  });
});
