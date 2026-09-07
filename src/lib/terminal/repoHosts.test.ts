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

  it("keeps a hidden tab's panel when the user switches to another repository", () => {
    const hosted = new Set(["a"]);
    expect(nextHostedTerminals(hosted, ["a", "b"], "b", true)).toEqual(new Set(["a", "b"]));
  });

  it("does not latch a newly visited tab while the dock is hidden", () => {
    const hosted = new Set(["a"]);
    expect(nextHostedTerminals(hosted, ["a", "b"], "b", false)).toEqual(new Set(["a"]));
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
