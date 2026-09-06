import { execFileSync } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * Tauri's `universal-apple-darwin` build lipos ONLY `mainBinaryName`, while its
 * macOS bundler copies EVERY `[[bin]]` declared in Cargo.toml into the .app.
 * `scripts/bin/lipo` closes that gap by piggy-backing on the main binary's lipo
 * and building the rest — which makes its list of binaries load-bearing for the
 * macOS release artifact.
 *
 * That list used to be written down (`for helper in gitpulsed gitpulse-mcp`),
 * and a third binary added later was not added to it. Nothing local caught it:
 * a plain `tauri build` targets one architecture, so every binary lands in the
 * same directory and the shim is never needed. Only the universal build — which
 * only the release workflow runs — can see it, and it reports the missing FILE
 * rather than the stale list.
 *
 * So the list has to be derived, and this proves it is derived by driving the
 * shim with a recorder in place of the real lipo and checking that every binary
 * the bundler will ask for actually gets built.
 */
const REPO_ROOT = fileURLToPath(new URL("..", import.meta.url));
const shimPath = join(REPO_ROOT, "scripts/bin/lipo");
const shim = readFileSync(shimPath, "utf8");
const cargoToml = readFileSync(join(REPO_ROOT, "src-tauri/Cargo.toml"), "utf8");
const tauriRunner = readFileSync(join(REPO_ROOT, "scripts/tauri.mjs"), "utf8");

/** Every `[[bin]]` the macOS bundler will copy, in manifest order. */
const declaredBinaries = [...cargoToml.matchAll(/^\[\[bin\]\]\s*\nname\s*=\s*"([^"]+)"/gm)].map(
  (match) => match[1],
);
const MAIN_BINARY = "gitpulse";

/** A stand-in for the real lipo that records its arguments and touches -output. */
function withRecorder<T>(run: (dir: string, env: NodeJS.ProcessEnv) => T): T {
  const dir = mkdtempSync(join(tmpdir(), "gitpulse-lipo-"));
  try {
    const recorder = join(dir, "recorder");
    writeFileSync(
      recorder,
      [
        "#!/usr/bin/env bash",
        'echo "$@" >> "$RECORD"',
        'args=("$@"); out=""',
        'for ((i=0;i<${#args[@]};i++)); do [ "${args[i]}" = "-output" ] && out="${args[i+1]}"; done',
        '[ -n "$out" ] && : > "$out"',
        "exit 0",
        "",
      ].join("\n"),
    );
    chmodSync(recorder, 0o755);
    for (const arch of ["aarch64-apple-darwin", "x86_64-apple-darwin", "universal-apple-darwin"]) {
      mkdirSync(join(dir, arch, "release"), { recursive: true });
    }
    return run(dir, { RECORD: join(dir, "log"), GITPULSE_REAL_LIPO: recorder });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

function archBinary(dir: string, arch: string, name: string): string {
  return join(dir, `${arch}-apple-darwin`, "release", name);
}

/** The invocation Tauri makes for the main binary, which the shim hooks. */
function mainLipoArgs(dir: string): string[] {
  return [
    "-create",
    "-output",
    archBinary(dir, "universal", MAIN_BINARY),
    archBinary(dir, "aarch64", MAIN_BINARY),
    archBinary(dir, "x86_64", MAIN_BINARY),
  ];
}

describe("universal-build lipo shim", () => {
  it("declares more than one binary, or this shim has nothing to guard", () => {
    // Guards the test itself: if the manifest parse broke, every assertion
    // below would pass vacuously.
    expect(declaredBinaries).toContain(MAIN_BINARY);
    expect(declaredBinaries.length).toBeGreaterThan(1);
  });

  it("is tracked with the executable bit git will hand to every clone", () => {
    // Read from git, not the filesystem: a Windows checkout stats 0644 for a
    // file git considers executable.
    const entry = execFileSync("git", ["ls-files", "-s", "scripts/bin/lipo"], {
      cwd: REPO_ROOT,
      encoding: "utf8",
    }).trim();
    expect(entry, "scripts/bin/lipo is not tracked").not.toBe("");
    expect(entry.split(/\s+/)[0]).toBe("100755");
  });

  it("is actually reached by the build that needs it", () => {
    // The shim only runs because scripts/tauri.mjs puts its directory first on
    // PATH for darwin. Drop that and the shim is dead code and the macOS
    // release breaks again, silently.
    expect(tauriRunner).toMatch(/scripts["'],\s*["']bin|scripts\/bin/);
    expect(tauriRunner).toContain('process.platform === "darwin"');
    expect(tauriRunner).toMatch(/env\.PATH\s*=/);
  });

  it("names no binary of its own, so the list cannot go stale", () => {
    // The exact regression: a hand-kept list that a new [[bin]] never joined.
    // Comments are stripped first — the invariant is that no binary name is
    // reachable as DATA; documenting the incident that caused this is fine.
    const code = shim
      .split("\n")
      .filter((line) => !/^\s*#/.test(line))
      .join("\n");
    for (const name of declaredBinaries.filter((bin) => bin !== MAIN_BINARY)) {
      expect(code, `scripts/bin/lipo hardcodes "${name}" instead of reading Cargo.toml`).not.toContain(
        name,
      );
    }
    expect(code, "the shim no longer reads the manifest").toContain("Cargo.toml");
  });

  describe.skipIf(process.platform === "win32")("driven with a recorder", () => {
    it("builds a universal binary for every [[bin]] the bundler will copy", () => {
      const recorded = withRecorder((dir, env) => {
        for (const name of declaredBinaries) {
          for (const arch of ["aarch64", "x86_64"]) writeFileSync(archBinary(dir, arch, name), "");
        }
        execFileSync(shimPath, mainLipoArgs(dir), { env: { ...process.env, ...env } });
        return readFileSync(env.RECORD as string, "utf8").trim().split("\n");
      });

      // One lipo per declared binary — the main one Tauri asked for, plus the
      // auxiliaries Tauri skips, each stitched from both architectures.
      expect(recorded).toHaveLength(declaredBinaries.length);
      for (const name of declaredBinaries) {
        const line = recorded.find((entry) => entry.includes(`/universal-apple-darwin/release/${name}`));
        expect(line, `no universal binary was built for "${name}"`).toBeTruthy();
        expect(line).toContain(`/aarch64-apple-darwin/release/${name}`);
        expect(line).toContain(`/x86_64-apple-darwin/release/${name}`);
      }
    });

    it("fails loudly when a binary the bundler needs was never built", () => {
      // Skipping quietly is what turned a stale list into a bundler error that
      // named a file instead of a cause.
      const missing = declaredBinaries.find((bin) => bin !== MAIN_BINARY) as string;
      const result = withRecorder((dir, env) => {
        for (const name of declaredBinaries) {
          for (const arch of ["aarch64", "x86_64"]) {
            if (name === missing && arch === "x86_64") continue;
            writeFileSync(archBinary(dir, arch, name), "");
          }
        }
        try {
          execFileSync(shimPath, mainLipoArgs(dir), {
            env: { ...process.env, ...env },
            stdio: ["ignore", "pipe", "pipe"],
          });
          return { status: 0, stderr: "" };
        } catch (error) {
          const failure = error as { status?: number; stderr?: Buffer };
          return { status: failure.status ?? -1, stderr: String(failure.stderr ?? "") };
        }
      });
      expect(result.status).not.toBe(0);
      expect(result.stderr).toContain(missing);
    });

    it("leaves a lipo call that is not the main binary alone", () => {
      // The shim shadows `lipo` for the whole build; anything else that calls
      // it must pass straight through.
      const recorded = withRecorder((dir, env) => {
        const other = join(dir, "unrelated");
        writeFileSync(other, "");
        execFileSync(shimPath, ["-create", "-output", join(dir, "out-unrelated"), other], {
          env: { ...process.env, ...env },
        });
        return readFileSync(env.RECORD as string, "utf8").trim().split("\n");
      });
      expect(recorded).toHaveLength(1);
    });
  });
});
