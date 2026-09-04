import { readFileSync } from "node:fs";
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
