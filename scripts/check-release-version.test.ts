import { execFile } from "node:child_process";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { afterAll, describe, expect, it } from "vitest";
import {
  CRATE_NAME,
  parseCargoLockVersion,
  parseCargoTomlVersion,
  parseTag,
} from "./check-release-version.mjs";

const execFileAsync = promisify(execFile);
const scriptPath = fileURLToPath(new URL("./check-release-version.mjs", import.meta.url));

const tempDirs: string[] = [];

afterAll(async () => {
  while (tempDirs.length > 0) {
    const dir = tempDirs.pop();
    if (dir) await rm(dir, { recursive: true, force: true });
  }
});

async function runScript(args: string[]) {
  try {
    const { stdout } = await execFileAsync(process.execPath, [scriptPath, ...args], {
      cwd: path.dirname(scriptPath),
    });
    return { code: 0, stdout };
  } catch (err) {
    const failure = err as { code?: number | null; stdout?: string; stderr?: string };
    return { code: failure.code ?? 1, stdout: (failure.stdout ?? "") + (failure.stderr ?? "") };
  }
}

/**
 * Build a synthetic repo root carrying all five version sources, so drift can
 * be seeded without ever touching the tracked tree.
 */
async function scratchTree(
  prefix: string,
  versions: Partial<{
    pkg: string;
    lockRoot: string;
    lockPackages: string;
    tauri: string;
    cargoToml: string;
    cargoLock: string;
  }> = {},
) {
  const base = "0.4.2";
  const v = {
    pkg: base,
    lockRoot: base,
    lockPackages: base,
    tauri: base,
    cargoToml: base,
    cargoLock: base,
    ...versions,
  };
  const dir = await mkdtemp(path.join(tmpdir(), `gitpulse-relver-${prefix}-`));
  tempDirs.push(dir);
  await mkdir(path.join(dir, "src-tauri"), { recursive: true });

  await writeFile(
    path.join(dir, "package.json"),
    JSON.stringify({ name: CRATE_NAME, version: v.pkg }, null, 2),
  );
  await writeFile(
    path.join(dir, "package-lock.json"),
    JSON.stringify(
      {
        name: CRATE_NAME,
        version: v.lockRoot,
        lockfileVersion: 3,
        packages: { "": { name: CRATE_NAME, version: v.lockPackages } },
      },
      null,
      2,
    ),
  );
  await writeFile(
    path.join(dir, "src-tauri", "tauri.conf.json"),
    JSON.stringify({ productName: "GitPulse", version: v.tauri }, null, 2),
  );
  await writeFile(
    path.join(dir, "src-tauri", "Cargo.toml"),
    // A dependency pinned to a *different* version sits above the package
    // version on purpose: a section-blind parser would read this one.
    `[dependencies]\nserde = { version = "1.0.203" }\n\n[package]\nname = "${CRATE_NAME}"\nversion = "${v.cargoToml}"\nedition = "2021"\n`,
  );
  await writeFile(
    path.join(dir, "src-tauri", "Cargo.lock"),
    `version = 3\n\n[[package]]\nname = "serde"\nversion = "1.0.203"\n\n[[package]]\nname = "${CRATE_NAME}"\nversion = "${v.cargoLock}"\ndependencies = [\n "serde",\n]\n`,
  );
  return dir;
}

describe("release version gate", () => {
  it("passes on the current tree and lists every version source", async () => {
    const { code, stdout } = await runScript([]);
    expect(code).toBe(0);
    expect(stdout).toMatch(/OK: all version sources agree on \d+\.\d+\.\d+/);
    for (const label of [
      "package.json",
      "package-lock.json (root)",
      'package-lock.json (packages[""])',
      "src-tauri/tauri.conf.json",
      "src-tauri/Cargo.toml",
      `src-tauri/Cargo.lock (${CRATE_NAME})`,
    ]) {
      expect(stdout).toContain(label);
    }
  });

  it("passes a synthetic tree whose sources agree, and accepts the matching tag", async () => {
    const dir = await scratchTree("agree");
    const { code, stdout } = await runScript(["--root", dir, "--tag", "v0.4.2"]);
    expect(code).toBe(0);
    expect(stdout).toMatch(/match tag v0\.4\.2/);
  });

  it("fails when tauri.conf.json drifts from the other manifests", async () => {
    // The exact shape that ships a release named for one version with assets
    // built at another.
    const dir = await scratchTree("tauri-drift", { tauri: "0.4.1" });
    const { code, stdout } = await runScript(["--root", dir]);
    expect(code).toBe(1);
    expect(stdout).toMatch(/manifest versions disagree: "0\.4\.2" vs "0\.4\.1"/);
    expect(stdout).toMatch(/src-tauri\/tauri\.conf\.json = "0\.4\.1"/);
    expect(stdout).toMatch(/FAIL: release version gate violated/);
  });

  it("fails when Cargo.lock lags Cargo.toml", async () => {
    const dir = await scratchTree("lock-lag", { cargoLock: "0.4.1" });
    const { code, stdout } = await runScript(["--root", dir]);
    expect(code).toBe(1);
    expect(stdout).toMatch(new RegExp(`src-tauri/Cargo\\.lock \\(${CRATE_NAME}\\) = "0\\.4\\.1"`));
  });

  it("fails when package-lock's nested packages[''] entry drifts alone", async () => {
    const dir = await scratchTree("nested-drift", { lockPackages: "0.4.1" });
    const { code, stdout } = await runScript(["--root", dir]);
    expect(code).toBe(1);
    expect(stdout).toMatch(/packages\[""\]\) = "0\.4\.1"/);
  });

  it("fails when the tag names a version the manifests do not carry", async () => {
    const dir = await scratchTree("tag-mismatch");
    const { code, stdout } = await runScript(["--root", dir, "--tag", "v0.5.0"]);
    expect(code).toBe(1);
    expect(stdout).toMatch(/tag v0\.5\.0 declares version "0\.5\.0" but package\.json = "0\.4\.2"/);
  });

  it("rejects malformed tags instead of guessing at them", async () => {
    const dir = await scratchTree("bad-tags");
    const noPrefix = await runScript(["--root", dir, "--tag", "0.4.2"]);
    expect(noPrefix.code).toBe(1);
    expect(noPrefix.stdout).toMatch(/must start with "v"/);

    const suffixed = await runScript(["--root", dir, "--tag", "v0.4.2-rc.1"]);
    expect(suffixed.code).toBe(1);
    expect(suffixed.stdout).toMatch(/must be v<major>\.<minor>\.<patch> with no suffix/);
  });

  it("treats an empty --tag as 'no tag', which is how a tag-push run invokes it", async () => {
    const dir = await scratchTree("empty-tag");
    const { code, stdout } = await runScript(["--root", dir, "--tag", ""]);
    expect(code).toBe(0);
    expect(stdout).not.toMatch(/git tag/);
  });

  it("reports a missing or unreadable manifest rather than letting the rest agree", async () => {
    const dir = await scratchTree("missing");
    await rm(path.join(dir, "src-tauri", "tauri.conf.json"));
    const { code, stdout } = await runScript(["--root", dir]);
    expect(code).toBe(1);
    expect(stdout).toMatch(/unreadable: src-tauri\/tauri\.conf\.json yielded no version/);
  });

  it("exits 2 on an unknown flag", async () => {
    const { code, stdout } = await runScript(["--nope"]);
    expect(code).toBe(2);
    expect(stdout).toMatch(/unknown argument: --nope/);
  });
});

describe("manifest parsers", () => {
  it("reads Cargo.toml's [package] version, not a dependency's", () => {
    const toml = [
      "[dependencies]",
      'serde = { version = "1.0.203" }',
      "",
      "[package]",
      'name = "gitpulse"',
      'version = "0.9.9"',
      "",
      "[dev-dependencies]",
      'tempfile = "3.10"',
    ].join("\n");
    expect(parseCargoTomlVersion(toml)).toBe("0.9.9");
  });

  it("ignores a commented-out Cargo.toml version", () => {
    const toml = ['[package]', '# version = "9.9.9"', 'version = "1.2.3"'].join("\n");
    expect(parseCargoTomlVersion(toml)).toBe("1.2.3");
  });

  it("returns null when Cargo.toml has no [package] version", () => {
    expect(parseCargoTomlVersion('[dependencies]\nserde = "1.0"\n')).toBeNull();
  });

  it("picks the named crate out of Cargo.lock's package blocks", () => {
    const lock = [
      "version = 3",
      "",
      "[[package]]",
      'name = "aho-corasick"',
      'version = "1.1.3"',
      "",
      "[[package]]",
      'name = "gitpulse"',
      'version = "0.3.0"',
      "dependencies = [",
      ' "serde",',
      "]",
      "",
      "[[package]]",
      'name = "zerocopy"',
      'version = "0.7.35"',
    ].join("\n");
    expect(parseCargoLockVersion(lock, "gitpulse")).toBe("0.3.0");
    expect(parseCargoLockVersion(lock, "zerocopy")).toBe("0.7.35");
    expect(parseCargoLockVersion(lock, "absent-crate")).toBeNull();
  });

  it("parses well-formed tags and refuses the rest", () => {
    expect(parseTag("v1.2.3")).toEqual({ ok: true, version: "1.2.3" });
    expect(parseTag("v1.2").ok).toBe(false);
    expect(parseTag("release-1.2.3").ok).toBe(false);
    expect(parseTag("v1.2.3+build").ok).toBe(false);
  });
});
