import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * The release workflow triggers on `push: tags: v*`, checks the tag out, and
 * builds THAT tree. Every gate it runs — the version manifests, the changelog
 * section — is evaluated at the tagged commit, so a tag left behind on an older
 * commit passes all of them and publishes a self-consistent build of the wrong
 * tree. The job cannot know which commit you meant; only the machine holding
 * both the tag and the work can tell. That is why the check is a pre-push hook
 * and why it needs a contract: an unrun hook and a passing hook look identical.
 *
 * Everything here is derived from the workflow and the scripts it calls, so a
 * renamed gate or a widened trigger fails this rather than the release.
 */
const REPO_ROOT = fileURLToPath(new URL("..", import.meta.url));

/**
 * Git reaches a hook through a shell — on Windows the `sh` bundled with Git for
 * Windows, because a file with a shebang is not directly executable there.
 * `execFileSync(".githooks/pre-push")` therefore died with ENOENT on the
 * Windows leg of CI, failing a check that had not run. Invoking bash by name is
 * both how git actually reaches the hook and the one spelling that works on all
 * three platforms; a relative path keeps Git Bash from having to read a
 * backslashed Windows path. Where there is no bash at all the case is SKIPPED
 * rather than passed, because a check that could not run must never look like
 * one that ran and passed.
 */
const hasBash = (() => {
  try {
    execFileSync("bash", ["-c", "exit 0"], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
})();
const hook = readFileSync(new URL("../.githooks/pre-push", import.meta.url), "utf8");
const workflow = readFileSync(new URL("../.github/workflows/release.yml", import.meta.url), "utf8");
const state = readFileSync(new URL("./release-state.mjs", import.meta.url), "utf8");

describe("pre-push release hook", () => {
  it("is tracked with the executable bit git will hand to every clone", () => {
    // `git ls-files -s` reports the INDEX mode, which is the same on every
    // platform. A filesystem stat would report 0644 on a Windows checkout for
    // a file git considers executable, so the mode has to be read from git.
    const entry = execFileSync("git", ["ls-files", "-s", ".githooks/pre-push"], {
      cwd: REPO_ROOT,
      encoding: "utf8",
    }).trim();
    expect(entry, ".githooks/pre-push is not tracked").not.toBe("");
    expect(entry.split(/\s+/)[0]).toBe("100755");
  });

  it("guards exactly the ref glob the release workflow triggers on", () => {
    // If the workflow ever widens its trigger, the hook stops covering it and
    // this fails rather than a release quietly slipping past.
    const trigger = workflow.match(/tags:\s*\n\s*-\s*'([^']+)'/);
    expect(trigger?.[1]).toBe("v*");
    expect(hook).toContain("refs/tags/v*)");
  });

  it("reuses the gates rather than reimplementing them", () => {
    // Named explicitly so renaming a script breaks the test, not the release.
    for (const gate of [
      "scripts/check-release-version.mjs",
      "scripts/release-notes.mjs",
      "scripts/release-state.mjs",
    ]) {
      expect(hook, `the hook no longer runs ${gate}`).toContain(gate);
    }
  });

  it("refuses a tag whose commit has not passed the CI the release gate requires", () => {
    // A tag can be well-formed, on HEAD, and agree with every manifest and
    // still be unreleasable: release-state refuses to prepare a draft without
    // a successful push run of both workflows on that exact commit. Running
    // the gate's own `ready` stage is what makes the hook's answer the same
    // answer the runner would give, half an hour earlier.
    expect(hook).toContain("release-state.mjs ready");
    expect(hook).toContain("RELEASE_COMMIT=");
    expect(state).toContain('if (stage === "ready")');
    for (const required of ["ci.yml", "coverage.yml"]) {
      expect(state, `the gate no longer requires ${required}`).toContain(required);
    }
  });

  it("checks CI locally because neither required workflow runs on the tag", () => {
    // The hook's whole premise. If ci.yml or coverage.yml ever gained a tag
    // trigger, pushing the tag would produce the runs the gate wants and this
    // check would become redundant — so the premise is asserted, not assumed.
    for (const name of ["ci.yml", "coverage.yml"]) {
      const text = readFileSync(new URL(`../.github/workflows/${name}`, import.meta.url), "utf8");
      const triggers = text.slice(text.indexOf("on:"), text.indexOf("concurrency:"));
      expect(triggers, `${name} has no push trigger`).toContain("branches:");
      expect(triggers, `${name} now runs on tags; the hook's CI check is stale`).not.toContain(
        "tags:",
      );
    }
  });

  it("refuses a tag that is not the commit being released", () => {
    // The whole point: a stale tag is the one failure CI cannot see.
    expect(hook).toMatch(/tag_commit.*!=.*head_sha|\$tag_commit"\s*!=\s*"\$head_sha/);
    expect(hook).toContain("git tag -f");
  });

  it("names an escape hatch, because refusing a deliberate re-release would be wrong", () => {
    // Re-releasing an older commit is legitimate; the hook must say how.
    expect(hook).toContain("--no-verify");
  });

  it.skipIf(!hasBash)("lets a branch push through untouched", () => {
    // A hook that blocks ordinary pushes gets uninstalled, and then guards
    // nothing at all.
    const result = execFileSync("bash", [".githooks/pre-push"], {
      cwd: REPO_ROOT,
      input: `refs/heads/main ${"a".repeat(40)} refs/heads/main ${"b".repeat(40)}\n`,
      encoding: "utf8",
    });
    expect(result).toBe("");
  });
});
