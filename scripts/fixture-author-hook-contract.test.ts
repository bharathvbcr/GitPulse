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
    const entry = execFileSync("git", ["ls-files", "-s", ".githooks/pre-commit"], {
      cwd: REPO_ROOT,
      encoding: "utf8",
    }).trim();
    expect(entry, ".githooks/pre-commit is not tracked").not.toBe("");
    expect(entry.split(/\s+/)[0]).toBe("100755");
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
