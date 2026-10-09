/**
 * A fixture identity written into this checkout's local git config becomes
 * the author of every later commit. The pre-commit hook is what stops that
 * commit from landing, and an unrun hook looks the same as a passing one, so
 * this drives the real hook against a real `git commit`.
 *
 * The denylist matches `GitWriter::is_fixture_author_email`.
 */
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
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

const LIB = join(HOOKS, "lib");

/** Runs a command, returning its exit status and output instead of throwing. */
function run(
  cwd: string,
  program: string,
  args: string[],
  input?: string,
): { status: number; stdout: string; stderr: string } {
  try {
    const stdout = execFileSync(program, args, {
      cwd,
      encoding: "utf8",
      env: envWithoutIdentity(),
      input,
      maxBuffer: 64 * 1024 * 1024,
    });
    return { status: 0, stdout, stderr: "" };
  } catch (error) {
    const e = error as { status?: number; stdout?: unknown; stderr?: unknown };
    return { status: e.status ?? -1, stdout: String(e.stdout ?? ""), stderr: String(e.stderr ?? "") };
  }
}

describe.skipIf(!hasBash)("the fixture address rule", () => {
  it("agrees with every row of the table GitWriter is tested against", () => {
    // fixture-identity.cases is read by GitWriter's Rust test too; this runs
    // the shell rule over the same rows, so the two cannot drift apart.
    const table = readFileSync(join(LIB, "fixture-identity.cases"), "utf8");
    const rows = table
      .split("\n")
      .filter((line) => line !== "" && !line.startsWith("#"))
      .map((line) => {
        const tab = line.indexOf("\t");
        expect(tab, `no tab in ${JSON.stringify(line)}`).toBeGreaterThan(0);
        const raw = line.slice(tab + 1);
        const quoted = raw.length >= 2 && raw.startsWith('"') && raw.endsWith('"');
        return { label: line.slice(0, tab), email: quoted ? raw.slice(1, -1) : raw };
      });
    expect(rows.filter((row) => row.label === "fixture").length).toBeGreaterThanOrEqual(20);
    expect(rows.filter((row) => row.label === "person").length).toBeGreaterThanOrEqual(10);
    // The quoting is what keeps these rows alive through an editor.
    expect(rows.some((row) => row.email === "")).toBe(true);
    expect(rows.some((row) => row.email !== row.email.trim() && row.email.trim() !== "")).toBe(true);
    // One shell for every row: each answer is printed on its own line.
    const script = `. "${LIB}/fixture-identity.sh"
while IFS= read -r email; do
  if is_fixture_email "$email"; then echo fixture; else echo person; fi
done`;
    const result = run(REPO_ROOT, "bash", ["-c", script], rows.map((row) => row.email).join("\n") + "\n");
    expect(result.status, result.stderr).toBe(0);
    const answers = result.stdout.trimEnd().split("\n");
    expect(answers.length).toBe(rows.length);
    const wrong = rows
      .map((row, i) => ({ ...row, got: answers[i] }))
      .filter((row) => row.got !== row.label)
      .map((row) => `${JSON.stringify(row.email)}: expected ${row.label}, got ${row.got}`);
    expect(wrong).toEqual([]);
  });
});

describe.skipIf(!gitAvailable || !hasBash)("fixture-commits.sh", () => {
  const script = join(LIB, "fixture-commits.sh");

  it("exits 0 for a clean range, 1 naming each fixture commit, 2 for a range it could not read", () => {
    const dir = initPushed();
    expect(run(dir, "bash", [script]).status).toBe(0);
    const bad = commitAs(dir, "a.txt", "contract@gitpulse.test");
    const found = run(dir, "bash", [script]);
    expect(found.status).toBe(1);
    expect(found.stdout).toBe(`${bad} <contract@gitpulse.test> <contract@gitpulse.test>\n`);
    for (const args of [["no-such-rev"], ["HEAD", "no-such-rev"], ["--output=/tmp/gitpulse-x"], ["HEAD", "--all"]]) {
      const result = run(dir, "bash", [script, ...args]);
      expect(result.status, `${args.join(" ")} was read as a range`).toBe(2);
      expect(result.stderr).toContain("is not a commit");
    }
  });

  it("reads an address holding a space or a separator-like character as one field", () => {
    const dir = initPushed();
    commitAs(dir, "a.txt", "two words@ci.test");
    commitAs(dir, "b.txt", "pipe|semi;colon@gitpulse.dev");
    const result = run(dir, "bash", [script]);
    expect(result.status).toBe(1);
    expect(result.stdout).toContain("<two words@ci.test> <two words@ci.test>");
    expect(result.stdout).not.toContain("gitpulse.dev");
  });

  it("checks merge commits, not only the commits they join", () => {
    const dir = initPushed();
    git(dir, ["switch", "-q", "-c", "side"]);
    commitAs(dir, "s.txt", "ada@gitpulse.dev");
    git(dir, ["switch", "-q", "main"]);
    commitAs(dir, "m.txt", "ada@gitpulse.dev");
    git(
      dir,
      ["merge", "--no-verify", "--no-ff", "-q", "-m", "merge", "side"],
      envWithoutIdentity({
        GIT_AUTHOR_NAME: "Contract",
        GIT_AUTHOR_EMAIL: "contract@gitpulse.test",
        GIT_COMMITTER_NAME: "Contract",
        GIT_COMMITTER_EMAIL: "contract@gitpulse.test",
      }),
    );
    const merge = git(dir, ["rev-parse", "--short", "HEAD"]).trim();
    const result = run(dir, "bash", [script]);
    expect(result.status).toBe(1);
    expect(result.stdout.trim().split("\n")).toEqual([
      `${merge} <contract@gitpulse.test> <contract@gitpulse.test>`,
    ]);
  });

  it("finds the one fixture commit in a five-thousand-commit history, quickly", () => {
    // fast-import writes the history in one process; the check walks all of it.
    const dir = mkdtempSync(join(tmpdir(), "gitpulse-fixture-large-"));
    dirs.push(dir);
    git(dir, ["init", "-q", "-b", "main"]);
    const total = 5000;
    const planted = 3217;
    let stream = "";
    for (let i = 1; i <= total; i++) {
      const who = i === planted ? "Contract <contract@gitpulse.test>" : `Dev ${i} <dev${i}@gitpulse.dev>`;
      const message = `c${i}`;
      stream += `commit refs/heads/main\nmark :${i}\nauthor ${who} ${1_700_000_000 + i} +0000\ncommitter Dev <dev@gitpulse.dev> ${1_700_000_000 + i} +0000\ndata ${message.length}\n${message}\n`;
      if (i > 1) stream += `from :${i - 1}\n`;
      stream += `M 644 inline f.txt\ndata ${String(i).length + 1}\n${i}\n\n`;
    }
    execFileSync("git", ["fast-import", "--quiet"], { cwd: dir, input: stream });
    git(dir, ["reset", "-q", "--hard", "main"]);
    const started = performance.now();
    const result = run(dir, "bash", [script, "HEAD", "--remotes"]);
    const elapsed = performance.now() - started;
    expect(result.status).toBe(1);
    const lines = result.stdout.trim().split("\n");
    expect(lines).toHaveLength(1);
    expect(git(dir, ["log", "-1", "--format=%s", lines[0].split(" ")[0]]).trim()).toBe(`c${planted}`);
    expect(elapsed, `took ${Math.round(elapsed)} ms`).toBeLessThan(15_000);
  }, 60_000);
});

// The multi-ref and long-history cases spawn hundreds of git processes. Their
// limits are set per test, well above what they take, because vitest's 5 s
// default is shorter than a loaded machine needs (measured: 5.4 s at load 28).
describe.skipIf(!gitAvailable || !hasBash)("pre-push across remotes, tags and many refs", () => {
  it("lets a published fixture commit reach a second remote", () => {
    // Excluding only the pushed-to remote's refs would refuse every push to a
    // new remote forever: the fixture commits published before the check
    // existed are history now.
    const dir = initPushed();
    commitAs(dir, "old.txt", "contract@gitpulse.test");
    git(dir, ["push", "-q", "--no-verify", "origin", "main"], envWithoutIdentity());
    const mirror = mkdtempSync(join(tmpdir(), "gitpulse-fixture-mirror-"));
    dirs.push(mirror);
    git(mirror, ["init", "-q", "--bare"]);
    git(dir, ["remote", "add", "mirror", mirror]);
    const result = tryGit(dir, ["push", "mirror", "main"]);
    expect(result.ok, result.stderr).toBe(true);
  });

  it("refuses an annotated tag whose tagger is a fixture, and allows one by a person", () => {
    const dir = initPushed();
    git(
      dir,
      ["tag", "-a", "fixture-tag", "-m", "t"],
      envWithoutIdentity({ GIT_COMMITTER_NAME: "Contract", GIT_COMMITTER_EMAIL: "contract@gitpulse.test" }),
    );
    const refused = tryGit(dir, ["push", "origin", "refs/tags/fixture-tag"]);
    expect(refused.ok, refused.stderr).toBe(false);
    expect(refused.stderr).toContain("tagger is the test fixture <contract@gitpulse.test>");
    git(dir, ["tag", "-a", "real-tag", "-m", "t"], envWithoutIdentity());
    const allowed = tryGit(dir, ["push", "origin", "refs/tags/real-tag"]);
    expect(allowed.ok, allowed.stderr).toBe(true);
  });

  it("names only the refs that carry a fixture commit when many are pushed at once", () => {
    const dir = initPushed();
    for (let i = 0; i < 40; i++) {
      git(dir, ["branch", `b${i}`, "main"]);
    }
    git(dir, ["switch", "-q", "b17"]);
    commitAs(dir, "x.txt", "contract@gitpulse.test");
    git(dir, ["switch", "-q", "b31"]);
    commitAs(dir, "y.txt", "ada@gitpulse.dev");
    git(dir, ["switch", "-q", "main"]);
    const result = tryGit(dir, ["push", "origin", "--all"]);
    expect(result.ok).toBe(false);
    expect(result.stderr).toContain("refs/heads/b17 would push 1 commit(s)");
    expect(result.stderr.match(/would push/g)).toHaveLength(1);
  }, 30_000);

  it("lists twenty fixture commits and counts the rest", () => {
    const dir = initPushed();
    for (let i = 0; i < 25; i++) {
      commitAs(dir, `f${i}.txt`, "contract@gitpulse.test");
    }
    const result = tryGit(dir, ["push", "origin", "main"]);
    expect(result.ok).toBe(false);
    expect(result.stderr).toContain("would push 25 commit(s)");
    expect(result.stderr).toContain("… and 5 more");
    expect(result.stderr.match(/<contract@gitpulse\.test> <contract@gitpulse\.test>/g)).toHaveLength(20);
  }, 30_000);

  it("refuses rather than passes when the pushed commit cannot be read", () => {
    // A ref pointing at an object this repository does not have is not a
    // range with no fixture commits in it.
    const dir = initPushed();
    const result = run(
      dir,
      "bash",
      [join(HOOKS, "pre-push"), "origin", "unused"],
      `refs/heads/main ${"1".repeat(40)} refs/heads/main ${"0".repeat(40)}\n`,
    );
    expect(result.status).toBe(1);
    expect(result.stderr).toContain("could not list the commits refs/heads/main would push");
  });
});
