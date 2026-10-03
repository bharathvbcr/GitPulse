/**
 * Run DevCouncil's per-repository setup when a repository is opened, instead
 * of asking the user to run it by hand in every clone.
 *
 * Two jobs, one entry point:
 *
 * * **Once per repository** — keep its DevMap state directory out of
 *   `git status`. Cheap, idempotent, and a prerequisite for everything else:
 *   nothing here may create untracked state in a tree that would then show it.
 * * **Whenever the open-tab set changes** — make the active repository's
 *   workspace registry match the open tabs. That registry is what cross-repo
 *   symbol search and import-link candidates read; without this it stays empty
 *   and both features answer "nothing" for a reason that has nothing to do
 *   with the code.
 *
 * The Rust side owns every decision about what is safe to write (see
 * `src-tauri/src/devmap/init.rs`); this module only decides *when* to ask, and
 * makes sure it does not ask more often than the answer can change.
 *
 * ## Why a completed run is not always a finished one
 *
 * `initialize` succeeds even when it could not do everything: with no `devmap`
 * on PATH it still runs its ignore hygiene but refuses to create state for a
 * tool that is not installed, and it declines the registry outright when the
 * state directory could not be hidden. Memoizing those the same way as a run
 * that finished would make installing devmap a no-op for every repository
 * already open — cross-repository search would keep answering "nothing" for
 * the rest of the session, for a reason that was fixed minutes ago.
 *
 * So the memo records *why* a run stopped short. A run blocked by a missing
 * devmap is dropped by [`AutoInitController.onToolsChanged`], which the tool
 * probe calls whenever it sees devmap present — covering an install made in
 * this app and one made in a terminal alike. A run the spawn gate deferred
 * learned nothing, so it is not a finished answer either: it is asked again
 * after the same backoff the live index uses, and a re-submitted scope during
 * that wait does not ask early. Every other shortfall keeps the ordinary
 * rule: memoized against a re-submitted scope so it cannot spin, and retried
 * when the open-tab set actually changes. Deliberately not cleared by a tool
 * probe, because a probe cannot fix them and a repository whose ignore rule
 * stays refused would otherwise be re-initialized every time a panel looked
 * at the tool list.
 */

import { writable } from "svelte/store";
import {
  deferredRetryDelayMs,
  isDeferredUnderLoad,
  MAX_DEFERRED_RETRIES,
} from "../async/deferral";
import type { BackgroundScope } from "../async/pacedQueue";
import { diagnostics } from "../diagnostics/diagnostics";
import { initializeDevcouncil } from "./client";
import type { InitReport } from "./types";

/** Coalesce the scope churn of a tab switch before touching the filesystem. */
export const AUTO_INIT_DEBOUNCE_MS = 250;

export interface AutoInitSnapshot {
  /** Last report for this repository, or null while none has completed. */
  report: InitReport | null;
  /** Failure from the last attempt; cleared by a successful one. */
  error: string | null;
  running: boolean;
}

/**
 * Whether a run stopped short for a reason installing a tool would fix.
 *
 * The narrow question on purpose. `initialize` succeeds without devmap — it
 * still runs its ignore hygiene — but refuses to create state for a tool that
 * is not installed, so the workspace registry every cross-repository answer
 * reads is left unwritten. That is the one shortfall a tool probe can observe
 * the repair of, and therefore the only one it may act on: a refused exclude
 * or a failed registry write is repaired by the user, not by an install, and
 * re-running initialization for those on every probe would buy nothing.
 */
export function blockedOnMissingTool(report: InitReport): boolean {
  return !report.devmap_available;
}

/**
 * Whether this report is an unexamined tree rather than a decision.
 *
 * The spawn gate's marker is the only classifier, same as the live index:
 * a refused exclude whose reason is a real git answer ("a later rule
 * re-includes it") is a finished refusal, and one whose reason carries the
 * spawn gate's deferral marker means `check-ignore` never ran.
 */
export function stoppedShortUnderLoad(report: InitReport): boolean {
  if (report.exclude.status === "refused" && isDeferredUnderLoad(report.exclude.reason)) {
    return true;
  }
  return report.workspace_reason != null && isDeferredUnderLoad(report.workspace_reason);
}

export interface AutoInitController {
  readonly snapshots: ReturnType<typeof writable<Record<string, AutoInitSnapshot>>>;
  setScope(scope: BackgroundScope): void;
  /**
   * The installed tool set changed: drop every result that stopped short
   * because devmap was missing, and re-run the current scope.
   */
  onToolsChanged(): void;
  get(repoPath: string): AutoInitSnapshot;
  reset(): void;
}

const EMPTY: AutoInitSnapshot = { report: null, error: null, running: false };

/** Stable identity for an open-tab set: order must not cause a re-sync. */
function signature(activeKey: string | null, retained: readonly string[]): string {
  return JSON.stringify([activeKey, [...retained].sort()]);
}

export function createAutoInit(opts?: {
  debounceMs?: number;
  initialize?: (repoPath: string, openRepos: string[]) => Promise<InitReport>;
  warn?: (scope: string, message: string) => void;
}): AutoInitController {
  const debounceMs = opts?.debounceMs ?? AUTO_INIT_DEBOUNCE_MS;
  const initialize = opts?.initialize ?? initializeDevcouncil;
  const warn = opts?.warn ?? ((scope: string, message: string) => diagnostics.warn(scope, message));
  const snapshots = writable<Record<string, AutoInitSnapshot>>({});
  /**
   * Signature already applied, per repository — the re-ask guard. `blocked`
   * records that the answer was cut short by a missing tool and is therefore
   * worth redoing if one appears; see `blockedOnMissingTool`.
   */
  const applied = new Map<string, { sig: string; blocked: boolean }>();
  /**
   * A deferred run is unfinished, so it is not in `applied`. `until` is the
   * earliest the same signature may be asked again; an earlier flush waits
   * instead of starting another child into the gate that just refused one.
   */
  const held = new Map<string, { sig: string; until: number; attempt: number }>();
  let timer: ReturnType<typeof setTimeout> | null = null;
  let pending: { activeKey: string | null; retained: string[] } | null = null;
  let inFlight: Promise<void> | null = null;
  /** Last scope actually flushed, so an invalidation has something to re-run. */
  let lastScope: { activeKey: string; retained: string[] } | null = null;

  function patch(repoPath: string, next: Partial<AutoInitSnapshot>) {
    snapshots.update((map) => ({
      ...map,
      [repoPath]: { ...(map[repoPath] ?? EMPTY), ...next },
    }));
  }

  async function run(activeKey: string, retained: string[], sig: string) {
    patch(activeKey, { running: true });
    try {
      const report = await initialize(activeKey, retained);
      const deferred = stoppedShortUnderLoad(report);
      const attempt = deferred ? (held.get(activeKey)?.attempt ?? 0) + 1 : 0;
      const willRetry = deferred && attempt <= MAX_DEFERRED_RETRIES;
      const stillCurrent =
        lastScope !== null &&
        lastScope.activeKey === activeKey &&
        signature(lastScope.activeKey, lastScope.retained) === sig;
      if (willRetry) {
        const delayMs = deferredRetryDelayMs(attempt);
        held.set(activeKey, { sig, until: Date.now() + delayMs, attempt });
        // `pending` is set by a scope change that has not flushed yet. Stealing
        // the timer from it would drop that change and retry the old scope.
        const queuedSig = pending?.activeKey
          ? signature(pending.activeKey, pending.retained)
          : null;
        if (queuedSig === null && stillCurrent) {
          pending = { activeKey, retained: [...retained] };
          if (timer !== null) clearTimeout(timer);
          timer = setTimeout(flush, delayMs);
        }
      } else {
        held.delete(activeKey);
        applied.set(activeKey, { sig, blocked: blockedOnMissingTool(report) });
      }
      patch(activeKey, { report, error: null, running: false });
      // A refusal is not an error — the Rust side declined on purpose and
      // said why — but it is the reason a repository keeps showing untracked
      // index state, so it must not vanish silently. A deferral that will be
      // retried warns once; repeating it on every backoff would fill the
      // diagnostics panel with the same unexamined check.
      const announce = !deferred || attempt === 1 || !willRetry;
      if (announce && report.exclude.status === "refused") {
        warn(
          "devcouncil-init",
          `${activeKey}: DevMap state could not be hidden from git status — ${report.exclude.reason}`,
        );
      }
      if (announce && report.workspace_registry === null && report.workspace_reason) {
        warn(
          "devcouncil-init",
          `${activeKey}: cross-repository search has no registry — ${report.workspace_reason}`,
        );
      } else if (report.skipped_untrusted.length > 0) {
        warn(
          "devcouncil-init",
          `${activeKey}: cross-repository search omitted untrusted repositories — ${report.skipped_untrusted.join(", ")}`,
        );
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      // Do not record the signature: a failure must be retried when the scope
      // next changes rather than remembered as done.
      patch(activeKey, { error: message, running: false });
      warn("devcouncil-init", `${activeKey}: ${message}`);
    }
  }

  function flush() {
    timer = null;
    const next = pending;
    pending = null;
    if (!next?.activeKey) return;
    const { activeKey, retained } = next;
    lastScope = { activeKey, retained: [...retained] };
    const sig = signature(activeKey, retained);
    const waiting = held.get(activeKey);
    if (waiting && waiting.sig !== sig) held.delete(activeKey);
    if (waiting && waiting.sig === sig && Date.now() < waiting.until) {
      pending = { activeKey, retained: [...retained] };
      timer = setTimeout(flush, waiting.until - Date.now());
      return;
    }
    if (applied.get(activeKey)?.sig === sig) return;
    // One at a time: initialization takes a cross-process lock on the registry,
    // and a queue of stale scopes would each wait for it in turn.
    const start = () => run(activeKey, retained, sig);
    inFlight = inFlight ? inFlight.then(start, start) : start();
    inFlight = inFlight.then(() => {
      inFlight = null;
    });
  }

  return {
    snapshots,
    setScope(scope) {
      const open = new Set(scope.retainedKeys);
      for (const key of [...applied.keys()]) {
        if (!open.has(key)) applied.delete(key);
      }
      for (const key of [...held.keys()]) {
        if (!open.has(key)) held.delete(key);
      }
      snapshots.update((map) => {
        const closed = Object.keys(map).filter((key) => !open.has(key));
        if (!closed.length) return map;
        const next = { ...map };
        for (const key of closed) delete next[key];
        return next;
      });
      pending = { activeKey: scope.activeKey, retained: [...scope.retainedKeys] };
      if (timer !== null) clearTimeout(timer);
      timer = setTimeout(flush, debounceMs);
    },
    onToolsChanged() {
      if (!lastScope) return;
      let stale = false;
      for (const [repoPath, entry] of [...applied]) {
        if (!entry.blocked) continue;
        applied.delete(repoPath);
        stale = true;
      }
      // Nothing was waiting on a tool, so nothing to redo. Probes are frequent
      // and this is the steady state: a repository that finished is never
      // re-initialized because a panel happened to look at the tool list. It
      // is also self-limiting — a redone run that is still short is short for
      // some other reason, so it is no longer blocked and is never cleared
      // here again.
      if (!stale) return;
      pending = { activeKey: lastScope.activeKey, retained: [...lastScope.retained] };
      if (timer !== null) clearTimeout(timer);
      timer = setTimeout(flush, debounceMs);
    },
    get(repoPath) {
      let current = EMPTY;
      snapshots.subscribe((map) => {
        current = map[repoPath] ?? EMPTY;
      })();
      return current;
    },
    reset() {
      if (timer !== null) clearTimeout(timer);
      timer = null;
      pending = null;
      inFlight = null;
      lastScope = null;
      applied.clear();
      held.clear();
      snapshots.set({});
    },
  };
}

/** App-wide controller — driven from `App.svelte`'s background scope. */
export const autoInit = createAutoInit();
