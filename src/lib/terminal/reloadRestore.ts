/**
 * Putting a repository's terminal tabs back after a page reload.
 *
 * A WebContent crash (or any reload) replaces every renderer object while
 * `gitpulse-ptyd` keeps the sessions running. The dock used to come back with
 * one fresh shell, and only the leaves of saved split groups found their
 * sessions again; every other tab became a "Still running — not shown in any
 * tab" record that needed a manual "Go to". Nothing said the work was alive.
 *
 * The strip is saved in the same storage value as the split layout
 * (`gitpulse.terminal.splitLayout:<repo>`, a `strip` field), by backend session
 * id. This module holds every decision about it as pure functions, because the
 * component's `$effect`s never run under vitest: what to save, what to
 * restore, which restored tabs wait until first shown. The component only
 * calls them.
 *
 * A tab is restored only when it was a tab of THIS dock at the reload (it is
 * in the saved strip) AND its session is still adopted by this page. A task
 * terminal, an agent child nobody opened a tab for, and a session some other
 * dock was showing are in no strip of this dock, so they stay in Sessions.
 */

import { LAUNCHERS, openAttachedChildTab, setTabTitle, type LauncherKind, type TabState, type TerminalTab } from "./tabs";
import { MAX_PERSISTED_SNIPPET_CHARS, parsePersistedLeaves, type PersistedSplitLeafMeta } from "./splitTree";

/** The tab strip as saved: order, the focused tab, and each tab's own record. */
export interface PersistedTabStrip {
  readonly activeId: string | null;
  readonly order: readonly string[];
  readonly leaves: Record<string, PersistedSplitLeafMeta>;
}

/** A session this page has adopted and could hand to a tab (`detachedSessions`). */
export interface AdoptedFact {
  readonly sessionId: string;
  readonly repoPath: string;
  readonly launcher: LauncherKind;
  readonly title?: string;
}

const MAX_STRIP_ENTRIES = 64;
const MAX_ID_LENGTH = 128;

function isSessionLauncher(launcher: string): launcher is LauncherKind {
  return LAUNCHERS.some((entry) => entry.kind === launcher);
}

/** What a tab saves about itself, for a split leaf or a strip entry alike. */
export function leafMetaForTab(tab: TerminalTab, slotIndex: number, backendId: string | null | undefined): PersistedSplitLeafMeta {
  return {
    launcher: tab.launcher,
    slotIndex,
    ...(tab.name ? { name: tab.name } : {}),
    ...(tab.launchName ? { launchName: tab.launchName } : {}),
    ...(tab.userNamed ? { userNamed: true } : {}),
    ...(tab.pinned ? { pinned: true } : {}),
    ...(tab.kept ? { kept: true } : {}),
    ...(backendId ? { backendSessionId: backendId } : {}),
    ...(tab.parentSessionId ? { parentSessionId: tab.parentSessionId } : {}),
    ...(tab.filePath ? { filePath: tab.filePath } : {}),
    ...(tab.revealLine != null ? { revealLine: tab.revealLine } : {}),
    ...(tab.revealColumn != null ? { revealColumn: tab.revealColumn } : {}),
    ...(tab.snippet
      ? { snippet: { title: tab.snippet.title, content: tab.snippet.content.slice(0, MAX_PERSISTED_SNIPPET_CHARS) } }
      : {}),
    ...(tab.url ? { url: tab.url } : {}),
    ...(tab.checkout ? { checkout: tab.checkout } : {}),
  };
}

/**
 * The strip to save: terminal tabs that have a backend session and have not
 * ended, in strip order. Null when there are none, so the saved value does not
 * claim a strip that nothing could restore.
 */
export function buildTabStrip(
  tabs: readonly TerminalTab[],
  activeId: string | null,
  backendIdOf: (tab: TerminalTab) => string | null | undefined,
  hasEnded: (tab: TerminalTab) => boolean,
): PersistedTabStrip | null {
  const order: string[] = [];
  const leaves: Record<string, PersistedSplitLeafMeta> = {};
  const seen = new Set<string>();
  tabs.forEach((tab, slotIndex) => {
    if (!isSessionLauncher(tab.launcher) || hasEnded(tab)) return;
    const backendId = backendIdOf(tab);
    if (!backendId || seen.has(backendId) || order.length >= MAX_STRIP_ENTRIES) return;
    seen.add(backendId);
    order.push(tab.id);
    leaves[tab.id] = leafMetaForTab(tab, slotIndex, backendId);
  });
  if (order.length === 0) return null;
  return { activeId: activeId !== null && order.includes(activeId) ? activeId : null, order, leaves };
}

/**
 * Adds `strip` to a serialized split workspace (or starts one with no groups).
 * The split parser reads a groupless value as "no split layout", so a strip
 * alone never invents a split.
 */
export function composeWorkspaceWithStrip(serialized: string | null, strip: PersistedTabStrip | null): string | null {
  if (!strip) return serialized;
  let payload: Record<string, unknown>;
  try {
    const parsed: unknown = serialized ? JSON.parse(serialized) : null;
    payload =
      parsed && typeof parsed === "object" && !Array.isArray(parsed)
        ? { ...(parsed as Record<string, unknown>) }
        : { version: 3, activeGroupId: null, groups: [], leaves: {} };
  } catch {
    payload = { version: 3, activeGroupId: null, groups: [], leaves: {} };
  }
  payload.strip = strip;
  return JSON.stringify(payload);
}

/** Reads the strip back, dropping anything malformed instead of trusting it. */
export function parsePersistedTabStrip(raw: string | null | undefined): PersistedTabStrip | null {
  if (!raw) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return null;
  const rawStrip = (parsed as Record<string, unknown>).strip;
  if (!rawStrip || typeof rawStrip !== "object" || Array.isArray(rawStrip)) return null;
  const record = rawStrip as Record<string, unknown>;
  const leaves = parsePersistedLeaves(record.leaves);
  const order: string[] = [];
  if (Array.isArray(record.order)) {
    for (const id of record.order.slice(0, MAX_STRIP_ENTRIES)) {
      if (typeof id !== "string" || !id || id.length > MAX_ID_LENGTH || order.includes(id) || !leaves[id]) continue;
      order.push(id);
    }
  }
  if (order.length === 0) return null;
  const activeId = typeof record.activeId === "string" && order.includes(record.activeId) ? record.activeId : null;
  return { activeId, order, leaves };
}

/** One tab to bring back. `eager` tabs attach now; the rest when first shown. */
export interface RestoreTab {
  readonly oldId: string;
  readonly sessionId: string;
  readonly launcher: LauncherKind;
  readonly meta: PersistedSplitLeafMeta;
  readonly title: string | undefined;
  readonly eager: boolean;
}

export interface RestorePlan {
  readonly tabs: readonly RestoreTab[];
  /** The old id of the tab to focus, or null when nothing is restored. */
  readonly activeOldId: string | null;
  /** Saved entries that could not be restored (session ended, other dock): reported, not shown. */
  readonly dropped: readonly string[];
}

/**
 * Decides which saved tabs come back.
 *
 * `available` holds the sessions adopted by this page that a tab could take
 * over (the caller has already left out task attempts and records with no
 * terminal to show). `belongsHere` says whether an adopted session is this
 * dock's: its repository, or the checkout the tab recorded it was hosted in.
 * `splitLeafIds` are saved split leaves, which are visible at once and so are
 * attached eagerly, together with the active tab.
 */
export function planReloadRestore(input: {
  strip: PersistedTabStrip | null;
  available: readonly AdoptedFact[];
  belongsHere: (fact: AdoptedFact, meta: PersistedSplitLeafMeta) => boolean;
  splitLeafIds: ReadonlySet<string>;
  maxTabs: number;
}): RestorePlan {
  const { strip, available, belongsHere, splitLeafIds, maxTabs } = input;
  if (!strip) return { tabs: [], activeOldId: null, dropped: [] };
  const bySession = new Map<string, AdoptedFact>();
  for (const fact of available) if (!bySession.has(fact.sessionId)) bySession.set(fact.sessionId, fact);

  const kept: Array<{ oldId: string; fact: AdoptedFact; meta: PersistedSplitLeafMeta }> = [];
  const dropped: string[] = [];
  const usedSessions = new Set<string>();
  const usedOldIds = new Set<string>();
  for (const oldId of strip.order) {
    const meta = strip.leaves[oldId];
    const sessionId = meta?.backendSessionId;
    if (!meta || !sessionId || usedOldIds.has(oldId) || usedSessions.has(sessionId) || !isSessionLauncher(meta.launcher)) {
      dropped.push(oldId);
      continue;
    }
    const fact = bySession.get(sessionId);
    if (!fact || !belongsHere(fact, meta) || kept.length >= maxTabs) {
      dropped.push(oldId);
      continue;
    }
    usedOldIds.add(oldId);
    usedSessions.add(sessionId);
    kept.push({ oldId, fact, meta });
  }
  if (kept.length === 0) return { tabs: [], activeOldId: null, dropped };

  const activeOldId =
    strip.activeId !== null && kept.some((entry) => entry.oldId === strip.activeId) ? strip.activeId : kept[0].oldId;
  return {
    activeOldId,
    dropped,
    tabs: kept.map(({ oldId, fact, meta }) => ({
      oldId,
      sessionId: fact.sessionId,
      launcher: fact.launcher,
      meta,
      title: fact.title,
      eager: oldId === activeOldId || splitLeafIds.has(oldId),
    })),
  };
}

export interface RestoredStrip {
  readonly state: TabState;
  /** Old saved tab id to the new tab id, for the split hydration to remap its leaves. */
  readonly idMap: ReadonlyMap<string, string>;
  /** New ids of tabs that mount their session only when first shown. */
  readonly deferred: ReadonlySet<string>;
}

/**
 * Builds the tab state for a plan through `openAttachedChildTab`, so a restored
 * tab is the same kind of tab a reload's split leaves and agent children
 * already are: it carries `attachSessionId` and takes its session over.
 */
export function buildRestoredStrip(plan: RestorePlan): RestoredStrip {
  let state: TabState = { tabs: [], activeId: null };
  const idMap = new Map<string, string>();
  const deferred = new Set<string>();
  for (const entry of plan.tabs) {
    const parentSessionId = entry.meta.parentSessionId;
    const opened = openAttachedChildTab(state, {
      sessionId: entry.sessionId,
      parentSessionId,
      launcher: entry.launcher,
      title: parentSessionId ? (entry.title ?? null) : null,
      focus: false,
    });
    if (!opened.id) continue;
    state = opened.state;
    if (!parentSessionId && entry.title) state = setTabTitle(state, opened.id, entry.title);
    idMap.set(entry.oldId, opened.id);
    if (!entry.eager) deferred.add(opened.id);
  }
  const activeId = plan.activeOldId ? (idMap.get(plan.activeOldId) ?? null) : null;
  return { state: { ...state, activeId: activeId ?? state.activeId }, idMap, deferred };
}

/** Whether a saved strip is worth holding the dock empty for until adoption has listed sessions. */
export function stripWorthWaitingFor(strip: PersistedTabStrip | null): boolean {
  return strip !== null && strip.order.some((id) => Boolean(strip.leaves[id]?.backendSessionId));
}
