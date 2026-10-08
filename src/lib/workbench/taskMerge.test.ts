import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { MAX_MERGE_CARDS, mergeEligibility } from "./taskMerge";

const card = (...repository_ids: string[]) => ({ repository_ids });

describe("when the board offers Merge", () => {
  it("needs two or more cards in one repository, each linked to it alone", () => {
    expect(mergeEligibility([card("r")])).toMatchObject({ ok: false });
    expect(mergeEligibility([card("r"), card("r")])).toEqual({ ok: true, repositoryId: "r" });
    expect(mergeEligibility([card("r"), card("s")])).toMatchObject({ ok: false, reason: expect.stringContaining("different repositories") });
    // The merge refuses a shared task — deleting it would delete it everywhere —
    // so the board must not offer one, wherever it sits in the selection.
    expect(mergeEligibility([card("r", "s"), card("r")])).toMatchObject({ ok: false, reason: expect.stringContaining("several repositories") });
    expect(mergeEligibility([card("r"), card("r", "s")])).toMatchObject({ ok: false, reason: expect.stringContaining("several repositories") });
    expect(mergeEligibility([card(), card()])).toMatchObject({ ok: false });
  });

  it("stops at the merge's own bound", () => {
    expect(mergeEligibility(Array.from({ length: MAX_MERGE_CARDS }, () => card("r"))).ok).toBe(true);
    expect(mergeEligibility(Array.from({ length: MAX_MERGE_CARDS + 1 }, () => card("r"))).ok).toBe(false);
  });

  // Transcribed from the host. A changed bound there fails here rather than
  // letting the board offer a selection the merge refuses.
  it("matches the host's source bound plus the target", () => {
    const intake = readFileSync(new URL("../../../src-tauri/src/workbench/intake.rs", import.meta.url), "utf8");
    const merge = readFileSync(new URL("../../../src-tauri/src/workbench/intake_merge.rs", import.meta.url), "utf8");
    expect(merge).toContain("pub(crate) const MAX_MERGE_SOURCES: usize = MAX_RELATED;");
    const related = Number(/pub\(crate\) const MAX_RELATED: usize = (\d+);/.exec(intake)?.[1]);
    expect(MAX_MERGE_CARDS).toBe(related + 1);
  });
});
