import { HANDOFF_MODEL_CHOICES } from "../terminal/agentDefaults";

export type TriggerKind = "/" | "@" | "&";

export interface ActiveTrigger {
  readonly kind: TriggerKind;
  readonly query: string;
  readonly start: number;
  readonly end: number;
}

export interface SlashCommandItem {
  readonly id: string;
  readonly command: string;
  readonly label: string;
  readonly description: string;
  readonly kind: "action" | "prompt";
  readonly insertText?: string;
  readonly requiresThread?: boolean;
  readonly requiresWorking?: boolean;
  readonly requiresStartPage?: boolean;
}

export interface AgentModelOption {
  readonly id: string;
  readonly agent: "claude" | "agy";
  readonly agentLabel: string;
  readonly model: string | null;
  readonly label: string;
  readonly detail: string;
  readonly isAgentDefault?: boolean;
}

const TRIGGER_CHARS = new Set<TriggerKind>(["/", "@", "&"]);

const SLASH_COMMANDS: readonly SlashCommandItem[] = [
  {
    id: "new",
    command: "/new",
    label: "New thread",
    description: "Start a fresh conversation",
    kind: "action",
    requiresThread: true,
  },
  {
    id: "changes",
    command: "/changes",
    label: "Toggle changes",
    description: "Show or hide the changed files panel",
    kind: "action",
    requiresThread: true,
  },
  {
    id: "terminal",
    command: "/terminal",
    label: "Open terminal",
    description: "Open a shell in this thread's worktree",
    kind: "action",
    requiresThread: true,
  },
  {
    id: "handoff",
    command: "/handoff",
    label: "Continue in terminal",
    description: "Move this conversation to a Claude Code terminal tab",
    kind: "action",
    requiresThread: true,
  },
  {
    id: "stop",
    command: "/stop",
    label: "Stop turn",
    description: "Interrupt the active agent turn",
    kind: "action",
    requiresThread: true,
    requiresWorking: true,
  },
  {
    id: "customize",
    command: "/customize",
    label: "Customize layout",
    description: "Open Mavi layout and behavior settings",
    kind: "action",
  },
  {
    id: "clear",
    command: "/clear",
    label: "Clear draft",
    description: "Clear the current composer text",
    kind: "action",
  },
  {
    id: "end",
    command: "/end",
    label: "End session",
    description: "End this thread's agent with an option to summarize work",
    kind: "action",
    requiresThread: true,
  },
  {
    id: "worktree",
    command: "/worktree",
    label: "Toggle worktree",
    description: "Switch whether this thread starts in a new sibling worktree",
    kind: "action",
    requiresStartPage: true,
  },
  {
    id: "explain",
    command: "/explain",
    label: "Explain structure",
    description: "Summarize how this repository is organized and its entry points",
    kind: "prompt",
    insertText: "Explain how this repository is structured and where the key entry points live.",
  },
  {
    id: "review",
    command: "/review",
    label: "Review changes",
    description: "Check uncommitted changes for bugs, edge cases, and missing tests",
    kind: "prompt",
    insertText: "Review the uncommitted changes for bugs, edge cases, and missing tests.",
  },
  {
    id: "diff",
    command: "/diff",
    label: "Summarize diff",
    description: "Summarize the current git diff and highlight any risky edits",
    kind: "prompt",
    insertText: "Show a concise summary of the current git diff and highlight any risky edits.",
  },
  {
    id: "fix",
    command: "/fix",
    label: "Fix issue",
    description: "Diagnose the root cause, fix it cleanly, and verify with tests",
    kind: "prompt",
    insertText: "Diagnose the issue, identify the root cause, fix it cleanly, and verify with tests.",
  },
  {
    id: "refactor",
    command: "/refactor",
    label: "Refactor target",
    description: "Improve clarity without changing behavior, then verify tests",
    kind: "prompt",
    insertText:
      "Refactor the target code for clarity and maintainability without changing behavior, then verify tests pass.",
  },
  {
    id: "compact",
    command: "/compact",
    label: "Compact summary",
    description: "Summarize progress so far, decisions made, and remaining work",
    kind: "prompt",
    requiresThread: true,
    insertText: "Summarize our progress so far, key decisions made, and what remains.",
  },
  {
    id: "tests",
    command: "/tests",
    label: "Fix failing tests",
    description: "Diagnose and fix failing tests, then verify the suite passes",
    kind: "prompt",
    insertText: "Find and fix the failing tests, then verify the suite passes.",
  },
  {
    id: "plan",
    command: "/plan",
    label: "Plan first",
    description: "Inspect relevant files and propose a clear plan before editing",
    kind: "prompt",
    insertText: "Inspect the relevant files and propose a clear plan before editing anything.",
  },
];

const FALLBACK_CLAUDE_MODELS: readonly string[] = HANDOFF_MODEL_CHOICES.claude?.model ?? [
  "fable",
  "opus",
  "sonnet",
  "haiku",
];

/**
 * Detects an active `/`, `@`, or `&` trigger token ending at `caret`.
 *
 * A trigger opens only when the marker sits at index 0 or immediately after
 * whitespace and no whitespace exists between the marker and `caret`, so
 * URLs (`https://...`), email addresses (`a@b.com`), and mid-word symbols
 * never open the menu.
 */
export function detectTrigger(text: string, caret: number): ActiveTrigger | null {
  if (!text || caret <= 0) return null;
  const end = Math.min(caret, text.length);
  if (end <= 0 || /\s/.test(text[end - 1])) return null;

  let start = end - 1;
  while (start > 0 && !/\s/.test(text[start - 1])) {
    start -= 1;
  }

  const ch = text[start] as TriggerKind;
  if (!TRIGGER_CHARS.has(ch)) return null;

  return {
    kind: ch,
    query: text.slice(start + 1, end),
    start,
    end,
  };
}

/**
 * Replaces the active trigger span `[trigger.start, trigger.end)` with
 * `replacement` and returns the updated text and caret position.
 */
export function applyTriggerReplacement(
  text: string,
  trigger: ActiveTrigger,
  replacement: string,
): { text: string; caret: number } {
  const before = text.slice(0, trigger.start);
  const after = text.slice(trigger.end);
  return {
    text: `${before}${replacement}${after}`,
    caret: trigger.start + replacement.length,
  };
}

/**
 * Filters slash commands by thread/working context and ranks matches against
 * the command name, label, and description.
 */
export function filterSlashCommands(
  query: string,
  context: { inThread: boolean; working: boolean },
): SlashCommandItem[] {
  const eligible = SLASH_COMMANDS.filter((item) => {
    if (item.requiresThread && !context.inThread) return false;
    if (item.requiresWorking && !context.working) return false;
    if (item.requiresStartPage && context.inThread) return false;
    return true;
  });

  const needle = query.trim().toLowerCase();
  if (!needle) return eligible;

  const prefixCmd: SlashCommandItem[] = [];
  const nameOrLabel: SlashCommandItem[] = [];
  const descriptionOnly: SlashCommandItem[] = [];

  for (const item of eligible) {
    const cmdName = item.command.slice(1).toLowerCase();
    const label = item.label.toLowerCase();
    const desc = item.description.toLowerCase();

    if (cmdName.startsWith(needle)) {
      prefixCmd.push(item);
    } else if (cmdName.includes(needle) || label.includes(needle)) {
      nameOrLabel.push(item);
    } else if (desc.includes(needle)) {
      descriptionOnly.push(item);
    }
  }

  return [...prefixCmd, ...nameOrLabel, ...descriptionOnly];
}

/**
 * Filters repository-relative paths, prioritizing `priorityPaths` (such as
 * recently touched files), then filename prefix/substring matches, then path
 * substring matches.
 */
export function filterRepoFiles(
  files: readonly string[],
  query: string,
  priorityPaths?: readonly string[],
  limit = 12,
): string[] {
  if (limit <= 0 || files.length === 0) return [];

  const priorityOrder = new Map<string, number>();
  if (priorityPaths) {
    for (let i = 0; i < priorityPaths.length; i += 1) {
      const path = priorityPaths[i];
      if (!priorityOrder.has(path)) priorityOrder.set(path, i);
    }
  }

  const needle = query.trim().toLowerCase();
  if (!needle) {
    const priorityHits: string[] = [];
    const regularHits: string[] = [];
    let foundPriority = 0;

    for (const file of files) {
      const pIdx = priorityOrder.get(file);
      if (pIdx !== undefined) {
        if (priorityHits[pIdx] === undefined) {
          priorityHits[pIdx] = file;
          foundPriority += 1;
        }
      } else if (regularHits.length < limit) {
        regularHits.push(file);
      }
      if (foundPriority === priorityOrder.size && regularHits.length >= limit) break;
    }

    const combined = [...priorityHits.filter((p): p is string => p !== undefined), ...regularHits];
    return combined.slice(0, limit);
  }

  const priorityMatches: Array<{ path: string; rank: number; priorityIdx: number }> = [];
  const basePrefix: string[] = [];
  const baseSubstring: string[] = [];
  const pathPrefix: string[] = [];
  const pathSubstring: string[] = [];

  for (const file of files) {
    const lowerPath = file.toLowerCase();
    if (!lowerPath.includes(needle)) continue;

    const slash = lowerPath.lastIndexOf("/");
    const lowerBase = slash >= 0 ? lowerPath.slice(slash + 1) : lowerPath;

    const matchRank = lowerBase.startsWith(needle)
      ? 0
      : lowerBase.includes(needle)
        ? 1
        : lowerPath.startsWith(needle)
          ? 2
          : 3;

    const pIdx = priorityOrder.get(file);
    if (pIdx !== undefined) {
      priorityMatches.push({ path: file, rank: matchRank, priorityIdx: pIdx });
      continue;
    }

    if (matchRank === 0) basePrefix.push(file);
    else if (matchRank === 1) baseSubstring.push(file);
    else if (matchRank === 2) pathPrefix.push(file);
    else pathSubstring.push(file);
  }

  priorityMatches.sort((a, b) => a.rank - b.rank || a.priorityIdx - b.priorityIdx);

  const byShorterPath = (a: string, b: string) => a.length - b.length;
  basePrefix.sort(byShorterPath);
  baseSubstring.sort(byShorterPath);
  pathPrefix.sort(byShorterPath);
  pathSubstring.sort(byShorterPath);

  const out: string[] = [];
  for (const hit of priorityMatches) {
    out.push(hit.path);
    if (out.length >= limit) return out;
  }
  for (const bucket of [basePrefix, baseSubstring, pathPrefix, pathSubstring]) {
    for (const file of bucket) {
      out.push(file);
      if (out.length >= limit) return out;
    }
  }
  return out;
}

/**
 * Builds the combined agent and model list for `&` completion from Claude Code
 * aliases/catalog and Antigravity's loaded catalog.
 */
export function buildAgentModelOptions(catalogs: {
  claude?: readonly { id: string; label: string | null }[];
  agy?: readonly { id: string; label: string | null }[];
}): AgentModelOption[] {
  const options: AgentModelOption[] = [
    {
      id: "claude",
      agent: "claude",
      agentLabel: "Claude Code",
      model: null,
      label: "Claude Code",
      detail: "Use Claude Code's default model",
      isAgentDefault: true,
    },
    {
      id: "agy",
      agent: "agy",
      agentLabel: "Antigravity",
      model: null,
      label: "Antigravity",
      detail: "Use Antigravity's default model",
      isAgentDefault: true,
    },
  ];

  const claudeLabels = new Map<string, string | null>();
  for (const entry of catalogs.claude ?? []) {
    if (!claudeLabels.has(entry.id)) claudeLabels.set(entry.id, entry.label);
  }

  const seenClaude = new Set<string>();
  const claudeEntries: { id: string; label: string | null }[] = [];
  for (const alias of FALLBACK_CLAUDE_MODELS) {
    if (seenClaude.has(alias)) continue;
    seenClaude.add(alias);
    claudeEntries.push({ id: alias, label: claudeLabels.get(alias) ?? null });
  }
  for (const entry of catalogs.claude ?? []) {
    if (seenClaude.has(entry.id)) continue;
    seenClaude.add(entry.id);
    claudeEntries.push(entry);
  }

  for (const entry of claudeEntries) {
    const cleanLabel = entry.label?.trim() || null;
    options.push({
      id: `claude:${entry.id}`,
      agent: "claude",
      agentLabel: "Claude Code",
      model: entry.id,
      label: cleanLabel ?? entry.id,
      detail: cleanLabel && cleanLabel !== entry.id ? entry.id : "Claude Code model",
    });
  }

  const seenAgy = new Set<string>();
  for (const entry of catalogs.agy ?? []) {
    if (seenAgy.has(entry.id)) continue;
    seenAgy.add(entry.id);
    const cleanLabel = entry.label?.trim() || null;
    options.push({
      id: `agy:${entry.id}`,
      agent: "agy",
      agentLabel: "Antigravity",
      model: entry.id,
      label: cleanLabel ?? entry.id,
      detail: cleanLabel && cleanLabel !== entry.id ? entry.id : "Antigravity model",
    });
  }

  return options;
}

/**
 * Filters agent/model options against `query`, matching `agent`, `agentLabel`,
 * `model`, `label`, and `detail` (plus `agent:model` id).
 */
export function filterAgentModels(
  options: readonly AgentModelOption[],
  query: string,
  limit = 14,
): AgentModelOption[] {
  if (limit <= 0) return [];
  const needle = query.trim().toLowerCase();
  if (!needle) return options.slice(0, limit);

  const prefixHits: AgentModelOption[] = [];
  const substringHits: AgentModelOption[] = [];

  for (const opt of options) {
    const fields = [
      opt.agent,
      opt.agentLabel,
      opt.model ?? "",
      opt.label,
      opt.detail,
      opt.id,
    ].map((value) => value.toLowerCase());

    if (!fields.some((value) => value.includes(needle))) continue;

    if (
      fields[0].startsWith(needle) ||
      fields[2].startsWith(needle) ||
      fields[3].startsWith(needle) ||
      fields[5].startsWith(needle)
    ) {
      prefixHits.push(opt);
    } else {
      substringHits.push(opt);
    }
  }

  return [...prefixHits, ...substringHits].slice(0, limit);
}
