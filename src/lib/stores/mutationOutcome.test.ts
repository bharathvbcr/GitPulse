import { get } from "svelte/store";
import { afterEach, describe, expect, it } from "vitest";
import { reportFailure } from "./mutationOutcome";
import { toastStore } from "./toastStore";

describe("reportFailure", () => {
  afterEach(() => toastStore.clear());

  it("says nothing for a success", () => {
    expect(reportFailure({ ok: true }, "Could not create branch")).toBe(true);
    expect(get(toastStore)).toEqual([]);
  });

  it("says nothing when the session error banner already surfaced it", () => {
    expect(reportFailure({ ok: false, error: "exists", surfaced: true }, "Could not create branch")).toBe(false);
    expect(get(toastStore)).toEqual([]);
  });

  it("names a failure nobody saw: a background target or a refusal", () => {
    expect(reportFailure({ ok: false, error: "exists", surfaced: false }, "Could not create branch")).toBe(false);
    expect(get(toastStore).map((t) => [t.kind, t.message])).toEqual([["error", "Could not create branch: exists"]]);
  });

  it("treats a failure with no surfaced flag as unseen", () => {
    reportFailure({ ok: false }, "Could not delete tag");
    expect(get(toastStore).map((t) => t.message)).toEqual(["Could not delete tag: it did not complete."]);
  });
});
