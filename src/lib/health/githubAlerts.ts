/**
 * GitHub Dependabot and code scanning alerts: fetch, cache, and launch notify.
 *
 * The Health panel used to require an explicit click because these calls use
 * the GitHub CLI, its credentials, and the network. Launch now runs the same
 * commands when Settings → Analysis is on (the default). A check that did not
 * complete never toasts — that would look like an all-clear, or like a
 * warning about a request the user did not make.
 *
 * Serious means critical or high after {@link normalizeSeverity}, so CodeQL
 * `error` is high and an unknown spelling is info, never a silent promotion.
 */
import { invoke } from "@tauri-apps/api/core";
import { createRepoPanelCache } from "../panels/repoPanelCache";
import { formatError } from "../ui/formatError";
import { normalizeSeverity } from "./format";
import type { CodeScanningReport, DependabotReport } from "./types";

/**
 * Registered backend commands. Module-level literals so
 * `scripts/check-ipc-contract.mjs` can resolve them statically.
 */
export const GITHUB_DEPENDABOT_COMMAND = "cmd_github_dependabot_alerts";
export const GITHUB_CODE_SCANNING_COMMAND = "cmd_github_code_scanning_alerts";

/** Injectable IPC seam so tests never touch the real bridge. */
export interface GithubAlertsCommands {
  dependabot: (repoPath: string) => Promise<DependabotReport>;
  codeScanning: (repoPath: string) => Promise<CodeScanningReport>;
}

const defaultCommands: GithubAlertsCommands = {
  dependabot: (repoPath) =>
    invoke<DependabotReport>(GITHUB_DEPENDABOT_COMMAND, { repoPath }),
  codeScanning: (repoPath) =>
    invoke<CodeScanningReport>(GITHUB_CODE_SCANNING_COMMAND, { repoPath }),
};

/**
 * One Dependabot + code scanning fetch, including IPC failures folded into
 * the same fail-closed envelope the Health panel already renders.
 */
export interface GithubAlertsSnapshot {
  dependabot: DependabotReport;
  dependabotRequestFailed: boolean;
  codeScanning: CodeScanningReport;
  codeScanningRequestFailed: boolean;
  checkedAt: number;
}

/**
 * Survives Health remounts and the App boot path, so opening Insights →
 * Health after a launch scan shows the result instead of "not checked".
 */
export const githubAlertsCache = createRepoPanelCache<GithubAlertsSnapshot>();

const inflight = new Map<string, Promise<GithubAlertsSnapshot>>();

/**
 * Shared fail-closed envelope. `cli_present: true` is a sentinel, not
 * evidence — the snapshot's `*RequestFailed` bits are what the panel consults
 * so a missing CLI and a thrown invoke cannot look the same.
 */
function githubUnavailableEnvelope(error: string) {
  return {
    available: false as const,
    cli_present: true,
    is_github_remote: false,
    slug: "",
    truncated: false,
    error,
  };
}

export function githubTransportFailure(error: string): DependabotReport {
  return { ...githubUnavailableEnvelope(error), alerts: [] };
}

function codeScanningTransportFailure(error: string): CodeScanningReport {
  return { ...githubUnavailableEnvelope(error), alerts: [] };
}

function snapshotFromSettled(
  depSettled: PromiseSettledResult<DependabotReport>,
  csSettled: PromiseSettledResult<CodeScanningReport>,
  checkedAt: number,
): GithubAlertsSnapshot {
  const dependabot: DependabotReport =
    depSettled.status === "fulfilled"
      ? depSettled.value
      : githubTransportFailure(formatError(depSettled.reason));
  const codeScanning: CodeScanningReport =
    csSettled.status === "fulfilled"
      ? csSettled.value
      : codeScanningTransportFailure(formatError(csSettled.reason));
  return {
    dependabot,
    dependabotRequestFailed: depSettled.status === "rejected",
    codeScanning,
    codeScanningRequestFailed: csSettled.status === "rejected",
    checkedAt,
  };
}

async function fetchGithubAlerts(
  repoPath: string,
  commands: GithubAlertsCommands,
  now: () => number,
): Promise<GithubAlertsSnapshot> {
  const [depSettled, csSettled] = await Promise.allSettled([
    commands.dependabot(repoPath),
    commands.codeScanning(repoPath),
  ]);
  return snapshotFromSettled(depSettled, csSettled, now());
}

export interface LoadGithubAlertsOptions {
  /** Bypass the per-repo cache (the Health panel's explicit refresh). */
  force?: boolean;
  commands?: GithubAlertsCommands;
  now?: () => number;
}

/**
 * One in-flight fetch per repository path. A second caller (Health opening
 * while launch is still waiting) shares the promise rather than issuing a
 * second pair of `gh api` calls.
 */
export function loadGithubAlerts(
  repoPath: string,
  options?: LoadGithubAlertsOptions,
): Promise<GithubAlertsSnapshot> {
  const commands = options?.commands ?? defaultCommands;
  const now = options?.now ?? Date.now;
  const force = options?.force === true;
  // Injected commands are tests: they must not share the production cache.
  if (options?.commands) {
    return fetchGithubAlerts(repoPath, commands, now);
  }
  if (!force) {
    const cached = githubAlertsCache.get(repoPath);
    if (cached) return Promise.resolve(cached);
    const pending = inflight.get(repoPath);
    if (pending) return pending;
  }
  const pending = fetchGithubAlerts(repoPath, commands, now).then((snapshot) => {
    // A slower launch fetch must not overwrite a later Health refresh.
    if (inflight.get(repoPath) === pending) {
      githubAlertsCache.set(repoPath, snapshot);
    }
    return snapshot;
  });
  inflight.set(repoPath, pending);
  void pending.finally(() => {
    if (inflight.get(repoPath) === pending) inflight.delete(repoPath);
  });
  return pending;
}

export function isSeriousGithubSeverity(severity: string): boolean {
  const tier = normalizeSeverity(severity);
  return tier === "critical" || tier === "high";
}

export function seriousGithubAlerts(snapshot: GithubAlertsSnapshot): {
  dependabot: DependabotReport["alerts"];
  codeScanning: CodeScanningReport["alerts"];
} {
  return {
    dependabot: snapshot.dependabot.available
      ? snapshot.dependabot.alerts.filter((alert) =>
          isSeriousGithubSeverity(alert.severity),
        )
      : [],
    codeScanning: snapshot.codeScanning.available
      ? snapshot.codeScanning.alerts.filter((alert) =>
          isSeriousGithubSeverity(alert.severity),
        )
      : [],
  };
}

function countTiers(
  alerts: { severity: string }[],
): { critical: number; high: number } {
  let critical = 0;
  let high = 0;
  for (const alert of alerts) {
    const tier = normalizeSeverity(alert.severity);
    if (tier === "critical") critical += 1;
    else if (tier === "high") high += 1;
  }
  return { critical, high };
}

/**
 * Human sentence for a toast, or null when nothing serious is known.
 *
 * Null covers three different facts: no open serious alerts, a check that
 * could not run, and a local-only repository. Callers must not treat null as
 * "the repository is clean".
 */
export function describeSeriousGithubAlerts(
  snapshot: GithubAlertsSnapshot,
): string | null {
  const serious = seriousGithubAlerts(snapshot);
  const all = [...serious.dependabot, ...serious.codeScanning];
  if (all.length === 0) return null;

  const { critical, high } = countTiers(all);
  const bits: string[] = [];
  if (critical > 0) bits.push(`${critical} critical`);
  if (high > 0) bits.push(`${high} high`);
  const truncated =
    (snapshot.dependabot.available && snapshot.dependabot.truncated) ||
    (snapshot.codeScanning.available && snapshot.codeScanning.truncated);
  const total = critical + high;
  const noun = total === 1 ? "alert" : "alerts";
  const slug =
    snapshot.dependabot.slug || snapshot.codeScanning.slug || "";
  const where = slug ? ` on ${slug}` : "";
  const prefix = truncated ? "At least " : "";
  const sources: string[] = [];
  if (serious.dependabot.length > 0) {
    sources.push(
      `${serious.dependabot.length} Dependabot`,
    );
  }
  if (serious.codeScanning.length > 0) {
    sources.push(
      `${serious.codeScanning.length} code scanning`,
    );
  }
  const sourceNote =
    sources.length > 1 ? ` (${sources.join(", ")})` : "";
  return `${prefix}${bits.join(" and ")} GitHub ${noun}${where}${sourceNote}.`;
}

export function seriousGithubFingerprint(snapshot: GithubAlertsSnapshot): string {
  const serious = seriousGithubAlerts(snapshot);
  const ids = [
    ...serious.dependabot.map((alert) => `d:${alert.number}`),
    ...serious.codeScanning.map((alert) => `c:${alert.number}`),
  ].sort();
  const truncated = [
    snapshot.dependabot.available && snapshot.dependabot.truncated ? "dt" : "",
    snapshot.codeScanning.available && snapshot.codeScanning.truncated ? "ct" : "",
  ]
    .filter(Boolean)
    .join(",");
  return `${ids.join(",")}|${truncated}`;
}

function githubCheckFailed(snapshot: GithubAlertsSnapshot): string | null {
  if (snapshot.dependabotRequestFailed && snapshot.dependabot.error) {
    return snapshot.dependabot.error;
  }
  if (snapshot.codeScanningRequestFailed && snapshot.codeScanning.error) {
    return snapshot.codeScanning.error;
  }
  if (
    snapshot.dependabot.available === false &&
    snapshot.dependabot.error &&
    snapshot.codeScanning.available === false &&
    snapshot.codeScanning.error
  ) {
    return snapshot.dependabot.error || snapshot.codeScanning.error;
  }
  if (snapshot.dependabot.available === false && snapshot.dependabot.error) {
    return snapshot.dependabot.error;
  }
  if (snapshot.codeScanning.available === false && snapshot.codeScanning.error) {
    return snapshot.codeScanning.error;
  }
  return null;
}

export type GithubNotifyOutcome =
  | "skipped"
  | "clean"
  | "notified"
  | "failed"
  | "unavailable";

export interface GithubNotifyDeps {
  repoPath: string;
  enabled: boolean;
  load: (repoPath: string) => Promise<GithubAlertsSnapshot>;
  notify: (snapshot: GithubAlertsSnapshot, message: string) => void;
  onError: (message: string) => void;
  /** Per-session fingerprints; injected so tests do not share global memory. */
  notified?: Map<string, string>;
}

/**
 * Launch / repository-open path.
 *
 *   1. Preference off, or no path → no network.
 *   2. A failed or unavailable check is diagnostics, never a toast.
 *   3. Only critical/high findings notify, and the same set stays quiet for
 *      the rest of the session.
 */
export async function maybeNotifyGithubAlerts(
  deps: GithubNotifyDeps,
): Promise<GithubNotifyOutcome> {
  if (!deps.enabled || !deps.repoPath) return "skipped";

  const snapshot = await deps.load(deps.repoPath);
  const message = describeSeriousGithubAlerts(snapshot);
  const failure = githubCheckFailed(snapshot);
  if (message === null) {
    if (failure) {
      deps.onError(failure);
      return "failed";
    }
    if (!snapshot.dependabot.available && !snapshot.codeScanning.available) {
      return "unavailable";
    }
    return "clean";
  }

  const notified = deps.notified ?? defaultNotified;
  const fingerprint = seriousGithubFingerprint(snapshot);
  if (notified.get(deps.repoPath) === fingerprint) return "clean";
  notified.set(deps.repoPath, fingerprint);
  deps.notify(snapshot, message);
  // The other half still has to say it did not run; otherwise Health is the
  // only place a partial failure is visible.
  if (failure) deps.onError(failure);
  return "notified";
}

const defaultNotified = new Map<string, string>();
