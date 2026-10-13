import { afterEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { toastStore } from "./toastStore";

/**
 * An error toast waits to be dismissed. A deferral under load is not an
 * error, and at launch it arrived from every panel at once: a stack of sticky
 * red toasts for reads that simply had not run yet.
 */
describe("a deferral routed to an error toast", () => {
  afterEach(() => toastStore.clear());

  it("is shown as a warning that expires, while a real error still waits", () => {
    toastStore.error(
      "git stash deferred under load after 2.004s: the git spawn rate limit admitted nothing sooner",
    );
    toastStore.add({ kind: "error", message: "git worktree deferred under load after 0.000s: shed" });
    toastStore.error("git stash failed: not a git repository");
    const [deferred, shed, failed] = get(toastStore);
    expect(deferred.kind).toBe("warning");
    expect(deferred.duration).toBeGreaterThan(0);
    expect(shed.kind).toBe("warning");
    expect(failed.kind).toBe("error");
    expect(failed.duration).toBe(0);
  });
});

describe("eviction at the cap (ft-038e6ba5)", () => {
  afterEach(() => toastStore.clear());

  it("drops the oldest non-error toast, never an error, while a non-error exists", () => {
    toastStore.error("first failure");
    for (let i = 0; i < 5; i += 1) toastStore.info(`note ${i}`);
    const kept = get(toastStore);
    expect(kept).toHaveLength(5);
    expect(kept.some((t) => t.message === "first failure")).toBe(true);
    expect(kept.filter((t) => t.message.startsWith("note")).map((t) => t.message)).toEqual(["note 1", "note 2", "note 3", "note 4"]);
  });

  it("evicts the oldest error only when every toast is an error", () => {
    for (let i = 0; i < 6; i += 1) toastStore.error(`failure ${i}`);
    expect(get(toastStore).map((t) => t.message)).toEqual(["failure 1", "failure 2", "failure 3", "failure 4", "failure 5"]);
  });
});
