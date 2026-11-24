import { get } from "svelte/store";
import { describe, expect, it, vi } from "vitest";
import { noteProtection, partitionByProtection, protectedSessions, replaceProtection, setSessionProtected, sparedNotice } from "./protection";

describe("protection", () => {
  it("replaces what is known with the host listing, so a reloaded page learns the locks", () => {
    replaceProtection([{ id: "a", protected: true }, { id: "b" }, { id: "c", protected: false }]);
    expect([...get(protectedSessions)]).toEqual(["a"]);
    replaceProtection([{ id: "b" }]);
    expect(get(protectedSessions).size).toBe(0);
  });

  it("sends the flag without a title, and shows it only once the backend stored it", async () => {
    replaceProtection([]);
    const invoke = vi.fn(async () => true);
    await setSessionProtected("a", true, invoke as never);
    expect(invoke).toHaveBeenCalledWith("cmd_terminal_set_title", { sessionId: "a", protected: true });
    expect(get(protectedSessions).has("a")).toBe(true);
    await setSessionProtected("a", false, invoke as never);
    expect(get(protectedSessions).has("a")).toBe(false);
  });

  it("draws no lock for a flag the backend refused to store", async () => {
    replaceProtection([]);
    const invoke = vi.fn(async () => { throw new Error("not recorded"); });
    await expect(setSessionProtected("a", true, invoke as never)).rejects.toThrow("not recorded");
    expect(get(protectedSessions).has("a")).toBe(false);
  });

  it("learns one session from a refusal", () => {
    replaceProtection([]);
    noteProtection("z", true);
    expect(get(protectedSessions).has("z")).toBe(true);
  });

  it("splits a batch before anything is asked or closed", () => {
    const { closable, spared } = partitionByProtection(["a", "b", "c"], (id) => id === "b");
    expect(closable).toEqual(["a", "c"]);
    expect(spared).toEqual(["b"]);
  });

  it("names the sessions it left running", () => {
    expect(sparedNotice([], "batch close")).toBeNull();
    expect(sparedNotice(["Claude"], "batch close")).toContain("Claude is protected from batch close and was left running");
    const many = sparedNotice(Array.from({ length: 8 }, (_, i) => `s${i}`), "Stop children") ?? "";
    expect(many).toContain("and 2 more");
    expect(many).toContain("are protected");
  });
});
