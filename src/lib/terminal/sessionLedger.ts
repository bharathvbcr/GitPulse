/**
 * Frontend client and reactive store for the durable backend session ledger
 * (`src-tauri/src/terminal/session_ledger.rs`, S2-03 & S2-05).
 *
 * Canonical owner for the ledger wire structs and enum (`SessionEndState`,
 * `TerminalResume`, `SessionLedgerOffer`, `SessionLedgerView`,
 * `SessionReopenCheck`): both `TerminalPanel.svelte` and Health consume this
 * module rather than localStorage or inline toast state.
 */

import { listen } from "@tauri-apps/api/event";
import { get, writable, type Readable } from "svelte/store";
import { invoke } from "../ipc/invoke";
import { isTauri } from "../platform";
import { isCaseInsensitiveFs, sameRepo, type PathIdentityOptions } from "../repos/paths";
import { formatError } from "../ui/formatError";
import { isPermissionMode, type PermissionMode } from "./agentDefaults";
import { repoStoreFocus, type RepoFocusActions } from "./sessionFocus";
import { launcherLabel } from "./tabs";
import { enqueueTaskTerminal, type TaskTerminalRequest } from "./taskLaunches";

/** Lifecycle end state of a recorded terminal tab row. */
export type SessionEndState =
  | "running"
  | "exited_clean"
  | "exited_signal"
  | "app_quit"
  | "lost"
  | "unreadable";

/** Typed resume (or fork-resume) request sent to `cmd_terminal_spawn` (S2-07). */
export interface TerminalResume {
  session_id: string;
  fork: boolean;
}

/** One recovery row projected from the durable backend ledger. */
export interface SessionLedgerOffer {
  row_id: string;
  terminal_id: string | null;
  launcher: string;
  native_id: string | null;
  repo_path: string;
  start_dir: string | null;
  cwd: string;
  mode: string | null;
  started_at: number;
  ended_at: number | null;
  end_state: SessionEndState;
  signal: string | null;
  resumable: boolean;
  reason: string | null;
  last_error: string | null;
  title?: string;
  /**
   * `row_id` of the orchestrating agent that spawned this one. Absent from
   * older backends, and omitted when there is no parent.
   */
  parent_row_id?: string;
}

/** Snapshot of durable recovery offers returned by the backend ledger. */
export interface SessionLedgerView {
  offers: SessionLedgerOffer[];
  reopenable_count: number;
  unreadable_count: number;
  /**
   * True while the backend has not yet reconciled the ledger against the live
   * processes, so an empty or short `offers` is not a statement that nothing
   * needs recovering. Absent from older backends: read it as false.
   */
  reconcile_pending?: boolean;
}

/** Result of pre-checking whether one ledger row can be reopened right now. */
export interface SessionReopenCheck {
  offer: SessionLedgerOffer;
}

export const SESSION_LEDGER_EVENT = "terminal-session-ledger";

const EMPTY_VIEW: SessionLedgerView = {
  offers: [],
  reopenable_count: 0,
  unreadable_count: 0,
};

const ledgerStore = writable<SessionLedgerView>(EMPTY_VIEW);

export const sessionLedgerStore: Readable<SessionLedgerView> = {
  subscribe: ledgerStore.subscribe,
};

function recomputeCounts(offers: readonly SessionLedgerOffer[], reconcilePending = false): SessionLedgerView {
  return {
    offers: [...offers],
    reopenable_count: offers.filter((offer) => offer.resumable).length,
    unreadable_count: offers.filter((offer) => offer.end_state === "unreadable").length,
    ...(reconcilePending ? { reconcile_pending: true } : {}),
  };
}

function mergeOfferInStore(updated: SessionLedgerOffer): void {
  ledgerStore.update((current) => {
    const idx = current.offers.findIndex((item) => item.row_id === updated.row_id);
    const nextOffers =
      idx === -1
        ? [updated, ...current.offers]
        : current.offers.map((item, i) => (i === idx ? updated : item));
    return recomputeCounts(nextOffers, current.reconcile_pending === true);
  });
}

function recordOfferError(rowId: string, error: string): SessionLedgerOffer | null {
  let recorded: SessionLedgerOffer | null = null;
  ledgerStore.update((current) => {
    const nextOffers = current.offers.map((item) => {
      if (item.row_id !== rowId) return item;
      recorded = { ...item, last_error: error };
      return recorded;
    });
    return recorded ? recomputeCounts(nextOffers, current.reconcile_pending === true) : current;
  });
  return recorded;
}

/** Replaces the reactive ledger snapshot in unit tests. */
export function setSessionLedgerViewForTest(view: SessionLedgerView): void {
  ledgerStore.set(recomputeCounts(view.offers, view.reconcile_pending === true));
}

export function normalizeSessionLedgerView(raw: unknown): SessionLedgerView {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return EMPTY_VIEW;
  const candidate = raw as Partial<SessionLedgerView>;
  if (!Array.isArray(candidate.offers)) return EMPTY_VIEW;
  return recomputeCounts(candidate.offers, candidate.reconcile_pending === true);
}

/** Reads the current durable session ledger from the backend. */
export async function refreshSessionLedger(): Promise<SessionLedgerView> {
  if (!isTauri()) {
    return get(ledgerStore);
  }
  const raw = await invoke<unknown>("cmd_terminal_session_ledger");
  const view = normalizeSessionLedgerView(raw);
  ledgerStore.set(view);
  return view;
}

let subscriberCount = 0;
let eventStop: (() => void) | null = null;
let listenEpoch = 0;

/**
 * Keeps `sessionLedgerStore` synchronized with `terminal-session-ledger`
 * events and loads the latest snapshot from disk on first subscription.
 */
export function subscribeSessionLedger(): () => void {
  subscriberCount += 1;
  if (subscriberCount === 1 && isTauri()) {
    const epoch = ++listenEpoch;
    void listen<unknown>(SESSION_LEDGER_EVENT, (event) => {
      ledgerStore.set(normalizeSessionLedgerView(event.payload));
    })
      .then((unlisten) => {
        if (epoch !== listenEpoch || subscriberCount === 0) {
          unlisten();
          return;
        }
        eventStop = unlisten;
      })
      .catch(() => {});
    void refreshSessionLedger().catch(() => {});
  }
  let active = true;
  return () => {
    if (!active) return;
    active = false;
    subscriberCount = Math.max(0, subscriberCount - 1);
    if (subscriberCount === 0) {
      listenEpoch += 1;
      eventStop?.();
      eventStop = null;
    }
  };
}

/** Explicit user dismiss: withdraws the ledger row from disk and all surfaces. */
export async function dismissSessionLedgerRow(rowId: string): Promise<SessionLedgerView> {
  if (!isTauri()) {
    let next: SessionLedgerView = EMPTY_VIEW;
    ledgerStore.update((current) => {
      next = recomputeCounts(
        current.offers.filter((offer) => offer.row_id !== rowId),
        current.reconcile_pending === true,
      );
      return next;
    });
    return next;
  }
  const view = await invoke<SessionLedgerView>("cmd_terminal_session_ledger_dismiss", { rowId });
  ledgerStore.set(view);
  return view;
}

/**
 * Re-checks whether `rowId` can be reopened right now (cwd exists and
 * transcript is present) and preserves per-row errors while keeping the row
 * listed if reopening fails.
 */
export async function checkReopenSessionLedgerRow(rowId: string): Promise<SessionReopenCheck> {
  const checked = await invoke<SessionReopenCheck>("cmd_terminal_session_ledger_reopen_check", { rowId });
  mergeOfferInStore(checked.offer);
  return checked;
}

/** Filters backend ledger rows for one repository checkout (keeping corrupt rows visible). */
export function filterOffersForRepo(
  offers: readonly SessionLedgerOffer[],
  repoPath: string | null | undefined,
  options: PathIdentityOptions = { caseInsensitive: isCaseInsensitiveFs() },
): SessionLedgerOffer[] {
  return offers.filter((offer) => {
    if (offer.end_state === "unreadable") return true;
    if (!repoPath || !offer.repo_path) return false;
    return sameRepo(offer.repo_path, repoPath, options);
  });
}

/** Subset of offers that can actually be reopened as agent tabs right now. */
export function reopenableOffers(offers: readonly SessionLedgerOffer[]): SessionLedgerOffer[] {
  return offers.filter((offer) => offer.resumable && Boolean(offer.native_id));
}

/** The offered ancestor a row hangs under, or null when it is itself top-level. */
function offeredRoot(offer: SessionLedgerOffer, byId: ReadonlyMap<string, SessionLedgerOffer>): SessionLedgerOffer | null {
  const seen = new Set<string>([offer.row_id]);
  let root: SessionLedgerOffer | null = null;
  let cursor = offer;
  for (;;) {
    const parentId = cursor.parent_row_id ?? null;
    const parent = parentId === null ? undefined : byId.get(parentId);
    // A parent that cannot be resumed will not re-spawn its children, so they
    // stand on their own (and count for "Reopen all") rather than wait on it.
    if (!parent || !parent.resumable) return root;
    // A cycle (or a self-parent) has no top: every member stays a normal offer.
    if (seen.has(parent.row_id)) return null;
    seen.add(parent.row_id);
    root = parent;
    cursor = parent;
  }
}

/**
 * Offers that stand on their own: those with no parent, or whose parent is not
 * itself offered. An orchestrated child whose parent is offered is not here:
 * resuming the parent may re-spawn it, so it is shown beneath the parent and
 * left out of every "Reopen all".
 */
export function topLevelOffers(offers: readonly SessionLedgerOffer[]): SessionLedgerOffer[] {
  const byId = new Map(offers.map((offer) => [offer.row_id, offer]));
  return offers.filter((offer) => offeredRoot(offer, byId) === null);
}

/** Children (any depth) of each top-level offer, keyed by the top-level row id, in ledger order. */
export function childOffersByParent(offers: readonly SessionLedgerOffer[]): Map<string, SessionLedgerOffer[]> {
  const byId = new Map(offers.map((offer) => [offer.row_id, offer]));
  const out = new Map<string, SessionLedgerOffer[]>();
  for (const offer of offers) {
    const root = offeredRoot(offer, byId);
    if (root) out.set(root.row_id, [...(out.get(root.row_id) ?? []), offer]);
  }
  return out;
}

/** Offers in display order: each top-level row followed by its children, which are marked `child`. */
export function offerRows(offers: readonly SessionLedgerOffer[]): { offer: SessionLedgerOffer; child: boolean }[] {
  const children = childOffersByParent(offers);
  return topLevelOffers(offers).flatMap((offer) => [
    { offer, child: false },
    ...(children.get(offer.row_id) ?? []).map((kid) => ({ offer: kid, child: true })),
  ]);
}

/** Heading for the persistent recovery banner/panel ("Reopen N agents"). */
export function reopenBannerHeading(reopenableCount: number): string {
  if (reopenableCount <= 0) return "Interrupted terminal sessions";
  return reopenableCount === 1 ? "Reopen 1 agent" : `Reopen ${reopenableCount} agents`;
}

/** Human-readable status label for a row's recorded lifecycle end state. */
export function describeEndState(state: SessionEndState, signal?: string | null): string {
  switch (state) {
    case "app_quit":
      return "App quit";
    case "lost":
      return "Lost (crash or reboot)";
    case "exited_signal":
      return signal ? `Signal (${signal})` : "Exited by signal";
    case "unreadable":
      return "unreadable";
    case "exited_clean":
      return "Exited";
    case "running":
      return "Running";
  }
}

export interface ReopenOfferDeps {
  check?: (rowId: string) => Promise<SessionReopenCheck>;
  repo?: Pick<
    RepoFocusActions,
    "showRepositorySurface" | "snapshot" | "activateTab" | "openRepo" | "setTerminalOpen" | "identity"
  >;
  enqueue?: (request: TaskTerminalRequest) => void;
}

export type ReopenOfferResult =
  | { reopened: true; offer: SessionLedgerOffer; error: null }
  | { reopened: false; offer: SessionLedgerOffer; error: string };

/**
 * Reopens one ledger row in its recorded repository, `cwd`/`startDir`, and
 * permission mode. On failure (e.g., gone `cwd`, missing transcript, or failed
 * repo open), keeps the row listed with a per-row error message.
 */
export async function reopenSessionLedgerOffer(
  offer: SessionLedgerOffer,
  deps: ReopenOfferDeps = {},
): Promise<ReopenOfferResult> {
  const checkFn = deps.check ?? checkReopenSessionLedgerRow;
  let checked: SessionReopenCheck;
  try {
    checked = await checkFn(offer.row_id);
  } catch (cause) {
    const error = formatError(cause);
    const updated = recordOfferError(offer.row_id, error) ?? { ...offer, last_error: error };
    return { reopened: false, offer: updated, error };
  }

  const latest = checked.offer;
  if (!latest.resumable || !latest.native_id) {
    const error = latest.last_error ?? latest.reason ?? "Session cannot be reopened.";
    return { reopened: false, offer: latest, error };
  }
  if (latest.launcher !== "claude" && latest.launcher !== "agy") {
    const error = latest.reason ?? "not restorable";
    const updated = recordOfferError(latest.row_id, error) ?? { ...latest, last_error: error };
    return { reopened: false, offer: updated, error };
  }

  const provider: "claude" | "agy" = latest.launcher;
  const mode: PermissionMode | null = isPermissionMode(latest.mode) ? latest.mode : null;
  const repo = deps.repo ?? repoStoreFocus;
  const enqueue = deps.enqueue ?? enqueueTaskTerminal;

  try {
    repo.showRepositorySurface();
    const state = repo.snapshot();
    const identity = repo.identity ?? { caseInsensitive: isCaseInsensitiveFs() };
    const existing = state.openTabs.find((tab) => sameRepo(tab.path, latest.repo_path, identity));
    if (existing) {
      if (existing.id !== state.activeTabId) {
        await repo.activateTab(existing.id);
      }
    } else {
      const opened = await repo.openRepo(latest.repo_path);
      if (!opened) {
        const error = `Couldn't open repository at ${latest.repo_path}.`;
        const updated = recordOfferError(latest.row_id, error) ?? { ...latest, last_error: error };
        return { reopened: false, offer: updated, error };
      }
    }
    const reopenedTitle = latest.title?.trim() || launcherLabel(provider);
    repo.setTerminalOpen(true);
    enqueue({
      runId: "",
      repoPath: latest.repo_path,
      provider,
      title: reopenedTitle,
      resume: {
        sessionId: latest.native_id,
        mode,
        ledgerRowId: latest.row_id,
      },
      ...(latest.start_dir ? { startDir: latest.start_dir } : {}),
    });
    return { reopened: true, offer: latest, error: null };
  } catch (cause) {
    const error = formatError(cause);
    const updated = recordOfferError(latest.row_id, error) ?? { ...latest, last_error: error };
    return { reopened: false, offer: updated, error };
  }
}

/** Rows currently being reopened, so two surfaces' "Reopen all" never double-spawn one agent. */
const reopeningRows = new Set<string>();

export interface ReopenAllDeps extends ReopenOfferDeps {
  /** Pause between rows so a post-crash "Reopen all" does not spawn every `--resume` at once. */
  delayMs?: number;
}

/**
 * Attempts to reopen every currently resumable offer in `offers`, one at a
 * time, returning counts so callers can keep any failed rows visible with
 * their per-row errors. Non-resumable rows are never touched, children of an
 * offered parent are left to their parent's resume, and a row that
 * another reopen is already handling is skipped (counted in `skipped`).
 */
export async function reopenAllSessionLedgerOffers(
  offers: readonly SessionLedgerOffer[],
  deps: ReopenAllDeps = {},
): Promise<{ reopened: number; failed: number; skipped: number; results: ReopenOfferResult[] }> {
  const candidates = reopenableOffers(topLevelOffers(offers));
  const results: ReopenOfferResult[] = [];
  let reopened = 0;
  let failed = 0;
  let skipped = 0;
  for (const offer of candidates) {
    if (reopeningRows.has(offer.row_id)) {
      skipped += 1;
      continue;
    }
    if (results.length > 0 && deps.delayMs && deps.delayMs > 0) {
      await new Promise((resolve) => setTimeout(resolve, deps.delayMs));
    }
    reopeningRows.add(offer.row_id);
    try {
      const outcome = await reopenSessionLedgerOffer(offer, deps);
      results.push(outcome);
      if (outcome.reopened) reopened += 1;
      else failed += 1;
    } finally {
      reopeningRows.delete(offer.row_id);
    }
  }
  return { reopened, failed, skipped, results };
}

/** Shown while the backend has not reconciled the ledger: silence there would read as "nothing to recover". */
export const LEDGER_UNRECONCILED_TEXT = "GitPulse couldn't check which agents survived yet.";

/** Agent rows the app-level "interrupted agents" notice speaks for. */
export interface InterruptedSummary {
  /** Every interrupted agent row, in every repository, open tab or not. */
  agents: SessionLedgerOffer[];
  /** The entries the notice counts: agents with children (whose parent is offered) folded under that parent. */
  entries: SessionLedgerOffer[];
  /** Children of each entry, keyed by entry row id. Never part of `resumable`. */
  children: Map<string, SessionLedgerOffer[]>;
  /** Entries from the latest incident: what the notice counts and "Reopen all" acts on. */
  latest: SessionLedgerOffer[];
  /** Resumable entries from the latest incident. */
  resumable: SessionLedgerOffer[];
  /** Entries from the latest incident that cannot be resumed. */
  unresumable: SessionLedgerOffer[];
  /** Entries from earlier incidents: still offered one by one in Review, never in "Reopen all". */
  earlier: SessionLedgerOffer[];
}

/**
 * How far before the newest interrupted entry another still belongs to the
 * same incident. A crash ends every session within seconds of the others; a
 * row lost days before (or repaired from an older build's ledger) is a
 * conversation the person may since have continued by hand, so "Reopen all"
 * must not resume it unasked.
 */
export const INCIDENT_WINDOW_MS = 60 * 60 * 1000;

/** Splits the ledger into agent rows that can be resumed and those that cannot (shell and corrupt rows are not agents). */
export function summarizeInterrupted(offers: readonly SessionLedgerOffer[]): InterruptedSummary {
  const agents = offers.filter((offer) => offer.end_state !== "unreadable" && offer.launcher !== "shell");
  const entries = topLevelOffers(agents);
  const newest = Math.max(0, ...entries.map((offer) => offer.ended_at ?? 0));
  // A row with no end time is still running or unknown: it is current, never "earlier".
  const isLatest = (offer: SessionLedgerOffer) =>
    offer.ended_at == null || offer.ended_at >= newest - INCIDENT_WINDOW_MS;
  const latest = entries.filter(isLatest);
  const resumable = reopenableOffers(latest);
  const ids = new Set(resumable.map((offer) => offer.row_id));
  return {
    agents,
    entries,
    children: childOffersByParent(agents),
    latest,
    resumable,
    unresumable: latest.filter((offer) => !ids.has(offer.row_id)),
    earlier: entries.filter((offer) => !isLatest(offer)),
  };
}

/** "3 agent sessions were interrupted. 1 cannot be resumed." */
export function interruptedNoticeText(summary: InterruptedSummary): string {
  const total = summary.latest.length;
  const head = `${total} agent ${total === 1 ? "session was" : "sessions were"} interrupted.`;
  const gone = summary.unresumable.length;
  const parts = [head];
  if (gone > 0) parts.push(`${gone} cannot be resumed.`);
  const older = summary.earlier.length;
  if (older > 0) parts.push(`${older} from earlier ${older === 1 ? "is" : "are"} in Review.`);
  return parts.join(" ");
}

/**
 * True when some interrupted row has not been dismissed yet. Keyed by row id,
 * so reopening or dismissing rows never re-raises a notice the user closed,
 * while a genuinely new interrupted row does.
 */
export function hasUndismissedInterrupted(
  summary: InterruptedSummary,
  dismissed: ReadonlySet<string>,
): boolean {
  return summary.agents.some((offer) => !dismissed.has(offer.row_id));
}
