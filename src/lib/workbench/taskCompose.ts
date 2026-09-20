import { PRIORITY_LABELS } from "./boardDrag";
import { STATUS_LABELS, type TaskDraft, type TaskStatus } from "./client";
import { displayTitle } from "./taskDelete";
import { formatLogsSection } from "./taskLogs";

export const MAX_AGENT_COPY_TASKS = 8;
export const MAX_SUBTASKS = 128;
export const SUBTASK_CAP = 4096;

const CHECKLIST_HEADING = /^(?:#{1,6}\s+)?(?:task\s+)?(?:sub[- ]?tasks?|checklist|acceptance\s+criteria|tasks|to-?dos?)(?:\s*:)?\s*$/i;
const CHECKLIST_COLON_LINE = /^(?:task\s+)?(?:sub[- ]?tasks?|checklist|acceptance\s+criteria|tasks|to-?dos?):\s*$/i;
const CHECKBOX_LINE = /^\s*[-*+]?\s*\[([ xX])\]\s*(.+)$/;
const BULLET_OR_NUMBER_LINE = /^\s*(?:[-*+]|\d+[\.)])\s+(.+)$/;
const PLACEHOLDER_CRITERIA = /^(?:no\s+acceptance\s+criteria\s+recorded|none|n\/a)\.?$/i;

function cleanSubtaskItem(raw: string): string {
  let text = raw.trim();
  text = text.replace(/^\[[ xX]\]\s*/, "");
  text = text.replace(/^[-*+]\s+/, "");
  text = text.replace(/^\d+[\.)]\s+/, "");
  text = text.trim();
  if (PLACEHOLDER_CRITERIA.test(text)) return "";
  const chars = [...text];
  return chars.length > SUBTASK_CAP ? chars.slice(0, SUBTASK_CAP).join("") : text;
}

export function mergeSubtasks(
  existing: readonly string[],
  incoming: readonly string[],
  cap = MAX_SUBTASKS,
): string[] {
  const limit = Number.isSafeInteger(cap) && cap > 0 ? Math.min(cap, MAX_SUBTASKS) : MAX_SUBTASKS;
  const result: string[] = [];
  const seen = new Set<string>();

  for (const item of existing ?? []) {
    const cleaned = cleanSubtaskItem(typeof item === "string" ? item : "");
    if (!cleaned) continue;
    const key = cleaned.toLowerCase();
    if (!seen.has(key)) {
      seen.add(key);
      result.push(cleaned);
    }
  }

  for (const item of incoming ?? []) {
    if (result.length >= limit) break;
    const cleaned = cleanSubtaskItem(typeof item === "string" ? item : "");
    if (!cleaned) continue;
    const key = cleaned.toLowerCase();
    if (!seen.has(key)) {
      seen.add(key);
      result.push(cleaned);
    }
  }

  return result.slice(0, limit);
}

export function extractSubtasksFromContent(content: unknown): {
  subtasks: string[];
  remainingText: string;
} {
  if (typeof content !== "string" || !content.trim()) {
    return { subtasks: [], remainingText: typeof content === "string" ? content : "" };
  }

  const lines = content.split(/\r?\n/);
  const subtasks: string[] = [];
  const keptLines: string[] = [];
  let inChecklistSection = false;

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const trimmed = line.trim();
    const isHeading = /^#{1,6}\s+/.test(trimmed);

    if (isHeading || CHECKLIST_COLON_LINE.test(trimmed)) {
      if (CHECKLIST_HEADING.test(trimmed) || CHECKLIST_COLON_LINE.test(trimmed)) {
        inChecklistSection = true;
        continue;
      } else {
        inChecklistSection = false;
      }
    }

    if (inChecklistSection) {
      if (!trimmed) {
        const nextNonEmpty = lines.slice(i + 1).find((l) => l.trim().length > 0);
        if (
          nextNonEmpty &&
          (CHECKBOX_LINE.test(nextNonEmpty.trim()) ||
            BULLET_OR_NUMBER_LINE.test(nextNonEmpty.trim()))
        ) {
          continue;
        } else {
          inChecklistSection = false;
          keptLines.push(line);
          continue;
        }
      }

      const checkboxMatch = trimmed.match(CHECKBOX_LINE);
      const bulletMatch = trimmed.match(BULLET_OR_NUMBER_LINE);
      if (checkboxMatch) {
        const item = cleanSubtaskItem(checkboxMatch[2]);
        if (item) subtasks.push(item);
        continue;
      } else if (bulletMatch) {
        const item = cleanSubtaskItem(bulletMatch[1]);
        if (item) subtasks.push(item);
        continue;
      } else {
        inChecklistSection = false;
        keptLines.push(line);
        continue;
      }
    }

    const standaloneMatch = trimmed.match(CHECKBOX_LINE);
    if (standaloneMatch) {
      const item = cleanSubtaskItem(standaloneMatch[2]);
      if (item) subtasks.push(item);
      continue;
    }

    keptLines.push(line);
  }

  let remainingText = keptLines.join("\n").replace(/\n{3,}/g, "\n\n").trim();
  if (content.endsWith("\n") && remainingText) {
    remainingText += "\n";
  }

  const uniqueSubtasks = mergeSubtasks([], subtasks);
  return { subtasks: uniqueSubtasks, remainingText };
}

export function extractSubtasks(text: unknown): string[] {
  return extractSubtasksFromContent(text).subtasks;
}

export function extractDraftSubtasks(
  draft: { description?: string; acceptance_criteria?: readonly string[] },
  notes?: string,
): { description: string; subtasks: string[]; extracted: boolean; count: number } {
  const fromDesc = extractSubtasksFromContent(draft.description ?? "");
  const fromNotes = extractSubtasks(notes ?? "");
  const allIncoming = [...fromDesc.subtasks, ...fromNotes];
  if (!allIncoming.length) {
    return {
      description: typeof draft.description === "string" ? draft.description : "",
      subtasks: (draft.acceptance_criteria ?? []).map((s) => s.trim()).filter(Boolean),
      extracted: false,
      count: 0,
    };
  }
  const existing = (draft.acceptance_criteria ?? []).map((s) => s.trim()).filter(Boolean);
  const existingSet = new Set(existing.map((s) => s.toLowerCase()));
  const newItems = allIncoming.filter((item) => !existingSet.has(item.toLowerCase()));
  const merged = mergeSubtasks(existing, allIncoming);
  return {
    description: fromDesc.remainingText,
    subtasks: merged,
    extracted: true,
    count: newItems.length,
  };
}


/**
 * The instruction every copied task carries into an agent.
 *
 * This is the whole prompt. A run started from the handoff form sends only
 * `{id, request_id, expected_revision}` to Manvi, and a clipboard copy is
 * pasted into a session that knows nothing about this task — so whatever is
 * not said here is not said at all.
 *
 * Three things it has to do, and the second and third were missing:
 *
 *  1. **Name the field roles.** Without them an agent reads the acceptance
 *     criteria as suggestions.
 *  2. **Keep the author's words intact.** The board's own tests are written
 *     around this ("Preserve E42"): a task's description is evidence, and an
 *     agent that paraphrases a reproduction step into a tidier one has
 *     answered a different question. `improve` already told the on-device
 *     model this; the agent handoff never did.
 *  3. **Point at the tools this project ships.** GitPulse and DevMap both
 *     expose skills and MCP tools that answer "who else is editing this" and
 *     "what calls this" directly, and an agent that does not know they exist
 *     greps instead — or, worse, concludes from silence.
 *
 * Named skills are asserted against the directories that ship them, so a
 * skill added or renamed cannot quietly fall out of this text.
 */
export const AGENT_COPY_PREAMBLE = [
  "GitPulse task for an AI agent. Use the title as the goal, the description as context, and the acceptance criteria as the definition of done. Raw logs, when present, are evidence. Do not invent repositories or skip criteria.",
  "Preserve the author's intent and message. The wording below is evidence: keep error codes, identifiers, paths, versions, commands and quoted text exactly as written, carry the original meaning into whatever you produce, and do not restate the task as a smaller or easier one.",
  "Orient with the tools this project ships before you edit, and skip any this session does not have. GitPulse — the gitpulse-insights and gitpulse-collisions skills, and the gitpulse_* MCP tools — for worktrees, in-flight changes and overlapping edits. DevMap — the devmap, devmap-debugging, devmap-exploring, devmap-impact and devmap-refactoring skills, and the devmap_* MCP tools — for symbols, callers, blast radius and affected tests. Pass the repository's absolute repo_path on every call. A tool that is unavailable, truncated or empty is not evidence that a symbol, caller or collision does not exist; name the check you could not run instead of reporting it as clean.",
].join("\n\n");

const TITLE_CAP = 300;
const NOTES_CAP = 65_536;
const WEAK_TITLE_CLAUSE = /^(?:\d+|[A-Za-z]{1,4}|[A-Za-z](?:\.[A-Za-z])+)$/;

function stripTitleMarkup(line: string): string {
  return line
    .replace(/^#{1,6}\s+/, "")
    .replace(/^[-*+]\s+(?:\[[ xX]\]\s+)?/, "")
    .trim();
}

function isWeakTitleClause(clause: string): boolean {
  const body = clause.trim().replace(/[.!?]+$/u, "").trim();
  return !body || WEAK_TITLE_CLAUSE.test(body);
}

function firstStrongClause(line: string): string {
  for (let i = 0; i < line.length; i++) {
    const ch = line[i];
    if (ch !== "." && ch !== "!" && ch !== "?") continue;
    if (i + 1 < line.length && !/\s/u.test(line[i + 1]!)) continue;
    const clause = line.slice(0, i + 1).trim();
    if (isWeakTitleClause(clause)) continue;
    return clause;
  }
  return line;
}

function titleSource(text: string): string {
  const lines = text.split(/\n/);
  for (const raw of lines) {
    const line = stripTitleMarkup(raw.trim());
    if (!line) continue;
    const clause = firstStrongClause(line);
    if (!isWeakTitleClause(clause)) return clause;
  }
  return stripTitleMarkup((lines[0] ?? text).trim()) || text;
}

export function sanitizeNotes(value: unknown, cap = NOTES_CAP): string {
  if (typeof value !== "string") return "";
  const cleaned = value.replace(/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/g, "").trim();
  if (!cleaned) return "";
  const limit = Number.isSafeInteger(cap) && cap >= 32 ? cap : NOTES_CAP;
  return [...cleaned].slice(0, limit).join("");
}

/** First strong line or sentence of notes, skipping list markers and abbreviations. */
export function titleFromNotes(notes: unknown, cap = TITLE_CAP): string {
  const limit = Number.isSafeInteger(cap) && cap >= 16 ? cap : TITLE_CAP;
  const text = sanitizeNotes(notes, NOTES_CAP);
  if (!text) return "";
  const source = titleSource(text);
  const characters = [...source];
  return characters.length > limit ? characters.slice(0, limit - 1).join("") + "…" : source;
}

export function applyNotesToDraft(
  draft: Pick<TaskDraft, "title" | "description">,
  notes: unknown,
): { title: string; description: string; extracted: boolean } {
  // Preserve all entered evidence; the save boundary reports oversized input.
  const text = sanitizeNotes(notes, typeof notes === "string" ? Math.max(NOTES_CAP, notes.length) : NOTES_CAP);
  if (!text) {
    return {
      title: typeof draft.title === "string" ? draft.title : "",
      description: typeof draft.description === "string" ? draft.description : "",
      extracted: false,
    };
  }
  const existingTitle = typeof draft.title === "string" ? draft.title.trim() : "";
  const title = existingTitle || titleFromNotes(text) || "Draft from notes";
  const description = typeof draft.description === "string" ? draft.description : "";
  return { title, description: description.trim() && description.trim() !== text ? `${description}\n\n${text}` : text, extracted: true };
}

/** Apply notes once, then clear them so a later save cannot overwrite Manvi’s result. */
export function consumeNotes(
  draft: Pick<TaskDraft, "title" | "description">,
  notes: unknown,
): { title: string; description: string; notes: string; extracted: boolean } {
  const applied = applyNotesToDraft(draft, notes);
  if (!applied.extracted) {
    return {
      title: typeof draft.title === "string" ? draft.title : "",
      description: typeof draft.description === "string" ? draft.description : "",
      notes: sanitizeNotes(notes),
      extracted: false,
    };
  }
  return { title: applied.title, description: applied.description, notes: "", extracted: true };
}

export function canAskManvi(draft: {
  title?: unknown;
  description?: unknown;
  repository_ids?: unknown;
}, notes?: unknown): string | null {
  const repos = Array.isArray(draft.repository_ids)
    ? draft.repository_ids.filter((id) => typeof id === "string" && id.length > 0)
    : [];
  if (!repos.length) return "Link a repository before asking Manvi.";
  const next = applyNotesToDraft(
    {
      title: typeof draft.title === "string" ? draft.title : "",
      description: typeof draft.description === "string" ? draft.description : "",
    },
    notes,
  );
  if (!next.title.trim()) return "Add a title, or describe what you need, before asking Manvi.";
  if (new TextEncoder().encode(next.description).length > NOTES_CAP) return "Description and notes exceed 64 KB. Shorten them before saving or asking Manvi.";
  return null;
}

export interface DraftAgentCopy {
  title: string;
  description: string;
  kind?: string;
  status?: TaskStatus;
  priority?: number;
  owner?: string | null;
  labels?: readonly string[];
  acceptance_criteria?: readonly string[];
  repositoryNames?: readonly string[];
  logs?: string;
}

/**
 * The description as the agent will read it, with any cut announced.
 *
 * An unsaved draft can hold more than a saved one: `applyNotesToDraft` raises
 * its own cap on purpose so nothing the author typed is lost before the save
 * boundary reports it, while a saved brief's description is bounded at
 * `NOTES_CAP` by the store. So this is the one path where the copy can be
 * shorter than the draft it came from.
 *
 * Bounding the packet is right; bounding it silently is not. A copy that stops
 * mid-sentence with no marker reads to the agent as the whole description, and
 * it answers as though it had seen the rest. Same contract as the model
 * budgeters in `ai/prompt.rs`: cut, and say so in the text that was cut.
 */
function copyDescription(value: unknown): string {
  const text = sanitizeNotes(value, Number.MAX_SAFE_INTEGER);
  const characters = [...text];
  if (characters.length <= NOTES_CAP) return text;
  return `${characters.slice(0, NOTES_CAP).join("")}\n\n[description truncated: ${NOTES_CAP} of ${characters.length} characters shown]`;
}

export function formatDraftAgentCopy(draft: DraftAgentCopy): string | null {
  const title = displayTitle(draft.title, TITLE_CAP);
  const description = copyDescription(draft.description);
  if (title === "(untitled)" && !description) return null;
  const criteria = (draft.acceptance_criteria ?? []).map((item) => item.trim()).filter(Boolean);
  const labels = (draft.labels ?? []).map((item) => item.trim()).filter(Boolean);
  const repos = (draft.repositoryNames ?? []).map((item) => item.trim()).filter(Boolean);
  const status = draft.status && STATUS_LABELS[draft.status] ? STATUS_LABELS[draft.status] : "Unspecified";
  const priority = PRIORITY_LABELS[(draft.priority ?? 1) as 0 | 1 | 2 | 3] ?? "Unknown";
  const lines = [
    AGENT_COPY_PREAMBLE,
    "",
    "# Unsaved GitPulse task draft",
    "This has not been saved. Treat these fields as the author's current intent, not a stored revision.",
    "",
    "## Title",
    title,
    "",
    `Type: ${draft.kind?.trim() || "Unspecified"}`,
    `Status: ${status}`,
    `Priority: ${priority}`,
    `Owner: ${draft.owner?.trim() || "Unassigned"}`,
    `Labels: ${labels.length ? labels.join(", ") : "None"}`,
    "",
    "## Repositories",
    repos.length ? repos.map((name) => `- ${name}`).join("\n") : "None linked yet.",
    "",
    "## Description",
    description || "(none)",
    "",
    "## Acceptance criteria",
    criteria.length ? criteria.map((item) => `- [ ] ${item}`).join("\n") : "No acceptance criteria recorded.",
  ];
  const logs = formatLogsSection(draft.logs);
  if (logs) lines.push("", logs);
  return lines.join("\n");
}

export function wrapSavedBriefForAgent(markdown: unknown): string | null {
  if (typeof markdown !== "string") return null;
  const body = markdown.replace(/\u0000/g, "").trim();
  if (!body || body.length > 2 * 1024 * 1024) return null;
  return `${AGENT_COPY_PREAMBLE}\n\n${body}`;
}

export function suggestionDiffers(current: unknown, proposed: unknown): boolean {
  const a = typeof current === "string" ? current.trim() : "";
  const b = typeof proposed === "string" ? proposed.trim() : "";
  if (!b) return false;
  return a !== b;
}

export function joinAgentCopies(parts: readonly string[], limit = MAX_AGENT_COPY_TASKS): string | null {
  const cleaned = (Array.isArray(parts) ? parts : [])
    .map((part) => (typeof part === "string" ? part.trim() : ""))
    .filter(Boolean)
    .slice(0, Number.isSafeInteger(limit) && limit > 0 ? Math.min(limit, 50) : MAX_AGENT_COPY_TASKS);
  if (!cleaned.length) return null;
  return cleaned.join("\n\n---\n\n");
}
