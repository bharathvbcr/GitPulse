import { describe, expect, it } from "vitest";
import { nextHostedTerminals } from "./repoHosts";

describe("nextHostedTerminals", () => {
  it("does not spawn a panel until the dock is open", () => {
    const hosted = new Set<string>();
    expect(nextHostedTerminals(hosted, ["a"], "a", false)).toEqual(new Set());
  });

  it("hosts the active tab the moment the dock opens", () => {
    expect(nextHostedTerminals(new Set(), ["a", "b"], "a", true)).toEqual(new Set(["a"]));
  });

  it("keeps a hidden tab's panel, and hosts the new one only when ITS dock is open", () => {
    // The fourth argument is the ACTIVE tab's own dock state. `true` here
    // means the user has opened the terminal on "b" too, so hosting it is
    // what they asked for; "a" stays hosted so its scrollback survives.
    const hosted = new Set(["a"]);
    expect(nextHostedTerminals(hosted, ["a", "b"], "b", true)).toEqual(new Set(["a", "b"]));
  });

  /**
   * The decoupling, stated as a rule.
   *
   * This case used to be unreachable: the dock's open state was one
   * workspace-wide boolean, so a user with a shell running in "a" arrived at
   * "b" with `true` and latched a panel — and a spawned shell — in a
   * repository they were only reading. Now "b" carries its own state, so
   * visiting it with its dock closed hosts nothing.
   */
  it("does not latch a newly visited repository whose own dock is closed", () => {
    const hosted = new Set(["a"]);
    expect(nextHostedTerminals(hosted, ["a", "b"], "b", false)).toEqual(new Set(["a"]));
    // …and it stays that way however many repositories the user walks past.
    expect(nextHostedTerminals(hosted, ["a", "b", "c", "d"], "c", false)).toBe(hosted);
    expect(nextHostedTerminals(hosted, ["a", "b", "c", "d"], "d", false)).toBe(hosted);
  });

  it("drops a panel when its repository tab closes", () => {
    const hosted = new Set(["a", "b"]);
    expect(nextHostedTerminals(hosted, ["a"], "a", true)).toEqual(new Set(["a"]));
  });

  it("drops every panel when the last repository tab closes", () => {
    const hosted = new Set(["a"]);
    expect(nextHostedTerminals(hosted, [], null, true)).toEqual(new Set());
  });

  it("ignores an active id that is not an open tab", () => {
    expect(nextHostedTerminals(new Set(["a"]), ["a"], "ghost", true)).toEqual(new Set(["a"]));
  });

  it("returns the same set object when membership is unchanged", () => {
    const hosted = new Set(["a"]);
    expect(nextHostedTerminals(hosted, ["a", "b"], "a", true)).toBe(hosted);
    expect(nextHostedTerminals(hosted, ["a"], "a", false)).toBe(hosted);
  });

  it("does not mutate the set it was given when membership changes", () => {
    const hosted = new Set(["a"]);
    const next = nextHostedTerminals(hosted, ["a", "b"], "b", true);
    expect(hosted).toEqual(new Set(["a"]));
    expect(next).toEqual(new Set(["a", "b"]));
  });
});
