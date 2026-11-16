import { buildDevmap, getCodeintelStatus } from "../codeintel/client";
import { buildFailureReason, type DevmapBuildOutcome } from "../codeintel/types";
import { boundText } from "../codeintel/walkIncomplete";

/**
 * Why a surface that reads the DevCouncil code map has nothing to show, from
 * structured evidence only: whether the graph answers a status read, and
 * whether the `devmap` CLI that builds it is installed. Never from the wording
 * of a reply's `reason`.
 *
 *  - `cli_missing`: the graph is unavailable and the CLI is not installed, so
 *    building is impossible until Setup installs it.
 *  - `no_index`: the graph is unavailable and the CLI is installed, so Build
 *    index is the remedy.
 *  - `other`: the graph answers (or could not be read at all), so the failure
 *    is something else and an install call-to-action would be wrong.
 */
export type GraphGap = "cli_missing" | "no_index" | "other";

/** What a status read said about the graph. `unknown` is a read that failed. */
export type GraphReading = "available" | "unavailable" | "unknown";

export function classifyGraphGap(input: {
  graph: GraphReading;
  /** `onboardingStore.status.devmap.installed`; null before the first read. */
  devmapInstalled: boolean | null;
}): GraphGap {
  if (input.graph !== "unavailable") return "other";
  return input.devmapInstalled === false ? "cli_missing" : "no_index";
}

/**
 * Ask the backend whether this repository has a usable graph. A thrown read is
 * `unknown`, not `unavailable`: a check that could not run must not trigger
 * the install prompt.
 */
export async function readGraphReading(
  repoPath: string,
  read: (repoPath: string) => Promise<{ available: boolean }> = getCodeintelStatus,
): Promise<GraphReading> {
  try {
    return (await read(repoPath)).available ? "available" : "unavailable";
  } catch {
    return "unknown";
  }
}

/** How long one repository's graph reading is shared between notices. */
export const GRAPH_READING_TTL_MS = 5_000;

const sharedReadings = new Map<string, { at: number; reading: Promise<GraphReading> }>();

/**
 * `readGraphReading`, shared: every notice for one repository mounted within
 * `GRAPH_READING_TTL_MS` (a diff pane remounts its impact panel per file)
 * gets the same status read instead of each issuing its own. A read that
 * could not run (`unknown`) is not kept, so the next mount asks again.
 */
export function readGraphReadingShared(
  repoPath: string,
  read: (repoPath: string) => Promise<{ available: boolean }> = getCodeintelStatus,
  now: () => number = Date.now,
): Promise<GraphReading> {
  const cached = sharedReadings.get(repoPath);
  if (cached && now() - cached.at < GRAPH_READING_TTL_MS) return cached.reading;
  const entry = { at: now(), reading: readGraphReading(repoPath, read) };
  sharedReadings.set(repoPath, entry);
  void entry.reading.then((reading) => {
    if (reading === "unknown" && sharedReadings.get(repoPath) === entry) sharedReadings.delete(repoPath);
  });
  return entry.reading;
}

/** Drop a repository's shared reading, e.g. after its index was built. */
export function forgetGraphReading(repoPath?: string): void {
  if (repoPath === undefined) sharedReadings.clear();
  else sharedReadings.delete(repoPath);
}

/**
 * Run the code map build the Map panel runs and say how it ended, in the CLI's
 * own words when it failed. A thrown build is a failure, never a success.
 */
export async function buildGraphIndex(
  repoPath: string,
  build: (repoPath: string) => Promise<DevmapBuildOutcome> = buildDevmap,
): Promise<{ ok: true } | { ok: false; error: string }> {
  try {
    const outcome = await build(repoPath);
    if (outcome.ok) return { ok: true };
    return {
      ok: false,
      error: boundText(buildFailureReason(outcome, outcome.stdout.trim() || "the code map build failed")),
    };
  } catch (err) {
    return { ok: false, error: boundText(err instanceof Error ? err.message : String(err)) };
  }
}
