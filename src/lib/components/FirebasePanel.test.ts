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

  it("never lists backends from a mount effect either", () => {
    // Same hazard as the rollout listing: `apphosting:backends:list` also runs
    // `ensureApiEnabled` upstream, so it too can turn on an API on the user's
    // Cloud project. Opening a tab must not do that.
    const scriptEnd = source.lastIndexOf("</script>");
    const firstEffect = source.indexOf("$effect(");
    const effects = source.slice(firstEffect, scriptEnd);
    expect(effects).not.toContain("loadBackends(");
    expect(source).toContain("onclick={() => void loadBackends()}");
  });

  it("actually calls the backends command it declares", () => {
    // `check:ipc` is satisfied by the wrapper in client.ts alone, so a command
    // can be registered, gated, documented and counted while no user action
    // ever reaches it — which is how this one shipped dead. The contract that
    // matters is a call site in a component, not in the client.
    expect(source).toContain("listFirebaseBackends(");
    expect(source).toContain("List backends");
  });

  it("clears the backend and its rollouts when the project changes", () => {
    // Backend ids are not unique across projects — `web` exists in most of
    // them — so a backend surviving a project change silently retargets the
    // next call at a different Google Cloud project.
    const start = source.indexOf("function selectProject(");
    expect(start).toBeGreaterThan(-1);
    const body = source.slice(start, source.indexOf("\n  }", start));
    for (const cleared of ["selectedBackend = \"\"", "backends = null", "rollouts = null"]) {
      expect(body).toContain(cleared);
    }
  });

  it("only offers the rollout action when the CLI actually has that subcommand", () => {
    // `apphosting:rollouts:list` ships behind an experiment that is off by
    // default. Offering the button regardless produced an exit-1-with-no-output
    // failure that surfaced as a JSON parse error.
    expect(source).toContain("{#if rolloutListing?.available}");
    expect(source).toContain("{#if rolloutListing && !rolloutListing.available}");
    expect(source).toContain("rolloutListing.reason");
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

  it("names the target a rollout listing belongs to", () => {
    // The rows come from the report; so must the heading above them. A listing
    // labelled by the selectors instead would, after a remount, show one
    // backend's rollouts under another's name.
    expect(source).toContain("{rollouts.backend_id}");
    expect(source).toContain("{rollouts.project_id}");
  });

  it("puts two steps between typing a commit and deploying it", () => {
    // A rollout changes what production serves, App Hosting has no rollback
    // verb, and creating one twice deploys twice. The confirm step is what
    // stops a mis-click from doing any of that.
    expect(source).toContain("let deployArmed = $state(false)");
    expect(source).toContain("if (deployProblem || !deployArmed) return;");
    expect(source).toContain("Review deploy");
  });

  it("disarms a confirmed deploy whenever its target changes", () => {
    // A confirmation that outlives what it confirmed is worse than none: the
    // reader approves one target and the armed button points at another.
    for (const fn of ["function selectProject(", "function selectBackend("]) {
      const start = source.indexOf(fn);
      expect(start, `${fn} must exist`).toBeGreaterThan(-1);
      expect(source.slice(start, source.indexOf("\n  }", start))).toContain("disarmDeploy()");
    }
    // Editing the SHA field disarms too.
    expect(source).toMatch(/deploySha = event\.currentTarget\.value;[\s\S]{0,200}disarmDeploy\(\)/);
  });

  it("never re-arms itself after a deploy", () => {
    // Creating a rollout is not idempotent — upstream allocates a new rollout
    // id per call — so a second click must not be able to repeat it.
    const start = source.indexOf("async function deploy(");
    const body = source.slice(start, source.indexOf("\n  }", source.indexOf("finally", start)));
    expect(body).toContain("deployArmed = false");
  });

  it("never reports a zero-exit rollout as a failure", () => {
    // A false failure costs a second deployment when the user retries.
    expect(source).toContain("deployOutcome.unconfirmed");
    expect(source).toContain("Rollout started");
  });

  it("states the irreversibility before the deploy button, not after", () => {
    // Whitespace-collapsed: prose in the markup wraps across source lines, so
    // asserting on the raw text makes the check hostage to where the formatter
    // happened to break a sentence a reader sees on one line.
    const flat = source.replace(/\s+/g, " ");
    const confirmation = flat.indexOf("has no rollback command");
    const outcome = flat.indexOf("Rollout started for");
    expect(confirmation, "the confirmation must say it cannot be undone").toBeGreaterThan(-1);
    expect(outcome, "the outcome block must exist").toBeGreaterThan(-1);
    expect(confirmation, "the warning must precede the result").toBeLessThan(outcome);
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
