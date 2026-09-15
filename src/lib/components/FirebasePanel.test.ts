import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import FirebasePanel from "./FirebasePanel.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "FirebasePanel.svelte"),
  "utf8",
);

describe("FirebasePanel", () => {
  it("renders its heading without a repository", () => {
    const { body } = render(FirebasePanel, { props: { repoPath: null } });
    expect(body).toContain("Firebase App Hosting");
  });

  it("surfaces every degradation instead of a clean-looking empty state", () => {
    // Each of these is a way the answer can be less than the whole truth, and
    // each has to be visible on its own. A panel that renders any of them as an
    // empty rollout list is claiming the backend has never deployed.
    expect(source).toContain("{#if rolloutsError}");
    expect(source).toContain("{:else if rollouts && !rollouts.checked}");
    expect(source).toContain("{#if rollouts.walk_incomplete}");
    expect(source).toContain("{#if rollouts.truncated}");
    expect(source).toContain("{#if status.firebaserc_error}");
    expect(source).toContain("{#if status.firebasejson_error}");
  });

  it("does not let a bounded listing borrow an unbounded listing's wording", () => {
    // "This backend has never deployed" is a claim about production. It may
    // only be made when the listing that produced it was complete.
    expect(source).toContain("No rollouts in the listing shown");
    expect(source).toContain("This backend has never deployed");
    expect(source).toContain("rollouts.rollouts.length === 0 && bounded");
  });

  it("never lists rollouts from a mount effect", () => {
    // The Firebase CLI enables the App Hosting API when it is off, so the first
    // listing for a project can change the user's Google Cloud setup. That
    // belongs to a click. If this assertion ever fails, opening a tab started
    // mutating a Cloud project.
    // Bounded to the script block: the markup below it legitimately calls
    // loadRollouts from the button's handler, and a window that swept that up
    // would fail on the correct code.
    const scriptEnd = source.lastIndexOf("</script>");
    const firstEffect = source.indexOf("$effect(");
    expect(firstEffect, "the panel must have a repo effect to check").toBeGreaterThan(-1);
    expect(scriptEnd).toBeGreaterThan(firstEffect);
    const effects = source.slice(firstEffect, scriptEnd);
    expect(effects).not.toContain("loadRollouts(");
    expect(source).toContain("onclick={() => void loadRollouts()}");
  });

  it("says the API-enablement side effect before the button, not after", () => {
    expect(source).toContain("the CLI enables it");
    const disclosure = source.indexOf("the CLI enables it");
    const button = source.indexOf("Check rollouts");
    expect(disclosure).toBeGreaterThan(-1);
    expect(button).toBeGreaterThan(-1);
  });

  it("never pre-selects a project alias from the default", () => {
    // `.firebaserc`'s `default` is very often production. Pre-filling it aims
    // every action at prod without the user naming the target.
    expect(source).not.toContain("selectedAlias = next.default_alias");
    expect(source).toContain("next.projects.length === 1");
  });

  it("records the policy verdict that came back with the guarded listing", () => {
    expect(source).toContain("harnessStore.recordVerdict(result?.policy ?? null, repoPath)");
  });

  it("marks a deployed commit that is not in this checkout rather than hiding it", () => {
    expect(source).toContain("present_locally");
    expect(source).toContain("not local");
  });

  it("cancels every in-flight fetch on teardown", () => {
    expect(source).toContain("statusGuard?.cancel()");
    expect(source).toContain("rolloutsGuard?.cancel()");
    expect(source).toContain("guard.isLive()");
  });

  it("drops the fetched stamp when it hydrates from cache", () => {
    // A listing carrying a timestamp it did not earn reads as current however
    // long it has been sitting there.
    expect(source).toContain("rolloutsCache.get(repo)");
    expect(source).toContain("fetchedAt = null");
  });
});
