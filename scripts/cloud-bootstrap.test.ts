import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { chmodSync, copyFileSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, describe, expect, it } from "vitest";

/**
 * The cloud/SSH bootstrap (docs/CLOUD_SESSIONS.md). Hermetic: every run gets a
 * temp HOME, a PATH of system dirs plus a stub dir, and stub `devmap`/`cargo`.
 * The things worth pinning: the hook is silent off-cloud, says "unavailable"
 * rather than nothing when devmap is missing, and setup.sh never reports
 * AVAILABLE for an install that did not happen.
 */
const root = path.resolve(__dirname, "..");
const hook = path.join(root, "scripts/cloud/session-start.sh");
const setup = path.join(root, "scripts/cloud/setup.sh");
const tmps: string[] = [];
afterAll(() => tmps.forEach((d) => rmSync(d, { recursive: true, force: true })));

function sandbox() {
  const dir = mkdtempSync(path.join(tmpdir(), "cloud-bootstrap-"));
  tmps.push(dir);
  const home = path.join(dir, "home");
  const stubs = path.join(dir, "stubs");
  mkdirSync(home);
  mkdirSync(stubs);
  const stub = (name: string, body: string) => {
    const p = path.join(stubs, name);
    writeFileSync(p, `#!/bin/bash\n${body}\n`);
    chmodSync(p, 0o755);
  };
  const run = (script: string, args: string[], env: Record<string, string> = {}, withStubs = true) =>
    spawnSync("/bin/bash", [script, ...args], {
      encoding: "utf8",
      timeout: 60_000,
      env: {
        PATH: `${withStubs ? `${stubs}:` : ""}/usr/bin:/bin`,
        HOME: home,
        DEVMAP_BIN_DIR: path.join(home, "bin"),
        DEVMAP_STATE_DIR: path.join(home, "state"),
        DEVMAP_SYSROOT: path.join(dir, "sysroot"),
        ...env,
      },
    });
  /** A devmap in the sandbox's install dir with a stamp, as setup.sh would leave it. */
  const installDevmap = (body: string, stamp?: string) => {
    mkdirSync(path.join(home, "bin"), { recursive: true });
    mkdirSync(path.join(home, "state"), { recursive: true });
    const p = path.join(home, "bin/devmap");
    writeFileSync(p, `#!/bin/bash\nif [ "$1" = --version ]; then echo "devmap 9.9.9"; exit 0; fi\n${body}\n`);
    chmodSync(p, 0o755);
    if (stamp !== undefined) writeFileSync(path.join(home, "state/stamp"), `${stamp}\n`);
  };
  return { dir, home, stubs, stub, run, installDevmap };
}

const PIN = /^ {2}DEVMAP_REV: ([0-9a-f]{40})$/m.exec(readFileSync(path.join(root, ".github/workflows/ci.yml"), "utf8"))?.[1] ?? "";
const RELEASE_SHA = "d3e36e56efbec1841846af50b431ecebd5b78b6a8c2a521fa664a06f62b99f7b";

/** The additional context, whether emitted as hook JSON or as plain stdout. */
function context(stdout: string): string {
  try {
    return JSON.parse(stdout).hookSpecificOutput.additionalContext as string;
  } catch {
    return stdout;
  }
}

describe("scripts/cloud/session-start.sh", () => {
  it("is silent and exits 0 when CLAUDE_CODE_REMOTE is not true", () => {
    const s = sandbox();
    const envs: Record<string, string>[] = [{}, { CLAUDE_CODE_REMOTE: "false" }, { CLAUDE_CODE_REMOTE: "1" }];
    for (const env of envs) {
      const r = s.run(hook, [], env);
      expect(r.status).toBe(0);
      expect(r.stdout).toBe("");
    }
  });

  it("says DevMap and GitPulse are unavailable when devmap is not installed", () => {
    const s = sandbox();
    const r = s.run(hook, [], { CLAUDE_CODE_REMOTE: "true" });
    expect(r.status).toBe(0);
    const text = context(r.stdout);
    expect(text).toMatch(/UNAVAILABLE/);
    expect(text).toMatch(/source inspection/);
    expect(text).not.toMatch(/DevMap is available/);
    expect(text.length).toBeLessThan(10_000);
  });

  it("runs `devmap build --manifest` from the repo root when devmap is present", () => {
    const s = sandbox();
    const log = path.join(s.dir, "calls.log");
    s.installDevmap(`echo "$PWD :: $*" >> "${log}"`, `source ${PIN}`);
    const r = s.run(hook, [], { CLAUDE_CODE_REMOTE: "true" });
    expect(r.status).toBe(0);
    expect(readFileSync(log, "utf8").trim()).toBe(`${root} :: build --manifest`);
    const text = context(r.stdout);
    expect(text).toMatch(/DevMap is available \(devmap 9\.9\.9\)/);
    expect(text).toMatch(/at the repo's pin/);
    expect(text).toMatch(/gitpulse-mcp .* not available/);
  });

  it("does not call a binary from an old pin, or an unstamped one, available", () => {
    for (const stamp of [`source ${"a".repeat(40)}`, undefined]) {
      const s = sandbox();
      s.installDevmap("true", stamp);
      const text = context(s.run(hook, [], { CLAUDE_CODE_REMOTE: "true" }).stdout);
      expect(text).toMatch(/NOT at the repo's pin/);
      expect(text).not.toMatch(/DevMap is available/);
    }
  });

  it("keeps warning about the stale release, which is never the repo's pin", () => {
    const s = sandbox();
    s.installDevmap("true", `release v0.2.3 ${RELEASE_SHA}`);
    const text = context(s.run(hook, [], { CLAUDE_CODE_REMOTE: "true" }).stdout);
    expect(text).toMatch(/STALE v0\.2\.3 release/);
    expect(text).not.toMatch(/DevMap is available/);
  });

  it("finds a devmap and stamp installed under the root layout", () => {
    const s = sandbox();
    const bin = path.join(s.dir, "sysroot/usr/local/bin");
    const state = path.join(s.dir, "sysroot/usr/local/share/gitpulse-devmap");
    mkdirSync(bin, { recursive: true });
    mkdirSync(state, { recursive: true });
    writeFileSync(path.join(bin, "devmap"), '#!/bin/bash\nif [ "$1" = --version ]; then echo "devmap 9.9.9"; fi\n');
    chmodSync(path.join(bin, "devmap"), 0o755);
    writeFileSync(path.join(state, "stamp"), `source ${PIN}\n`);
    const text = context(s.run(hook, [], { CLAUDE_CODE_REMOTE: "true", DEVMAP_BIN_DIR: "", DEVMAP_STATE_DIR: "" }).stdout);
    expect(text).toMatch(/at the repo's pin/);
  });

  it("an opt-in env var enables it on an SSH host, but SSH_CONNECTION alone does not", () => {
    const s = sandbox();
    s.installDevmap("true", `source ${PIN}`);
    expect(s.run(hook, [], { SSH_CONNECTION: "1.2.3.4 22 5.6.7.8 22" }).stdout).toBe("");
    expect(context(s.run(hook, [], { GITPULSE_SESSION_BOOTSTRAP: "1" }).stdout)).toMatch(/DevMap is available/);
  });

  it("reports a failed build instead of claiming the index is ready", () => {
    const s = sandbox();
    s.installDevmap("echo boom >&2; exit 7", `source ${PIN}`);
    const r = s.run(hook, [], { CLAUDE_CODE_REMOTE: "true" });
    expect(r.status).toBe(0);
    const text = context(r.stdout);
    expect(text).toMatch(/FAILED/);
    expect(text).toMatch(/boom/);
    expect(text).not.toMatch(/DevMap is available/);
  });

  it("reports a timed-out build as a timeout and kills the build's children", () => {
    const s = sandbox();
    const pidFile = path.join(s.dir, "child.pid");
    s.installDevmap(`sleep 30 & echo $! > "${pidFile}"; wait`, `source ${PIN}`);
    const r = s.run(hook, [], { CLAUDE_CODE_REMOTE: "true", DEVMAP_BUILD_TIMEOUT: "1" });
    expect(context(r.stdout)).toMatch(/TIMED OUT after 1s/);
    const pid = Number(readFileSync(pidFile, "utf8").trim());
    expect(() => process.kill(pid, 0)).toThrow();
  });

  it("is registered as the SessionStart hook and keeps the attribution block", () => {
    const settings = JSON.parse(readFileSync(path.join(root, ".claude/settings.json"), "utf8"));
    expect(settings.attribution).toEqual({ commit: "", pr: "", sessionUrl: false });
    const commands = settings.hooks.SessionStart.flatMap((g: { hooks: { command: string }[] }) => g.hooks.map((h) => h.command));
    expect(commands.join("\n")).toContain("scripts/cloud/session-start.sh");
    // Not on clear/compact. `resume` stays: reopening a session whose cloud VM was reclaimed
    // starts a fresh VM with no index, and the cloud-environments docs use this same matcher.
    // 180 s leaves margin over the 120 s build bound.
    const group = settings.hooks.SessionStart[0];
    expect(group.matcher).toBe("startup|resume");
    expect(group.hooks[0].timeout).toBe(180);
  });
});

describe("scripts/cloud/setup.sh", () => {
  const pin = /^ {2}DEVMAP_REV: ([0-9a-f]{40})$/m.exec(readFileSync(path.join(root, ".github/workflows/ci.yml"), "utf8"))?.[1];

  it("finds the pin in ci.yml", () => expect(pin).toBeDefined());

  it("exits 0 with UNAVAILABLE (never AVAILABLE) when cargo is missing, and 1 under --strict", () => {
    const s = sandbox();
    const r = s.run(setup, [], {}, false);
    expect(r.status).toBe(0);
    const last = r.stdout.trim().split("\n").at(-1) ?? "";
    expect(last).toMatch(/^devmap: UNAVAILABLE: cargo not found/);
    expect(r.stdout).not.toMatch(/devmap: AVAILABLE/);
    expect(r.stdout).toMatch(/gitpulse-mcp: not available on Linux/);
    expect(existsSync(path.join(s.home, "bin/devmap"))).toBe(false);
    expect(s.run(setup, ["--strict"], {}, false).status).toBe(1);
  });

  it("does not use the stale release without the opt-in", () => {
    const s = sandbox();
    const r = s.run(setup, [], {}, false);
    expect(r.stdout).toMatch(/not used unless --allow-stale-release/);
  });

  it("installs from the ci.yml pin with --locked, and a second run does not rebuild", () => {
    const s = sandbox();
    const log = path.join(s.dir, "cargo.log");
    s.stub(
      "cargo",
      `echo "$*" >> "${log}"\nroot=""\nwhile [ $# -gt 0 ]; do [ "$1" = --root ] && root="$2"; shift; done\nmkdir -p "$root/bin"\nprintf '#!/bin/bash\\necho "devmap 9.9.9"\\n' > "$root/bin/devmap"\nchmod +x "$root/bin/devmap"`,
    );
    const first = s.run(setup, []);
    expect(first.status).toBe(0);
    expect(first.stdout.trim().split("\n").at(-1)).toMatch(/^devmap: AVAILABLE devmap 9\.9\.9 .*built from source at pinned rev/);
    const calls = readFileSync(log, "utf8");
    expect(calls).toContain(`--rev ${pin}`);
    expect(calls).toContain("--locked");
    expect(calls).toContain("devmap-cli");

    const second = s.run(setup, []);
    expect(second.stdout.trim().split("\n").at(-1)).toMatch(/already installed at pinned rev/);
    expect(readFileSync(log, "utf8").trim().split("\n")).toHaveLength(1);
  });

  it("reports UNAVAILABLE when the cargo build fails", () => {
    const s = sandbox();
    s.stub("cargo", "echo 'segfault in rustc' >&2\nexit 101");
    const r = s.run(setup, []);
    expect(r.status).toBe(0);
    const last = r.stdout.trim().split("\n").at(-1) ?? "";
    expect(last).toMatch(/^devmap: UNAVAILABLE: cargo install .* failed with exit status 101/);
    // What happened, not a guess: no timeout claim, no network guess, the real stderr tail.
    expect(last).not.toMatch(/timed out/);
    expect(last).not.toMatch(/network|github\.com/i);
    expect(last).toContain("segfault in rustc");
    expect(existsSync(path.join(s.home, "bin/devmap"))).toBe(false);
  });

  it("bounds the stderr tail it reports to 5 lines and 600 characters", () => {
    const s = sandbox();
    s.stub("cargo", `for i in 1 2 3 4 5 6 7 8; do echo "line$i ${"x".repeat(200)}" >&2; done\nexit 1`);
    const last = (s.run(setup, []).stdout.trim().split("\n").at(-1) ?? "");
    const tail = (last.split("last output: ")[1] ?? "").split("; the stale")[0];
    expect(tail.length).toBeLessThanOrEqual(600);
    expect(last).not.toContain("line1 ");
    expect(last).toContain("line");
  });

  it("names a network error only when the output shows one", () => {
    const s = sandbox();
    s.stub("cargo", "echo 'fatal: unable to access https://github.com/x: Could not resolve host: github.com' >&2\nexit 101");
    expect(s.run(setup, []).stdout).toMatch(/output shows a network error/);
  });

  it("still says timed out when the build overruns its timeout", () => {
    const s = sandbox();
    s.stub("cargo", "exec sleep 30");
    const r = s.run(setup, [], { DEVMAP_INSTALL_TIMEOUT: "1" });
    expect(r.status).toBe(0);
    expect(r.stdout.trim().split("\n").at(-1)).toMatch(/^devmap: UNAVAILABLE: cargo install .* timed out after 1s \(exit 124\)/);
  });

  it("a second --allow-stale-release run with the release already stamped downloads nothing", () => {
    const s = sandbox();
    const log = path.join(s.dir, "curl.log");
    s.stub("curl", `echo "$*" >> "${log}"\nexit 22`);
    mkdirSync(path.join(s.home, "bin"));
    mkdirSync(path.join(s.home, "state"));
    const bin = path.join(s.home, "bin/devmap");
    writeFileSync(bin, '#!/bin/bash\necho "devmap 0.2.3"\n');
    chmodSync(bin, 0o755);
    writeFileSync(path.join(s.home, "state/stamp"), `release v0.2.3 ${RELEASE_SHA}\n`);
    const r = s.run(setup, ["--allow-stale-release"]);
    expect(r.status).toBe(0);
    expect(r.stdout.trim().split("\n").at(-1)).toMatch(/^devmap: AVAILABLE devmap 0\.2\.3 .*STALE release v0\.2\.3 already installed/);
    expect(existsSync(log), "curl must not be called").toBe(false);
    expect(r.stderr).not.toMatch(/downloading/);
  });

  it("a stamped stale release is not reused without the opt-in", () => {
    const s = sandbox();
    mkdirSync(path.join(s.home, "bin"));
    mkdirSync(path.join(s.home, "state"));
    const bin = path.join(s.home, "bin/devmap");
    writeFileSync(bin, '#!/bin/bash\necho "devmap 0.2.3"\n');
    chmodSync(bin, 0o755);
    writeFileSync(path.join(s.home, "state/stamp"), `release v0.2.3 ${RELEASE_SHA}\n`);
    const r = s.run(setup, []);
    expect(r.stdout.trim().split("\n").at(-1)).toMatch(/^devmap: UNAVAILABLE/);
  });

  it("rebuilds when the pin changes", () => {
    const s = sandbox();
    const log = path.join(s.dir, "cargo.log");
    s.stub("cargo", `echo "$*" >> "${log}"\nroot=""\nwhile [ $# -gt 0 ]; do [ "$1" = --root ] && root="$2"; shift; done\nmkdir -p "$root/bin"\nprintf '#!/bin/bash\\necho "devmap 9.9.9"\\n' > "$root/bin/devmap"\nchmod +x "$root/bin/devmap"`);
    s.run(setup, [], { DEVMAP_REV: "a".repeat(40) });
    s.run(setup, [], { DEVMAP_REV: "b".repeat(40) });
    expect(readFileSync(log, "utf8").trim().split("\n")).toHaveLength(2);
  });

  it("refuses a malformed pin rather than installing something unpinned", () => {
    const s = sandbox();
    const r = s.run(setup, [], { DEVMAP_REV: "main" });
    expect(r.stdout).toMatch(/UNAVAILABLE: could not resolve a 40-character DEVMAP_REV/);
  });

  const cargoOk = `root=""\nwhile [ $# -gt 0 ]; do [ "$1" = --root ] && root="$2"; shift; done\nmkdir -p "$root/bin"\nprintf '#!/bin/bash\\necho "devmap 9.9.9"\\n' > "$root/bin/devmap"\nchmod +x "$root/bin/devmap"`;
  const lastLine = (out: string) => out.trim().split("\n").at(-1) ?? "";

  it("keeps its exit-0 contract with no HOME at all", () => {
    const r = spawnSync("/bin/bash", [setup], { encoding: "utf8", timeout: 60_000, env: { PATH: "/usr/bin:/bin" } });
    expect(r.status).toBe(0);
    expect(r.stderr).not.toMatch(/unbound variable/);
    expect(lastLine(r.stdout)).toMatch(/^devmap: UNAVAILABLE/);
  });

  it("turns an unwritable install dir into an UNAVAILABLE line, and --strict into exit 1", () => {
    const s = sandbox();
    s.stub("cargo", cargoOk);
    const blocker = path.join(s.dir, "file");
    writeFileSync(blocker, "x");
    const env = { DEVMAP_BIN_DIR: path.join(blocker, "bin") };
    const r = s.run(setup, [], env);
    expect(r.status).toBe(0);
    expect(lastLine(r.stdout)).toMatch(/^devmap: UNAVAILABLE: cannot create/);
    expect(s.run(setup, ["--strict"], env).status).toBe(1);
  });

  it("as root installs a real 0755 copy and a world-readable stamp, not a symlink into root's home", () => {
    const s = sandbox();
    s.stub("cargo", cargoOk);
    s.stub("id", 'echo 0');
    const r = s.run(setup, [], { DEVMAP_BIN_DIR: "", DEVMAP_STATE_DIR: "" });
    expect(r.status).toBe(0);
    const bin = path.join(s.dir, "sysroot/usr/local/bin/devmap");
    const stamp = path.join(s.dir, "sysroot/usr/local/share/gitpulse-devmap/stamp");
    expect(lastLine(r.stdout)).toMatch(/^devmap: AVAILABLE/);
    expect(lstatSync(bin).isSymbolicLink()).toBe(false);
    expect(lstatSync(bin).mode & 0o777).toBe(0o755);
    expect(lstatSync(stamp).mode & 0o777).toBe(0o644);
    expect(readFileSync(stamp, "utf8").trim()).toBe(`source ${PIN}`);
    expect(existsSync(path.join(s.home, ".local"))).toBe(false);
  });

  it("says where the pin came from, and an explicit DEVMAP_REV wins", () => {
    const s = sandbox();
    s.stub("cargo", cargoOk);
    expect(lastLine(s.run(setup, []).stdout)).toContain(`pin ${PIN.slice(0, 12)} from repo ci.yml`);
    const s2 = sandbox();
    s2.stub("cargo", cargoOk);
    expect(lastLine(s2.run(setup, [], { DEVMAP_REV: "e".repeat(40) }).stdout)).toContain("pin eeeeeeeeeeee from env DEVMAP_REV");
  });

  it("labels a pin fetched from GitHub main when the repo is not present", () => {
    const s = sandbox();
    s.stub("cargo", cargoOk);
    s.stub("curl", `echo "  DEVMAP_REV: ${"d".repeat(40)}"`);
    const pasted = path.join(s.dir, "pasted/setup.sh");
    mkdirSync(path.dirname(pasted));
    copyFileSync(setup, pasted);
    expect(lastLine(s.run(pasted, []).stdout)).toContain("pin dddddddddddd from github main ci.yml");
  });

  describe("the stale release", () => {
    const linuxX64 = (s: ReturnType<typeof sandbox>, payload: string) => {
      s.stub("uname", 'case "$1" in -s) echo Linux;; -m) echo x86_64;; *) echo Linux;; esac');
      s.stub("xz", "exit 0");
      s.stub("curl", `while [ $# -gt 0 ]; do [ "$1" = -o ] && out="$2"; shift; done\nprintf '%s' '${payload}' > "$out"`);
      s.stub("tar", `while [ $# -gt 0 ]; do [ "$1" = -C ] && d="$2"; shift; done\nprintf '#!/bin/bash\\necho "devmap 0.2.3"\\n' > "$d/devmap"`);
    };

    it("refuses a download whose sha256 is not the pinned value, and installs nothing", () => {
      const s = sandbox();
      linuxX64(s, "tampered");
      const r = s.run(setup, ["--allow-stale-release"]);
      expect(r.status).toBe(0);
      expect(lastLine(r.stdout)).toMatch(/^devmap: UNAVAILABLE: release sha256 does not match the pinned value/);
      expect(existsSync(path.join(s.home, "bin/devmap"))).toBe(false);
    });

    it("installs when the sha256 matches the pinned value", () => {
      const s = sandbox();
      linuxX64(s, "payload");
      const sha = createHash("sha256").update("payload").digest("hex");
      const r = s.run(setup, ["--allow-stale-release"], { DEVMAP_RELEASE_URL: "https://example.invalid/x.tar.xz", DEVMAP_RELEASE_SHA256: sha });
      expect(lastLine(r.stdout)).toMatch(/^devmap: AVAILABLE .*sha256 matches the pinned value/);
    });

    it("will not repoint the URL without an explicit hash", () => {
      const s = sandbox();
      linuxX64(s, "payload");
      const r = s.run(setup, ["--allow-stale-release"], { DEVMAP_RELEASE_URL: "https://example.invalid/x.tar.xz" });
      expect(lastLine(r.stdout)).toMatch(/UNAVAILABLE: DEVMAP_RELEASE_URL is refused without an explicit DEVMAP_RELEASE_SHA256/);
    });
  });

  it("kills the build's children, not just cargo, when it overruns", () => {
    const s = sandbox();
    const pidFile = path.join(s.dir, "child.pid");
    s.stub("cargo", `sleep 30 &\necho $! > "${pidFile}"\nwait`);
    const r = s.run(setup, [], { DEVMAP_INSTALL_TIMEOUT: "1" });
    expect(lastLine(r.stdout)).toMatch(/timed out after 1s/);
    expect(() => process.kill(Number(readFileSync(pidFile, "utf8").trim()), 0)).toThrow();
  });

  it("the devcontainer setup calls the same installer", () => {
    expect(readFileSync(path.join(root, ".devcontainer/setup.sh"), "utf8")).toContain("scripts/cloud/setup.sh");
  });
});
