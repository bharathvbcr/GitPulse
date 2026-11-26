import { writable, get, derived } from "svelte/store";
import { invoke, readIpcError } from "../ipc/invoke";
import { askConfirm, type ConfirmPromptOptions } from "./modalStore";
import { clean } from "../terminal/sessionActivity";
import { formatDisplayFailure } from "../diagnostics/report";
import { mergeEvents } from "../ledger/projection";
import {
  claudeCredentialProblem,
  claudeCredentialStatus,
  type ClaudeCredentialStatus,
} from "../workbench/claudeKey";
import type { ModelSelection } from "../workbench/taskModel";
import {
  ledgerStatus,
  type LedgerEvent,
  type LedgerStatus,
} from "../ledger/types";
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

/**
 * What `manvi use --json` said, read by the backend (`cmd_ai_status`). `state`
 * is `ready` when Manvi answered with a mode someone chose. `not_chosen` means
 * Manvi answered but its mode is the built-in default (cloud) nobody picked:
 * the fields say what it would run, `selection` is null, and AI features use
 * GitPulse's own loopback client until a mode is chosen in the Manvi pane.
 * `not_installed`, `needs_update` (a Manvi without `manvi use`) and `failed`
 * carry why in `detail`.
 */
export interface ManviStatus {
  state: "ready" | "not_chosen" | "not_installed" | "needs_update" | "failed";
  detail: string;
  /** `cloud`, `local` or `other`. */
  mode: string;
  provider: string;
  provider_from: string;
  model: string;
  model_from: string;
  /** Environment variables that override the saved choice for this process. */
  shadowed_by: string[];
  user_config: string;
  local_base_url: string;
  local_model: string;
  cloud_model: string;
  warnings: string[];
  /** Why GitPulse cannot use this mode; empty when `selection` is set. */
  unsupported: string;
  /** What the worker is spawned with, in the wire shape of a model selection. */
  selection: ModelSelection | null;
}

export interface AiStatus {
  harness: HarnessStatus;
  endpoints: DiscoveredEndpoint[];
  selected: AiSelection | null;
  model_info: ProbeResult | null;
  model_detail: string;
  ready: boolean;
  detail: string;
  manvi: ManviStatus;
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

/** The credential check for a Claude-selected Manvi: unknown until answered. */
export interface ClaudeCredentialState {
  status: ClaudeCredentialStatus | null;
  /** Why the check itself failed; never conflated with "no credential". */
  error: string | null;
}

/** Whether GitPulse's AI features can run right now, and on what. */
export interface AiReadiness {
  ready: boolean;
  /** What answers: the Claude API (cloud) or a model server on this machine (local). */
  target: "cloud" | "local";
  /**
   * Who carries the request: Manvi (`work.assist.complete`, in both modes), or
   * GitPulse's own loopback client when Manvi cannot be used.
   */
  via: "manvi" | "loopback";
  /** The model that would answer ("" when none is chosen). */
  model: string;
  /** Why it is not ready; empty when it is. */
  reason: string;
}

/**
 * The one answer to "can an AI feature run?", from what Manvi itself is set to
 * run on (`ai.manvi`, read by the backend from `manvi use`).
 *
 * - Cloud: ready only when the Claude credential check answered with no
 *   problem (an unanswered check is not ready, never assumed fine).
 * - Local through Manvi: ready when the local server is up (local discovery
 *   follows Manvi's own server and model).
 * - A mode GitPulse cannot use: not ready, and says so.
 * - Manvi unreadable (not installed, no `manvi use`, failed), on a mode nobody
 *   chose (`not_chosen`), or not asked yet: GitPulse's own loopback client,
 *   with local discovery's verdict.
 *
 * Pure, so it is testable without Svelte.
 */
export function deriveAiReadiness(state: Pick<HarnessState, "ai" | "claudeCredential">): AiReadiness {
  const ai = state.ai;
  const manvi = ai?.manvi;
  if (ai && manvi?.state === "ready") {
    const target = manvi.mode === "cloud" ? "cloud" : "local";
    if (!manvi.selection) {
      return { ready: false, target, via: "manvi", model: manvi.model, reason: manvi.unsupported };
    }
    if (manvi.mode === "cloud") {
      const { status, error } = state.claudeCredential;
      if (error) {
        return { ready: false, target, via: "manvi", model: manvi.model, reason: `Couldn't check the Claude credential. ${error}` };
      }
      if (!status) {
        return { ready: false, target, via: "manvi", model: manvi.model, reason: "Checking the Claude credential…" };
      }
      const problem = claudeCredentialProblem(status);
      return { ready: problem === null, target, via: "manvi", model: manvi.model, reason: problem ?? "" };
    }
    return {
      ready: ai.ready,
      target,
      via: "manvi",
      model: manvi.model,
      reason: ai.ready ? "" : ai.detail || "The local model server Manvi uses did not answer.",
    };
  }
  return {
    ready: ai?.ready ?? false,
    target: "local",
    via: "loopback",
    model: ai?.selected?.model ?? "",
    reason: ai?.detail || (ai ? "" : "Local models have not been checked."),
  };
}

/**
 * Whether a person can set Manvi's mode from GitPulse (`cmd_manvi_use`): Manvi
 * answered `manvi use`, with a chosen mode (`ready`) or with none
 * (`not_chosen`, which is exactly when a choice is wanted).
 */
export function manviWritable(manvi: Pick<ManviStatus, "state"> | null | undefined): boolean {
  return manvi?.state === "ready" || manvi?.state === "not_chosen";
}

/** How AI features name where a request goes, for tooltips and notes. */
export function describeAiTarget(readiness: AiReadiness): string {
  if (readiness.via === "loopback") return "the local model";
  return readiness.target === "cloud"
    ? `${readiness.model} (Claude API, via Manvi)`
    : `${readiness.model || "the local model"} (local model, via Manvi)`;
}

export interface HarnessState {
  harness: HarnessStatus | null;
  ai: AiStatus | null;
  /**
   * The local model the user pinned, or null to let discovery choose. Manvi's
   * own config (`ai.manvi`, from `manvi use`) decides what GitPulse's AI
   * features and task enhancements run on; this is what GitPulse's own
   * loopback client uses when Manvi cannot be read, and picking one here also
   * writes it to Manvi (`useManvi`).
   */
  preferred: AiSelection | null;
  /**
   * What the last credential check answered, for a cloud Manvi. Owned here so
   * the Manvi pill and every AI control read one answer
   * ([`deriveAiReadiness`]) instead of each asking the Keychain.
   */
  claudeCredential: ClaudeCredentialState;
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
/** Where GitPulse once kept its own copy of the Claude choice; Manvi's config owns it now. */
const RETIRED_KEY_MANVI_CLAUDE = "gitpulse_manvi_claude_model";

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

/**
 * The reason a status read failed, as the Manvi chip's tooltip prints it. A
 * runtime fault in this page is replaced by plain copy and recorded in
 * Diagnostics; anything else keeps the redacted text it always had.
 */
function statusFailure(detail: unknown): string {
  return formatDisplayFailure(detail, "manvi");
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
    error: statusFailure(detail),
    error_code: "status_unavailable",
  };
}

function loadPreferred(): AiSelection | null {
  try {
    const raw = localStorage.getItem(STORAGE_KEY_MODEL);
    if (!raw) return null;
    const parsed = JSON.parse(raw);
    if (
      parsed &&
      typeof parsed.base_url === "string" &&
      typeof parsed.model === "string"
    ) {
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
  /** Asks the user a yes/no question. Defaults to the in-app confirm dialog. */
  confirm?: (options: Omit<ConfirmPromptOptions, "mode">) => Promise<boolean>;
}

/** A model server the backend found but the user has not chosen. */
export interface UnconfirmedEndpoint {
  selection: AiSelection;
  /** The saved selection's server no longer serves its model; nothing to confirm. */
  stale: boolean;
  /** "Ollama on 127.0.0.1:11434": the runtime and the address that would receive the data. */
  label: string;
}

const MAX_PROMPT_NAME = 120;

/**
 * Text from a listener nobody vetted, made safe to show: the shared `clean()`
 * (hidden characters dropped, controls collapsed), bounded with an ellipsis.
 */
export function promptSafe(text: string): string {
  const cleaned = clean(text, MAX_PROMPT_NAME + 1) ?? "";
  const chars = Array.from(cleaned);
  return chars.length <= MAX_PROMPT_NAME
    ? cleaned
    : `${chars.slice(0, MAX_PROMPT_NAME).join("")}\u2026`;
}

/**
 * Reads the backend's "confirm this endpoint first" refusal. Only the typed
 * kind counts; nothing inside the message is read. Null for any other failure,
 * and for a detail that does not name an endpoint and a model.
 */
export function unconfirmedEndpoint(
  error: unknown,
): UnconfirmedEndpoint | null {
  const typed = readIpcError(error);
  if (typed?.kind !== "ai_endpoint_unconfirmed") return null;
  const detail = typed.detail;
  if (detail === null || typeof detail !== "object") return null;
  const { base_url, model, label, stale } = detail as Record<string, unknown>;
  if (
    typeof base_url !== "string" ||
    typeof model !== "string" ||
    typeof label !== "string"
  ) {
    return null;
  }
  if (!base_url || !model) return null;
  return {
    selection: { base_url, model: promptSafe(model) },
    stale: stale === true,
    label: promptSafe(label),
  };
}

export function createHarnessStore(deps: HarnessStoreDeps = {}) {
  const invokeFn = deps.invoke ?? invoke;
  const confirmFn = deps.confirm ?? askConfirm;
  try {
    localStorage.removeItem(RETIRED_KEY_MANVI_CLAUDE);
  } catch {
    /* no storage: nothing was kept there */
  }
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
    claudeCredential: { status: null, error: null },
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
  function rawRepositoryAddress(repoPath: string): {
    key: string;
    path: string;
  } {
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
        ? (source.catchUp ?? target.catchUp)
        : (target.catchUp ?? source.catchUp),
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
    if (
      !current ||
      (generation !== undefined && current.generation !== generation)
    ) {
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
      if (activeRepositoryKey === UNSCOPED_REPOSITORY) return;
      activeRepositoryKey = UNSCOPED_REPOSITORY;
      publishBucket(
        UNSCOPED_REPOSITORY,
        ledgerBuckets.get(UNSCOPED_REPOSITORY)!,
      );
      return;
    }
    const { key, bucket } = bucketFor(repoPath);
    // App's catch-up $effect used to call this on every repoStore emission
    // (~6s status poll). Each call built a new projection object and notified
    // every $harnessStore subscriber, even when the journal was already this
    // repository. A no-op when the key is unchanged is the same contract as
    // repoStore.setError: publishing identity is how a forwarding effect
    // becomes effect_update_depth_exceeded.
    if (key === activeRepositoryKey) return;
    activeRepositoryKey = key;
    publishBucket(key, bucket);
  }

  function currentPreferred(): AiSelection | null {
    return get({ subscribe }).preferred;
  }

  /**
   * One confirmation shared by every AI call that is refused while it is open:
   * a second concurrent action joins the dialog already on screen instead of
   * raising another. Cleared when no call is left waiting, so a later action
   * asks afresh.
   */
  let consentFlight: Promise<AiSelection | null> | null = null;
  let consentWaiters = 0;

  function askEndpointConsent(
    found: UnconfirmedEndpoint,
  ): Promise<AiSelection | null> {
    consentFlight ??= (async () => {
      const confirmed = await confirmFn({
        title: "Send to this local model server?",
        message:
          `${found.label}\n` +
          `Model: ${found.selection.model}\n\n` +
          "Waylode found this server running on this machine. Your staged changes or commit " +
          "will be sent to it. Nothing has been sent yet.",
        confirmLabel: "Send and remember",
        cancelLabel: "Don't send",
      });
      if (!confirmed) return null;
      // The dialog promises to remember this server for GitPulse's own
      // loopback client, nothing more: Manvi's config is written only from
      // the Manvi pane and the model pickers (`selectModel`).
      savePreferred(found.selection);
      update((s) => ({ ...s, preferred: found.selection }));
      void probeAi(++probeToken);
      return found.selection;
    })();
    return consentFlight;
  }

  /**
   * Runs one AI call with the saved selection. With none saved the backend
   * finds a server but sends nothing, answering `ai_endpoint_unconfirmed`; this
   * asks the user about exactly that runtime and address, saves the choice as
   * GitPulse's own loopback selection (`askEndpointConsent`: `savePreferred`
   * only, never `selectModel`, so Manvi's config is not written), and asks
   * again.
   * A saved selection whose server no longer serves its model is refused the
   * same way, marked stale: the saved choice is dropped and the refusal shown,
   * with no prompt about a server that is not there.
   * Declining rethrows the refusal, so nothing is ever sent unconfirmed.
   */
  async function withEndpointConsent<T>(
    call: (preferred: AiSelection | null) => Promise<T>,
  ): Promise<T> {
    consentWaiters += 1;
    try {
      try {
        return await call(currentPreferred());
      } catch (err: unknown) {
        const found = unconfirmedEndpoint(err);
        if (!found) throw err;
        if (found.stale) {
          if (currentPreferred() !== null) void store.selectModel(null);
          throw err;
        }
        const chosen = await askEndpointConsent(found);
        if (!chosen) throw err;
        return await call(chosen);
      }
    } finally {
      consentWaiters -= 1;
      if (consentWaiters === 0) consentFlight = null;
    }
  }

  let credentialToken = 0;
  /** Asks the backend which credential a Claude-selected Manvi would use. */
  async function refreshClaudeCredential(): Promise<void> {
    const token = ++credentialToken;
    try {
      const status = claudeCredentialStatus(await invokeFn<unknown>("cmd_claude_key_status"));
      if (token !== credentialToken) return;
      update((s) => ({ ...s, claudeCredential: { status, error: null } }));
    } catch (err: unknown) {
      if (token !== credentialToken) return;
      const error = formatDisplayFailure(err, "manvi");
      update((s) => ({ ...s, claudeCredential: { status: null, error } }));
    }
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
      // Cloud bills a credential GitPulse checks itself. Every refresh path
      // (startup, Refresh, Reconnect, a change of mode) ends here, so a stale
      // or stuck answer clears.
      if (ai.manvi?.state === "ready" && ai.manvi.mode === "cloud") void refreshClaudeCredential();
      return ai;
    } catch (err: unknown) {
      if (token !== probeToken) return null;
      update((s) => ({
        ...s,
        isProbing: false,
        harness: unavailableHarness(s.harness, err),
        error: statusFailure(err),
      }));
      return null;
    }
  }

  /** `manvi use <mode>`; see `store.useManvi`. */
  async function useManvi(
    mode: "cloud" | "local",
    options: { model?: string; baseUrl?: string } = {},
  ): Promise<ManviStatus> {
    const manvi = await invokeFn<ManviStatus>("cmd_manvi_use", {
      mode,
      model: options.model ?? null,
      baseUrl: options.baseUrl ?? null,
    });
    update((s) => ({
      ...s,
      ai: s.ai ? { ...s.ai, manvi } : s.ai,
      // A new choice invalidates the last credential answer.
      claudeCredential: { status: null, error: null },
    }));
    void probeAi(++probeToken);
    return manvi;
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
      const cursor = ledgerBuckets.get(currentRun.key)!.ledgerCursor;
      // The journal keeps only the newest MAX_AGENT_ACTIONS rows, so one
      // request for the newest window after the cursor is the whole answer.
      // Paging forward from 0 to reach it read the entire history, one
      // request per page, and each request runs `git worktree list` to
      // authenticate the repository: ~148 git processes at launch on a
      // 29,573-row ledger, for rows the cap then discarded.
      const events = await invokeFn<LedgerEvent[]>("cmd_ledger_tail", {
        repoPath: currentRun.repoPath,
        cursor,
        limit: MAX_AGENT_ACTIONS,
        newest: true,
      });
      if (!isCurrentLedgerRun(currentRun)) return;
      if (events.length === 0) return;
      const nextCursor = events.reduce(
        (highest, event) => Math.max(highest, event.id),
        cursor,
      );
      // The backend query is already repository-scoped. Checking the row's
      // stored canonical path as well means a corrupt/misrouted row cannot
      // cross the final UI projection boundary.
      const matching = events.filter(
        (event) => repositoryAddress(event.repo_path).key === currentRun.key,
      );
      const repositoryMismatch = matching.length !== events.length;
      mutateBucket(
        currentRun.key,
        (bucket) => ({
          ...bucket,
          actions: mergeEvents(bucket.actions, matching, MAX_AGENT_ACTIONS),
          ledgerCursor: Math.max(bucket.ledgerCursor, nextCursor),
          ledger: repositoryMismatch
            ? ledgerStatus({
                recording: false,
                path: bucket.ledger?.path ?? "",
                dropped: bucket.ledger?.dropped ?? 0,
                error:
                  "The ledger returned rows for a different repository; those rows were not displayed.",
                error_code: "repository_mismatch",
              })
            : bucket.ledger,
        }),
        currentRun.generation,
      );
    } catch (e) {
      // A ledger read that fails must not blank the journal: the rows already
      // projected are still true. Record why, and leave them.
      mutateBucket(
        currentRun.key,
        (bucket) => ({
          ...bucket,
          ledger: ledgerStatus({
            recording: false,
            path: bucket.ledger?.path ?? "",
            dropped: bucket.ledger?.dropped ?? 0,
            error: statusFailure(e),
            error_code: "read_failed",
          }),
        }),
        currentRun.generation,
      );
    }
  }

  /**
   * One sync per repository at a time, plus at most one after it.
   *
   * Every in-app append announces itself, and a catch-up appends a row per
   * replayed event, so a burst of announcements arrived while a sync was
   * still in flight, and each started another. Each sync runs
   * `cmd_ledger_status` and `cmd_ledger_tail`, and each of those runs
   * `git worktree list`, so it cost two git processes per appended row. A
   * request during a sync now marks it to run once more when it finishes. The
   * sync reads from the stored cursor, so that one re-run collects every row
   * the joined requests announced, and every joined caller resolves after it.
   */
  const ledgerSyncs = new Map<
    string,
    { again: boolean; done: Promise<void> }
  >();

  function syncLedger(repoPath: string): Promise<void> {
    if (!repoPath) return Promise.resolve();
    const key = resolvedRepositoryKey(repositoryAddress(repoPath).key);
    const running = ledgerSyncs.get(key);
    if (running) {
      running.again = true;
      return running.done;
    }
    const sync = { again: false, done: Promise.resolve() };
    sync.done = (async () => {
      try {
        do {
          sync.again = false;
          await syncLedgerRun(beginLedgerRun(repoPath));
        } while (sync.again);
      } finally {
        ledgerSyncs.delete(key);
      }
    })();
    ledgerSyncs.set(key, sync);
    return sync.done;
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
          error: statusFailure(err),
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
          error: statusFailure(err),
        }));
      }
      return probeAi(token);
    },

    /**
     * Picks a local model. Saved as GitPulse's own selection (the loopback
     * fallback uses it) and, when Manvi can be written, made Manvi's choice
     * too: `manvi use local --base-url U --model M`. Manvi can be written when
     * it answered, with a chosen mode or with none (`not_chosen`): picking a
     * model is the person's choice. `null` (Automatic) only clears the saved
     * selection; Manvi's config is left as it is.
     */
    selectModel: async (selection: AiSelection | null) => {
      savePreferred(selection);
      update((s) => ({ ...s, preferred: selection }));
      if (selection && manviWritable(get({ subscribe }).ai?.manvi)) {
        try {
          await useManvi("local", { baseUrl: selection.base_url, model: selection.model });
          return get({ subscribe }).ai;
        } catch (err: unknown) {
          update((s) => ({ ...s, error: statusFailure(err) }));
        }
      }
      return probeAi(++probeToken);
    },

    /**
     * The one writer of Manvi's own choice (`manvi use cloud|local …`), called
     * only from a person's action in the Manvi pane. The backend validates the
     * shapes and retires the live worker; this re-reads status afterwards so
     * every control shows what Manvi now says.
     */
    useManvi,

    /** Re-asks which Claude credential applies (after a fix in Settings). */
    refreshClaudeCredential,

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

    /**
     * Drops a closed repository's journal cache: its bucket and the spellings
     * that resolved to it. The ledger on disk is the record; this is only the
     * in-memory copy, which otherwise grows with every repository ever
     * opened. The unscoped bucket is never dropped, and an active bucket
     * falls back to it until the next repository is activated.
     */
    forgetRepository: (repoPath: string): void => {
      const key = repositoryAddress(repoPath).key;
      if (key === UNSCOPED_REPOSITORY || !ledgerBuckets.has(key)) return;
      ledgerBuckets.delete(key);
      for (const [alias, destination] of [...repositoryAliases]) {
        if (alias === key || destination === key)
          repositoryAliases.delete(alias);
      }
      repositoryAliases.delete(rawRepositoryAddress(repoPath).key);
      if (activeRepositoryKey === key) activateRepository(null);
    },

    /** The repositories whose journals are cached, for tests and diagnostics that watch for growth. */
    cachedRepositories: (): number => ledgerBuckets.size - 1,

    /**
     * Drops cached in-memory journal buckets for every repository except the
     * active one (and the unscoped bucket). Disk ledgers stay intact and reload
     * lazily on repo switch. Returns how many buckets were evicted.
     */
    evictInactiveRepositories: (): number => {
      let evicted = 0;
      for (const key of [...ledgerBuckets.keys()]) {
        if (key === UNSCOPED_REPOSITORY || key === activeRepositoryKey) continue;
        ledgerBuckets.delete(key);
        for (const [alias, destination] of [...repositoryAliases]) {
          if (alias === key || destination === key) repositoryAliases.delete(alias);
        }
        evicted += 1;
      }
      return evicted;
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
        mutateCatchUpBucket(run, (bucket) => ({
          ...bucket,
          catchUp: {
            recorded: 0,
            transcripts: 0,
            skipped_lines: 0,
            reflog_entries: 0,
            refused_outside_corpus: 0,
            truncated: false,
            error: statusFailure(e),
          },
        }));
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

    // Where these go is the backend's call: Manvi's own choice (`manvi use`),
    // through `work.assist.complete`, in either mode. `baseUrl`/`model` here
    // are only what GitPulse's own loopback client needs when Manvi cannot be
    // used; the consent flow below belongs to that fallback path alone.
    generateCommitMessage: async (repoPath: string): Promise<AiGeneration> => {
      return withEndpointConsent((preferred) =>
        invokeFn<AiGeneration>("cmd_ai_generate_commit_message", {
          repoPath,
          baseUrl: preferred?.base_url ?? null,
          model: preferred?.model ?? null,
        }),
      );
    },

    explainCommit: async (
      repoPath: string,
      commitId: string,
    ): Promise<AiGeneration> => {
      return withEndpointConsent((preferred) =>
        invokeFn<AiGeneration>("cmd_ai_explain_commit", {
          repoPath,
          commitId,
          baseUrl: preferred?.base_url ?? null,
          model: preferred?.model ?? null,
        }),
      );
    },

    suggestBranchName: async (repoPath: string): Promise<AiGeneration> => {
      return withEndpointConsent((preferred) =>
        invokeFn<AiGeneration>("cmd_ai_suggest_branch_name", {
          repoPath,
          baseUrl: preferred?.base_url ?? null,
          model: preferred?.model ?? null,
        }),
      );
    },

    /** Turns a rendered dependency-health report into a remediation plan. */
    fixHealth: async (
      repoPath: string,
      report: string,
    ): Promise<AiGeneration> => {
      return withEndpointConsent((preferred) =>
        invokeFn<AiGeneration>("cmd_ai_fix_health", {
          repoPath,
          report,
          baseUrl: preferred?.base_url ?? null,
          model: preferred?.model ?? null,
        }),
      );
    },

    /** Turns a rendered coverage report into an analysis by the selected model. */
    coverageReport: async (
      repoPath: string,
      report: string,
    ): Promise<AiGeneration> => {
      return withEndpointConsent((preferred) =>
        invokeFn<AiGeneration>("cmd_ai_coverage_report", {
          repoPath,
          report,
          baseUrl: preferred?.base_url ?? null,
          model: preferred?.model ?? null,
        }),
      );
    },
  };

  return store;
}

export const harnessStore = createHarnessStore();

/** The one readiness every AI control reads (see [`deriveAiReadiness`]). */
export const aiReadiness = derived(harnessStore, deriveAiReadiness);

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
      return "The Manvi gate ran and no rule fired.";
    case "demoted":
      return verdict.demoted || verdict.reason;
    case "granted":
      // A rule fired and someone waived it. Naming the grantor is the point:
      // an unattributed waiver is indistinguishable from no rule firing.
      return verdict.granted_by
        ? `${verdict.rule} waived by ${verdict.granted_by} (${verdict.grant_id}).`
        : `${verdict.rule} waived by grant ${verdict.grant_id}.`;
    case "widened":
      return `Authorized by scope the agent added to its own task: ${verdict.widened}.`;
    case "degraded":
      return `Allowed, but these checks couldn't run: ${verdict.degraded.join(", ")}.`;
    case "warned":
      return verdict.reason;
    case "blocked":
      return `${verdict.rule}: ${verdict.reason}`;
    case "unchecked":
      return `No gate ran: ${verdict.detail}`;
  }
}
