/**
 * A fixture identity written into this checkout's local git config becomes
 * the author of every later commit. The pre-commit hook is what stops that
 * commit from landing, and an unrun hook looks the same as a passing one, so
 * this drives the real hook against a real `git commit`.
 *
 * The denylist matches `GitWriter::is_fixture_author_email`.
 */
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";

const REPO_ROOT = fileURLToPath(new URL("..", import.meta.url));
const HOOKS = join(REPO_ROOT, ".githooks");

const hasBash = (() => {
  try {
    execFileSync("bash", ["-c", "exit 0"], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
})();

const gitAvailable = (() => {
  try {
    execFileSync("git", ["--version"], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
})();

const dirs: string[] = [];

afterEach(() => {
  for (const dir of dirs.splice(0)) {
    rmSync(dir, { recursive: true, force: true });
  }
});

function git(cwd: string, args: string[], env: NodeJS.ProcessEnv = process.env): string {
  return execFileSync("git", args, { cwd, encoding: "utf8", env });
}

function initRepo(): string {
  const dir = mkdtempSync(join(tmpdir(), "gitpulse-fixture-author-"));
  dirs.push(dir);
  git(dir, ["init", "-q", "-b", "main"]);
  git(dir, ["config", "core.hooksPath", HOOKS]);
  git(dir, ["config", "commit.gpgsign", "false"]);
  git(dir, ["config", "user.name", "Ada"]);
  git(dir, ["config", "user.email", "ada@gitpulse.dev"]);
  writeFileSync(join(dir, "f.txt"), "one\n");
  git(dir, ["add", "--", "f.txt"]);
  return dir;
}

function envWithoutIdentity(extra: Record<string, string> = {}): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...process.env, ...extra };
  for (const key of ["GIT_AUTHOR_NAME", "GIT_AUTHOR_EMAIL", "GIT_COMMITTER_NAME", "GIT_COMMITTER_EMAIL"]) {
    if (!(key in extra)) delete env[key];
  }
  return env;
}

function tryGit(
  dir: string,
  args: string[],
  env: NodeJS.ProcessEnv = envWithoutIdentity(),
): { ok: boolean; stderr: string } {
  try {
    git(dir, args, env);
    return { ok: true, stderr: "" };
  } catch (error) {
    const stderr =
      error instanceof Error && "stderr" in error ? String((error as { stderr?: unknown }).stderr ?? "") : "";
    return { ok: false, stderr };
  }
}

describe.skipIf(!gitAvailable || !hasBash)("fixture author pre-commit hook", () => {
  it("is tracked with the executable bit git hands to every clone", () => {
    for (const hook of ["pre-commit", "pre-merge-commit", "pre-push"]) {
      const entry = execFileSync("git", ["ls-files", "-s", `.githooks/${hook}`], {
        cwd: REPO_ROOT,
        encoding: "utf8",
      }).trim();
      expect(entry, `.githooks/${hook} is not tracked`).not.toBe("");
      expect(entry.split(/\s+/)[0]).toBe("100755");
    }
    const lib = execFileSync("git", ["ls-files", ".githooks/lib/fixture-identity.sh"], {
      cwd: REPO_ROOT,
      encoding: "utf8",
    }).trim();
    expect(lib, "the hooks source .githooks/lib/fixture-identity.sh, which is not tracked").not.toBe("");
  });

  it("refuses the contract identity and the other fixture addresses", () => {
    for (const email of [
      "contract@gitpulse.test",
      "GitPulse@test.local",
      "person@example.invalid",
      "person@example.com",
      "t@t",
      "t@e.com",
      "a@test",
      "a@invalid",
    ]) {
      const dir = initRepo();
      git(dir, ["config", "user.email", email]);
      const result = tryGit(dir, ["commit", "-m", "subject"]);
      expect(result.ok, `${email} was recorded\n${result.stderr}`).toBe(false);
      expect(result.stderr).toContain(email.toLowerCase());
      expect(result.stderr).toContain("test fixture");
    }
  });

  it("refuses a fixture committer even when the author is real", () => {
    const dir = initRepo();
    const result = tryGit(
      dir,
      ["commit", "-m", "subject"],
      envWithoutIdentity({
        GIT_COMMITTER_NAME: "GitPulse Contract",
        GIT_COMMITTER_EMAIL: "contract@gitpulse.test",
      }),
    );
    expect(result.ok, result.stderr).toBe(false);
    expect(result.stderr).toContain("committer");
    expect(result.stderr).toContain("contract@gitpulse.test");
  });

  it("refuses an amend that would keep a fixture author", () => {
    const dir = initRepo();
    git(dir, ["config", "user.name", "GitPulse Contract"]);
    git(dir, ["config", "user.email", "contract@gitpulse.test"]);
    git(dir, ["commit", "--no-verify", "-q", "-m", "fixture"], envWithoutIdentity());
    git(dir, ["config", "user.name", "Ada"]);
    git(dir, ["config", "user.email", "ada@gitpulse.dev"]);
    const result = tryGit(dir, ["commit", "--amend", "-m", "subject"]);
    expect(result.ok, result.stderr).toBe(false);
    expect(result.stderr).toContain("contract@gitpulse.test");
    expect(git(dir, ["log", "-1", "--format=%ae"]).trim()).toBe("contract@gitpulse.test");
  });

  it("lets a real identity commit", () => {
    const dir = initRepo();
    const result = tryGit(
      dir,
      ["commit", "-m", "subject"],
      envWithoutIdentity({
        GIT_AUTHOR_NAME: "Ada",
        GIT_AUTHOR_EMAIL: "ada@gitpulse.dev",
        GIT_COMMITTER_NAME: "Ada",
        GIT_COMMITTER_EMAIL: "ada@gitpulse.dev",
      }),
    );
    expect(result.ok, result.stderr).toBe(true);
    expect(git(dir, ["log", "-1", "--format=%ae"]).trim()).toBe("ada@gitpulse.dev");
  });
});

/** Commits in `dir` as `email`, bypassing pre-commit the way a merge, rebase or old commit does. */
function commitAs(dir: string, file: string, email: string): string {
  writeFileSync(join(dir, file), `${email}\n`);
  git(dir, ["add", "--", file]);
  git(
    dir,
    ["commit", "--no-verify", "-q", "-m", file],
    envWithoutIdentity({
      GIT_AUTHOR_NAME: "Someone",
      GIT_AUTHOR_EMAIL: email,
      GIT_COMMITTER_NAME: "Someone",
      GIT_COMMITTER_EMAIL: email,
    }),
  );
  return git(dir, ["rev-parse", "--short", "HEAD"]).trim();
}

/** A repository with one real commit pushed to a bare `origin`. */
function initPushed(): string {
  const dir = initRepo();
  git(dir, ["commit", "-q", "-m", "base"], envWithoutIdentity());
  const remote = mkdtempSync(join(tmpdir(), "gitpulse-fixture-remote-"));
  dirs.push(remote);
  git(remote, ["init", "-q", "--bare"]);
  git(dir, ["remote", "add", "origin", remote]);
  git(dir, ["push", "-q", "origin", "main"], envWithoutIdentity());
  return dir;
}

describe.skipIf(!gitAvailable || !hasBash)("fixture author merge and push hooks", () => {
  it("refuses a merge commit recorded as a fixture", () => {
    const dir = initRepo();
    git(dir, ["commit", "-q", "-m", "base"], envWithoutIdentity());
    git(dir, ["switch", "-q", "-c", "side"]);
    commitAs(dir, "side.txt", "ada@gitpulse.dev");
    git(dir, ["switch", "-q", "main"]);
    commitAs(dir, "main.txt", "ada@gitpulse.dev");
    git(dir, ["config", "user.email", "contract@gitpulse.test"]);
    const result = tryGit(dir, ["merge", "--no-ff", "-m", "merge", "side"]);
    expect(result.ok, `the fixture merge was recorded\n${result.stderr}`).toBe(false);
    expect(result.stderr).toContain("contract@gitpulse.test");
    expect(git(dir, ["log", "-1", "--format=%s"]).trim()).toBe("main.txt");
  });

  it("refuses to push a fixture commit that bypassed pre-commit, and names it", () => {
    const dir = initPushed();
    const fixture = commitAs(dir, "a.txt", "contract@gitpulse.test");
    commitAs(dir, "b.txt", "ada@gitpulse.dev");
    const result = tryGit(dir, ["push", "origin", "main"]);
    expect(result.ok, `the fixture commit was pushed\n${result.stderr}`).toBe(false);
    expect(result.stderr).toContain("1 commit(s) authored or committed as a test fixture");
    expect(result.stderr).toContain(`${fixture} <contract@gitpulse.test>`);
  });

  it("refuses a fixture committer under a real author", () => {
    const dir = initPushed();
    writeFileSync(join(dir, "c.txt"), "c\n");
    git(dir, ["add", "--", "c.txt"]);
    git(
      dir,
      ["commit", "--no-verify", "-q", "-m", "c"],
      envWithoutIdentity({ GIT_COMMITTER_NAME: "Bot", GIT_COMMITTER_EMAIL: "gitpulse@test.local" }),
    );
    const result = tryGit(dir, ["push", "origin", "main"]);
    expect(result.ok, result.stderr).toBe(false);
    expect(result.stderr).toContain("<gitpulse@test.local>");
  });

  it("refuses a fixture commit on a new branch the remote has never seen", () => {
    const dir = initPushed();
    git(dir, ["switch", "-q", "-c", "topic"]);
    commitAs(dir, "t.txt", "person@example.com");
    const result = tryGit(dir, ["push", "origin", "topic"]);
    expect(result.ok, result.stderr).toBe(false);
    expect(result.stderr).toContain("<person@example.com>");
  });

  it("does not refuse a fixture commit the remote already holds", () => {
    // Commits already published are not this push's to refuse; refusing them
    // would block every later push until history was rewritten.
    const dir = initPushed();
    commitAs(dir, "old.txt", "contract@gitpulse.test");
    git(dir, ["push", "-q", "--no-verify", "origin", "main"], envWithoutIdentity());
    commitAs(dir, "new.txt", "ada@gitpulse.dev");
    const result = tryGit(dir, ["push", "origin", "main"]);
    expect(result.ok, result.stderr).toBe(true);
  });

  it("does not refuse a published fixture commit when pushing to a bare URL", () => {
    // A URL push has no remote-tracking refs; the remote's own tip is what
    // says the commit is already there.
    const dir = initPushed();
    const url = git(dir, ["remote", "get-url", "origin"]).trim();
    commitAs(dir, "old.txt", "contract@gitpulse.test");
    git(dir, ["push", "-q", "--no-verify", url, "main"], envWithoutIdentity());
    git(dir, ["update-ref", "-d", "refs/remotes/origin/main"]);
    commitAs(dir, "new.txt", "ada@gitpulse.dev");
    const result = tryGit(dir, ["push", url, "main"]);
    expect(result.ok, result.stderr).toBe(true);
  });

  it("lets real commits push", () => {
    const dir = initPushed();
    commitAs(dir, "r.txt", "ada@gitpulse.dev");
    const result = tryGit(dir, ["push", "origin", "main"]);
    expect(result.ok, result.stderr).toBe(true);
  });
});
