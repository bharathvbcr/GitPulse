/**
 * Fold walk_incomplete (and similar qualification) strings so N per-seed
 * walks cannot dump N copies of the same corpus disclaimer.
 *
 * Kernel reasons embed per-walk counters ("9456 traversed edges unrecorded")
 * inside an otherwise identical essay. Exact-string dedupe therefore fails:
 * 25 available seeds produce ~10KB of repeated "repository-wide counts"
 * copy. Split on the kernel's joiners, fingerprint by stripping digits, and
 * merge numeric slots into a range over the full population — not a sample.
 *
 * Already-folded text (en-dash ranges, "(N seeds)") is normalized so a second
 * pass, or an incremental append, keeps merging instead of starting a new
 * essay.
 */

export const WALK_INCOMPLETE_MAX_CHARS = 720;
export const WALK_INCOMPLETE_TOOLTIP_CHARS = 480;

const CLAUSE_SPLIT = /\s*·\s*|; /;
const SEED_SUFFIX = /\s*\((\d+)\s+seeds\)\s*$/i;
const SIMILAR_SUFFIX = /\s*\((\d+)\s+similar\)\s*$/i;

type Slot = { min: number; max: number };

type ClauseGroup = {
  first: string;
  count: number;
  template: string;
  slots: Slot[];
  mergeable: boolean;
  identical: boolean;
};

export function summarizeWalkIncomplete(
  parts: Array<string | null | undefined>,
  maxChars = WALK_INCOMPLETE_MAX_CHARS,
): string | null {
  const clauses = collectClauses(parts);
  if (clauses.length === 0) return null;

  const groups = new Map<string, ClauseGroup>();
  const order: string[] = [];
  for (const clause of clauses) {
    const key = fingerprint(clause);
    const existing = groups.get(key);
    if (existing) {
      ingest(existing, clause);
    } else {
      groups.set(key, newGroup(clause));
      order.push(key);
    }
  }

  const merged = order.map((key) => emitGroup(groups.get(key)!));
  return boundJoined(merged, Math.max(1, maxChars));
}

/** Summarize, then clip to a native tooltip budget. */
export function tooltipWalkIncomplete(
  parts: Array<string | null | undefined>,
  maxChars = WALK_INCOMPLETE_TOOLTIP_CHARS,
): string | null {
  return summarizeWalkIncomplete(parts, maxChars);
}

/** Clip assembled title text (not a walk essay) to the tooltip budget. */
export function boundText(
  text: string,
  maxChars = WALK_INCOMPLETE_TOOLTIP_CHARS,
): string {
  return clipText(text, Math.max(1, maxChars));
}

/**
 * Join a list for a title attribute without dumping hundreds of paths.
 * Carries both the shown sample and the real total.
 */
export function boundedJoin(
  items: readonly string[],
  maxItems = 8,
  maxChars = WALK_INCOMPLETE_TOOLTIP_CHARS,
): string {
  const cleaned = items.map((item) => item.trim()).filter((item) => item.length > 0);
  if (cleaned.length === 0) return "";
  const shown = cleaned.slice(0, Math.max(1, maxItems));
  const omitted = cleaned.length - shown.length;
  // Put the real total first so a tight clip cannot drop it.
  const out =
    omitted > 0
      ? `${cleaned.length} total: ${shown.join(", ")}, …`
      : shown.join(", ");
  return clipText(out, Math.max(1, maxChars));
}

function collectClauses(parts: Array<string | null | undefined>): string[] {
  const clauses: string[] = [];
  for (const part of parts) {
    if (part == null) continue;
    const trimmed = part.trim();
    if (!trimmed) continue;
    for (const clause of trimmed.split(CLAUSE_SPLIT)) {
      const item = clause.trim();
      if (item && /[\p{L}\p{N}]/u.test(item)) clauses.push(item);
    }
  }
  return clauses;
}

function canonicalClause(clause: string): string {
  return clause.replace(SEED_SUFFIX, "").replace(SIMILAR_SUFFIX, "").trim();
}

function seedWeight(clause: string): number {
  const trimmed = clause.trim();
  const seeds = trimmed.match(SEED_SUFFIX);
  const similar = trimmed.match(SIMILAR_SUFFIX);
  const raw = seeds?.[1] ?? similar?.[1];
  if (raw == null) return 1;
  const value = Number(raw);
  return Number.isSafeInteger(value) && value > 0 ? value : 1;
}

function fingerprint(clause: string): string {
  return canonicalClause(clause)
    .replace(/\d+\u2013\d+/g, "#")
    .replace(/\d+/g, "#")
    .replace(/\s+/g, " ")
    .toLowerCase();
}

function slotTemplate(clause: string): string {
  return canonicalClause(clause)
    .replace(/\d+\u2013\d+/g, "\0")
    .replace(/\d+/g, "\0");
}

function slotPattern(): RegExp {
  return /(\d+)\u2013(\d+)|\d+/g;
}

function extractSlots(clause: string): Slot[] | null {
  const slots: Slot[] = [];
  for (const match of canonicalClause(clause).matchAll(slotPattern())) {
    if (match[1] != null && match[2] != null) {
      const a = Number(match[1]);
      const b = Number(match[2]);
      if (!Number.isSafeInteger(a) || !Number.isSafeInteger(b)) return null;
      slots.push({ min: Math.min(a, b), max: Math.max(a, b) });
    } else {
      const n = Number(match[0]);
      if (!Number.isSafeInteger(n)) return null;
      slots.push({ min: n, max: n });
    }
  }
  return slots;
}

function newGroup(clause: string): ClauseGroup {
  const slots = extractSlots(clause);
  return {
    first: clause,
    count: seedWeight(clause),
    template: slotTemplate(clause),
    slots: slots ?? [],
    mergeable: slots != null,
    identical: true,
  };
}

function ingest(group: ClauseGroup, clause: string): void {
  // Canonical-equal copies (verbatim repeats, or a prior "(N seeds)"
  // summary) are the same population — take the heavier weight, do not add.
  if (clause === group.first || canonicalClause(clause) === canonicalClause(group.first)) {
    group.count = Math.max(group.count, seedWeight(clause));
    return;
  }
  group.count += seedWeight(clause);
  group.identical = false;
  const slots = extractSlots(clause);
  if (
    !group.mergeable ||
    slots == null ||
    slotTemplate(clause) !== group.template ||
    slots.length !== group.slots.length
  ) {
    group.mergeable = false;
    return;
  }
  for (let i = 0; i < slots.length; i++) {
    group.slots[i] = {
      min: Math.min(group.slots[i]!.min, slots[i]!.min),
      max: Math.max(group.slots[i]!.max, slots[i]!.max),
    };
  }
}

function emitGroup(group: ClauseGroup): string {
  if (group.identical) {
    const weight = seedWeight(group.first);
    if (group.count > 1 && weight !== group.count) {
      return `${canonicalClause(group.first)} (${group.count} seeds)`;
    }
    return group.first;
  }
  const base = canonicalClause(group.first);
  if (!group.mergeable) {
    return group.count > 1 ? `${base} (${group.count} similar)` : base;
  }
  let index = 0;
  const merged = base.replace(slotPattern(), () => {
    const slot = group.slots[index]!;
    index += 1;
    return slot.min === slot.max ? String(slot.min) : `${slot.min}\u2013${slot.max}`;
  });
  return group.count > 1 ? `${merged} (${group.count} seeds)` : merged;
}

function boundJoined(parts: string[], maxChars: number): string {
  const full = parts.join("; ");
  if (codePointCount(full) <= maxChars) return full;

  const kept: string[] = [];
  for (let i = 0; i < parts.length; i++) {
    const omitted = parts.length - i - 1;
    const suffix = omitted > 0 ? `; ${omitted} more distinct qualification(s) omitted` : "";
    const candidate = kept.length === 0 ? parts[i]! : `${kept.join("; ")}; ${parts[i]}`;
    if (codePointCount(candidate) + codePointCount(suffix) <= maxChars) {
      kept.push(parts[i]!);
      continue;
    }
    if (kept.length === 0) {
      const room = Math.max(1, maxChars - codePointCount(suffix));
      return clipText(clipText(parts[i]!, room) + suffix, maxChars);
    }
    return clipText(
      `${kept.join("; ")}; ${parts.length - i} more distinct qualification(s) omitted`,
      maxChars,
    );
  }
  return clipText(kept.join("; "), maxChars);
}

function codePointCount(text: string): number {
  return Array.from(text).length;
}

function clipText(text: string, maxChars: number): string {
  const chars = Array.from(text);
  if (chars.length <= maxChars) return text;
  if (maxChars <= 1) return "…";
  return `${chars.slice(0, maxChars - 1).join("")}…`;
}
