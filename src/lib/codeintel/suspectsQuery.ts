/**
 * The regression-suspects question History › Graph is asking, and its answer.
 *
 * Suspects is one of Graph's three lists (`views/historyList.ts`). Its inputs
 * sit in History's context bar, its rows in the list pane, and its verdict on
 * the selected commit in the details pane; the commit and HEAD-moves lists
 * mark the commits it ranked. All of them read this one store, so the answer
 * survives switching lists (the list pane is swapped with `{#if}`, which would
 * drop component state) and every surface describes the same run.
 *
 * Kept free of the code-graph client: the context bar, the commit rows and the
 * details pane import this eagerly, and the client stays in its own chunk
 * until the first run asks for it (`run` loads it on demand).
 *
 * Two readings this must never let a surface blur:
 *
 *  - An empty list after a real walk ("nothing in the cone changed") and a
 *    refusal ("the walk never ran") are different answers: `available`.
 *  - `score` is comparable only within one answer. Surfaces render it as a
 *    rank and a bar relative to the top hit, never as a percentage.
 */
import { writable, get } from "svelte/store";
import { createAsyncGuard, type AsyncGuard } from "../async/guard";
import type { CodeintelSuspect, CodeintelSuspectScope, CodeintelSuspectsPayload } from "./types";

/**
 * The deepest cone the backend accepts (`MAX_CONE_DEPTH` in
 * src-tauri/src/codeintel/suspects.rs, held equal by a test). It refuses an
 * out-of-range depth rather than clamping it, so the input is bounded here.
 */
export const MAX_SUSPECT_CONE_DEPTH = 10;

/**
 * Default window. `HEAD~20` rather than the root commit because every commit
 * in the window costs a `git blame` per file the cone reaches, and a default
 * that walks the whole history would make the first run look broken.
 */
export const DEFAULT_SUSPECTS_SINCE = "HEAD~20";
export const DEFAULT_SUSPECTS_DEPTH = 3;

/** Symptom symbols are qualified names, not prose; this is a generous cap. */
export const MAX_SYMPTOM_LENGTH = 512;

/** One completed run: what was asked, and what came back. */
export interface SuspectsAnswer {
  symptom: string;
  since: string;
  available: boolean;
  /** Why the walk did not run; null whenever `available`. */
  reason: string | null;
  /** Ranked most-suspect first; empty when unavailable. */
  items: CodeintelSuspect[];
  /** What the walk saw; never below `items.length`. */
  total: number;
  truncated: boolean;
  walkIncomplete: string | null;
  /** Null only when the run was refused before it established a window. */
  scope: CodeintelSuspectScope | null;
}

export interface SuspectsQueryState {
  /** The repository the inputs and the answer belong to. */
  repoPath: string | null;
  symptom: string;
  since: string;
  depth: number;
  loading: boolean;
  /** A thrown error. Not a completed run: `answer` is null beside it. */
  error: string | null;
  /** Null until a run completes, so "no suspects" is never shown cold. */
  answer: SuspectsAnswer | null;
  /**
   * Set to ask the symptom field to take focus; the field clears it. A flag,
   * not a counter, because the field may mount only after the request (the
   * request is also what switches Graph onto this list).
   */
  focusPending: boolean;
}

/** A ranked commit, as the other lists and the details pane mark it. */
export interface SuspectMark {
  /** 1-based, in the answer's order. */
  rank: number;
  suspect: CodeintelSuspect;
  /** Names the symptom and window the rank came from. */
  title: string;
}

export function initialSuspectsState(repoPath: string | null): SuspectsQueryState {
  return {
    repoPath,
    symptom: "",
    since: DEFAULT_SUSPECTS_SINCE,
    depth: DEFAULT_SUSPECTS_DEPTH,
    loading: false,
    error: null,
    answer: null,
    focusPending: false,
  };
}

/** Folds the backend payload into what the surfaces render. */
export function answerFromPayload(
  payload: CodeintelSuspectsPayload,
  symptom: string,
  since: string,
): SuspectsAnswer {
  const response = payload.response;
  const available = response.available;
  return {
    symptom,
    since,
    available,
    reason: available ? null : (response.reason?.trim() || "the suspects walk did not run"),
    items: available ? response.items : [],
    // `total` counts what the walk saw; `items` is what fitted in the budget.
    // Never below the row count, so a heading cannot claim fewer than it lists.
    total: available ? Math.max(response.total ?? 0, response.items.length) : 0,
    truncated: available && response.truncated === true,
    walkIncomplete: response.walk_incomplete?.trim() || null,
    scope: payload.scope,
  };
}

/** Whether Run can be pressed: a repository, a symptom and a base ref, and nothing in flight. */
export function canRunSuspects(state: SuspectsQueryState, repoPath: string | null): boolean {
  return (
    Boolean(repoPath) &&
    state.symptom.trim().length > 0 &&
    state.since.trim().length > 0 &&
    Number.isInteger(state.depth) &&
    state.depth >= 1 &&
    state.depth <= MAX_SUSPECT_CONE_DEPTH &&
    !state.loading
  );
}

/**
 * Ranked commits by id, for the lists and the details pane to mark. Empty
 * unless the answer is this repository's and the walk ran: a refused answer
 * ranks nothing, and another repository's ranks are not this one's.
 */
export function suspectMarks(
  state: SuspectsQueryState,
  repoPath: string | null,
): ReadonlyMap<string, SuspectMark> {
  const marks = new Map<string, SuspectMark>();
  const answer = state.answer;
  if (!answer || !answer.available || !repoPath || state.repoPath !== repoPath) return marks;
  answer.items.forEach((suspect, index) => {
    if (marks.has(suspect.commit)) return;
    const rank = index + 1;
    marks.set(suspect.commit, {
      rank,
      suspect,
      title: `Suspect #${rank} of ${answer.total} for ${answer.symptom}, since ${answer.since} (${evidenceLabel(suspect.evidence)}, ${hopsLabel(suspect.nearest_distance)} from the symptom). Ranked by what the call graph reaches, not by date; cleared when the answer is.`,
    });
  });
  return marks;
}

/** Evidence is a three-state enum from the backend; unknown values pass through. */
export function evidenceLabel(evidence: string): string {
  if (evidence === "body_changed") return "body changed";
  if (evidence === "moved_only") return "moved only";
  if (evidence === "unknown") return "unknown";
  return evidence;
}

/** The chip tone for an evidence class (`gp-chip` `data-tone`). */
export function evidenceTone(evidence: string): "danger" | "warn" | "unknown" {
  if (evidence === "body_changed") return "danger";
  if (evidence === "moved_only") return "warn";
  return "unknown";
}

export function evidenceTitle(evidence: string): string {
  if (evidence === "body_changed")
    return "The commit changed the body of a symbol the symptom reaches.";
  if (evidence === "moved_only")
    return "The commit moved these lines without changing the body — a reformat or a move, not a behaviour change on its own.";
  return "Whether the body changed couldn't be established. Not the same as unchanged.";
}

/** Three states, never folded: "could not tell" is not "unchanged". */
export function bodyChangedLabel(changed: boolean | null): string {
  if (changed === true) return "changed";
  if (changed === false) return "moved";
  return "couldn't tell";
}

export function hopsLabel(distance: number): string {
  return `${distance} hop${distance === 1 ? "" : "s"}`;
}

/**
 * The bar width for a score, relative to the top hit in the same answer.
 * Never a percentage of anything else, and a zero top score draws no bar.
 */
export function relativeScoreWidth(score: number, topScore: number): number {
  return topScore > 0 ? Math.round((score / topScore) * 100) : 0;
}

export function topSuspectScore(items: readonly CodeintelSuspect[]): number {
  return items.reduce((max, s) => Math.max(max, s.score), 0);
}

type FetchSuspects = (
  repoPath: string,
  symptom: string,
  since: string,
  depth: number,
) => Promise<CodeintelSuspectsPayload>;

export interface SuspectsQueryDeps {
  fetch?: FetchSuspects;
  /** Records a thrown error and returns the message to show. */
  onError?: (err: unknown) => string | Promise<string>;
}

async function defaultFetch(...args: Parameters<FetchSuspects>) {
  const { getSuspects } = await import("./client");
  return getSuspects(...args);
}

async function defaultOnError(err: unknown): Promise<string> {
  const [{ reportPanelError }, { formatError }] = await Promise.all([
    import("../diagnostics/report"),
    import("../ui/formatError"),
  ]);
  reportPanelError("suspects", err);
  return formatError(err);
}

export function createSuspectsQuery(deps: SuspectsQueryDeps = {}) {
  const fetchSuspects = deps.fetch ?? defaultFetch;
  const onError = deps.onError ?? defaultOnError;
  const store = writable<SuspectsQueryState>(initialSuspectsState(null));
  let inflight: AsyncGuard | null = null;

  function cancel() {
    inflight?.cancel();
    inflight = null;
  }

  /**
   * Holds the store to a repository. A different repository invalidates the
   * answer and the inputs alike: stale suspects under a new repository's name
   * would be read as that repository's, and its symbols are not this one's.
   */
  function bindRepo(repoPath: string | null) {
    const held = get(store);
    if (held.repoPath === repoPath) return;
    cancel();
    // A focus request outlives the bind: the palette asks for the symptom
    // field before History has mounted and bound its repository.
    store.set({ ...initialSuspectsState(repoPath), focusPending: held.focusPending });
  }

  /** Inputs change the question, not the answer: the last run stays until the next one lands. */
  function setInputs(patch: Partial<Pick<SuspectsQueryState, "symptom" | "since" | "depth">>) {
    store.update((s) => ({ ...s, ...patch }));
  }

  async function run(repoPath: string | null): Promise<void> {
    bindRepo(repoPath);
    const state = get(store);
    // A run in flight does not block a new question: the button is disabled
    // meanwhile, but a re-seed from the details pane supersedes it, and the
    // guard below is what keeps the older answer from landing.
    if (!repoPath || !canRunSuspects({ ...state, loading: false }, repoPath)) return;
    const symptom = state.symptom.trim().slice(0, MAX_SYMPTOM_LENGTH);
    const since = state.since.trim();
    cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    store.update((s) => ({ ...s, loading: true, error: null }));
    try {
      const payload = await fetchSuspects(repoPath, symptom, since, state.depth);
      if (!guard.isLive()) return;
      store.update((s) => ({ ...s, answer: answerFromPayload(payload, symptom, since) }));
    } catch (err: unknown) {
      if (!guard.isLive()) return;
      const message = await onError(err);
      if (!guard.isLive()) return;
      // A thrown error is not a completed run: no answer, so no list claims
      // to have found nothing, and no other list keeps marking last run's ranks.
      store.update((s) => ({ ...s, error: message, answer: null }));
    } finally {
      if (guard.isLive()) {
        inflight = null;
        store.update((s) => ({ ...s, loading: false }));
      }
    }
  }

  return {
    subscribe: store.subscribe,
    bindRepo,
    setInputs,
    run,
    /**
     * "Find suspects since this commit": the selected commit becomes the last
     * known good. Runs at once when a symptom is already named; otherwise asks
     * the symptom field for focus, since that is the one thing still missing.
     */
    seedSince: (repoPath: string | null, commitId: string) => {
      bindRepo(repoPath);
      store.update((s) => ({ ...s, since: commitId }));
      if (get(store).symptom.trim()) return run(repoPath);
      store.update((s) => ({ ...s, focusPending: true }));
      return Promise.resolve();
    },
    /**
     * Asks about a symptom named somewhere else (Blame's file, a diff hunk's
     * symbol, a node on the code graph): it becomes the symptom and the
     * question runs against the window already set.
     */
    askAbout: (repoPath: string | null, symptom: string) => {
      const named = symptom.trim().slice(0, MAX_SYMPTOM_LENGTH);
      if (!named) return Promise.resolve();
      bindRepo(repoPath);
      store.update((s) => ({ ...s, symptom: named }));
      return run(repoPath);
    },
    /** Asks the symptom field for focus, once it is on screen. */
    requestFocus: () => store.update((s) => ({ ...s, focusPending: true })),
    /** The symptom field took focus. */
    consumeFocus: () =>
      store.update((s) => (s.focusPending ? { ...s, focusPending: false } : s)),
  };
}

export type SuspectsQuery = ReturnType<typeof createSuspectsQuery>;

export const suspectsQuery = createSuspectsQuery();
