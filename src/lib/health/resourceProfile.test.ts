import { describe, expect, it } from "vitest";
import type { ResourceProfile } from "../stores/hostLoadStore";
import { asProfileChoice, PROFILE_CHOICES, profileNote } from "./hostLoad";

const profile = (over: Partial<ResourceProfile> = {}): ResourceProfile => ({
  tier: "compact",
  basis: "memory",
  memory_bytes: 8 * 2 ** 30,
  logical_cores: 8,
  budgets: { gpu_contexts: 2, gpu_hide_grace_ms: 15_000, graph_cache_repositories: 2 },
  ...over,
});

describe("the memory profile choices", () => {
  it("offers auto first and the three tiers in plain words", () => {
    expect(PROFILE_CHOICES.map((c) => c.value)).toEqual(["auto", "compact", "standard", "roomy"]);
    for (const choice of PROFILE_CHOICES) expect(choice.label).toMatch(/^[A-Z][a-z]+/);
  });

  it("reads a select's value back as a choice, and anything else as auto", () => {
    for (const choice of PROFILE_CHOICES) expect(asProfileChoice(choice.value)).toBe(choice.value);
    expect(asProfileChoice("tiny")).toBe("auto");
    expect(asProfileChoice("")).toBe("auto");
  });
});

describe("profileNote", () => {
  it("says the tier was derived from the memory it read", () => {
    expect(profileNote(profile())).toBe("In use: compact, from 8.00 GB of memory.");
  });

  it("says when the user chose the tier", () => {
    expect(profileNote(profile({ basis: "override", tier: "roomy" }))).toBe("In use: roomy, as you chose.");
  });

  it("never presents an unreadable memory size as a derived answer", () => {
    const note = profileNote(profile({ basis: "memory_unreadable", tier: "standard", memory_bytes: null }));
    expect(note).toBe("In use: standard, because this computer's memory could not be read.");
  });
});
