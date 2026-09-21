/**
 * What an agent CLI started from a terminal tab begins with.
 *
 * ## What this module does and does not know
 *
 * It knows mode *names* and how to describe them to a reader. It does not know
 * which flag means "plan" for which CLI — that is `policy()` in
 * `src-tauri/src/workbench/terminal_command.rs`, the same table the workbench
 * handoff expands, and the backend applies it at the spawn boundary. A second
 * copy of the flags here is precisely the transcription that drifts, so the
 * only thing that crosses the IPC boundary is a mode name.
 *
 * `agentDefaults.contract.test.ts` reads the Rust source and fails if these
 * names drift from the ones that table can expand.
 *
 * ## Why bypass is storable here but never applied here
 *
 * `sanitizeHandoff` refuses to *restore* bypass for the workbench handoff,
 * because a mode that disables a safety control must be chosen deliberately.
 * That reasoning is about acknowledgement rather than storage: what must not
 * happen is a session that skips permission checks without the user saying so
 * at that moment. So bypass is a storable preference and the acknowledgement
 * is asked per launch — and the backend refuses the expansion without one, so
 * a bug here cannot produce a bypassed session on its own.
 */

import type { LauncherKind } from "./tabs";

/**
 * Permission modes, least authority first, so the dangerous end of the range
 * is the far end rather than a neighbour of the safe default.
 */
export const PERMISSION_MODES = [
  "inspect",
  "ask",
  "edit",
  "auto_review",
  "preapproved",
  "bypass",
] as const;

export type PermissionMode = (typeof PERMISSION_MODES)[number];

/** The one mode that turns a safety control off. Named once. */
export const BYPASS_MODE: PermissionMode = "bypass";

/**
 * Launchers that accept a permission mode.
 *
 * Strictly smaller than the tab strip's list, and for a mechanical reason: a
 * mode is expanded through a provider's own flags, and `shell` and `manvi`
 * have no entry in that table. The backend derives the authoritative list and
 * sends it with the settings; this is the fallback used before it answers and
 * in a browser preview, and the contract test binds the two.
 */
export const PERMISSION_LAUNCHERS: readonly LauncherKind[] = ["claude", "codex", "grok", "agy"];

/**
 * How each mode is described to a reader.
 *
 * Written as what the agent may *do*, not as the flag's spelling: a reader
 * choosing a default is deciding how much authority to hand over, and
 * `--permission-mode acceptEdits` does not answer that question.
 */
export const PERMISSION_LABELS: Record<PermissionMode, { label: string; detail: string }> = {
  inspect: { label: "Plan only", detail: "Reads and proposes. Changes nothing." },
  ask: { label: "Ask every time", detail: "Prompts before each action it takes." },
  edit: { label: "Edit files", detail: "Accepts file edits; still asks for commands." },
  auto_review: { label: "Auto-review", detail: "Acts, and surfaces its work for review." },
  preapproved: { label: "Pre-approved", detail: "Acts without asking, inside its sandbox." },
  bypass: {
    label: "Skip permissions",
    detail: "No permission checks and no sandbox. Confirmed at each launch.",
  },
};

export function isPermissionMode(value: unknown): value is PermissionMode {
  return PERMISSION_MODES.some((mode) => mode === value);
}

/**
 * The stored defaults.
 *
 * `permission` is a map rather than one field per launcher so that a provider
 * gaining a policy does not change this shape. An absent entry means "the
 * CLI's own default", which is a distinct answer from any mode this can name
 * and is why it is absence rather than a sentinel string.
 */
export interface AgentDefaults {
  permission: Partial<Record<LauncherKind, PermissionMode>>;
}

export const EMPTY_AGENT_DEFAULTS: AgentDefaults = { permission: {} };

/**
 * The mode a launch will actually use, or `null` for "the CLI's own default".
 *
 * Returns null for a launcher with no policy even when something is stored
 * against it — a hand-edited `tools.json` can contain one, and the backend
 * would refuse it at spawn. Refusing to *offer* it here means the refusal is
 * never reached, while still never pretending a mode was applied.
 */
export function effectiveMode(
  defaults: AgentDefaults,
  launcher: LauncherKind,
  supported: readonly LauncherKind[] = PERMISSION_LAUNCHERS,
): PermissionMode | null {
  if (!supported.includes(launcher)) return null;
  const mode = defaults.permission[launcher];
  return isPermissionMode(mode) ? mode : null;
}

/** Whether this launch needs the reader to acknowledge it before it starts. */
export function requiresAcknowledgement(mode: PermissionMode | null): boolean {
  return mode === BYPASS_MODE;
}

/**
 * A stored value made safe to use.
 *
 * Every rule here closes a hole a plain `JSON.parse` would leave: a non-object
 * becomes the empty default, an unknown launcher key is dropped, and an
 * unknown mode is dropped rather than coerced to a safe-looking one.
 *
 * Note what is deliberately *not* here: bypass is not stripped. It is a
 * legitimate stored preference, and what makes it safe is the acknowledgement
 * the launch asks for, not the parser pretending it was never chosen.
 *
 * Which launcher a *new tab* starts is deliberately not part of this: that is
 * `interfaceStore.terminalLauncher`, which already owns it. A second stored
 * answer to the same question is how two settings come to disagree.
 */
export function sanitizeAgentDefaults(
  value: unknown,
  supported: readonly LauncherKind[] = PERMISSION_LAUNCHERS,
): AgentDefaults {
  if (!value || typeof value !== "object") return { permission: {} };
  const raw = value as Partial<AgentDefaults>;
  const permission: Partial<Record<LauncherKind, PermissionMode>> = {};
  if (raw.permission && typeof raw.permission === "object") {
    for (const [launcher, mode] of Object.entries(raw.permission)) {
      if (!supported.includes(launcher as LauncherKind)) continue;
      if (!isPermissionMode(mode)) continue;
      permission[launcher as LauncherKind] = mode;
    }
  }
  return { permission };
}
