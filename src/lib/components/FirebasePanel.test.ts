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

  // Rendered in harness/firebase.html, each by name: unchecked, capped,
  // unreachable-region and refused listings; bounded vs complete empty
  // wording; mount lists neither rollouts nor backends and each click lists
  // once; switching project clears backends and rollouts; a CLI without the
  // rollout subcommand hides the action and says why; review is not deploy;
  // confirming deploys exactly once with the full SHA; editing the commit or
  // changing the backend disarms; the button disarms after a deploy; a
  // zero-exit rollout reads as started; the confirmation says it cannot be
  // undone; the API-enablement side effect is disclosed; no alias is
  // preselected from a choice; an unreadable .firebaserc is reported; a
  // commit absent from this checkout is marked. What stays is what the
  // fixture does not reach.
  it("reports a listing error and an unreadable firebase.json on their own", () => {
    expect(source).toContain("{#if rolloutsError}");
    expect(source).toContain("{#if status.firebasejson_error}");
  });
  it("attributes a policy verdict to its own call and not to the app's last one", () => {
    // `$harnessStore.lastVerdict` is the last verdict recorded anywhere for the
    // repository, so rendering it here would show a commit gate's decision
    // under a Firebase listing that may never have been judged.
    //
    // Scoped to the markup: the script above legitimately *names* that store in
    // the comment explaining why it is not used, and an assertion over the
    // whole file would fail on the very code that documents the fix.
    const markup = source.slice(source.lastIndexOf("</script>"));
    expect(markup).not.toContain("$harnessStore.lastVerdict");
    expect(source).toContain("let lastVerdict = $state<PolicyVerdict | null>(null)");
    expect(source).toContain("Policy on {lastVerdictAction}");
  });

  it("hands the timeline every rollout, not a collapsed slice", () => {
    // Collapse lives inside DeliveryTimeline so Actions runs and App Hosting
    // rollouts cannot disagree about what "show all" means. Slicing here
    // would also shrink the sample the card measures.
    expect(source).toContain("rows={rolloutRows}");
    expect(source).not.toContain("previewSlice(rolloutRows");
    expect(source).not.toContain("previewSlice(rollout");
  });

  it("names the project a rollout listing belongs to from the report", () => {
    // The backend name is rendered ("the listing names the backend it is of").
    expect(source).toContain("{rollouts.project_id}");
  });
  it("disarms a confirmed deploy when the project changes", () => {
    // Editing the commit and changing the backend are rendered; the fixture
    // never changes project with a deploy armed.
    const start = source.indexOf("function selectProject(");
    expect(start).toBeGreaterThan(-1);
    expect(source.slice(start, source.indexOf("\n  }", start))).toContain("disarmDeploy()");
  });
  it("records the policy verdict that came back with the guarded listing", () => {
    expect(source).toContain("harnessStore.recordVerdict(result?.policy ?? null, repoPath)");
  });

  it("cancels every in-flight fetch on teardown", () => {
    expect(source).toContain("statusGuard?.cancel()");
    expect(source).toContain("backendsGuard?.cancel()");
    expect(source).toContain("rolloutsGuard?.cancel()");
    expect(source).toContain("guard.isLive()");
  });

  it("drops the fetched stamp when it hydrates from cache", () => {
    // A listing carrying a timestamp it did not earn reads as current however
    // long it has been sitting there.
    expect(source).toContain("snapshotCache.get(repo)");
    expect(source).toContain("fetchedAt = null");
  });

  it("caches the selection together with the listings it produced", () => {
    // Separate caches would hydrate a rollout listing beside an empty selector,
    // leaving rows on screen with nothing naming the backend they came from.
    expect(source).toContain("interface FirebasePanelSnapshot");
    for (const field of ["alias:", "backend:", "backendsReport:", "rolloutsReport:"]) {
      expect(source).toContain(field);
    }
  });

  it("does not name snapshot fields after the state the hydrating effect writes", () => {
    // `effect-loop-contract` counts reads by word boundary, so a field called
    // `backends` reads to it as a read of the `backends` state that the same
    // effect assigns — a self-invalidating effect, and the one class `npm test`
    // cannot catch because the node environment compiles `$effect` out.
    const start = source.indexOf("$effect(() => {");
    const body = source.slice(start, source.indexOf("void loadStatus(repo)", start));
    expect(body).not.toMatch(/snapshot\?\.(backends|rollouts)\b/);
  });

  it("keeps an unchecked backends report out of the picker", () => {
    // A report that could not run carries an empty array for the same reason
    // every report here does. Reading it as "this project has no backends"
    // would be the substitution the whole type exists to prevent.
    expect(source).toContain("backends && backends.checked ? backends.backends : []");
  });
});

describe("FirebasePanel live deploy polling stays inside the click-only rule", () => {
  it("never polls a target no listing has answered for", () => {
    // The whole justification: listing rollouts can enable the App Hosting API
    // on a project where it is off, which is a change to a Cloud project. The
    // enabling happens only when the API is off, so a listing that already
    // answered proves it is on — and that success, nothing else, is what
    // authorises a refresh. `pollableTarget` starts null, so mounting the
    // panel can never start a poll.
    expect(source).toContain("let pollableTarget = $state<string | null>(null);");
    expect(source).toContain("if (result.output.checked) {");
    expect(source).toContain("pollableTarget = `${selectedProjectId}/${selectedBackend}`;");
  });

  it("keys the authorisation on project AND backend together", () => {
    // A backend id is not unique across projects, so a project-only key would
    // let one project's success authorise another project's poll.
    expect(source).toMatch(/currentTarget = \$derived\(\s*\n?\s*selectedProjectId && selectedBackend/);
    expect(source).toContain("`${selectedProjectId}/${selectedBackend}`");
  });

  it("withdraws the authorisation whenever the target moves or fails", () => {
    // Both directions matter: a changed backend must not inherit the previous
    // one's authorisation, and a failed listing proves nothing about the API.
    const resets = source.match(/pollableTarget = null;/g) ?? [];
    expect(resets.length, "reset on target change and on listing failure").toBeGreaterThanOrEqual(2);
  });

  it("requires both authorisation and work in flight before any timer starts", () => {
    expect(source).toContain("const authorised = pollableTarget !== null && pollableTarget === currentTarget;");
    expect(source).toContain("livePoll?.sync(authorised && anyInFlight(rolloutRows))");
  });

  it("re-checks the target after the CLI returns, not only before", () => {
    // The selection can move while the CLI runs; applying that answer would
    // show one backend's rollouts under another backend's name.
    const fn = source.slice(source.indexOf("async function pollRolloutsOnce"));
    const body = fn.slice(0, fn.indexOf("\n  }"));
    expect(body.match(/pollableTarget !== target/g) ?? []).toHaveLength(2);
  });

  it("drops a poll whose listing could not run", () => {
    // `checked: false` must not overwrite real rows with an empty list, and
    // must count as a failure so the scheduler backs off.
    expect(source).toContain("if (!result.output.checked) return false;");
  });

  it("tears the driver down per target", () => {
    expect(source).toContain("driver.dispose()");
  });
});
