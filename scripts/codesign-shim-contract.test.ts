import { execFileSync, spawnSync } from "node:child_process";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * Tauri copies every `[[bin]]` into Contents/MacOS and then codesigns them
 * in manifest order — main binary first. Once Info.plist is in the bundle,
 * codesign treats that file as the bundle executable, and every other Mach-O
 * in the same directory is nested code that must already be signed. `lipo`
 * produces unsigned helpers, so the main-binary sign dies with
 * `code object is not signed at all / In subcomponent: <helper>`.
 *
 * `scripts/bin/codesign` closes that by piggy-backing on the main binary's
 * sign, using the same identity and flags, for every sibling Cargo.toml
 * declares. The list is derived, never written down — a fourth binary must not
 * be able to recreate this the way `gitpulse-hook` recreated the lipo miss.
 *
 * Nothing local `tauri build` (single architecture) can see: linker ad-hoc
 * signatures survive a thin copy. Only the universal release artifact lipos
 * the helpers and then asks codesign to sign the main binary first.
 */
const REPO_ROOT = fileURLToPath(new URL("..", import.meta.url));
const shimPath = join(REPO_ROOT, "scripts/bin/codesign");
const shim = readFileSync(shimPath, "utf8");
const cargoToml = readFileSync(join(REPO_ROOT, "src-tauri/Cargo.toml"), "utf8");
const tauriConf = readFileSync(join(REPO_ROOT, "src-tauri/tauri.conf.json"), "utf8");
const tauriRunner = readFileSync(join(REPO_ROOT, "scripts/tauri.mjs"), "utf8");

const declaredBinaries = [...cargoToml.matchAll(/^\[\[bin\]\]\s*\nname\s*=\s*"([^"]+)"/gm)].map(
  (match) => match[1],
);
const MAIN_BINARY = /"mainBinaryName"\s*:\s*"([^"]+)"/.exec(tauriConf)?.[1] ?? "";
const helpers = declaredBinaries.filter((bin) => bin !== MAIN_BINARY);

function withRecorder<T>(run: (dir: string, env: NodeJS.ProcessEnv) => T): T {
  const dir = mkdtempSync(join(tmpdir(), "gitpulse-codesign-"));
  try {
    const recorder = join(dir, "recorder");
    writeFileSync(
      recorder,
      ["#!/usr/bin/env bash", 'echo "$@" >> "$RECORD"', "exit 0", ""].join("\n"),
    );
    chmodSync(recorder, 0o755);
    return run(dir, { RECORD: join(dir, "log"), GITPULSE_REAL_CODESIGN: recorder });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

function signArgs(path: string): string[] {
  // The exact vector tauri-macos-sign uses for an ad-hoc identity.
  return ["--force", "-s", "-", path];
}

function writeSiblingBins(dir: string, names: string[]): void {
  for (const name of names) writeFileSync(join(dir, name), "");
}

describe("universal-build codesign shim", () => {
  it("declares a main binary and at least one helper, or this shim has nothing to guard", () => {
    expect(MAIN_BINARY).not.toBe("");
    expect(declaredBinaries).toContain(MAIN_BINARY);
    expect(helpers.length).toBeGreaterThan(0);
  });

  it("is executable, and 100755 once git tracks it", () => {
    const entry = execFileSync("git", ["ls-files", "-s", "scripts/bin/codesign"], {
      cwd: REPO_ROOT,
      encoding: "utf8",
    }).trim();
    if (entry) {
      expect(entry.split(/\s+/)[0]).toBe("100755");
    } else {
      expect(statSync(shimPath).mode & 0o111, "scripts/bin/codesign is not executable").not.toBe(0);
    }
  });

  it("is actually reached by the build that needs it", () => {
    expect(tauriRunner).toMatch(/scripts["'],\s*["']bin|scripts\/bin/);
    expect(tauriRunner).toContain('process.platform === "darwin"');
    expect(tauriRunner).toMatch(/env\.PATH\s*=/);
    expect(existsSync(shimPath)).toBe(true);
  });

  it("names no helper of its own, so the list cannot go stale", () => {
    const code = shim
      .split("\n")
      .filter((line) => !/^\s*#/.test(line))
      .join("\n");
    for (const name of helpers) {
      expect(code, `scripts/bin/codesign hardcodes "${name}" instead of reading Cargo.toml`).not.toContain(
        name,
      );
    }
    expect(code, "the shim no longer reads the cargo manifest").toContain("Cargo.toml");
    expect(code, "the shim no longer reads the bundler's main binary name").toContain("mainBinaryName");
  });

  describe.skipIf(process.platform === "win32")("driven with a recorder", () => {
    it("signs every sibling [[bin]] before the main binary, with the same flags", () => {
      const recorded = withRecorder((dir, env) => {
        writeSiblingBins(dir, declaredBinaries);
        execFileSync(shimPath, signArgs(join(dir, MAIN_BINARY)), { env: { ...process.env, ...env } });
        return readFileSync(env.RECORD as string, "utf8").trim().split("\n");
      });

      expect(recorded).toHaveLength(declaredBinaries.length);
      const last = recorded.at(-1) ?? "";
      expect(last.endsWith(`/${MAIN_BINARY}`) || last.endsWith(` ${MAIN_BINARY}`)).toBe(true);
      expect(last).toContain("--force");
      expect(last).toContain("-s");
      for (const helper of helpers) {
        const index = recorded.findIndex((line) => line.endsWith(`/${helper}`) || line.endsWith(` ${helper}`));
        expect(index, `nested helper "${helper}" was not signed`).toBeGreaterThanOrEqual(0);
        expect(index).toBeLessThan(recorded.length - 1);
        expect(recorded[index]).toContain("--force");
        expect(recorded[index]).toContain("-s");
      }
    });

    it("leaves a sign of a helper alone, so it cannot walk back to the main binary", () => {
      const helper = helpers[0];
      const recorded = withRecorder((dir, env) => {
        writeSiblingBins(dir, declaredBinaries);
        execFileSync(shimPath, signArgs(join(dir, helper)), { env: { ...process.env, ...env } });
        return readFileSync(env.RECORD as string, "utf8").trim().split("\n");
      });
      expect(recorded).toHaveLength(1);
      expect(recorded[0].endsWith(`/${helper}`) || recorded[0].endsWith(` ${helper}`)).toBe(true);
    });

    it("does not mutate binaries on a display/verify invocation", () => {
      const recorded = withRecorder((dir, env) => {
        writeSiblingBins(dir, declaredBinaries);
        execFileSync(shimPath, ["-d", "-v", join(dir, MAIN_BINARY)], { env: { ...process.env, ...env } });
        return readFileSync(env.RECORD as string, "utf8").trim().split("\n");
      });
      expect(recorded).toHaveLength(1);
      expect(recorded[0]).toContain("-d");
    });

    it("skips a helper that is not in the directory rather than inventing a file", () => {
      const recorded = withRecorder((dir, env) => {
        writeFileSync(join(dir, MAIN_BINARY), "");
        execFileSync(shimPath, signArgs(join(dir, MAIN_BINARY)), { env: { ...process.env, ...env } });
        return readFileSync(env.RECORD as string, "utf8").trim().split("\n");
      });
      expect(recorded).toHaveLength(1);
      expect(recorded[0].endsWith(`/${MAIN_BINARY}`) || recorded[0].endsWith(` ${MAIN_BINARY}`)).toBe(true);
    });
  });

  describe.skipIf(process.platform !== "darwin" || !existsSync("/usr/bin/codesign") || !existsSync("/bin/ls"))(
    "against real codesign in an app bundle",
    () => {
      function withBundle<T>(run: (macos: string) => T): T {
        const dir = mkdtempSync(join(tmpdir(), "gitpulse-codesign-app-"));
        try {
          const macos = join(dir, "GitPulse.app", "Contents", "MacOS");
          mkdirSync(macos, { recursive: true });
          writeFileSync(
            join(dir, "GitPulse.app", "Contents", "Info.plist"),
            [
              `<?xml version="1.0" encoding="UTF-8"?>`,
              `<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">`,
              `<plist version="1.0"><dict>`,
              `<key>CFBundleExecutable</key><string>${MAIN_BINARY}</string>`,
              `<key>CFBundleIdentifier</key><string>com.gitpulse.desktop</string>`,
              `<key>CFBundlePackageType</key><string>APPL</string>`,
              `</dict></plist>`,
              "",
            ].join("\n"),
          );
          for (const name of declaredBinaries) {
            copyFileSync("/bin/ls", join(macos, name));
            spawnSync("/usr/bin/codesign", ["--remove-signature", join(macos, name)], { encoding: "utf8" });
          }
          return run(macos);
        } finally {
          rmSync(dir, { recursive: true, force: true });
        }
      }

      it("is the failure the universal release hit: unsigned nested helper, main signed first", () => {
        // Characterises Apple's rule against unmodified codesign, so a
        // future codesign that stops caring would fail this rather than
        // leave the shim looking load-bearing for a bug that no longer exists.
        const result = withBundle((macos) =>
          spawnSync("/usr/bin/codesign", signArgs(join(macos, MAIN_BINARY)), { encoding: "utf8" }),
        );
        expect(result.status).not.toBe(0);
        expect(result.stderr).toMatch(/code object is not signed at all/);
        expect(result.stderr).toMatch(/In subcomponent:/);
        expect(
          helpers.some((helper) => result.stderr.includes(helper)),
          `stderr named none of the declared helpers:\n${result.stderr}`,
        ).toBe(true);
      });

      it("lets the same sign succeed by signing nested helpers first", () => {
        const result = withBundle((macos) =>
          spawnSync(shimPath, signArgs(join(macos, MAIN_BINARY)), {
            encoding: "utf8",
            env: { ...process.env, GITPULSE_REAL_CODESIGN: "/usr/bin/codesign" },
          }),
        );
        expect(result.status, result.stderr).toBe(0);
      });
    },
  );
});
