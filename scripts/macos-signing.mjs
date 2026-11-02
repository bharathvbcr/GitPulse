import { spawnSync } from "node:child_process";

/**
 * Picks the identity a local macOS `tauri build` signs with.
 *
 * `tauri.conf.json` signs ad-hoc (`"-"`), and an ad-hoc signature's only
 * identity is its code hash. macOS pins every LaunchAgent it loads at login to
 * the identity of the program it starts, so after each rebuild the cleaner
 * agent's next run is killed with a launch constraint violation, and privacy
 * grants keyed to the old hash are asked for again. A team signature is pinned
 * by team and signing identifier instead, which every later build keeps.
 *
 * Tauri reads `APPLE_SIGNING_IDENTITY` before the config, so an explicit value
 * (including `-`) always wins. CI is left alone: a release must not be signed
 * by whichever development certificate a runner happens to hold.
 */

/** Preferred certificate kinds, best first. */
export const IDENTITY_KINDS = ["Developer ID Application", "Apple Development"];

/**
 * Valid code-signing identities from `security find-identity -v -p codesigning`.
 *
 * @param {string} output
 * @returns {Array<{ hash: string, name: string }>}
 */
export function parseCodesignIdentities(output) {
  const identities = [];
  for (const line of output.split("\n")) {
    const match = /^\s*\d+\)\s+([0-9A-F]{40})\s+"(.+)"\s*$/.exec(line);
    if (match) identities.push({ hash: match[1], name: match[2] });
  }
  return identities;
}

/**
 * The identity to sign with: the first kind in {@link IDENTITY_KINDS} the
 * keychain holds. Its name when that is unique, else its hash, because
 * codesign refuses a name two certificates share (a renewed one, say).
 *
 * @param {Array<{ hash: string, name: string }>} identities
 * @returns {{ value: string, name: string } | null}
 */
export function chooseSigningIdentity(identities) {
  for (const kind of IDENTITY_KINDS) {
    const found = identities.find((identity) => identity.name.startsWith(`${kind}:`));
    if (!found) continue;
    const shared = identities.filter((identity) => identity.name === found.name).length > 1;
    return { value: shared ? found.hash : found.name, name: found.name };
  }
  return null;
}

/**
 * Whether these `tauri` arguments sign a bundle.
 *
 * @param {string[]} args
 */
export function isSigningBuild(args) {
  return (
    (args[0] === "build" || args[0] === "bundle") &&
    !args.some((arg) => arg === "--help" || arg === "-h" || arg === "--no-sign" || arg === "--no-bundle")
  );
}

/** The real keychain query, bounded so a locked keychain cannot stall a build. */
export function findKeychainIdentities() {
  const result = spawnSync("/usr/bin/security", ["find-identity", "-v", "-p", "codesigning"], {
    encoding: "utf8",
    timeout: 10_000,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`security find-identity exited ${result.status}: ${result.stderr.trim()}`);
  }
  return parseCodesignIdentities(result.stdout);
}

/**
 * Sets `env.APPLE_SIGNING_IDENTITY` for a local signing build when the
 * keychain holds a team identity, and returns the line to print about it.
 * Returns null when nothing is decided here.
 *
 * @param {{ args: string[], env: NodeJS.ProcessEnv, platform?: string, find?: () => Array<{ hash: string, name: string }> }} options
 * @returns {string | null}
 */
export function applySigningIdentity({ args, env, platform = process.platform, find = findKeychainIdentities }) {
  if (platform !== "darwin" || !isSigningBuild(args)) return null;
  if (env.APPLE_SIGNING_IDENTITY !== undefined) return null;
  if (env.CI) return null;
  let identities;
  try {
    identities = find();
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    return `[gitpulse] could not read signing identities (${reason}); this build is signed ad-hoc`;
  }
  const chosen = chooseSigningIdentity(identities);
  if (!chosen) {
    return "[gitpulse] no Developer ID or Apple Development identity in the keychain; this build is signed ad-hoc";
  }
  env.APPLE_SIGNING_IDENTITY = chosen.value;
  return `[gitpulse] signing with "${chosen.name}"; set APPLE_SIGNING_IDENTITY=- for an ad-hoc build`;
}
