import { createHash } from "node:crypto";
import { mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { releaseRevisions } from "./app-version.mjs";

const DAY_MS = 24 * 60 * 60 * 1000;

/**
 * How many snapshots survive a successful build.
 *
 * A dirty snapshot is a dev build, never a release (a release is built from a
 * clean tree), so it is bounded by both count and age. A clean snapshot whose
 * revision a tag points at is a release and is never pruned. A clean snapshot
 * of an untagged commit may still be the build behind an installed app, so it
 * is only ever bounded by count, and by a larger one.
 */
export const DEFAULT_RETENTION = Object.freeze({ keepDirty: 3, maxDirtyAgeMs: 14 * DAY_MS, keepClean: 5 });

/**
 * @typedef {{ keepDirty?: number, maxDirtyAgeMs?: number, keepClean?: number,
 *   releaseRevisions?: () => Set<string> | null, now?: () => number }} Retention
 */

/** Keep symbolication evidence private; hidden source maps alone still ship.
 *
 * Retention runs in `writeBundle`, which the bundler calls only once every
 * output file has been written. A build that fails earlier never reaches it,
 * so a failed replacement can never cost the evidence of the build in use.
 * @param {ReturnType<typeof import('./app-version.mjs').appBuild>} stamp
 * @param {string} [root]
 * @param {Retention} [retention]
 * @returns {import('vite').Plugin}
 */
export function privateSourceMaps(stamp, root = path.resolve(".build-evidence"), retention = {}) {
  const evidenceRoot = path.resolve(root);
  const directory = path.resolve(evidenceRoot, stamp.id);
  return {
    name: "gitpulse-private-source-maps",
    apply: "build",
    enforce: "post",
    generateBundle: {
      order: "post",
      handler(_options, bundle) {
        mkdirSync(directory, { recursive: true, mode: 0o700 });
        /** @type {Record<string, string>} */
        const chunks = {};
        let maps = 0;
        for (const [name, output] of Object.entries(bundle)) {
          const destination = path.resolve(directory, name);
          if (!destination.startsWith(`${directory}${path.sep}`)) this.error("Unsafe build evidence path");
          if (output.type === "chunk" || name.endsWith(".map")) {
            const content = output.type === "chunk" ? output.code : output.source;
            mkdirSync(path.dirname(destination), { recursive: true, mode: 0o700 });
            writeFileSync(destination, content, { mode: 0o600 });
            if (output.type === "chunk") chunks[name] = createHash("sha256").update(output.code).digest("hex");
            if (name.endsWith(".map")) { maps++; delete bundle[name]; }
          }
        }
        if (!maps) this.error("Build produced no private source maps");
        writeFileSync(path.join(directory, "manifest.json"), JSON.stringify({ ...stamp, chunks, maps }, null, 2), { mode: 0o600 });
        this.emitFile({ type: "asset", fileName: "build-info.json", source: JSON.stringify(stamp) });
      },
    },
    writeBundle() {
      const { pruned, failed } = pruneEvidence(evidenceRoot, stamp.id, retention);
      // A snapshot that could not be removed is a disk-space problem, not a
      // broken build: the bundle is already written. Say so, loudly.
      for (const failure of failed) this.warn(`build evidence ${failure.id} was not pruned: ${failure.error}`);
      if (pruned.length) this.info(`pruned ${pruned.length} build evidence snapshot(s)`);
    },
  };
}

/**
 * Removes the snapshots under `root` that retention no longer covers.
 *
 * Fails toward keeping. Never pruned: the snapshot `currentId` names; any
 * entry whose manifest is missing, unreadable, or does not name its own
 * directory (it is not proven to be ours); any whose `dirty`, `revision` or
 * `builtAt` cannot be read; and, when tags cannot be listed, every clean one.
 * @param {string} root
 * @param {string} currentId
 * @param {Retention} [retention]
 * @returns {{ pruned: string[], failed: { id: string, error: string }[] }}
 */
export function pruneEvidence(root, currentId, retention = {}) {
  const keepDirty = retention.keepDirty ?? DEFAULT_RETENTION.keepDirty;
  const maxDirtyAgeMs = retention.maxDirtyAgeMs ?? DEFAULT_RETENTION.maxDirtyAgeMs;
  const keepClean = retention.keepClean ?? DEFAULT_RETENTION.keepClean;
  const now = (retention.now ?? Date.now)();
  /** @type {{ id: string, builtAt: number }[]} */
  const dirty = [];
  /** @type {{ id: string, builtAt: number, revision: string }[]} */
  const clean = [];
  let entries;
  try {
    entries = readdirSync(root, { withFileTypes: true });
  } catch {
    return { pruned: [], failed: [] };
  }
  for (const entry of entries) {
    // A symlink's Dirent is not a directory, so a link is never followed.
    if (!entry.isDirectory() || entry.name === currentId) continue;
    const snapshot = readSnapshot(root, entry.name);
    if (!snapshot) continue;
    if (snapshot.dirty) dirty.push(snapshot);
    else clean.push(snapshot);
  }
  const newestFirst = (/** @type {{ builtAt: number }} */ a, /** @type {{ builtAt: number }} */ b) => b.builtAt - a.builtAt;
  dirty.sort(newestFirst);
  clean.sort(newestFirst);
  const doomed = dirty.filter((s, i) => i >= keepDirty || now - s.builtAt > maxDirtyAgeMs).map((s) => s.id);
  const released = clean.length ? (retention.releaseRevisions ?? releaseRevisions)() : null;
  if (released) {
    doomed.push(...clean.filter((s) => !released.has(s.revision)).slice(keepClean).map((s) => s.id));
  }
  /** @type {string[]} */
  const pruned = [];
  /** @type {{ id: string, error: string }[]} */
  const failed = [];
  for (const id of doomed) {
    try {
      rmSync(path.join(root, id), { recursive: true });
      pruned.push(id);
    } catch (error) {
      failed.push({ id, error: error instanceof Error ? error.message : String(error) });
    }
  }
  return { pruned, failed };
}

/**
 * @param {string} root
 * @param {string} id
 * @returns {{ id: string, dirty: boolean, revision: string, builtAt: number } | null}
 */
function readSnapshot(root, id) {
  try {
    const manifest = JSON.parse(readFileSync(path.join(root, id, "manifest.json"), "utf8"));
    const builtAt = Date.parse(manifest?.builtAt);
    if (manifest?.id !== id || typeof manifest.dirty !== "boolean" || typeof manifest.revision !== "string"
      || !manifest.revision || !Number.isFinite(builtAt)) return null;
    return { id, dirty: manifest.dirty, revision: manifest.revision, builtAt };
  } catch {
    return null;
  }
}
