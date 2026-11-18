import { get } from "svelte/store";
import { beforeEach, describe, expect, it } from "vitest";
import {
  MAX_REMEMBERED_GROUPS,
  clearGroupState,
  collapsedInactiveGroups,
  explicitlyExpandedGroups,
  setGroupExplicitlyExpanded,
  setGroupInactiveCollapsed,
  toggleGroupInactiveCollapsed,
} from "./groupState";

const collapsed = () => [...get(collapsedInactiveGroups)];
const expanded = () => [...get(explicitlyExpandedGroups)];

beforeEach(() => clearGroupState());

describe("setGroupInactiveCollapsed", () => {
  it("remembers a collapsed group under its trimmed name and forgets it when un-collapsed", () => {
    setGroupInactiveCollapsed("  work ", true);
    expect(collapsed()).toEqual(["work"]);
    setGroupInactiveCollapsed("work", false);
    expect(collapsed()).toEqual([]);
  });

  it("ignores blank and non-string names", () => {
    setGroupInactiveCollapsed("   ", true);
    setGroupInactiveCollapsed(undefined as unknown as string, true);
    expect(collapsed()).toEqual([]);
  });

  it("keeps the same set instance when nothing changes", () => {
    setGroupInactiveCollapsed("a", true);
    const before = get(collapsedInactiveGroups);
    setGroupInactiveCollapsed("a", true);
    expect(get(collapsedInactiveGroups)).toBe(before);
  });

  it("collapsing a group withdraws its explicit expansion", () => {
    setGroupExplicitlyExpanded("a", true);
    setGroupInactiveCollapsed("a", true);
    expect(expanded()).toEqual([]);
    expect(collapsed()).toEqual(["a"]);
  });

  it("collapsing leaves other groups' explicit expansion alone", () => {
    setGroupExplicitlyExpanded("b", true);
    setGroupInactiveCollapsed("a", true);
    expect(expanded()).toEqual(["b"]);
  });

  it("evicts the oldest remembered group past the cap", () => {
    for (let i = 0; i < MAX_REMEMBERED_GROUPS + 3; i++) setGroupInactiveCollapsed(`g${i}`, true);
    const names = collapsed();
    expect(names).toHaveLength(MAX_REMEMBERED_GROUPS);
    expect(names).not.toContain("g0");
    expect(names).toContain(`g${MAX_REMEMBERED_GROUPS + 2}`);
  });
});

describe("setGroupExplicitlyExpanded", () => {
  it("remembers an expanded group under its trimmed name and forgets it when un-expanded", () => {
    setGroupExplicitlyExpanded(" a  ", true);
    expect(expanded()).toEqual(["a"]);
    setGroupExplicitlyExpanded("a", false);
    expect(expanded()).toEqual([]);
  });

  it("ignores blank and non-string names", () => {
    setGroupExplicitlyExpanded("", true);
    setGroupExplicitlyExpanded(null as unknown as string, true);
    expect(expanded()).toEqual([]);
  });

  it("keeps the same set instance when nothing changes", () => {
    setGroupExplicitlyExpanded("a", true);
    const before = get(explicitlyExpandedGroups);
    setGroupExplicitlyExpanded("a", true);
    expect(get(explicitlyExpandedGroups)).toBe(before);
  });

  it("expanding a group withdraws its collapse, and leaves other groups collapsed", () => {
    setGroupInactiveCollapsed("a", true);
    setGroupInactiveCollapsed("b", true);
    setGroupExplicitlyExpanded("a", true);
    expect(collapsed()).toEqual(["b"]);
  });

  it("evicts the oldest remembered group past the cap", () => {
    for (let i = 0; i < MAX_REMEMBERED_GROUPS + 2; i++) setGroupExplicitlyExpanded(`g${i}`, true);
    const names = expanded();
    expect(names).toHaveLength(MAX_REMEMBERED_GROUPS);
    expect(names).not.toContain("g0");
  });
});

describe("toggleGroupInactiveCollapsed and clearGroupState", () => {
  it("toggles a group in and out of the collapsed set", () => {
    toggleGroupInactiveCollapsed("a");
    expect(collapsed()).toEqual(["a"]);
    toggleGroupInactiveCollapsed(" a ");
    expect(collapsed()).toEqual([]);
  });

  it("ignores a blank toggle", () => {
    toggleGroupInactiveCollapsed("  ");
    expect(collapsed()).toEqual([]);
  });

  it("clears both sets", () => {
    setGroupInactiveCollapsed("a", true);
    setGroupExplicitlyExpanded("b", true);
    clearGroupState();
    expect(collapsed()).toEqual([]);
    expect(expanded()).toEqual([]);
  });
});
