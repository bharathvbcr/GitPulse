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
import { DEFAULT_LIVE_RUNS, MAX_LIVE_RUNS } from "../workbench/vocabulary";
import { DEFAULT_TERMINAL_SESSIONS, isTerminalSessionLimit } from "./sessionLimit";

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
  /**
   * How many task attempts may be live at once, across every repository.
   * Absent means the store's default ({@link DEFAULT_LIVE_RUNS}); the host
   * passes it to the store at each launch, so a change applies to the next.
   */
  max_live_runs?: number;
  /**
   * Which of Claude Code's settings files an agent GitPulse starts in a
   * terminal loads (`--setting-sources`). Absent means the CLI's own default,
   * every one of {@link CLAUDE_SETTING_SOURCES}; a stored value is a
   * non-empty proper subset in that order, which is how the backend stores it.
   */
  claude_setting_sources?: ClaudeSettingSource[];
  /**
   * How many terminal sessions may be open at once, across every repository.
   * Absent means {@link DEFAULT_TERMINAL_SESSIONS}; see `sessionLimit.ts`.
   */
  max_terminal_sessions?: number;
  /**
   * Which model each agent CLI starts with, by launcher. Absent means the
   * CLI's own choice; an entry is never stored empty. Names and levels only —
   * which flag carries them is `model_control` in `terminal_command.rs`.
   */
  models?: Partial<Record<LauncherKind, ModelChoice>>;
}

/**
 * One launcher's model settings. Mirrors `tool_config::ModelChoice`; which
 * fields a launcher takes is the backend's `model_fields`, and a field it
 * does not take is refused at save rather than left out of the launch.
 */
export interface ModelChoice {
  model?: string;
  effort?: EffortLevel;
  fallback?: string[];
  advisor?: string;
}

/**
 * The model controls, in the order a settings row shows them. Mirrors
 * `terminal_command::MODEL_FIELDS`; `agentDefaults.contract.test.ts` binds them.
 */
export const MODEL_FIELDS = ["model", "effort", "fallback", "advisor"] as const;
export type ModelField = (typeof MODEL_FIELDS)[number];

/** Reasoning-effort levels, least first. Mirrors `terminal_command::EFFORT_LEVELS`. */
export const EFFORT_LEVELS = ["low", "medium", "high", "xhigh", "max"] as const;
export type EffortLevel = (typeof EFFORT_LEVELS)[number];

/** Mirrors `terminal_command::MAX_FALLBACK_MODELS`. */
export const MAX_FALLBACK_MODELS = 4;
/** Mirrors `terminal_command::MAX_MODEL_ID_LEN`. */
export const MAX_MODEL_ID_LEN = 160;

/**
 * The fields each launcher takes, used before the backend answers and in a
 * browser preview. The backend derives the authoritative map from its flag
 * table and sends it with the settings; the contract test binds the two.
 */
export const MODEL_FIELDS_BY_LAUNCHER: Readonly<Partial<Record<LauncherKind, readonly ModelField[]>>> = {
  agy: ["model", "effort"],
  claude: ["model", "effort", "fallback", "advisor"],
  codex: ["model"],
  grok: ["model"],
};

/**
 * Advisor names a field offers before any list is fetched: the three Claude
 * Code's own advisor message names. Suggestions only. Model and fallback
 * suggestions come from the CLI's catalog (`agentModelCatalog.ts`), which owns
 * Claude Code's aliases; a second copy here would be the one that drifts.
 */
export const MODEL_SUGGESTIONS: Readonly<Partial<Record<LauncherKind, Partial<Record<ModelField, readonly string[]>>>>> = {
  claude: { advisor: ["fable", "opus", "sonnet"] },
};

/**
 * Whether `id` has the shape of a model name — the same rule as
 * `terminal_command::validate_model_id`. Shape only: whether the model
 * exists is the CLI's to decide when it starts.
 */
export function isModelId(value: unknown): value is string {
  return (
    typeof value === "string" &&
    value.length > 0 &&
    value.length <= MAX_MODEL_ID_LEN &&
    /^[A-Za-z0-9][A-Za-z0-9._:/@[\]-]*$/.test(value)
  );
}

export function isEffortLevel(value: unknown): value is EffortLevel {
  return EFFORT_LEVELS.some((level) => level === value);
}

/**
 * A model choice made safe to send or use: fields the launcher does not take
 * are dropped, each value is dropped unless it has a valid shape, and a choice
 * left with nothing is `null` — which is stored as no entry at all.
 */
export function sanitizeModelChoice(value: unknown, fields: readonly ModelField[]): ModelChoice | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const raw = value as Record<string, unknown>;
  const choice: ModelChoice = {};
  if (fields.includes("model") && isModelId(raw.model)) choice.model = raw.model;
  if (fields.includes("effort") && isEffortLevel(raw.effort)) choice.effort = raw.effort;
  if (fields.includes("fallback") && Array.isArray(raw.fallback)) {
    const list = raw.fallback.filter(isModelId);
    const unique = list.filter((id, index) => list.indexOf(id) === index);
    if (unique.length > 0 && unique.length <= MAX_FALLBACK_MODELS && unique.length === raw.fallback.length) {
      choice.fallback = unique;
    }
  }
  if (fields.includes("advisor") && isModelId(raw.advisor)) choice.advisor = raw.advisor;
  return Object.keys(choice).length ? choice : null;
}

/**
 * Splits what a reader typed into fallback models: commas or whitespace
 * separate them, blanks are dropped. Validation is the caller's.
 */
export function parseFallbackList(text: string): string[] {
  return text
    .split(/[\s,]+/)
    .map((part) => part.trim())
    .filter(Boolean);
}

/**
 * Claude Code's settings sources, in the order `--setting-sources` names them.
 * Mirrors `tool_config::CLAUDE_SETTING_SOURCES`; `agentDefaults.test.ts` reads
 * the Rust source and fails if the two differ.
 */
export const CLAUDE_SETTING_SOURCES = ["user", "project", "local"] as const;
export type ClaudeSettingSource = (typeof CLAUDE_SETTING_SOURCES)[number];

/**
 * A setting-sources choice as the backend stores it: known names, each once,
 * in canonical order — `undefined` when every source is chosen (the CLI's own
 * default, so nothing is passed) and `null` when none is, which no launch can
 * use. Anything that is not an array of strings is `null` too.
 */
export function canonicalSettingSources(value: unknown): ClaudeSettingSource[] | undefined | null {
  if (!Array.isArray(value) || value.length > 16) return null;
  if (!value.every((source) => (CLAUDE_SETTING_SOURCES as readonly unknown[]).includes(source))) return null;
  const kept = CLAUDE_SETTING_SOURCES.filter((source) => value.includes(source));
  if (kept.length === 0) return null;
  return kept.length === CLAUDE_SETTING_SOURCES.length ? undefined : kept;
}

/** The sources a Claude launch will load: the stored subset, or all of them. */
export function settingSources(defaults: AgentDefaults): ClaudeSettingSource[] {
  const stored = canonicalSettingSources(defaults.claude_setting_sources);
  return stored ?? [...CLAUDE_SETTING_SOURCES];
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
  modelFields: Readonly<Partial<Record<LauncherKind, readonly ModelField[]>>> = MODEL_FIELDS_BY_LAUNCHER,
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
  const sanitized: AgentDefaults = { permission };
  if (isLiveRunLimit(raw.max_live_runs)) sanitized.max_live_runs = raw.max_live_runs;
  if (isTerminalSessionLimit(raw.max_terminal_sessions)) sanitized.max_terminal_sessions = raw.max_terminal_sessions;
  const sources = canonicalSettingSources(raw.claude_setting_sources);
  if (sources) sanitized.claude_setting_sources = sources;
  if (raw.models && typeof raw.models === "object" && !Array.isArray(raw.models)) {
    const models: Partial<Record<LauncherKind, ModelChoice>> = {};
    for (const [launcher, choice] of Object.entries(raw.models)) {
      const fields = modelFields[launcher as LauncherKind];
      if (!fields) continue;
      const clean = sanitizeModelChoice(choice, fields);
      if (clean) models[launcher as LauncherKind] = clean;
    }
    if (Object.keys(models).length) sanitized.models = models;
  }
  return sanitized;
}

/** The stored model choice for `launcher`, or an empty one. */
export function modelChoiceOf(defaults: AgentDefaults, launcher: LauncherKind): ModelChoice {
  return defaults.models?.[launcher] ?? {};
}

/**
 * The defaults with `launcher`'s model choice replaced — removed when the
 * choice is empty, so "the CLI's own choice" is stored as absence.
 */
export function withModelChoice(defaults: AgentDefaults, launcher: LauncherKind, choice: ModelChoice): AgentDefaults {
  const models = { ...(defaults.models ?? {}) };
  const kept = Object.fromEntries(
    Object.entries(choice).filter(([, value]) => value !== undefined && value !== "" && !(Array.isArray(value) && value.length === 0)),
  ) as ModelChoice;
  if (Object.keys(kept).length) models[launcher] = kept;
  else delete models[launcher];
  const next: AgentDefaults = { ...defaults };
  if (Object.keys(models).length) next.models = models;
  else delete next.models;
  return next;
}

/** Whether `value` is a limit the store accepts: a whole number in 1..=ceiling. */
export function isLiveRunLimit(value: unknown): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= 1 && value <= MAX_LIVE_RUNS;
}

/** The limit a launch will pass: the stored one, or the store's default. */
export function liveRunLimit(defaults: AgentDefaults): number {
  return isLiveRunLimit(defaults.max_live_runs) ? defaults.max_live_runs : DEFAULT_LIVE_RUNS;
}

/** The terminal session limit a save will apply: the stored one, or the default. */
export function terminalSessionLimitOf(defaults: AgentDefaults): number {
  return isTerminalSessionLimit(defaults.max_terminal_sessions) ? defaults.max_terminal_sessions : DEFAULT_TERMINAL_SESSIONS;
}
