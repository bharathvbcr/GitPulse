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
