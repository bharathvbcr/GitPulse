import { get } from "svelte/store";
import { describe, expect, it, vi } from "vitest";
import { createSessionRegistry, type TerminalSessionRecord } from "./sessionRegistry";
import { MAX_TERMINAL_TABS } from "./tabs";

function record(overrides: Partial<TerminalSessionRecord> = {}): TerminalSessionRecord {
  return {
    key: "session-a",
    repoPath: "/repo/a",
    label: "Shell",
    status: "running",
    close: vi.fn(),
    ...overrides,
  };
}

describe("terminal session registry", () => {
  it("publishes session insertions and updates to subscribers", () => {
    const registry = createSessionRegistry();
    const snapshots: Array<readonly TerminalSessionRecord[]> = [];
    registry.subscribe((value) => snapshots.push(value));

    const lease = registry.reserve(record({ key: "a", status: "starting" }));
    expect(snapshots.at(-1)?.map((entry) => entry.status)).toEqual(["starting"]);

    lease.update("ready");
    expect(snapshots.at(-1)?.map((entry) => entry.status)).toEqual(["ready"]);
    expect(snapshots).toHaveLength(3);
    expect(snapshots[0]).toEqual([]);
  });

  it("releases a session and removes it from published state", () => {
    const registry = createSessionRegistry();
    const updates: Array<number> = [];
    registry.subscribe((value) => updates.push(value.length));

    const lease = registry.reserve(record({ key: "a", status: "ready" }));
    lease.release();

    expect(updates).toEqual([0, 1, 0]);
    expect(get(registry)).toHaveLength(0);
  });

  it("ignores updates and releases when already disposed", () => {
    const registry = createSessionRegistry();
    const lease = registry.reserve(record({ key: "a", status: "starting" }));
    lease.release();
    lease.update("stale");
    lease.release();

    expect(get(registry)).toHaveLength(0);
  });

  it("rejects duplicate keys", () => {
    const registry = createSessionRegistry();
    registry.reserve(record({ key: "a" }));
    expect(() => registry.reserve(record({ key: "a", status: "other" }))).toThrow(
      "This terminal already owns a session slot",
    );
  });

  it("rejects reservations above the terminal capacity", () => {
    const registry = createSessionRegistry();
    for (let index = 0; index < MAX_TERMINAL_TABS; index += 1) {
      registry.reserve(record({ key: String(index) }));
    }

    expect(() => registry.reserve(record({ key: String(MAX_TERMINAL_TABS + 1) }))).toThrow(
      `All ${MAX_TERMINAL_TABS} terminal sessions are in use across repositories`,
    );
    expect(get(registry)).toHaveLength(MAX_TERMINAL_TABS);
  });

  it("keeps reserved slots across status transitions while open", () => {
    const registry = createSessionRegistry();
    const lease = registry.reserve(record({ key: "a", status: "ready" }));
    lease.update("busy");
    lease.update("ready");
    lease.update("closing");

    expect(get(registry)).toHaveLength(1);
    expect(get(registry)[0].status).toBe("closing");
  });
});
