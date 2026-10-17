/**
 * Whether Claude Code saved a hosted conversation, asked before GitPulse offers
 * to resume it.
 *
 * Holding a session id is not evidence of a conversation: a Claude tab killed
 * before its first message has none, and `claude --resume <id>` then fails.
 * The backend looks for the transcript Claude Code writes
 * (`cmd_terminal_conversations`, the same search a task attempt's "Resume
 * conversation" uses), and only `found` is offered. `unknown` — the directory
 * could not be read — is said as such and never taken for `absent`.
 *
 * Not verified against the CLI, and recorded here because it decides what an
 * answer means: whether `/clear` or `/resume` inside a session moves it to
 * another session id (the transcript of the id GitPulse recorded would then
 * be stale), and whether a session killed before its first message ever has a
 * transcript. A Claude Code hook reporting the native id (after CARD 2) is what
 * would close the first.
 */

import { invoke as tauriInvoke } from "../ipc/invoke";
import { isSessionId } from "./tabs";

type Invoke = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;

/**
 * One answer of `cmd_terminal_conversations`. Mirrors
 * `workbench::conversation::ConversationTranscript`; held to it by
 * `check:types`.
 */
export interface ConversationTranscript {
  session_id: string;
  /** `found`, `absent` or `unknown`. */
  transcript: string;
  /** Why it could not be checked, for `unknown`. */
  reason: string | null;
}

export type TranscriptState = "found" | "absent" | "unknown";

export interface TranscriptAnswer {
  state: TranscriptState;
  /** Why the state is `unknown`; null otherwise. */
  reason: string | null;
}

/** Most ids one call asks about: the backend's `MAX_TRANSCRIPT_QUERIES`. */
export const MAX_TRANSCRIPT_QUERIES = 512;

const STATES: readonly TranscriptState[] = ["found", "absent", "unknown"];

/**
 * The answers by session id, one for every id asked. An id the backend did not
 * answer, or answered in a shape this page cannot read, is `unknown` — never
 * `absent`, which would withhold an offer on no evidence.
 */
export function readTranscripts(raw: unknown, asked: readonly string[]): Map<string, TranscriptAnswer> {
  const answers = new Map<string, TranscriptAnswer>();
  if (Array.isArray(raw)) {
    for (const row of raw) {
      if (typeof row !== "object" || row === null) continue;
      const r = row as Record<string, unknown>;
      if (typeof r.session_id !== "string" || !asked.includes(r.session_id)) continue;
      const state = STATES.find((known) => known === r.transcript);
      if (!state) continue;
      const reason = typeof r.reason === "string" ? r.reason.slice(0, 512) : null;
      answers.set(r.session_id, { state, reason: state === "unknown" ? reason ?? "no reason given" : null });
    }
  }
  for (const id of asked) {
    if (!answers.has(id)) answers.set(id, { state: "unknown", reason: "GitPulse did not get an answer for it." });
  }
  return answers;
}

/**
 * Asks once for every id (deduplicated, canonical ids only, at most
 * {@link MAX_TRANSCRIPT_QUERIES}). A call that fails answers every id
 * `unknown`, with the failure as the reason.
 */
export async function checkTranscripts(
  sessionIds: readonly string[],
  invoke: Invoke = tauriInvoke as Invoke,
): Promise<Map<string, TranscriptAnswer>> {
  const asked = [...new Set(sessionIds.map((id) => id.toLowerCase()).filter(isSessionId))].slice(0, MAX_TRANSCRIPT_QUERIES);
  if (asked.length === 0) return new Map();
  try {
    return readTranscripts(await invoke<unknown>("cmd_terminal_conversations", { sessionIds: asked }), asked);
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    return new Map(asked.map((id) => [id, { state: "unknown" as const, reason }]));
  }
}
