import { writable, get } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import { formatError } from "../ui/formatError";
import {
  appendAction,
  makeAgentAction,
  type AgentActionEntry,
} from "../agents/activity";

/**
 * A policy decision from the MANVI harness.
 *
 * `status` and `checked` are separate on purpose: "allowed" and "nobody was
 * there to check" are different events, and the UI must never render them the
 * same way.
 */
export type PolicyStatus = "allowed" | "demoted" | "warned" | "blocked" | "unchecked";

export interface PolicyVerdict {
  status: PolicyStatus;
  checked: boolean;
  target: string;
  rule: string;
  severity: string;
  reason: string;
  demoted: string;
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
  /** Most recent policy decision, for the status strip. */
  lastVerdict: PolicyVerdict | null;
  error: string | null;
  /** Journal of guarded actions, newest last. Survives for the session. */
  actions: AgentActionEntry[];
}

const STORAGE_KEY_MODEL = "gitpulse_ai_model";

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
    return { ...incoming, error: current.error, error_code: current.error_code };
  }
  return incoming;
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
  invoke?: typeof invoke;
}

export function createHarnessStore(deps: HarnessStoreDeps = {}) {
  const invokeFn = deps.invoke ?? invoke;
  const { subscribe, update } = writable<HarnessState>({
    harness: null,
    ai: null,
    preferred: loadPreferred(),
    isProbing: false,
    lastVerdict: null,
    error: null,
    actions: [],
  });

  /**
   * Monotonic token for AI probes. refreshAi / selectModel / reconnect can
   * overlap (each awaits a slow local model server); when an older call
   * resolves late it must not overwrite the newer answer.
   */
  let probeToken = 0;

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
      }));
      return ai;
    } catch (err: unknown) {
      if (token !== probeToken) return null;
      update((s) => ({ ...s, isProbing: false, error: s.error ?? formatError(err) }));
      return null;
    }
  }

  return {
    subscribe,

    /** Handshakes the sidecar and sweeps for local model servers. */
    refresh: async () => {
      const token = ++probeToken;
      try {
        const harness = await invokeFn<HarnessStatus>("cmd_harness_status");
        if (token !== probeToken) return null;
        update((s) => ({ ...s, harness }));
      } catch (err: unknown) {
        if (token !== probeToken) return null;
        update((s) => ({ ...s, error: formatError(err) }));
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
        update((s) => ({ ...s, harness }));
      } catch (err: unknown) {
        if (token !== probeToken) return null;
        update((s) => ({ ...s, error: formatError(err) }));
      }
      return probeAi(token);
    },

    selectModel: (selection: AiSelection | null) => {
      savePreferred(selection);
      update((s) => ({ ...s, preferred: selection }));
      return probeAi(++probeToken);
    },

    recordVerdict: (verdict: PolicyVerdict | null) => {
      update((s) => ({ ...s, lastVerdict: verdict }));
    },

    /**
     * Files one performed action into the journal. The entry is built here so
     * callers cannot forget the timestamp or id.
     */
    recordAction: (
      input: { kind: string; label: string; ok: boolean; verdict?: PolicyVerdict | null },
      now?: number
    ) => {
      const entry = makeAgentAction(
        {
          kind: input.kind,
          label: input.label,
          ok: input.ok,
          verdict: input.verdict ?? null,
        },
        now
      );
      update((s) => ({ ...s, actions: appendAction(s.actions, entry) }));
    },

    clearActions: () => {
      update((s) => ({ ...s, actions: [] }));
    },

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
}

export const harnessStore = createHarnessStore();

/** How a verdict is labelled in the UI. */
export function verdictLabel(verdict: PolicyVerdict): string {
  switch (verdict.status) {
    case "allowed":
      return "Policy: clean";
    case "demoted":
      return "Policy: allowed (no task scope)";
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
    case "warned":
      return verdict.reason;
    case "blocked":
      return `${verdict.rule}: ${verdict.reason}`;
    case "unchecked":
      return `No gate ran: ${verdict.detail}`;
  }
}
