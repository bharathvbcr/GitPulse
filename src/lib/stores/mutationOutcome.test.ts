import { get } from "svelte/store";
import { afterEach, describe, expect, it } from "vitest";
import { reportFailure } from "./mutationOutcome";
import { toastStore } from "./toastStore";

describe("reportFailure", () => {
  afterEach(() => toastStore.clear());

  it("says nothing for a success", () => {
    expect(reportFailure({ ok: true }, "Couldn't create the branch")).toBe(true);
    expect(get(toastStore)).toEqual([]);
  });

  it("says nothing when the session error banner already surfaced it", () => {
    expect(reportFailure({ ok: false, error: "exists", surfaced: true }, "Couldn't create the branch")).toBe(false);
    expect(get(toastStore)).toEqual([]);
  });

  it("names a failure nobody saw: a background target or a refusal", () => {
    expect(reportFailure({ ok: false, error: "exists", surfaced: false }, "Couldn't create the branch")).toBe(false);
    expect(get(toastStore).map((t) => [t.kind, t.message])).toEqual([["error", "Couldn't create the branch. Exists."]]);
  });

  it("treats a failure with no surfaced flag as unseen", () => {
    reportFailure({ ok: false }, "Couldn't delete the tag");
    expect(get(toastStore).map((t) => t.message)).toEqual(["Couldn't delete the tag. It did not complete."]);
  });
});
