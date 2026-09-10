import { PRIORITY_LABELS } from "./boardDrag";
import { STATUS_LABELS, type TaskDraft, type TaskStatus } from "./client";
import { displayTitle } from "./taskDelete";

export const MAX_AGENT_COPY_TASKS = 8;
export const AGENT_COPY_PREAMBLE =
  "GitPulse task for an AI agent. Use the title as the goal, the description as context, and the acceptance criteria as the definition of done. Do not invent repositories or skip criteria.";

const TITLE_CAP = 300;
const NOTES_CAP = 65_536;

export function sanitizeNotes(value: unknown, cap = NOTES_CAP): string {
  if (typeof value !== "string") return "";
  const cleaned = value.replace(/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/g, "").trim();
  if (!cleaned) return "";
  const limit = Number.isSafeInteger(cap) && cap >= 32 ? cap : NOTES_CAP;
  return [...cleaned].slice(0, limit).join("");
}

/** First line or sentence of notes, capped for `items.put`. */
export function titleFromNotes(notes: unknown, cap = TITLE_CAP): string {
  const limit = Number.isSafeInteger(cap) && cap >= 16 ? cap : TITLE_CAP;
  const text = sanitizeNotes(notes, NOTES_CAP);
  if (!text) return "";
  const line = (text.split(/\n/)[0] ?? text).trim();
  const sentence = (line.split(/(?<=[.!?])\s+/)[0] ?? line).trim();
  const source = sentence || line || text;
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
}

export function formatDraftAgentCopy(draft: DraftAgentCopy): string | null {
  const title = displayTitle(draft.title, TITLE_CAP);
  const description = sanitizeNotes(draft.description);
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
