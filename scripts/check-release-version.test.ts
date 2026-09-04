import { execFile } from "node:child_process";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { afterAll, describe, expect, it } from "vitest";
import {
  CRATE_NAME,
  discoverPluginPackages,
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
 * Build a synthetic repo root carrying all app and plugin version sources, so
 * drift can be seeded without ever touching the tracked tree.
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
    plugin: string;
    pluginClaude: string;
    pluginCodex: string;
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
    plugin: base,
    pluginClaude: base,
    pluginCodex: base,
    ...versions,
  };
  const dir = await mkdtemp(path.join(tmpdir(), `gitpulse-relver-${prefix}-`));
  tempDirs.push(dir);
  await mkdir(path.join(dir, "src-tauri"), { recursive: true });
  await mkdir(path.join(dir, "plugins", "gitpulse", ".claude-plugin"), { recursive: true });
  await mkdir(path.join(dir, "plugins", "gitpulse", ".codex-plugin"), { recursive: true });

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
  await writeFile(
    path.join(dir, "plugins", "gitpulse", "plugin.json"),
    JSON.stringify({ name: CRATE_NAME, version: v.plugin }, null, 2),
  );
  await writeFile(
    path.join(dir, "plugins", "gitpulse", ".claude-plugin", "plugin.json"),
    JSON.stringify({ name: CRATE_NAME, version: v.pluginClaude }, null, 2),
  );
  await writeFile(
    path.join(dir, "plugins", "gitpulse", ".codex-plugin", "plugin.json"),
    JSON.stringify({ name: CRATE_NAME, version: v.pluginCodex }, null, 2),
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
      "plugins/gitpulse/plugin.json",
      "plugins/gitpulse/.claude-plugin/plugin.json",
      "plugins/gitpulse/.codex-plugin/plugin.json",
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

  it("fails when the Agent Plugins manifest lags the app manifests", async () => {
    // The shape found in the tree at v0.0.5: every build manifest had been
    // bumped and the Agent Plugins package still advertised the prior version,
    // so agents installed 0.0.4 metadata against an 0.0.5 binary.
    const dir = await scratchTree("plugin-drift", { plugin: "0.4.1" });
    const { code, stdout } = await runScript(["--root", dir]);
    expect(code).toBe(1);
    expect(stdout).toMatch(/plugins\/gitpulse\/plugin\.json = "0\.4\.1"/);
    expect(stdout).toMatch(/FAIL: release version gate violated/);
  });

  it("fails when the Claude Code plugin manifest drifts from the Agent Plugins one", async () => {
    // Two manifests describing one package: whichever client reads the stale
    // one records an install version the MCP handshake will not corroborate.
    const dir = await scratchTree("plugin-claude-drift", { pluginClaude: "0.4.1" });
    const { code, stdout } = await runScript(["--root", dir]);
    expect(code).toBe(1);
    expect(stdout).toMatch(/plugins\/gitpulse\/\.claude-plugin\/plugin\.json = "0\.4\.1"/);
  });

  it("fails when the native Codex manifest drifts from the package", async () => {
    const dir = await scratchTree("plugin-codex-drift", { pluginCodex: "0.4.1" });
    const { code, stdout } = await runScript(["--root", dir]);
    expect(code).toBe(1);
    expect(stdout).toMatch(/plugins\/gitpulse\/\.codex-plugin\/plugin\.json = "0\.4\.1"/);
  });

  it("reports a missing plugin manifest rather than passing on the remaining sources", async () => {
    // Discovery must not turn a deleted manifest into "nothing to check":
    // plugin.json is required per package for exactly this reason.
    const dir = await scratchTree("plugin-missing");
    await rm(path.join(dir, "plugins", "gitpulse", "plugin.json"));
    const { code, stdout } = await runScript(["--root", dir]);
    expect(code).toBe(1);
    expect(stdout).toMatch(/unreadable: plugins\/gitpulse\/plugin\.json yielded no version/);
  });

  it("discovers a second package under plugins/ without being told about it", async () => {
    // A newly added package must be covered the moment it exists.
    const dir = await scratchTree("plugins-dir");
    await mkdir(path.join(dir, "plugins", "extra", ".codex-plugin"), { recursive: true });
    await writeFile(
      path.join(dir, "plugins", "extra", "plugin.json"),
      JSON.stringify({ name: CRATE_NAME, version: "0.4.2" }, null, 2),
    );
    await writeFile(
      path.join(dir, "plugins", "extra", ".codex-plugin", "plugin.json"),
      JSON.stringify({ name: CRATE_NAME, version: "0.4.1" }, null, 2),
    );
    const { code, stdout } = await runScript(["--root", dir]);
    expect(code).toBe(1);
    expect(stdout).toContain("plugins/extra/plugin.json");
    expect(stdout).toMatch(/plugins\/extra\/\.codex-plugin\/plugin\.json = "0\.4\.1"/);
  });

  it("requires plugin.json in a package directory that only carries client manifests", async () => {
    const dir = await scratchTree("plugins-no-base");
    await mkdir(path.join(dir, "plugins", "orphan", ".claude-plugin"), { recursive: true });
    await writeFile(
      path.join(dir, "plugins", "orphan", ".claude-plugin", "plugin.json"),
      JSON.stringify({ name: CRATE_NAME, version: "0.4.2" }, null, 2),
    );
    const { code, stdout } = await runScript(["--root", dir]);
    expect(code).toBe(1);
    expect(stdout).toMatch(/unreadable: plugins\/orphan\/plugin\.json yielded no version/);
  });

  it("fails closed when the repo carries no plugin package at all", async () => {
    const dir = await scratchTree("no-package");
    await rm(path.join(dir, "plugins"), { recursive: true, force: true });
    const { code, stdout } = await runScript(["--root", dir]);
    expect(code).toBe(1);
    expect(stdout).toMatch(/no plugins\/<name>\/ package directory found/);
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

describe("discoverPluginPackages", () => {
  /**
   * These build their own trees rather than reusing scratchTree: the walker's
   * contract is "every plugins/<name>/", independent of which packages the
   * repo happens to ship.
   */
  async function bareTree(prefix: string, dirs: string[]) {
    const dir = await mkdtemp(path.join(tmpdir(), `gitpulse-discover-${prefix}-`));
    tempDirs.push(dir);
    for (const rel of dirs) await mkdir(path.join(dir, rel), { recursive: true });
    return dir;
  }

  it("returns every plugins/<name>/ in a stable order", async () => {
    const dir = await bareTree("order", ["plugins/beta", "plugins/alpha"]);
    expect(discoverPluginPackages(dir).map((p: string) => path.relative(dir, p))).toEqual([
      path.join("plugins", "alpha"),
      path.join("plugins", "beta"),
    ]);
  });

  it("does not resurrect the retired top-level plugin/ directory", async () => {
    // The package moved to plugins/<name>/; a leftover plugin/ on someone's
    // disk must not be picked up as a second package and gate the release.
    const dir = await bareTree("retired", ["plugin", "plugins/gitpulse"]);
    expect(discoverPluginPackages(dir).map((p: string) => path.relative(dir, p))).toEqual([
      path.join("plugins", "gitpulse"),
    ]);
  });

  it("ignores loose files beside the packages", async () => {
    const dir = await bareTree("files", ["plugins/gitpulse"]);
    await writeFile(path.join(dir, "plugins", "README.md"), "not a package");
    expect(discoverPluginPackages(dir).map((p: string) => path.relative(dir, p))).toEqual([
      path.join("plugins", "gitpulse"),
    ]);
  });

  it("returns an empty list for a tree with no packages, rather than throwing", async () => {
    const dir = await bareTree("none", ["src"]);
    expect(discoverPluginPackages(dir)).toEqual([]);
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
