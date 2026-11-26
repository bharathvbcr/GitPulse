/**
 * Sessions left running in a terminal host of an earlier GitPulse version.
 *
 * An update that changes the host protocol starts a new host; the old one keeps
 * every process it already runs. This version cannot attach to them or type
 * into them, but the backend can list them, stop them and read their saved
 * output (`cmd_terminal_legacy_hosts`, `_kill`, `_transcript`). Those sessions
 * are a different population from `terminalSessions`: terminal ids repeat
 * across hosts, so each is keyed by protocol AND id, and none is ever sent
 * through `cmd_terminal_kill` or `cmd_terminal_transcript`.
 *
 * A host that couldn't be listed is reported as that, never as holding
 * nothing: the sessions may well be running.
 */

import { invoke as tauriInvoke } from "../ipc/invoke";
import { describeError } from "../ui/copy/describeFailure";
import { readSavedTranscript, type SavedTranscript, type TranscriptDeps, TRANSCRIPT_READ_MS } from "./savedTranscript";
import { terminalDeadline } from "./inputQueue";

type Invoke = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

export interface LegacySession {
  readonly id: string;
  readonly program: string;
  readonly cwd: string;
  readonly repo: string;
  readonly launcher: string | null;
  readonly title: string | null;
  readonly ending: boolean;
}

/** One earlier host: its sessions when it was listed, or why it could not be. */
export type LegacyHost =
  | { readonly state: "listed"; readonly protocol: number; readonly build: string; readonly sessions: readonly LegacySession[] }
  | { readonly state: "unlisted"; readonly protocol: number; readonly reason: string };

/** The host list's answer. A failed question is `failed`, never an empty list. */
export type LegacyHostsRead =
  | { readonly kind: "ok"; readonly hosts: readonly LegacyHost[] }
  | { readonly kind: "failed"; readonly reason: string };

const MAX_HOSTS = 16;
const MAX_SESSIONS = 256;
const LIST_MS = 15_000;
const KILL_MS = 30_000;

function text(value: unknown, max = 4096): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= max && !value.includes("\0");
}

function parseSession(raw: unknown): LegacySession | null {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const row = raw as Record<string, unknown>;
  if (!text(row.id, 128) || !text(row.cwd) || !text(row.repo)) return null;
  const program = typeof row.program === "string" ? row.program : "";
  const launcher = text(row.launcher, 128) ? row.launcher : null;
  const title = text(row.title, 512) ? row.title.trim() || null : null;
  return { id: row.id, program, cwd: row.cwd, repo: row.repo, launcher, title, ending: row.ending === true };
}

const UNLISTED_REASONS: Record<string, string> = {
  no_compat: "this version has no way to talk to that host",
};

/**
 * Validates the host list at the boundary. A host whose answer is malformed is
 * `unlisted` with a reason rather than dropped, so the reader still learns
 * there is a host; a malformed session inside a listed host makes the host
 * unlisted for the same reason (a half-read list must not read as complete).
 */
export function parseLegacyHosts(raw: unknown): LegacyHost[] | null {
  if (!Array.isArray(raw)) return null;
  const hosts: LegacyHost[] = [];
  for (const entry of raw.slice(0, MAX_HOSTS)) {
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) return null;
    const report = entry as Record<string, unknown>;
    const protocol = report.protocol;
    if (typeof protocol !== "number" || !Number.isSafeInteger(protocol) || protocol < 0) return null;
    const status = report.status;
    if (!status || typeof status !== "object" || Array.isArray(status)) return null;
    const body = status as Record<string, unknown>;
    if (body.state === "listed") {
      const info = body.info && typeof body.info === "object" ? (body.info as Record<string, unknown>) : {};
      const rawSessions = Array.isArray(body.sessions) ? body.sessions : null;
      const sessions = rawSessions ? rawSessions.slice(0, MAX_SESSIONS).map(parseSession) : null;
      if (!sessions || sessions.some((session) => session === null)) {
        hosts.push({ state: "unlisted", protocol, reason: "its session list was unreadable" });
        continue;
      }
      hosts.push({
        state: "listed",
        protocol,
        build: typeof info.build === "string" ? info.build : "",
        sessions: sessions as LegacySession[],
      });
    } else if (typeof body.state === "string") {
      const reason = typeof body.reason === "string" && body.reason ? body.reason : (UNLISTED_REASONS[body.state] ?? body.state.replaceAll("_", " "));
      hosts.push({ state: "unlisted", protocol, reason });
    } else {
      return null;
    }
  }
  return hosts;
}

/** Asks for the earlier hosts. Never throws. */
export async function readLegacyHosts(invoke: Invoke = tauriInvoke as Invoke): Promise<LegacyHostsRead> {
  try {
    const raw = await terminalDeadline(invoke<unknown>("cmd_terminal_legacy_hosts"), LIST_MS, "Listing earlier terminal hosts");
    const hosts = parseLegacyHosts(raw);
    if (!hosts) return { kind: "failed", reason: "The terminal host's list of earlier hosts was malformed." };
    return { kind: "ok", hosts };
  } catch (error) {
    return { kind: "failed", reason: describeError(error, "terminal") };
  }
}

/** The list rows and the hosts that could not be read, for one render. */
export function legacyView(read: LegacyHostsRead | null): {
  rows: { key: string; protocol: number; session: LegacySession }[];
  problems: string[];
} {
  if (!read) return { rows: [], problems: [] };
  if (read.kind === "failed") return { rows: [], problems: [`Sessions in a previous version's terminal host couldn't be listed: ${read.reason}`] };
  const rows: { key: string; protocol: number; session: LegacySession }[] = [];
  const problems: string[] = [];
  for (const host of read.hosts) {
    if (host.state === "unlisted") {
      problems.push(`A previous version's terminal host (protocol ${host.protocol}) is running sessions that couldn't be listed: ${host.reason}.`);
      continue;
    }
    for (const session of host.sessions) rows.push({ key: `legacy:${host.protocol}:${session.id}`, protocol: host.protocol, session });
  }
  return { rows, problems };
}

/** What a legacy session is called in the list. */
export function legacySessionLabel(session: LegacySession): string {
  return session.title || session.launcher || session.program.split("/").pop() || "Shell";
}

/** The question before stopping one. Always asked: it is running, and nothing here has seen what it is doing. */
export function legacyStopQuestion(session: LegacySession): { title: string; message: string } {
  const label = legacySessionLabel(session);
  return {
    title: `Stop ${label}?`,
    message: `${label} is still running in the previous Waylode version's terminal host. Stopping it ends whatever it is doing; this version cannot reattach to it.`,
  };
}

/**
 * Stops one session on an earlier host, after asking. Resolves to whether it
 * was stopped; a declined question is not a stop. It rejects when the host
 * could not be asked or refused, so the caller shows why.
 */
export async function stopLegacySession(
  protocol: number,
  session: LegacySession,
  deps: {
    invoke?: Invoke;
    confirm: (options: { title: string; message: string; confirmLabel: string; destructive: boolean }) => Promise<boolean>;
  },
): Promise<boolean> {
  if (!(await deps.confirm({ ...legacyStopQuestion(session), confirmLabel: "Stop", destructive: true }))) return false;
  const invoke = deps.invoke ?? (tauriInvoke as Invoke);
  await terminalDeadline(invoke("cmd_terminal_legacy_kill", { protocol, sessionId: session.id }), KILL_MS, "Stopping a session on the earlier terminal host");
  return true;
}

/**
 * The newest saved output of an earlier host's session, read-only, with
 * secrets masked in the host. The same reader and the same six outcomes as
 * `readSavedTranscript`: only the command differs.
 */
export function readLegacyTranscript(
  protocol: number,
  sessionId: string,
  invoke: Invoke = tauriInvoke as Invoke,
): Promise<SavedTranscript> {
  const deps: TranscriptDeps = {
    read: (args) => invoke<unknown>("cmd_terminal_legacy_transcript", { protocol, sessionId: args.sessionId, maxBytes: args.maxBytes, redact: args.redact }),
    deadlineMs: TRANSCRIPT_READ_MS,
  };
  return readSavedTranscript(sessionId, { redact: true }, deps);
}
