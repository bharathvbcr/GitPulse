import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import path from "node:path";
import { fileURLToPath } from "node:url";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/**
 * The app version, read from package.json at config time.
 *
 * package.json is a safe single source here because `check:release` already
 * discovers and gates every app and plugin manifest plus the release tag, so
 * this cannot quietly disagree with the version baked into the bundle.
 *
 * Shared by vite.config.ts and vitest.config.ts so the build and the tests
 * cannot define `__APP_VERSION__` differently.
 */
export function appVersion() {
  const pkg = JSON.parse(readFileSync(path.join(REPO_ROOT, "package.json"), "utf8"));
  if (typeof pkg.version !== "string" || !pkg.version) {
    throw new Error("package.json has no usable version");
  }
  return pkg.version;
}

/** Unique per config evaluation, including separate builds of a dirty tree. */
export function appBuild() {
  /** @type {string | null} */
  let revision = null;
  /** @type {boolean | null} */
  let dirty = null;
  try {
    /** @type {import('node:child_process').ExecFileSyncOptionsWithStringEncoding} */
    const opts = { cwd: REPO_ROOT, encoding: "utf8", timeout: 2_000, stdio: ["ignore", "pipe", "ignore"] };
    revision = execFileSync("git", ["rev-parse", "HEAD"], opts).trim();
    dirty = execFileSync("git", ["status", "--porcelain", "--untracked-files=normal"], opts).trim().length > 0;
  } catch {
    // Source archives may have no Git metadata. The unique build ID still
    // matches its own retained bundles; unavailable provenance stays null.
  }
  return { id: randomUUID(), version: appVersion(), builtAt: new Date().toISOString(), revision, dirty };
}
