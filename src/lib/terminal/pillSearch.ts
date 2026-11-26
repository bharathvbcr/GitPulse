/**
 * Search, filtering, match highlighting, and quick-launch resolution for the
 * view bar's Terminal pill hover menu.
 *
 * Kept separate from `statusPill.ts` so the hover menu (and `tabSearch.ts`) stays
 * out of the startup entry chunk until the user hovers or opens the menu.
 */

import type { MatchChunk } from "../branches/groupBranches";
import { displayName, identityKey, isCaseInsensitiveFs, sameRepo, type PathIdentityOptions } from "../repos/paths";
import { describeCheckout, groupSessionsByRepository, type CheckoutTab } from "./checkoutLabel";
import type { PillSessionView } from "./statusPill";
import {
  fieldSearchRank,
  normalizeTerminalSearchQuery,
  terminalSearchRank,
} from "./tabSearch";
import {
  cleanTabName,
  isHandoffCapableLauncher,
  offeredSessionKind,
  sessionTypesFor,
  type SessionKind,
} from "./tabs";

const NAME_LIMIT = 28;

/** Maximum quick-launch targets rendered at once; `hiddenLaunches` carries any remainder. */
export const MAX_PILL_QUICK_LAUNCHES = 8;

const LAUNCH_VERBS = new Set(["new", "launch", "start"]);

export type PillSessionFilter = "all" | "agents" | "attention" | "current";

export interface PillFilterCounts {
  total: number;
  agents: number;
  attention: number;
  current: number;
  /** True only when at least one chip actually narrows the session list. */
  showBar: boolean;
}

export interface PillOpenTab extends CheckoutTab {
  group?: string | null;
}

export interface PillQuickLaunch {
  /** Stable key (`<repoIdentity>::<launcher>`). */
  key: string;
  repoPath: string;
  repository: string;
  checkoutName: string | null;
  logoPath: string;
  isCurrentRepo: boolean;
  /** Whether the target repository is already open in a workspace tab or a recent checkout. */
  source: "open" | "recent";
  /** Workspace tab group name, when assigned on an open tab. */
  group: string | null;
  launcher: SessionKind;
  launcherLabel: string;
  label: string;
}

export interface PillSearchResults {
  query: string;
  sessions: PillSessionView[];
  launches: PillQuickLaunch[];
  /** Total matching quick-launch targets before capping (`launches.length + hiddenLaunches`). */
  totalLaunches: number;
  hiddenLaunches: number;
}

export interface PillSearchInput {
  sessions: readonly PillSessionView[];
  query: unknown;
  filter?: PillSessionFilter;
  openTabs?: readonly PillOpenTab[];
  recentRepos?: readonly string[];
  currentRepoPath?: string | null;
  defaultLauncher?: unknown;
  browserSessions?: boolean;
  identity?: PathIdentityOptions;
  maxLaunches?: number;
}

export type PillSearchAction =
  | { kind: "session"; key: string; optionId: string; session: PillSessionView }
  | { kind: "launch"; key: string; optionId: string; launch: PillQuickLaunch };

interface ResolvedLaunchRepo {
  idKey: string;
  repoPath: string;
  repository: string;
  checkoutName: string | null;
  logoPath: string;
  isCurrentRepo: boolean;
  source: "open" | "recent";
  group: string | null;
  searchFields: string[];
}

function clip(text: string, max: number): string {
  const glyphs = Array.from(text);
  if (glyphs.length <= max) return text;
  return `${glyphs.slice(0, max - 1).join("")}…`;
}

export function pillFilterCounts(sessions: readonly PillSessionView[]): PillFilterCounts {
  const safe = Array.isArray(sessions) ? sessions : [];
  let agents = 0;
  let attention = 0;
  let current = 0;
  for (const session of safe) {
    if (isHandoffCapableLauncher(session.launcherKind)) agents += 1;
    if (session.attention) attention += 1;
    if (session.isCurrentRepo) current += 1;
  }
  const total = safe.length;
  const showBar =
    total > 1 &&
    (attention > 0 || (agents > 0 && agents < total) || (current > 0 && current < total));
  return { total, agents, attention, current, showBar };
}

export function filterPillSessions(
  sessions: readonly PillSessionView[],
  filter: PillSessionFilter = "all",
): PillSessionView[] {
  const safe = Array.isArray(sessions) ? sessions : [];
  switch (filter) {
    case "agents":
      return safe.filter((session) => isHandoffCapableLauncher(session.launcherKind));
    case "attention":
      return safe.filter((session) => Boolean(session.attention));
    case "current":
      return safe.filter((session) => session.isCurrentRepo);
    default:
      return [...safe];
  }
}

function launcherTokenRank(
  tokenLower: string,
  launcher: { kind: SessionKind; label: string },
): number {
  return Math.max(
    fieldSearchRank(tokenLower, launcher.label),
    fieldSearchRank(tokenLower, launcher.kind),
  );
}

/**
 * Repository matching requires a strong match (`rank >= 2`: exact, prefix,
 * word-boundary, camelCase initials, or contiguous substring) on every token so
 * typing `"cl"` for Claude never spuriously matches `"DevCouncil"`.
 */
function repoTokensRank(tokens: readonly string[], fields: readonly string[]): number {
  if (tokens.length === 0 || fields.length === 0) return 0;
  let minRank = 5;
  for (const token of tokens) {
    let best = 0;
    for (const field of fields) {
      const rank = fieldSearchRank(token, field);
      if (rank > best) best = rank;
      if (best === 5) break;
    }
    if (best < 2) return 0;
    if (best < minRank) minRank = best;
  }
  return minRank;
}

function resolveLaunchRepos(
  openTabs: readonly PillOpenTab[],
  recentRepos: readonly string[],
  currentRepoPath: string | null | undefined,
  identity: PathIdentityOptions,
): { repos: ResolvedLaunchRepo[]; groupByPathKey: Map<string, string> } {
  const checkoutGroups = groupSessionsByRepository(
    openTabs.map((tab) => ({ repoPath: tab.path })),
    currentRepoPath ?? null,
    openTabs,
    identity,
  );
  const groupPrimaryByPathKey = new Map<string, string>();
  for (const grp of checkoutGroups) {
    const primary = grp.sessions[0]?.repoPath;
    if (!primary) continue;
    for (const entry of grp.sessions) {
      const k = identityKey(entry.repoPath, identity);
      if (k) groupPrimaryByPathKey.set(k, primary);
    }
  }

  const seen = new Set<string>();
  const repos: ResolvedLaunchRepo[] = [];
  const groupByPathKey = new Map<string, string>();

  const pushRepo = (tab: PillOpenTab, source: "open" | "recent") => {
    const path = tab.path?.trim();
    if (!path) return;
    const idKey = identityKey(path, identity);
    if (!idKey || seen.has(idKey)) return;
    seen.add(idKey);
    const desc = describeCheckout(path, openTabs, identity);
    const rawRepo = desc.repository || displayName(path).trim() || "Repository";
    const repository = clip(cleanTabName(rawRepo, 64) || "Repository", NAME_LIMIT);
    const checkoutName = desc.checkout ? clip(cleanTabName(desc.checkout, 64), NAME_LIMIT) : null;
    const isCurrentRepo = Boolean(currentRepoPath && sameRepo(path, currentRepoPath, identity));
    const logoPath = checkoutName ? (groupPrimaryByPathKey.get(idKey) ?? path) : path;
    const group = tab.group?.trim() || null;
    if (group) {
      groupByPathKey.set(idKey, group);
      const primaryKey = identityKey(logoPath, identity);
      if (primaryKey && !groupByPathKey.has(primaryKey)) groupByPathKey.set(primaryKey, group);
    }
    // Deliberately exclude raw filesystem paths so `.claude/worktrees/…` does
    // not falsely match a search for the `"claude"` agent.
    const searchFields = [desc.repository, desc.checkout, tab.label, tab.currentBranch, group]
      .filter((value): value is string => typeof value === "string" && value.trim().length > 0);
    repos.push({
      idKey,
      repoPath: path,
      repository,
      checkoutName,
      logoPath,
      isCurrentRepo,
      source,
      group,
      searchFields,
    });
  };

  for (const tab of openTabs) pushRepo(tab, "open");
  if (currentRepoPath?.trim()) {
    pushRepo({ path: currentRepoPath, label: displayName(currentRepoPath) }, "open");
  }
  for (const recentPath of recentRepos) {
    if (typeof recentPath !== "string" || !recentPath.trim()) continue;
    pushRepo({ path: recentPath, label: displayName(recentPath) }, "recent");
  }

  repos.sort((left, right) => {
    if (left.isCurrentRepo !== right.isCurrentRepo) return left.isCurrentRepo ? -1 : 1;
    if (left.source !== right.source) return left.source === "open" ? -1 : 1;
    return 0;
  });
  return { repos, groupByPathKey };
}

/**
 * Filters live sessions and computes ranked quick-launch actions for the
 * terminal pill's hover search. Empty queries apply only the active filter chip
 * and emit no quick-launch noise.
 */
export function searchTerminalPill(input: PillSearchInput): PillSearchResults {
  const baseSessions = filterPillSessions(input.sessions, input.filter ?? "all");
  const normalized = normalizeTerminalSearchQuery(input.query);
  if (!normalized) {
    return {
      query: "",
      sessions: baseSessions,
      launches: [],
      totalLaunches: 0,
      hiddenLaunches: 0,
    };
  }

  const identity = input.identity ?? { caseInsensitive: isCaseInsensitiveFs() };
  const tabs = Array.isArray(input.openTabs) ? input.openTabs : [];
  const recents = Array.isArray(input.recentRepos) ? input.recentRepos : [];
  const browserSessions = input.browserSessions === true;
  const availableLaunchers = sessionTypesFor(browserSessions);
  const defaultKind = offeredSessionKind(input.defaultLauncher, browserSessions);
  const defaultLauncherEntry =
    availableLaunchers.find((entry) => entry.kind === defaultKind) ?? availableLaunchers[0];
  const { repos, groupByPathKey } = resolveLaunchRepos(
    tabs,
    recents,
    input.currentRepoPath,
    identity,
  );
  const currentRepo =
    repos.find((repo) => repo.isCurrentRepo) ?? repos.find((repo) => repo.source === "open") ?? null;

  const rankedSessions = baseSessions
    .map((session, index) => {
      const tabGroup =
        groupByPathKey.get(identityKey(session.repoPath, identity)) ??
        groupByPathKey.get(identityKey(session.logoPath, identity)) ??
        null;
      return {
        session,
        index,
        rank: terminalSearchRank(
          normalized,
          session.displayTitle,
          session.name,
          session.title,
          session.agentLabel,
          session.launcherKind,
          session.repository,
          session.checkoutName,
          session.where,
          tabGroup,
          session.attention,
          session.phrase,
          session.status,
          session.isActive ? "active" : null,
          session.splitSlot ? `#${session.splitSlot}` : null,
        ),
      };
    })
    // Require exact, prefix, word/initials, or contiguous substring (`rank >= 2`)
    // so typing e.g. "manvi" never matches scattered letters in unrelated titles
    // or raw filesystem path prefixes.
    .filter((entry) => entry.rank >= 2)
    .sort((a, b) => (b.rank !== a.rank ? b.rank - a.rank : a.index - b.index))
    .map((entry) => entry.session);

  const candidates = new Map<string, { launch: PillQuickLaunch; score: number; order: number }>();
  let nextOrder = 0;

  const addCandidate = (
    repo: ResolvedLaunchRepo,
    launcher: { kind: SessionKind; label: string },
    score: number,
  ) => {
    const key = `${repo.idKey}::${launcher.kind}`;
    const adjustedScore = score + (repo.source === "open" ? 6 : 0);
    const existing = candidates.get(key);
    if (existing) {
      if (adjustedScore > existing.score) existing.score = adjustedScore;
      return;
    }
    const targetName = repo.checkoutName
      ? `${repo.repository}/${repo.checkoutName}`
      : repo.repository;
    candidates.set(key, {
      launch: {
        key,
        repoPath: repo.repoPath,
        repository: repo.repository,
        checkoutName: repo.checkoutName,
        logoPath: repo.logoPath,
        isCurrentRepo: repo.isCurrentRepo,
        source: repo.source,
        group: repo.group,
        launcher: launcher.kind,
        launcherLabel: launcher.label,
        label: repo.isCurrentRepo
          ? `New ${launcher.label} session`
          : `New ${launcher.label} in ${targetName}`,
      },
      score: adjustedScore,
      order: nextOrder++,
    });
  };

  const rawTokens = normalized.toLowerCase().split(/\s+/).filter(Boolean);
  const hasLeadingVerb = rawTokens.length > 0 && LAUNCH_VERBS.has(rawTokens[0]);
  const strippedTokens = hasLeadingVerb ? rawTokens.slice(1) : rawTokens;

  if (hasLeadingVerb && strippedTokens.length === 0 && currentRepo) {
    for (const launcher of availableLaunchers) {
      addCandidate(currentRepo, launcher, launcher.kind === defaultKind ? 42 : 40);
    }
  }

  const tokenSets =
    hasLeadingVerb && strippedTokens.length > 0 ? [rawTokens, strippedTokens] : [rawTokens];

  for (const tokens of tokenSets) {
    if (tokens.length === 1) {
      const token = tokens[0];
      if (currentRepo) {
        for (const launcher of availableLaunchers) {
          const rank = launcherTokenRank(token, launcher);
          if (rank >= 1) {
            addCandidate(
              currentRepo,
              launcher,
              rank * 10 + (launcher.kind === defaultKind ? 2 : 0) + 1,
            );
          }
        }
      }
      if (defaultLauncherEntry) {
        for (const repo of repos) {
          const rank = repoTokensRank(tokens, repo.searchFields);
          if (rank >= 2) {
            addCandidate(repo, defaultLauncherEntry, rank * 10 + (repo.isCurrentRepo ? 2 : 0));
          }
        }
      }
      continue;
    }

    // Multi-token: first check repo-only matches (e.g. "gitpulse sidebar").
    if (defaultLauncherEntry) {
      for (const repo of repos) {
        const rank = repoTokensRank(tokens, repo.searchFields);
        if (rank >= 2) {
          addCandidate(repo, defaultLauncherEntry, rank * 10 + (repo.isCurrentRepo ? 2 : 0));
        }
      }
    }

    // Multi-token cross-match: one token names a launcher and the remaining tokens
    // name an open or recent repository/checkout, in either word order.
    for (const launcher of availableLaunchers) {
      for (let i = 0; i < tokens.length; i += 1) {
        const lRank = launcherTokenRank(tokens[i], launcher);
        if (lRank === 0) continue;
        if (lRank === 1 && tokens[i].length < 2) continue;
        const rest = tokens.filter((_, index) => index !== i);
        for (const repo of repos) {
          const rRank = repoTokensRank(rest, repo.searchFields);
          if (rRank < 2) continue;
          const score = Math.min(lRank, rRank) * 10 + lRank + rRank + (repo.isCurrentRepo ? 2 : 0);
          addCandidate(repo, launcher, score);
        }
      }
    }
  }

  const sortedLaunches = [...candidates.values()]
    .sort((a, b) => {
      if (b.score !== a.score) return b.score - a.score;
      if (a.launch.source !== b.launch.source) return a.launch.source === "open" ? -1 : 1;
      if (a.launch.isCurrentRepo !== b.launch.isCurrentRepo) {
        return a.launch.isCurrentRepo ? -1 : 1;
      }
      return a.order - b.order;
    })
    .map((entry) => entry.launch);

  const cap =
    typeof input.maxLaunches === "number" && Number.isFinite(input.maxLaunches) && input.maxLaunches >= 1
      ? Math.floor(input.maxLaunches)
      : MAX_PILL_QUICK_LAUNCHES;

  const launches = sortedLaunches.slice(0, cap);
  return {
    query: normalized,
    sessions: rankedSessions,
    launches,
    totalLaunches: sortedLaunches.length,
    hiddenLaunches: Math.max(0, sortedLaunches.length - launches.length),
  };
}

/**
 * Keyboard-activatable rows in search order: revealable sessions first, then
 * quick-launch targets. Uses deterministic index-based DOM option IDs so
 * arbitrary paths/keys never enter `id` or `aria-activedescendant`.
 */
export function pillSearchActions(results: PillSearchResults): PillSearchAction[] {
  const actions: PillSearchAction[] = [];
  for (const session of results.sessions) {
    if (!session.canReveal) continue;
    actions.push({
      kind: "session",
      key: session.key,
      optionId: `terminal-pill-opt-${actions.length}`,
      session,
    });
  }
  for (const launch of results.launches) {
    actions.push({
      kind: "launch",
      key: launch.key,
      optionId: `terminal-pill-opt-${actions.length}`,
      launch,
    });
  }
  return actions;
}

/** Stable selection identifier that survives live session churn while searching. */
export function pillActionId(action: Pick<PillSearchAction, "kind" | "key">): string {
  return `${action.kind}:${action.key}`;
}

export function resolvePillActionIndex(
  actions: readonly PillSearchAction[],
  selectedId: string | null | undefined,
): number {
  if (!Array.isArray(actions) || actions.length === 0) return -1;
  if (selectedId) {
    const found = actions.findIndex((action) => pillActionId(action) === selectedId);
    if (found >= 0) return found;
  }
  return 0;
}

function isBoundarySep(code: number): boolean {
  return (
    code === 32 ||
    code === 9 ||
    code === 45 ||
    code === 95 ||
    code === 47 ||
    code === 92 ||
    code === 46 ||
    code === 58
  );
}

function initialIndices(text: string): number[] {
  const indices: number[] = [];
  for (let i = 0; i < text.length; i += 1) {
    const code = text.charCodeAt(i);
    if (isBoundarySep(code)) continue;
    if (i === 0) {
      indices.push(i);
      continue;
    }
    const prev = text.charCodeAt(i - 1);
    const currUpper = code >= 65 && code <= 90;
    const prevLowerOrDigit = (prev >= 97 && prev <= 122) || (prev >= 48 && prev <= 57);
    if (isBoundarySep(prev) || (currUpper && prevLowerOrDigit)) {
      indices.push(i);
    }
  }
  return indices;
}

/**
 * Multi-token match highlighter for search cards: marks contiguous token hits
 * and camelCase/separator initial matches (e.g. `"dc"` -> `D`ev`C`ouncil),
 * merging overlaps cleanly with zero regex.
 */
export function highlightSearchText(text: string, query: unknown): MatchChunk[] {
  if (!text) return [];
  const normalized = normalizeTerminalSearchQuery(query).toLowerCase();
  if (!normalized) return [{ text, matched: false }];
  const folded = text.toLowerCase();
  // Guard against locale-dependent case-folding length shifts (e.g. U+0130).
  if (folded.length !== text.length) return [{ text, matched: false }];

  const tokens = normalized.split(/\s+/).filter(Boolean);
  if (tokens.length === 0) return [{ text, matched: false }];

  const mask = new Uint8Array(text.length);
  let anyMatch = false;
  let cachedInitials: number[] | null = null;

  for (const token of tokens) {
    const at = folded.indexOf(token);
    if (at !== -1) {
      for (let i = at; i < at + token.length; i += 1) mask[i] = 1;
      anyMatch = true;
      continue;
    }
    if (token.length >= 2) {
      if (!cachedInitials) cachedInitials = initialIndices(text);
      if (cachedInitials.length >= token.length) {
        let matched = true;
        for (let i = 0; i < token.length; i += 1) {
          if (folded[cachedInitials[i]] !== token[i]) {
            matched = false;
            break;
          }
        }
        if (matched) {
          for (let i = 0; i < token.length; i += 1) mask[cachedInitials[i]] = 1;
          anyMatch = true;
        }
      }
    }
  }

  if (!anyMatch) return [{ text, matched: false }];

  const chunks: MatchChunk[] = [];
  let start = 0;
  let current = mask[0] === 1;
  for (let i = 1; i < text.length; i += 1) {
    const next = mask[i] === 1;
    if (next !== current) {
      chunks.push({ text: text.slice(start, i), matched: current });
      start = i;
      current = next;
    }
  }
  chunks.push({ text: text.slice(start), matched: current });
  return chunks;
}
