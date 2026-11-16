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
}

/** Snapshot of durable recovery offers returned by the backend ledger. */
export interface SessionLedgerView {
  offers: SessionLedgerOffer[];
  reopenable_count: number;
  unreadable_count: number;
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

function recomputeCounts(offers: readonly SessionLedgerOffer[]): SessionLedgerView {
  return {
    offers: [...offers],
    reopenable_count: offers.filter((offer) => offer.resumable).length,
    unreadable_count: offers.filter((offer) => offer.end_state === "unreadable").length,
  };
}

function mergeOfferInStore(updated: SessionLedgerOffer): void {
  ledgerStore.update((current) => {
    const idx = current.offers.findIndex((item) => item.row_id === updated.row_id);
    const nextOffers =
      idx === -1
        ? [updated, ...current.offers]
        : current.offers.map((item, i) => (i === idx ? updated : item));
    return recomputeCounts(nextOffers);
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
    return recorded ? recomputeCounts(nextOffers) : current;
  });
  return recorded;
}

/** Replaces the reactive ledger snapshot in unit tests. */
export function setSessionLedgerViewForTest(view: SessionLedgerView): void {
  ledgerStore.set(recomputeCounts(view.offers));
}

/** Reads the current durable session ledger from the backend. */
export async function refreshSessionLedger(): Promise<SessionLedgerView> {
  if (!isTauri()) {
    return get(ledgerStore);
  }
  const view = await invoke<SessionLedgerView>("cmd_terminal_session_ledger");
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
    void listen<SessionLedgerView>(SESSION_LEDGER_EVENT, (event) => {
      ledgerStore.set(event.payload);
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
      next = recomputeCounts(current.offers.filter((offer) => offer.row_id !== rowId));
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
    repo.setTerminalOpen(true);
    enqueue({
      runId: "",
      repoPath: latest.repo_path,
      provider,
      title: launcherLabel(provider),
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

/**
 * Attempts to reopen every currently resumable offer in `offers`, returning
 * counts so callers can keep any failed rows visible with their per-row errors.
 */
export async function reopenAllSessionLedgerOffers(
  offers: readonly SessionLedgerOffer[],
  deps: ReopenOfferDeps = {},
): Promise<{ reopened: number; failed: number; results: ReopenOfferResult[] }> {
  const candidates = reopenableOffers(offers);
  const results: ReopenOfferResult[] = [];
  let reopened = 0;
  let failed = 0;
  for (const offer of candidates) {
    const outcome = await reopenSessionLedgerOffer(offer, deps);
    results.push(outcome);
    if (outcome.reopened) reopened += 1;
    else failed += 1;
  }
  return { reopened, failed, results };
}
