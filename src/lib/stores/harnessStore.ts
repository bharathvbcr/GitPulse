import { writable, get } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import { formatDiagnosticFailure } from "../diagnostics/diagnostics";
import { mergeEvents } from "../ledger/projection";
import type { LedgerEvent, LedgerStatus } from "../ledger/types";
import type { CatchUp } from "../ingest/types";
import {
  identityKey,
  isCaseInsensitiveFs,
  normalizeRepoPath,
  type PathIdentityOptions,
} from "../repos/paths";
import {
  appendAction,
  makeAgentAction,
  MAX_AGENT_ACTIONS,
  mergeActionEntries,
  type AgentActionEntry,
} from "../agents/activity";

/**
 * A policy decision from the MANVI harness.
 *
 * `status` and `checked` are separate on purpose: "allowed" and "nobody was
 * there to check" are different events, and the UI must never render them the
 * same way.
 */
export type PolicyStatus =
  | "allowed"
  | "demoted"
  | "granted"
  | "widened"
  | "degraded"
  | "warned"
  | "blocked"
  | "unchecked";

export interface PolicyVerdict {
  status: PolicyStatus;
  checked: boolean;
  target: string;
  rule: string;
  severity: string;
  reason: string;
  demoted: string;
  /** Non-empty when an override grant cleared a soft block. */
  grant_id: string;
  /** Who issued the grant named by `grant_id`. */
  granted_by: string;
  /** Non-empty when executor-appended scope, not the plan, authorised this. */
  widened: string;
  /** Checks that could not run. Empty means every rung ran. */
  degraded: string[];
  /**
   * The task this decision was measured against, empty when none was declared.
   *
   * The harness has always sent it; this end used to drop it, so a verdict
   * could not be attributed to the work it belonged to.
   */
  task_id: string;
  detail: string;
  detail_code: string;
}

/** A Git action's result together with the verdict it ran under. */
export interface Guarded<T> {
  policy: PolicyVerdict;
  output: T;
}

export interface HarnessStatus {
  available: boolean;
  binary: string;
  protocol: number;
  posture: string;
  ops: string[];
  error: string;
  error_code: string;
}

export interface DiscoveredEndpoint {
  base_url: string;
  models: string[];
  reachable: boolean;
  detail: string;
}

/**
 * The capability probe's result — named for the harness protocol type it
 * mirrors (`PrepareResult` / `ProbeResult` / `SettleResult`), so the wire
 * contract checker can see the two sides are the same type.
 */
export interface ProbeResult {
  model: string;
  context_window: number;
  source: string;
  discovered: boolean;
  describe: string;
  max_output_tokens: number;
  capabilities_known: boolean;
  supports_tools: boolean;
  supports_vision: boolean;
  supports_reasoning: boolean;
  embedding: boolean;
  served: string[];
}

export interface AiSelection {
  base_url: string;
  model: string;
}

export interface AiStatus {
  harness: HarnessStatus;
  endpoints: DiscoveredEndpoint[];
  selected: AiSelection | null;
  model_info: ProbeResult | null;
  model_detail: string;
  ready: boolean;
  detail: string;
}

export interface BudgetReport {
  planned_by_harness: boolean;
  before_tokens: number;
  threshold_tokens: number;
  insufficient: boolean;
  calibration_samples: number;
}

export interface AiGeneration {
  text: string;
  reasoning: string;
  model: string;
  base_url: string;
  context_window: number;
  context_source: string;
  prompt_tokens: number;
  completion_tokens: number;
  truncated: boolean;
  diff_truncated: boolean;
  diff_used_bytes: number;
  diff_total_bytes: number;
  budget: BudgetReport;
  warnings: string[];
  elapsed_ms: number;
}

export interface HarnessState {
  harness: HarnessStatus | null;
  ai: AiStatus | null;
  /** Model the user pinned, or null to let discovery choose. */
  preferred: AiSelection | null;
  isProbing: boolean;
  /** Most recent policy decision for the active repository, for the status strip. */
  lastVerdict: PolicyVerdict | null;
  error: string | null;
  /**
   * Journal of guarded actions, newest last.
   *
   * A *projection* of the durable ledger, not a store of its own: the rows are
   * on disk, this holds the tail of them for display. What is dropped past the
   * display cap is still queryable.
   */
  actions: AgentActionEntry[];
  /** Highest ledger id projected so far; where the next tail resumes. */
  ledgerCursor: number;
  /**
   * What the last catch-up found — the "while you were gone" summary.
   *
   * `skipped_lines` is part of it deliberately: a transcript line this build
   * could not read is a gap in what was observed, and a summary that hid it
   * would make a partial history look complete.
   */
  catchUp: CatchUp | null;
  /**
   * Whether the ledger is recording. Null before the first check.
   *
   * Rendered rather than assumed: a repository whose ledger cannot be opened
   * shows an empty journal, and so does a repository where nothing has
   * happened. Without this the two are the same picture.
   */
  ledger: LedgerStatus | null;
}

const STORAGE_KEY_MODEL = "gitpulse_ai_model";

/**
 * Rows per `cmd_ledger_tail` call.
 *
 * Distinct from `MAX_AGENT_ACTIONS`, which is now only a *display* cap: this
 * is how much is read per round trip while draining a repository's history.
 */
const LEDGER_PAGE = 200;

type RepositoryProjectionState = Pick<
  HarnessState,
  "lastVerdict" | "actions" | "ledgerCursor" | "catchUp" | "ledger"
>;

interface LedgerBucket extends RepositoryProjectionState {
  /** Canonical path used for backend calls and ephemeral action identity. */
  repoPath: string;
  /** Orders verdict writes when canonical and alias buckets converge. */
  verdictGeneration: number;
  /** Invalidates late status/tail responses for this repository. */
  generation: number;
  /** Independent because catch-up writes trigger their own tail notifications. */
  catchUpGeneration: number;
}

interface LedgerRun {
  key: string;
  repoPath: string;
  generation: number;
}

const UNSCOPED_REPOSITORY = "unscoped";
const LEDGER_PATH_SUFFIX = "/.devcouncil/ledger.sqlite";

function emptyLedgerBucket(repoPath: string): LedgerBucket {
  return {
    repoPath,
    verdictGeneration: 0,
    generation: 0,
    catchUpGeneration: 0,
    lastVerdict: null,
    actions: [],
    ledgerCursor: 0,
    catchUp: null,
    ledger: null,
  };
}

function ledgerProjection(bucket: LedgerBucket): RepositoryProjectionState {
  return {
    lastVerdict: bucket.lastVerdict,
    actions: bucket.actions,
    ledgerCursor: bucket.ledgerCursor,
    catchUp: bucket.catchUp,
    ledger: bucket.ledger,
  };
}

/**
 * An AI status probe nests a HarnessStatus that is often empty even when the
 * preceding sidecar handshake recorded a connection error. Keep that error
 * rather than letting a "clean" nested payload wipe it on refresh.
 */
function retainHarnessError(
  current: HarnessStatus | null,
  incoming: HarnessStatus,
): HarnessStatus {
  if (incoming.error) return incoming;
  if (current?.error) {
    return {
      ...incoming,
      available: false,
      ops: [],
      error: current.error,
      error_code: current.error_code,
    };
  }
  return incoming;
}

function unavailableHarness(
  current: HarnessStatus | null,
  detail: unknown,
): HarnessStatus {
  return {
    available: false,
    binary: current?.binary ?? "",
    protocol: current?.protocol ?? 0,
    posture: current?.posture ?? "",
    ops: [],
    error: formatDiagnosticFailure(detail),
    error_code: "status_unavailable",
  };
}

function loadPreferred(): AiSelection | null {
  try {
    const raw = localStorage.getItem(STORAGE_KEY_MODEL);
    if (!raw) return null;
    const parsed = JSON.parse(raw);
    if (parsed && typeof parsed.base_url === "string" && typeof parsed.model === "string") {
      return parsed;
    }
  } catch {
    /* a corrupt or unreadable preference just means "discover it" */
  }
  return null;
}

function savePreferred(selection: AiSelection | null) {
  try {
    if (selection) {
      localStorage.setItem(STORAGE_KEY_MODEL, JSON.stringify(selection));
    } else {
      localStorage.removeItem(STORAGE_KEY_MODEL);
    }
  } catch {
    /* ignore quota / private-mode failures */
  }
}

export interface HarnessStoreDeps {
  /** The object-argument subset of Tauri invoke used by this store. */
  invoke?: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
}

export function createHarnessStore(deps: HarnessStoreDeps = {}) {
  const invokeFn = deps.invoke ?? invoke;
  const pathIdentityOptions: PathIdentityOptions = {
    caseInsensitive: isCaseInsensitiveFs(),
  };
  const ledgerBuckets = new Map<string, LedgerBucket>();
  const repositoryAliases = new Map<string, string>();
  ledgerBuckets.set(UNSCOPED_REPOSITORY, emptyLedgerBucket(""));
  let activeRepositoryKey = UNSCOPED_REPOSITORY;
  // Globally ordered so two not-yet-resolved spellings of the same repository
  // cannot both carry an apparently-current per-bucket token when they merge.
  let nextLedgerGeneration = 0;
  let nextCatchUpGeneration = 0;
  let nextVerdictGeneration = 0;
  const { subscribe, update } = writable<HarnessState>({
    harness: null,
    ai: null,
    preferred: loadPreferred(),
    isProbing: false,
    lastVerdict: null,
    error: null,
    actions: [],
    ledgerCursor: 0,
    catchUp: null,
    ledger: null,
  });

  /**
   * Monotonic token for AI probes. refreshAi / selectModel / reconnect can
   * overlap (each awaits a slow local model server); when an older call
   * resolves late it must not overwrite the newer answer.
   */
  let probeToken = 0;

  /**
   * The native boundary and repoStore supply canonical paths. Normalization
   * here collapses harmless spelling differences (separator, trailing slash,
   * Unicode, and host filesystem case) so one canonical repository cannot
   * acquire two frontend cursors.
   */
  function rawRepositoryAddress(repoPath: string): { key: string; path: string } {
    const path = normalizeRepoPath(repoPath) ?? repoPath;
    const identity = identityKey(path, pathIdentityOptions);
    return { key: `repo:${identity || path}`, path };
  }

  function resolvedRepositoryKey(rawKey: string): string {
    let key = rawKey;
    const visited = new Set<string>();
    while (!visited.has(key)) {
      visited.add(key);
      const next = repositoryAliases.get(key);
      if (!next) break;
      key = next;
    }
    return key;
  }

  function repositoryAddress(repoPath: string): { key: string; path: string } {
    const raw = rawRepositoryAddress(repoPath);
    const key = resolvedRepositoryKey(raw.key);
    return { key, path: ledgerBuckets.get(key)?.repoPath ?? raw.path };
  }

  function bucketFor(repoPath: string): { key: string; bucket: LedgerBucket } {
    const address = repositoryAddress(repoPath);
    let bucket = ledgerBuckets.get(address.key);
    if (!bucket) {
      bucket = emptyLedgerBucket(address.path);
      ledgerBuckets.set(address.key, bucket);
    }
    return { key: address.key, bucket };
  }

  function canonicalRepoFromStatus(status: LedgerStatus): string | null {
    const path = normalizeRepoPath(status.path);
    if (!path?.endsWith(LEDGER_PATH_SUFFIX)) return null;
    const repoPath = path.slice(0, -LEDGER_PATH_SUFFIX.length);
    return normalizeRepoPath(repoPath);
  }

  /**
   * Native ledger storage canonicalizes repository paths, while older append
   * notifications can carry the caller's symlink spelling. The status path is
   * canonical and lets the frontend converge those spellings without a second
   * filesystem authority or a lossy lexical guess.
   */
  function adoptCanonicalRun(
    run: LedgerRun,
    status: LedgerStatus,
  ): LedgerRun | null {
    const source = ledgerBuckets.get(run.key);
    if (!source || source.generation !== run.generation) return null;
    const canonicalPath = canonicalRepoFromStatus(status);
    if (!canonicalPath) {
      mutateBucket(
        run.key,
        (bucket) => ({ ...bucket, ledger: status }),
        run.generation,
      );
      return run;
    }

    const canonicalRaw = rawRepositoryAddress(canonicalPath);
    const canonicalKey = resolvedRepositoryKey(canonicalRaw.key);
    if (canonicalKey === run.key) {
      mutateBucket(
        run.key,
        (bucket) => ({ ...bucket, repoPath: canonicalPath, ledger: status }),
        run.generation,
      );
      return { ...run, repoPath: canonicalPath };
    }

    const target =
      ledgerBuckets.get(canonicalKey) ?? emptyLedgerBucket(canonicalPath);
    const sourceLedgerIsNewer = source.generation >= target.generation;
    const generation = Math.max(source.generation, target.generation);
    const sourceCatchUpIsNewer =
      source.catchUpGeneration >= target.catchUpGeneration;
    const sourceVerdictIsNewer =
      source.verdictGeneration >= target.verdictGeneration;
    const merged: LedgerBucket = {
      repoPath: canonicalPath,
      verdictGeneration: Math.max(
        source.verdictGeneration,
        target.verdictGeneration,
      ),
      generation,
      catchUpGeneration: Math.max(
        source.catchUpGeneration,
        target.catchUpGeneration,
      ),
      actions: mergeActionEntries(
        target.actions,
        source.actions,
        MAX_AGENT_ACTIONS,
      ),
      ledgerCursor: Math.max(source.ledgerCursor, target.ledgerCursor),
      catchUp: sourceCatchUpIsNewer
        ? source.catchUp ?? target.catchUp
        : target.catchUp ?? source.catchUp,
      lastVerdict: sourceVerdictIsNewer
        ? source.lastVerdict
        : target.lastVerdict,
      ledger: sourceLedgerIsNewer ? status : target.ledger,
    };

    repositoryAliases.set(run.key, canonicalKey);
    for (const [alias, destination] of repositoryAliases) {
      if (destination === run.key) repositoryAliases.set(alias, canonicalKey);
    }
    ledgerBuckets.delete(run.key);
    ledgerBuckets.set(canonicalKey, merged);
    if (activeRepositoryKey === run.key) activeRepositoryKey = canonicalKey;
    publishBucket(canonicalKey, merged);
    return sourceLedgerIsNewer
      ? { key: canonicalKey, repoPath: canonicalPath, generation }
      : null;
  }

  function publishBucket(key: string, bucket: LedgerBucket): void {
    if (key !== activeRepositoryKey) return;
    update((state) => ({ ...state, ...ledgerProjection(bucket) }));
  }

  function mutateBucket(
    key: string,
    mutate: (bucket: LedgerBucket) => LedgerBucket,
    generation?: number,
  ): boolean {
    const current = ledgerBuckets.get(key);
    if (!current || (generation !== undefined && current.generation !== generation)) {
      return false;
    }
    const next = mutate(current);
    ledgerBuckets.set(key, next);
    publishBucket(key, next);
    return true;
  }

  function isCurrentLedgerRun(run: LedgerRun): boolean {
    return ledgerBuckets.get(run.key)?.generation === run.generation;
  }

  function isCurrentCatchUpRun(run: LedgerRun): boolean {
    const key = resolvedRepositoryKey(run.key);
    return ledgerBuckets.get(key)?.catchUpGeneration === run.generation;
  }

  function beginLedgerRun(repoPath: string): LedgerRun {
    const { key, bucket } = bucketFor(repoPath);
    const next = { ...bucket, generation: ++nextLedgerGeneration };
    ledgerBuckets.set(key, next);
    return { key, repoPath: next.repoPath, generation: next.generation };
  }

  function beginCatchUpRun(repoPath: string): LedgerRun {
    const { key, bucket } = bucketFor(repoPath);
    const next = {
      ...bucket,
      catchUpGeneration: ++nextCatchUpGeneration,
    };
    ledgerBuckets.set(key, next);
    return {
      key,
      repoPath: next.repoPath,
      generation: next.catchUpGeneration,
    };
  }

  function mutateCatchUpBucket(
    run: LedgerRun,
    mutate: (bucket: LedgerBucket) => LedgerBucket,
  ): boolean {
    const key = resolvedRepositoryKey(run.key);
    const current = ledgerBuckets.get(key);
    if (!current || current.catchUpGeneration !== run.generation) return false;
    const next = mutate(current);
    ledgerBuckets.set(key, next);
    publishBucket(key, next);
    return true;
  }

  function activateRepository(repoPath: string | null): void {
    if (!repoPath) {
      activeRepositoryKey = UNSCOPED_REPOSITORY;
      publishBucket(
        UNSCOPED_REPOSITORY,
        ledgerBuckets.get(UNSCOPED_REPOSITORY)!,
      );
      return;
    }
    const { key, bucket } = bucketFor(repoPath);
    activeRepositoryKey = key;
    publishBucket(key, bucket);
  }

  function currentPreferred(): AiSelection | null {
    return get({ subscribe }).preferred;
  }

  async function probeAi(token: number): Promise<AiStatus | null> {
    const preferred = currentPreferred();
    update((s) => ({ ...s, isProbing: true }));
    try {
      const ai = await invokeFn<AiStatus>("cmd_ai_status", {
        baseUrl: preferred?.base_url ?? null,
        model: preferred?.model ?? null,
      });
      if (token !== probeToken) return null;
      update((s) => ({
        ...s,
        ai,
        harness: retainHarnessError(s.harness, ai.harness),
        isProbing: false,
        error: null,
      }));
      return ai;
    } catch (err: unknown) {
      if (token !== probeToken) return null;
      update((s) => ({
        ...s,
        isProbing: false,
        harness: unavailableHarness(s.harness, err),
        error: formatDiagnosticFailure(err),
      }));
      return null;
    }
  }

  async function syncLedgerRun(run: LedgerRun): Promise<void> {
    let currentRun = run;
    try {
      const status = await invokeFn<LedgerStatus>("cmd_ledger_status", {
        repoPath: currentRun.repoPath,
      });
      const canonicalRun = adoptCanonicalRun(currentRun, status);
      if (!canonicalRun) return;
      currentRun = canonicalRun;
      let cursor = ledgerBuckets.get(currentRun.key)!.ledgerCursor;
      // Page until drained, so opening a repo with a long history shows all
      // of it rather than only the first window.
      for (;;) {
        const events = await invokeFn<LedgerEvent[]>("cmd_ledger_tail", {
          repoPath: currentRun.repoPath,
          cursor,
          limit: LEDGER_PAGE,
        });
        if (!isCurrentLedgerRun(currentRun)) return;
        if (events.length === 0) break;
        const nextCursor = events.reduce(
          (highest, event) => Math.max(highest, event.id),
          cursor,
        );
        // The backend query is already repository-scoped. Checking the row's
        // stored canonical path as well means a corrupt/misrouted row cannot
        // cross the final UI projection boundary.
        const matching = events.filter(
          (event) =>
            repositoryAddress(event.repo_path).key === currentRun.key,
        );
        const repositoryMismatch = matching.length !== events.length;
        if (
          !mutateBucket(
            currentRun.key,
            (bucket) => ({
              ...bucket,
              actions: mergeEvents(bucket.actions, matching, MAX_AGENT_ACTIONS),
              ledgerCursor: Math.max(bucket.ledgerCursor, nextCursor),
              ledger: repositoryMismatch
                ? {
                    recording: false,
                    path: bucket.ledger?.path ?? "",
                    dropped: bucket.ledger?.dropped ?? 0,
                    error:
                      "The ledger returned rows for a different repository; those rows were not displayed.",
                    error_code: "repository_mismatch",
                  }
                : bucket.ledger,
            }),
            currentRun.generation,
          )
        ) {
          return;
        }
        if (nextCursor <= cursor) break;
        cursor = nextCursor;
        if (events.length < LEDGER_PAGE) break;
      }
    } catch (e) {
      // A ledger read that fails must not blank the journal: the rows already
      // projected are still true. Record why, and leave them.
      mutateBucket(
        currentRun.key,
        (bucket) => ({
          ...bucket,
          ledger: {
            recording: false,
            path: bucket.ledger?.path ?? "",
            dropped: bucket.ledger?.dropped ?? 0,
            error: formatDiagnosticFailure(e),
            error_code: "read_failed",
          },
        }),
        currentRun.generation,
      );
    }
  }

  async function syncLedger(repoPath: string): Promise<void> {
    if (!repoPath) return;
    await syncLedgerRun(beginLedgerRun(repoPath));
  }

  const store = {
    subscribe,

    /** Projects only this canonical repository's ledger bucket into the UI. */
    activateRepository,

    /** Handshakes the sidecar and sweeps for local model servers. */
    refresh: async () => {
      const token = ++probeToken;
      try {
        const harness = await invokeFn<HarnessStatus>("cmd_harness_status");
        if (token !== probeToken) return null;
        update((s) => ({ ...s, harness, error: null }));
      } catch (err: unknown) {
        if (token !== probeToken) return null;
        update((s) => ({
          ...s,
          harness: unavailableHarness(s.harness, err),
          error: formatDiagnosticFailure(err),
        }));
      }
      return probeAi(token);
    },

    refreshAi: () => probeAi(++probeToken),

    /** Restarts the sidecar — the affordance after installing or updating MANVI. */
    reconnect: async () => {
      const token = ++probeToken;
      update((s) => ({ ...s, isProbing: true }));
      try {
        const harness = await invokeFn<HarnessStatus>("cmd_harness_reconnect");
        if (token !== probeToken) return null;
        update((s) => ({ ...s, harness, error: null }));
      } catch (err: unknown) {
        if (token !== probeToken) return null;
        update((s) => ({
          ...s,
          harness: unavailableHarness(s.harness, err),
          error: formatDiagnosticFailure(err),
        }));
      }
      return probeAi(token);
    },

    selectModel: (selection: AiSelection | null) => {
      savePreferred(selection);
      update((s) => ({ ...s, preferred: selection }));
      return probeAi(++probeToken);
    },

    recordVerdict: (verdict: PolicyVerdict | null, repoPath?: string) => {
      const target = repoPath
        ? bucketFor(repoPath)
        : {
            key: activeRepositoryKey,
            bucket: ledgerBuckets.get(activeRepositoryKey)!,
          };
      mutateBucket(target.key, (bucket) => ({
        ...bucket,
        lastVerdict: verdict,
        verdictGeneration: ++nextVerdictGeneration,
      }));
    },

    /**
     * Files one performed action into the journal. The entry is built here so
     * callers cannot forget the timestamp or id.
     */
    recordAction: (
      input: {
        /** Canonical repository that initiated an asynchronous action. */
        repoPath?: string;
        kind: string;
        label: string;
        ok: boolean;
        verdict?: PolicyVerdict | null;
      },
      now?: number,
    ) => {
      const target = input.repoPath
        ? bucketFor(input.repoPath)
        : {
            key: activeRepositoryKey,
            bucket: ledgerBuckets.get(activeRepositoryKey)!,
          };
      const entry = makeAgentAction(
        {
          kind: input.kind,
          label: input.label,
          ok: input.ok,
          verdict: input.verdict ?? null,
        },
        now,
        target.bucket.repoPath,
      );
      mutateBucket(target.key, (bucket) => ({
        ...bucket,
        actions: appendAction(bucket.actions, entry),
      }));
    },

    clearActions: () => {
      // Clears the *view*, never the ledger. The record on disk is what makes
      // the history answerable after a crash; a UI button must not be able to
      // erase it.
      mutateBucket(activeRepositoryKey, (bucket) => ({
        ...bucket,
        actions: [],
        generation: ++nextLedgerGeneration,
      }));
    },

    /**
     * Replays what happened while GitPulse was closed, then projects it.
     *
     * Both sources are observed rather than self-reported — git's reflog and
     * the agent transcripts — and both replays are idempotent, so this is safe
     * on every repo open. A failure is recorded and does not stop the ledger
     * sync: an incomplete catch-up still leaves the events already on disk
     * worth showing.
     */
    catchUp: async (repoPath: string): Promise<CatchUp | null> => {
      if (!repoPath) return null;
      // catchUp is the existing repo-open entry point, so activating here keeps
      // every current consumer compatible while switching the public
      // projection synchronously, before any backend response can arrive.
      activateRepository(repoPath);
      const run = beginCatchUpRun(repoPath);
      let summary: CatchUp | null = null;
      try {
        summary = await invokeFn<CatchUp>("cmd_catch_up", {
          repoPath: run.repoPath,
        });
        if (!isCurrentCatchUpRun(run)) return summary;
        mutateCatchUpBucket(run, (bucket) => ({ ...bucket, catchUp: summary }));
      } catch (e) {
        if (!isCurrentCatchUpRun(run)) return null;
        mutateCatchUpBucket(
          run,
          (bucket) => ({
            ...bucket,
            catchUp: {
              recorded: 0,
              transcripts: 0,
              skipped_lines: 0,
              reflog_entries: 0,
              error: formatDiagnosticFailure(e),
            },
          }),
        );
      }
      if (isCurrentCatchUpRun(run)) await syncLedger(run.repoPath);
      return summary;
    },

    /**
     * Loads durable events for a repository into the journal.
     *
     * Called on repo open and again whenever `ledger-appended` fires. Paging
     * from the stored cursor means a missed notification costs nothing: the
     * next call collects whatever was appended in between.
     */
    syncLedger,

    generateCommitMessage: async (repoPath: string): Promise<AiGeneration> => {
      const preferred = currentPreferred();
      return invokeFn<AiGeneration>("cmd_ai_generate_commit_message", {
        repoPath,
        baseUrl: preferred?.base_url ?? null,
        model: preferred?.model ?? null,
      });
    },

    explainCommit: async (repoPath: string, commitId: string): Promise<AiGeneration> => {
      const preferred = currentPreferred();
      return invokeFn<AiGeneration>("cmd_ai_explain_commit", {
        repoPath,
        commitId,
        baseUrl: preferred?.base_url ?? null,
        model: preferred?.model ?? null,
      });
    },

    suggestBranchName: async (repoPath: string): Promise<AiGeneration> => {
      const preferred = currentPreferred();
      return invokeFn<AiGeneration>("cmd_ai_suggest_branch_name", {
        repoPath,
        baseUrl: preferred?.base_url ?? null,
        model: preferred?.model ?? null,
      });
    },

    /** Turns a rendered dependency-health report into a remediation plan. */
    fixHealth: async (repoPath: string, report: string): Promise<AiGeneration> => {
      const preferred = currentPreferred();
      return invokeFn<AiGeneration>("cmd_ai_fix_health", {
        repoPath,
        report,
        baseUrl: preferred?.base_url ?? null,
        model: preferred?.model ?? null,
      });
    },

    /** Turns a rendered coverage report into a local-model analysis. */
    coverageReport: async (repoPath: string, report: string): Promise<AiGeneration> => {
      const preferred = currentPreferred();
      return invokeFn<AiGeneration>("cmd_ai_coverage_report", {
        repoPath,
        report,
        baseUrl: preferred?.base_url ?? null,
        model: preferred?.model ?? null,
      });
    },
  };

  return store;
}

export const harnessStore = createHarnessStore();

/** How a verdict is labelled in the UI. */
export function verdictLabel(verdict: PolicyVerdict): string {
  switch (verdict.status) {
    case "allowed":
      return "Policy: clean";
    case "demoted":
      return "Policy: allowed (no task scope)";
    case "granted":
      return "Policy: allowed by grant";
    case "widened":
      return "Policy: allowed by widened scope";
    case "degraded":
      return "Policy: allowed, not fully checked";
    case "warned":
      return "Policy: allowed with a warning";
    case "blocked":
      return "Policy: blocked";
    case "unchecked":
      return "Policy: not checked";
  }
}

/** The sentence under the label: why the verdict says what it says. */
export function verdictDetail(verdict: PolicyVerdict): string {
  switch (verdict.status) {
    case "allowed":
      return "The MANVI gate ran and no rule fired.";
    case "demoted":
      return verdict.demoted || verdict.reason;
    case "granted":
      // A rule fired and someone waived it. Naming the grantor is the point:
      // an unattributed waiver is indistinguishable from no rule firing.
      return verdict.granted_by
        ? `${verdict.rule} waived by ${verdict.granted_by} (${verdict.grant_id}).`
        : `${verdict.rule} waived by grant ${verdict.grant_id}.`;
    case "widened":
      return `Authorised by scope the agent added to its own task: ${verdict.widened}.`;
    case "degraded":
      return `Allowed, but these checks could not run: ${verdict.degraded.join(", ")}.`;
    case "warned":
      return verdict.reason;
    case "blocked":
      return `${verdict.rule}: ${verdict.reason}`;
    case "unchecked":
      return `No gate ran: ${verdict.detail}`;
  }
}
