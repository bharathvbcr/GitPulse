import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { appVersion } from "./app-version.mjs";
import { collectVersions, defaultSources } from "./check-release-version.mjs";

/**
 * The app version has exactly one source: package.json, read through
 * `appVersion()`. Nothing in source may retype it.
 *
 * `codex-plugin-contract.test.ts` asserted `manifest.version === "0.0.5"`
 * against the real tree. The literal was correct on the day it was written —
 * that is the whole problem. It survives review because it passes, and then
 * every release bump either breaks the build or, worse, leaves a test that
 * still passes while asserting about a release nobody is shipping any more
 * (`release-notes.test.ts` called an 0.0.5 section "the current" one three
 * versions later).
 *
 * A hardcoded version is only ever catchable at the moment it is typed,
 * because at that moment it necessarily equals package.json's version — which
 * is why the guard below compares against the *current* version rather than
 * hunting for stale-looking ones. Versions only go up, so historical fixtures
 * ("0.0.3", "v0.0.2") can never collide with it, and a synthetic unit-test
 * value should be an obviously-fake version like "1.2.3" anyway.
 *
 * Derived by walking the tree, not by listing the files that got it wrong.
 */
const REPO_ROOT = fileURLToPath(new URL("..", import.meta.url));
const VERSION = appVersion();

/**
 * Roots that hold code and CI definitions, plus the repo-root config files —
 * `vite.config.ts` and `vitest.config.ts` are where `__APP_VERSION__` is
 * defined, so they are the likeliest place for a second version to appear.
 *
 * Manifests (`package.json`, `Cargo.toml`, `tauri.conf.json`, the plugin
 * manifests) and the changelog are excluded by extension: carrying the version
 * is their job, and `check-release-version.mjs` is what holds them to it.
 */
const ROOTS = ["scripts", "src", "src-tauri/src", "harness", ".github"];
const SKIP = new Set(["node_modules", "dist", ".git", "coverage", "target", "gen"]);
const SOURCE = /\.(ts|mts|cts|mjs|cjs|js|svelte|rs|yml|yaml)$/;

function sourceFiles(dir: string): string[] {
  let entries: string[];
  try {
    entries = readdirSync(dir);
  } catch {
    return [];
  }
  return entries.flatMap((entry) => {
    if (SKIP.has(entry)) return [];
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) return sourceFiles(path);
    return SOURCE.test(entry) ? [path] : [];
  });
}

/**
 * A whole-token match for `version`, with an optional `v` tag prefix.
 *
 * Bounded on both sides so "1.2.3" does not match inside "11.2.3", "1.2.30" or
 * "1.2.3-rc.1" — three different versions that are not this one, and flagging
 * them would train people to ignore this test.
 */
export function versionLiteralMatcher(version: string): RegExp {
  const escaped = version.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(String.raw`(?<![\w.])v?${escaped}(?![\w.-])`);
}

/** Repo-root config files, without descending into every sibling directory. */
function rootConfigFiles(): string[] {
  return readdirSync(REPO_ROOT)
    .map((entry) => join(REPO_ROOT, entry))
    .filter((path) => !statSync(path).isDirectory() && SOURCE.test(path));
}

const FILES = [...ROOTS.flatMap((root) => sourceFiles(join(REPO_ROOT, root))), ...rootConfigFiles()];
const rel = (file: string) => relative(REPO_ROOT, file).split(sep).join("/");

describe("the app version is never retyped in source", () => {
  it("scans a real tree, so a passing run is not a vacuous one", () => {
    expect(FILES.length).toBeGreaterThan(200);
    const names = FILES.map(rel);
    expect(names).toContain("scripts/codex-plugin-contract.test.ts");
    // The two files that define `__APP_VERSION__`, reached by rootConfigFiles
    // rather than the recursive walk — a broken root scan would otherwise
    // leave them silently unchecked.
    expect(names).toContain("vite.config.ts");
    expect(names).toContain("vitest.config.ts");
  });

  it("uses a matcher that actually matches, and only the version it was given", () => {
    // A silently non-matching regex would turn the guard below into a test
    // that reports success over an empty result set — the exact failure this
    // whole file exists to prevent.
    const probe = versionLiteralMatcher("1.2.3");
    expect(probe.test('expect(manifest.version).toBe("1.2.3");')).toBe(true);
    expect(probe.test('run: gh release create "v1.2.3"')).toBe(true);
    expect(probe.test('"11.2.3"')).toBe(false);
    expect(probe.test('"1.2.30"')).toBe(false);
    expect(probe.test('"1.2.3-rc.1"')).toBe(false);
    expect(versionLiteralMatcher(VERSION).test(`"${VERSION}"`)).toBe(true);
  });

  it("has no source file carrying the current version as a literal", () => {
    const matcher = versionLiteralMatcher(VERSION);
    const offenders: string[] = [];
    for (const file of FILES) {
      const relPath = rel(file);
      const lines = readFileSync(file, "utf8").split(/\r?\n/);
      lines.forEach((line, index) => {
        if (!matcher.test(line)) return;
        // External schema URLs (e.g. Agent Plugins 1.0.0 specification)
        if (/agent-plugins\.org\/schemas\/|Agent Plugins 1\.0\.0/i.test(line)) return;
        // External scanner or tool protocols (e.g. govulncheck protocol_version)
        if (/protocol_version|scanner_name|govulncheck/i.test(line)) return;
        // UI placeholder for tag creation input
        if (/placeholder:\s*["']v?1\.0\.0["']/i.test(line)) return;
        // Test files and test fixtures testing semver logic, third-party crates, or mock git tags,
        // unless asserting/declaring the GitPulse manifest or app version directly.
        if (relPath.includes(".test.") || relPath.includes(".stress.test.") || relPath.endsWith(".rs")) {
          const isVersionAssertion = /expect\([^)]*version[^)]*\)\.(?:toBe|toEqual)\(/i.test(line);
          const isAppVersionDecl = /(?:app_version|APP_VERSION|gitpulse_version)\s*=/i.test(line);
          if (!isVersionAssertion && !isAppVersionDecl) return;
        }
        offenders.push(`${relPath}:${index + 1}`);
      });
    }
    expect(
      offenders,
      `read the version from scripts/app-version.mjs (appVersion()) instead of typing ${VERSION}`,
    ).toEqual([]);
  });
});

/**
 * `check-release-version.mjs` owns manifest agreement and runs as its own CI
 * step (`npm run check:release`), but every one of its own tests builds a
 * synthetic scratch tree — none of them points it at this repository. So the
 * gate that exists to stop a mismatched release had no coverage over the tree
 * it actually gates. These two run it here, under `npm test`, against the real
 * root, using its own discovery rather than a list of manifest paths.
 */
describe("every discovered manifest carries package.json's version", () => {
  const found = collectVersions(defaultSources(REPO_ROOT));

  it("discovers the manifests it claims to check", () => {
    // Six fixed sources plus at least the required plugin manifest. Without
    // this, a discovery that returned nothing would pass the check below by
    // having nothing to disagree with.
    expect(found.length).toBeGreaterThanOrEqual(7);
    expect(found.map((entry) => entry.label)).toContain("package.json");
    expect(found.some((entry) => entry.label.startsWith("plugins/"))).toBe(true);
  });

  it("finds no manifest disagreeing with appVersion()", () => {
    const drifted = found
      .filter((entry) => entry.version !== VERSION)
      .map(
        (entry) =>
          `${entry.label} = ${entry.version ?? `<unreadable${entry.error ? `: ${entry.error}` : ""}>`}`,
      );
    expect(drifted, `every manifest must carry ${VERSION}`).toEqual([]);
  });
});
