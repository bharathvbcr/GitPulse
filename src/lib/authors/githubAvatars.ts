/**
 * GitHub profile pictures for commit authors: the single owner of avatar URLs.
 *
 * Two sources, in order of cost:
 *  1. A GitHub noreply address (`<id>+<login>@users.noreply.github.com`) names
 *     its account, so the URL is derived locally with no lookup.
 *  2. Any other email is asked of GitHub through `cmd_github_author_avatars`
 *     (the GitHub CLI), once per email per repository per session, and only
 *     when the repository has a GitHub remote.
 *
 * Failure never blocks a paint and is never silent: a lookup that could not
 * run (gh missing or signed out, a timeout, GitHub refusing) is written to
 * Diagnostics once per repository and reason, and the repository is not asked
 * again for {@link LOOKUP_RETRY_AFTER_MS}; the emails it was asked about are
 * asked again after that. A repository without a GitHub remote is not asked
 * for the rest of the session, and an email with no account is not retried.
 * The initials disc stays underneath everywhere, so nothing here can leave an
 * author without an avatar.
 *
 * Plain TypeScript on purpose (no `$effect`): callers invoke
 * {@link requestAuthorAvatars} and re-derive when {@link githubAvatarsVersion}
 * moves.
 */
import { get, readable, writable, type Readable } from "svelte/store";
import { diagnostics } from "../diagnostics/diagnostics";
import { invoke } from "../ipc/invoke";
import { parseRemoteList } from "../repos/remotes";
import { interfaceStore } from "../stores/interfaceStore";

/** Registered backend command; a module-level literal for check-ipc-contract. */
export const GITHUB_AUTHOR_AVATARS_COMMAND = "cmd_github_author_avatars";
const LIST_REMOTES_COMMAND = "cmd_list_remotes";

export interface AuthorAvatar { email: string; login: string; avatar_url: string }
export interface AuthorAvatarReport {
  available: boolean;        // false: no GitHub remote, gh missing/unauthenticated, request failed
  avatars: AuthorAvatar[];
  no_account: string[];      // emails with no linked GitHub account
  unresolved: string[];      // not looked up / commits unknown to GitHub (unpushed) / past caps
  truncated: boolean;
  error: string | null;
}

/** Backend limits mirrored here so a request never exceeds what is honoured. */
export const MAX_EMAILS_PER_REQUEST = 150;
export const MAX_OIDS_PER_EMAIL = 3;
/** Positive-result cache bound; the oldest entry is evicted first. */
export const MAX_CACHED_AVATARS = 5000;
const MAX_EMAIL_LENGTH = 254;
const AVATAR_HOST = "avatars.githubusercontent.com";
/** How long a repository whose lookup failed is left alone before asking again. */
export const LOOKUP_RETRY_AFTER_MS = 10 * 60_000;

const FAILED_REPORT: AuthorAvatarReport = {
  available: false,
  avatars: [],
  no_account: [],
  unresolved: [],
  truncated: false,
  error: "The avatar lookup was unreadable.",
};

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

/**
 * Fail-closed parse of the backend report. Anything malformed is an
 * unavailable report: a payload that cannot be read must never be treated as
 * "these authors have no account".
 */
export function parseAuthorAvatarReport(value: unknown): AuthorAvatarReport {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return FAILED_REPORT;
  const rec = value as Record<string, unknown>;
  if (
    typeof rec.available !== "boolean" ||
    typeof rec.truncated !== "boolean" ||
    !Array.isArray(rec.avatars) ||
    !isStringArray(rec.no_account) ||
    !isStringArray(rec.unresolved) ||
    (rec.error !== null && typeof rec.error !== "string")
  ) {
    return FAILED_REPORT;
  }
  const avatars: AuthorAvatar[] = [];
  for (const item of rec.avatars) {
    if (item === null || typeof item !== "object") return FAILED_REPORT;
    const a = item as Record<string, unknown>;
    if (typeof a.email !== "string" || typeof a.login !== "string" || typeof a.avatar_url !== "string") {
      return FAILED_REPORT;
    }
    avatars.push({ email: a.email, login: a.login, avatar_url: a.avatar_url });
  }
  return {
    available: rec.available,
    avatars,
    no_account: rec.no_account,
    unresolved: rec.unresolved,
    truncated: rec.truncated,
    error: rec.error as string | null,
  };
}

const NOREPLY = /^(?:(\d{1,20})\+)?([a-z0-9](?:[a-z0-9-]{0,38}))(\[bot\])?@users\.noreply\.github\.com$/i;

/**
 * The avatar URL a GitHub noreply address names, or null. Pure; no network.
 * `<digits>+<login>` resolves by the immutable numeric id, a bare `<login>`
 * by name. The login is restricted to GitHub's own charset so nothing else
 * can reach a URL.
 */
export function noreplyAvatarUrl(email: string): string | null {
  const match = NOREPLY.exec(email.trim());
  if (!match) return null;
  const [, id, login, bot] = match;
  if (id !== undefined) return `https://${AVATAR_HOST}/u/${id}?s=64&v=4`;
  return `https://${AVATAR_HOST}/${login}${bot ? "%5Bbot%5D" : ""}?s=64`;
}

/** Cache key: lowercased trimmed email; null for anything that is not one. */
export function normalizeAuthorEmail(email: string | null | undefined): string | null {
  if (typeof email !== "string") return null;
  const key = email.trim().toLowerCase();
  if (key.length === 0 || key.length > MAX_EMAIL_LENGTH || !key.includes("@")) return null;
  return key;
}

/** Only GitHub's avatar CDN over https is ever loaded; sized to the discs. */
function sanitizeAvatarUrl(raw: string): string | null {
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return null;
  }
  if (url.protocol !== "https:" || url.hostname !== AVATAR_HOST) return null;
  if (!url.searchParams.has("s")) url.searchParams.set("s", "64");
  return url.toString();
}

// Positive results are global: email -> GitHub account is not per repository.
const urlByEmail = new Map<string, string>();
// Per-repo ledger of emails already asked, so each is asked once per session.
const askedByRepo = new Map<string, Set<string>>();
// Repository -> time (ms) before which it is not asked again. Infinity: no
// GitHub remote. A finite time: the lookup failed and is retried after it.
const unavailableRepos = new Map<string, number>();
// "repo\0reason" pairs already written to Diagnostics, so a failure is
// reported once rather than on every load of the same repository.
const reportedFailures = new Set<string>();

function isUnavailable(repoPath: string): boolean {
  const until = unavailableRepos.get(repoPath);
  if (until === undefined) return false;
  if (Date.now() < until) return true;
  unavailableRepos.delete(repoPath);
  return false;
}

function reportFailure(repoPath: string, reason: string): void {
  const key = `${repoPath}\u0000${reason}`;
  if (reportedFailures.has(key)) return;
  reportedFailures.add(key);
  diagnostics.warn("avatars", `GitHub avatar lookup for ${repoPath}: ${reason}`);
}
const githubRemoteByRepo = new Map<string, Promise<boolean>>();

interface RepoFlight {
  /** Candidates that arrived while a request was running: email -> oids. */
  pending: Map<string, string[]> | null;
}
const flights = new Map<string, RepoFlight>();

const versionStore = writable(0);
/** Increments whenever new URLs arrive, so components and the canvas re-derive. */
export const githubAvatarsVersion: Readable<number> = readable(0, (set) =>
  versionStore.subscribe(set),
);

// App-lifetime subscription: the store is a singleton and so is this module.
let avatarsEnabled = get(interfaceStore).showGithubAvatars;
interfaceStore.subscribe((prefs) => {
  avatarsEnabled = prefs.showGithubAvatars;
});

function rememberUrl(email: string, url: string): boolean {
  if (urlByEmail.get(email) === url) return false;
  urlByEmail.delete(email);
  urlByEmail.set(email, url);
  while (urlByEmail.size > MAX_CACHED_AVATARS) {
    const oldest = urlByEmail.keys().next();
    if (oldest.done) break;
    urlByEmail.delete(oldest.value);
  }
  return true;
}

/**
 * The picture for an author email, or null (initials only). Honours the
 * setting: with it off nothing is ever returned, so every surface falls back
 * to initials in the same render.
 */
export function avatarUrlFor(email: string | null | undefined): string | null {
  // The canvas asks once per drawn row when it rebuilds a tile; `get()` would
  // subscribe and unsubscribe each time, so read the live module copy.
  if (!avatarsEnabled) return null;
  const key = normalizeAuthorEmail(email);
  if (key === null) return null;
  return noreplyAvatarUrl(key) ?? urlByEmail.get(key) ?? null;
}

export interface AvatarRequestRow {
  id: string;
  author_email: string;
}

/**
 * Per-email candidate commits: up to {@link MAX_OIDS_PER_EMAIL} ids, preferring
 * the OLDEST loaded commits (rows are newest first), since those are the most
 * likely to be pushed and so known to GitHub. Noreply emails are skipped.
 */
function collectCandidates(rows: readonly AvatarRequestRow[]): Map<string, string[]> {
  const out = new Map<string, string[]>();
  for (let i = rows.length - 1; i >= 0; i--) {
    const row = rows[i];
    const key = normalizeAuthorEmail(row.author_email);
    if (key === null || noreplyAvatarUrl(key) !== null) continue;
    if (!/^(?:[0-9a-f]{40}|[0-9a-f]{64})$/i.test(row.id)) continue;
    const ids = out.get(key);
    if (ids === undefined) out.set(key, [row.id]);
    else if (ids.length < MAX_OIDS_PER_EMAIL && !ids.includes(row.id)) ids.push(row.id);
  }
  return out;
}

function mergeCandidates(into: Map<string, string[]>, more: Map<string, string[]>): void {
  for (const [email, ids] of more) {
    const have = into.get(email);
    if (have === undefined) {
      into.set(email, ids.slice());
      continue;
    }
    for (const id of ids) {
      if (have.length >= MAX_OIDS_PER_EMAIL) break;
      if (!have.includes(id)) have.push(id);
    }
  }
}

async function hasGithubRemote(repoPath: string): Promise<boolean> {
  let probe = githubRemoteByRepo.get(repoPath);
  if (probe === undefined) {
    probe = (async () => {
      try {
        const parsed = parseRemoteList(await invoke(LIST_REMOTES_COMMAND, { repoPath }));
        if (parsed.failed) return false;
        return parsed.remotes.some((remote) =>
          [remote.fetch_url, remote.push_url].some(
            (url) => typeof url === "string" && /(^|[@/.])github\.com[:/]/i.test(url),
          ),
        );
      } catch {
        return false;
      }
    })();
    githubRemoteByRepo.set(repoPath, probe);
  }
  return probe;
}

async function runBatch(repoPath: string, candidates: Map<string, string[]>): Promise<Map<string, string[]>> {
  const asked = askedByRepo.get(repoPath) ?? new Set<string>();
  askedByRepo.set(repoPath, asked);
  const batch: { email: string; oids: string[] }[] = [];
  const leftover = new Map<string, string[]>();
  for (const [email, oids] of candidates) {
    if (asked.has(email) || urlByEmail.has(email)) continue;
    if (batch.length >= MAX_EMAILS_PER_REQUEST) leftover.set(email, oids);
    else batch.push({ email, oids: oids.slice(0, MAX_OIDS_PER_EMAIL) });
  }
  if (batch.length === 0) return leftover;
  for (const entry of batch) asked.add(entry.email);

  let report: AuthorAvatarReport;
  try {
    report = parseAuthorAvatarReport(
      await invoke(GITHUB_AUTHOR_AVATARS_COMMAND, { repoPath, authors: batch }),
    );
  } catch (error) {
    report = { ...FAILED_REPORT, error: error instanceof Error ? error.message : String(error) };
  }
  if (!report.available) {
    // Not a verdict about these authors: forget that they were asked, so the
    // retry after the pause asks again.
    for (const entry of batch) asked.delete(entry.email);
    unavailableRepos.set(repoPath, Date.now() + LOOKUP_RETRY_AFTER_MS);
    reportFailure(repoPath, report.error ?? "the lookup did not run");
    return new Map();
  }
  if (report.error !== null) reportFailure(repoPath, report.error);
  let changed = false;
  for (const avatar of report.avatars) {
    const key = normalizeAuthorEmail(avatar.email);
    const url = sanitizeAvatarUrl(avatar.avatar_url);
    if (key !== null && url !== null && rememberUrl(key, url)) changed = true;
  }
  if (changed) versionStore.update((n) => n + 1);
  return leftover;
}

/**
 * Ask GitHub about the authors of `rows`. Never throws. Single-flight per
 * repository: a call during a request queues at most one follow-up carrying
 * the union of what arrived meanwhile.
 */
export async function requestAuthorAvatars(
  repoPath: string,
  rows: readonly AvatarRequestRow[],
): Promise<void> {
  if (!repoPath || !get(interfaceStore).showGithubAvatars) return;
  if (isUnavailable(repoPath)) return;
  const candidates = collectCandidates(rows);
  if (candidates.size === 0) return;

  const flight = flights.get(repoPath);
  if (flight !== undefined) {
    flight.pending ??= new Map();
    mergeCandidates(flight.pending, candidates);
    return;
  }
  const mine: RepoFlight = { pending: null };
  flights.set(repoPath, mine);
  try {
    if (!(await hasGithubRemote(repoPath))) {
      // Not a failure: there is simply no GitHub account to ask about.
      unavailableRepos.set(repoPath, Number.POSITIVE_INFINITY);
      return;
    }
    let work: Map<string, string[]> | null = candidates;
    while (work !== null && work.size > 0) {
      if (isUnavailable(repoPath) || !avatarsEnabled) break;
      const leftover = await runBatch(repoPath, work);
      const next: Map<string, string[]> = leftover;
      if (mine.pending !== null) {
        mergeCandidates(next, mine.pending);
        mine.pending = null;
      }
      work = next;
    }
  } catch (error) {
    // The initials disc is already on screen; say why no picture replaced it.
    reportFailure(repoPath, error instanceof Error ? error.message : String(error));
  } finally {
    flights.delete(repoPath);
  }
}

/** Test seam: forget everything learned this session. */
export function resetGithubAvatarsForTests(): void {
  urlByEmail.clear();
  askedByRepo.clear();
  unavailableRepos.clear();
  reportedFailures.clear();
  githubRemoteByRepo.clear();
  flights.clear();
  versionStore.set(0);
}
