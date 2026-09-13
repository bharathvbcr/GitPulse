/**
 * Apple Intelligence, as the task sheet sees it.
 *
 * GitPulse already had one way to write a task from notes: a local model
 * server, discovered over loopback and driven by the Manvi worker. On a Mac
 * that can run Apple Intelligence there is a second, and it is better in the
 * two ways that matter here — there is nothing to install, and the text never
 * reaches a socket.
 *
 * ## It is an engine, not a second lifecycle
 *
 * The proposal is still created in the local store, still has an id, a
 * revision and a source revision, and is still published with
 * `enhancements.complete`. Only the middle step changes: instead of
 * `enhancements.generate` reaching the Manvi sidecar, `draftWithApple` runs
 * the on-device model and the caller completes the proposal itself. Accept,
 * undo, dismiss, history and field locks never knew which engine wrote the
 * text and still do not.
 *
 * ## Three different "no"
 *
 * `AppleIntelligenceStatus` refuses to collapse them, because they call for
 * different things from the reader: a build with no bridge is not something
 * they can fix, Apple Intelligence switched off is one setting away, and a
 * model still downloading just needs a minute.
 */

import { invoke } from "@tauri-apps/api/core";

export interface AppleIntelligenceStatus {
  /** False when this build has no Foundation Models bridge at all. */
  compiled: boolean;
  state: "available" | "unavailable" | "unsupported_os";
  reason: string | null;
  detail: string;
}

export interface AppleIntelligenceRequest {
  kind: "draft" | "improve" | "extract";
  fields: string[];
  notes: string;
  title: string;
  description: string;
  context: string;
}

export interface AppleIntelligenceDraft {
  title: string | null;
  description: string | null;
  rationale: string;
}

export interface AppleIntelligenceError {
  code: string;
  message: string;
}

/** Matches `MAX_INPUT_CHARS` in `src-tauri/src/ai/apple.rs`. */
export const MAX_APPLE_INPUT_CHARS = 12_000;

export const APPLE_ENGINE = "apple-intelligence";
export const APPLE_MODEL = "on-device";

const UNKNOWN: AppleIntelligenceStatus = {
  compiled: false,
  state: "unsupported_os",
  reason: "not_compiled",
  detail: "Apple Intelligence is not available in this build.",
};

export function isAppleStatus(value: unknown): value is AppleIntelligenceStatus {
  if (!value || typeof value !== "object") return false;
  const raw = value as Partial<AppleIntelligenceStatus>;
  return typeof raw.compiled === "boolean"
    && (raw.state === "available" || raw.state === "unavailable" || raw.state === "unsupported_os")
    && (raw.reason === null || typeof raw.reason === "string")
    && typeof raw.detail === "string";
}

export function appleReady(status: AppleIntelligenceStatus | null): boolean {
  return status?.state === "available";
}

/**
 * A short line for the engine picker, or "" when there is nothing to say.
 *
 * Deliberately does not repeat `detail` — that is the sentence shown when the
 * engine is chosen and cannot run. This is the one word beside its name.
 */
export function appleBadge(status: AppleIntelligenceStatus | null): string {
  if (!status) return "";
  if (status.state === "available") return "On this Mac";
  if (!status.compiled) return "Not in this build";
  if (status.reason === "apple_intelligence_not_enabled") return "Turned off";
  if (status.reason === "model_not_ready") return "Preparing";
  if (status.reason === "device_not_eligible") return "Unsupported Mac";
  return "Unavailable";
}

/**
 * The context line handed to the model.
 *
 * Kept to facts the task already carries. The model is explicitly told not to
 * invent file names or ticket numbers, and giving it a repository name it can
 * echo is the difference between a generic brief and a specific one.
 */
export function appleContext(input: {
  kind?: string;
  repositories?: readonly string[];
  labels?: readonly string[];
}): string {
  const parts: string[] = [];
  const repositories = (input.repositories ?? []).filter((name) => name.trim()).slice(0, 8);
  if (repositories.length) parts.push(`Repository: ${repositories.join(", ")}`);
  if (input.kind?.trim()) parts.push(`Task type: ${input.kind.trim()}`);
  const labels = (input.labels ?? []).filter((label) => label.trim()).slice(0, 12);
  if (labels.length) parts.push(`Labels: ${labels.join(", ")}`);
  return parts.join(". ");
}

/**
 * Whether this request is worth sending, and what to say if not.
 *
 * The same rules run again in Rust, which is where they are enforced; this
 * copy exists so the button can be disabled with a reason instead of the
 * reader pressing it and waiting for a refusal.
 */
export function appleGate(
  status: AppleIntelligenceStatus | null,
  request: Pick<AppleIntelligenceRequest, "fields" | "notes" | "title" | "description" | "context">,
): { ok: boolean; reason: string } {
  if (!status) return { ok: false, reason: "Checking Apple Intelligence…" };
  if (status.state !== "available") return { ok: false, reason: status.detail };
  if (!request.fields.length) return { ok: false, reason: "Choose a field to write." };
  const size = [request.notes, request.title, request.description, request.context]
    .reduce((total, text) => total + [...text].length, 0);
  if (size > MAX_APPLE_INPUT_CHARS) {
    return { ok: false, reason: `Keep the task below ${MAX_APPLE_INPUT_CHARS.toLocaleString()} characters for the on-device model.` };
  }
  if (!request.notes.trim() && !request.title.trim() && !request.description.trim()) {
    return { ok: false, reason: "Write some notes first." };
  }
  return { ok: true, reason: "" };
}

export function explainAppleError(cause: unknown): string {
  if (cause && typeof cause === "object" && "message" in cause) {
    const error = cause as AppleIntelligenceError;
    if (typeof error.message === "string" && error.message.trim()) return error.message;
  }
  if (cause instanceof Error && cause.message) return cause.message;
  return "Apple Intelligence could not finish.";
}

export function appleErrorCode(cause: unknown): string {
  if (cause && typeof cause === "object" && "code" in cause) {
    const code = (cause as AppleIntelligenceError).code;
    if (typeof code === "string" && code) return code;
  }
  return "worker_error";
}

export async function appleIntelligenceStatus(): Promise<AppleIntelligenceStatus> {
  try {
    const status = await invoke<unknown>("cmd_apple_intelligence_status");
    return isAppleStatus(status) ? status : UNKNOWN;
  } catch {
    // An unreachable command is a fact about this build, not about the Mac.
    return UNKNOWN;
  }
}

export async function draftWithApple(request: AppleIntelligenceRequest): Promise<AppleIntelligenceDraft> {
  return await invoke<AppleIntelligenceDraft>("cmd_apple_intelligence_draft", { request });
}
