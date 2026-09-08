import { recordEditorDrafts } from "../files/editorDraftRegistry";
import type { ConflictDocument, ConflictResolutionChoice } from "./conflict";

export interface ConflictStage {
  stage: number;
  mode: string;
  oid: string;
  size: number;
  text: string | null;
}
export interface ConflictSnapshot {
  file_path: string;
  revision: string;
  operation: string;
  document: ConflictDocument | null;
  stages: ConflictStage[];
  reason: string | null;
  worktree_mode: string;
}
export type WholeFileChoice = "Ours" | "Theirs" | "WorkingTree";
export type ConflictFileChoice = { Chunks: ConflictResolutionChoice[] } | WholeFileChoice | "StageOnly";
export interface ConflictSaveRequest {
  file_path: string;
  revision: string;
  choice: ConflictFileChoice;
}
export interface ConflictSaveOutcome {
  written: boolean;
  staged: boolean;
  message: string;
  recovery_path: string | null;
  snapshot: ConflictSnapshot | null;
}
export interface ResolutionState {
  choices: ConflictResolutionChoice[];
  custom: Record<string, string>;
  whole: WholeFileChoice | null;
}
export interface RecoveredResolution {
  snapshot: ConflictSnapshot;
  state: ResolutionState;
}
export interface ConflictDraft extends RecoveredResolution {
  repo: string;
  undo: ResolutionState[];
  redo: ResolutionState[];
  recovery: RecoveredResolution[];
  pending: ConflictSnapshot | null;
  group: string | null;
  editedAt: number;
}

const STORAGE_KEY = "gitpulse.conflict-drafts.v1";
const MAX_STORAGE_CHARS = 2 * 1024 * 1024;
const MAX_HISTORY_CHARS = 1024 * 1024;
const MAX_RECORDS = 64;
const MAX_MEMORY_CHARS = 32 * 1024 * 1024;
const keyFor = (repo: string, file: string) => JSON.stringify([repo, file]);
const copy = <T>(value: T): T => structuredClone(value);
export const initialResolution = (snapshot: ConflictSnapshot): ResolutionState => ({
  choices: snapshot.document?.segments.flatMap(segment => segment.Conflict ? [segment.Conflict.resolution] : []) ?? [],
  custom: {}, whole: null,
});
export const hasResolution = (draft: RecoveredResolution): boolean => draft.state.whole !== null || draft.state.choices.some(choice => choice !== "Unresolved") || Object.keys(draft.state.custom).length > 0;

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}
function isChoice(value: unknown): value is ConflictResolutionChoice {
  return typeof value === "string"
    ? ["Unresolved", "AcceptOurs", "AcceptTheirs", "AcceptBothOursFirst", "AcceptBothTheirsFirst"].includes(value)
    : isRecord(value) && typeof value.Custom === "string";
}
function isState(value: unknown): value is ResolutionState {
  if (!isRecord(value) || !Array.isArray(value.choices)) return false;
  const choices = value.choices;
  return choices.length <= 2000 && choices.every(isChoice)
    && choices.every(choice => typeof choice === "string" || choice.Custom.length <= 1024 * 1024)
    && isRecord(value.custom) && Object.keys(value.custom).length <= choices.length
    && Object.entries(value.custom).every(([key, text]) => /^(0|[1-9]\d*)$/.test(key) && Number(key) < choices.length && typeof text === "string" && text.length <= 1024 * 1024)
    && (value.whole === null || ["Ours", "Theirs", "WorkingTree"].includes(String(value.whole)));
}
function isDocument(value: unknown): value is ConflictDocument {
  if (!isRecord(value) || typeof value.file_path !== "string" || !Array.isArray(value.segments) || value.segments.length > 100_000
    || !Number.isInteger(value.total_conflicts) || typeof value.total_conflicts !== "number" || value.total_conflicts > 2000
    || !Number.isInteger(value.marker_size) || typeof value.marker_size !== "number" || value.marker_size < 1 || value.marker_size > 256
    || typeof value.crlf !== "boolean" || typeof value.trailing_newline !== "boolean" || typeof value.final_crlf !== "boolean"
    || !Array.isArray(value.diagnostics) || !value.diagnostics.every(item => typeof item === "string")
    || !Array.isArray(value.normal_crlf_flags) || !value.normal_crlf_flags.every(row => Array.isArray(row) && row.every(item => typeof item === "boolean"))) return false;
  let chunks = 0;
  for (const segment of value.segments) {
    if (!isRecord(segment)) return false;
    if (typeof segment.Normal === "string" && segment.Conflict === undefined) continue;
    const chunk = segment.Conflict;
    if (!isRecord(chunk) || segment.Normal !== undefined || chunk.chunk_index !== chunks++ || !isChoice(chunk.resolution)
      || typeof chunk.ours_content !== "string" || typeof chunk.theirs_content !== "string" || typeof chunk.ours_label !== "string" || typeof chunk.theirs_label !== "string"
      || !(chunk.base_content === null || chunk.base_content === undefined || typeof chunk.base_content === "string")
      || !Number.isInteger(chunk.start_line) || !Number.isInteger(chunk.end_line) || typeof chunk.local_crlf !== "boolean"
      || !Array.isArray(chunk.ours_crlf) || !chunk.ours_crlf.every(flag => typeof flag === "boolean")
      || !Array.isArray(chunk.theirs_crlf) || !chunk.theirs_crlf.every(flag => typeof flag === "boolean")
      || !(chunk.base_crlf === null || chunk.base_crlf === undefined || (Array.isArray(chunk.base_crlf) && chunk.base_crlf.every(flag => typeof flag === "boolean")))) return false;
  }
  return chunks === value.total_conflicts;
}
function isSnapshot(value: unknown): value is ConflictSnapshot {
  return isRecord(value) && typeof value.file_path === "string" && typeof value.revision === "string" && typeof value.operation === "string"
    && (value.document === null || (isDocument(value.document) && value.document.file_path === value.file_path))
    && typeof value.worktree_mode === "string" && (value.reason === null || typeof value.reason === "string")
    && Array.isArray(value.stages) && value.stages.length <= 3 && value.stages.every(stage => isRecord(stage)
      && Number.isInteger(stage.stage) && typeof stage.mode === "string" && typeof stage.oid === "string" && typeof stage.size === "number" && Number.isFinite(stage.size)
      && (stage.text === null || typeof stage.text === "string"));
}
function isRecovered(value: unknown): value is RecoveredResolution {
  return isRecord(value) && isSnapshot(value.snapshot) && isState(value.state)
    && value.state.choices.length === (value.snapshot.document?.total_conflicts ?? 0);
}
function isDraft(value: unknown): value is ConflictDraft {
  return isRecord(value) && isRecovered(value) && typeof value.repo === "string" && !value.repo.includes("\0")
    && Array.isArray(value.undo) && value.undo.length <= 50 && value.undo.every(state => isState(state) && state.choices.length === value.state.choices.length)
    && Array.isArray(value.redo) && value.redo.length <= 50 && value.redo.every(state => isState(state) && state.choices.length === value.state.choices.length)
    && Array.isArray(value.recovery) && value.recovery.length <= 8 && value.recovery.every(isRecovered)
    && (value.pending === null || (isSnapshot(value.pending) && value.pending.file_path === value.snapshot.file_path)) && (value.group === null || typeof value.group === "string")
    && typeof value.editedAt === "number" && Number.isFinite(value.editedAt);
}

// Account for strings and collection entries without serializing every retained
// source on each keystroke. The bounded history and native document limits also
// apply; this cap protects the aggregate across files and recovered versions.
function stateSize(state: ResolutionState): number {
  return state.choices.reduce((size, choice) => size + (typeof choice === "string" ? 32 : choice.Custom.length), 0)
    + Object.values(state.custom).reduce((size, text) => size + text.length + 32, 0);
}
function snapshotSize(snapshot: ConflictSnapshot): number {
  const doc = snapshot.document;
  return snapshot.file_path.length + snapshot.revision.length + snapshot.operation.length + (snapshot.reason?.length ?? 0)
    + snapshot.stages.reduce((size, stage) => size + (stage.text?.length ?? 0) + 128, 0)
    + (doc ? doc.normal_crlf_flags.reduce((size, flags) => size + flags.length + 16, 0)
      + doc.segments.reduce((size, segment) => {
        const chunk = segment.Conflict;
        return size + (chunk ? chunk.ours_content.length + chunk.theirs_content.length + (chunk.base_content?.length ?? 0) + chunk.ours_label.length + chunk.theirs_label.length
          + chunk.ours_crlf.length + chunk.theirs_crlf.length + (chunk.base_crlf?.length ?? 0) + 256 : (segment.Normal?.length ?? 0) + 32);
      }, 0) : 0);
}
function draftSize(draft: ConflictDraft): number {
  return snapshotSize(draft.snapshot) + stateSize(draft.state) + [...draft.undo, ...draft.redo].reduce((size, state) => size + stateSize(state), 0)
    + draft.recovery.reduce((size, recovery) => size + snapshotSize(recovery.snapshot) + stateSize(recovery.state), 0) + (draft.pending ? snapshotSize(draft.pending) : 0);
}

export function materializeResolution(draft: RecoveredResolution): ConflictDocument | null {
  const doc = copy(draft.snapshot.document);
  if (doc) for (const segment of doc.segments) {
    if (segment.Conflict) segment.Conflict.resolution = copy(draft.state.choices[segment.Conflict.chunk_index] ?? "Unresolved");
  }
  return doc;
}

export function createConflictSessions(storage?: Pick<Storage, "getItem" | "setItem" | "removeItem">) {
  const records = new Map<string, ConflictDraft>();
  let warning: string | null = storage ? null : "Recovery storage is unavailable. Drafts remain in memory; copy them before closing this window.";
  const listeners = new Set<(warning: string | null) => void>();
  function setWarning(value: string | null) { warning = value; for (const listener of listeners) listener(warning); }
  function storeDraft(key: string, draft: ConflictDraft) {
    let size = draftSize(draft);
    for (const [otherKey, other] of records) if (otherKey !== key) size += draftSize(other);
    if (size > MAX_MEMORY_CHARS) throw new Error("Conflict recovery memory limit reached. Copy and discard older drafts before adding more content.");
    records.set(key, draft);
  }
  let timer: ReturnType<typeof setTimeout> | undefined;
  function syncRegistry(repo: string) {
    recordEditorDrafts(repo, [...records.values()].filter(draft => draft.repo === repo && (hasResolution(draft) || draft.recovery.length > 0 || draft.pending)).map(draft => draft.snapshot.file_path), "conflicts");
  }
  function flush() {
    clearTimeout(timer); timer = undefined;
    if (!storage) return;
    const dirty = [...records.values()].filter(draft => hasResolution(draft) || draft.recovery.length > 0 || draft.pending);
    const payload = JSON.stringify({ version: 1, drafts: dirty });
    if (payload.length > MAX_STORAGE_CHARS) { setWarning("Recovery storage limit reached. Drafts remain in memory; copy them before closing this window."); return; }
    try { storage.setItem(STORAGE_KEY, payload); setWarning(null); }
    catch { setWarning("Recovery storage is unavailable. Drafts remain in memory; copy them before closing this window."); }
  }
  function changed(repo: string) {
    syncRegistry(repo);
    clearTimeout(timer);
    timer = setTimeout(flush, 200);
  }
  if (storage) {
    try {
      const raw = storage.getItem(STORAGE_KEY);
      if (raw) {
        if (raw.length > MAX_STORAGE_CHARS) throw new Error("Recovery size exceeds limit");
        const parsed: unknown = JSON.parse(raw);
        if (!isRecord(parsed) || parsed.version !== 1 || !Array.isArray(parsed.drafts) || parsed.drafts.length > MAX_RECORDS || !parsed.drafts.every(isDraft)) throw new Error("Invalid recovery payload");
        if (new Set(parsed.drafts.map(draft => keyFor(draft.repo, draft.snapshot.file_path))).size !== parsed.drafts.length) throw new Error("Duplicate recovery identity");
        for (const draft of parsed.drafts) records.set(keyFor(draft.repo, draft.snapshot.file_path), draft);
        for (const repo of new Set([...records.values()].map(draft => draft.repo))) syncRegistry(repo);
      }
    } catch { warning = "Stored conflict recovery could not be read. It was left untouched; new drafts are kept in memory."; storage = undefined; }
  }
  return {
    get warning() { return warning; },
    subscribeWarning(listener: (warning: string | null) => void) { listeners.add(listener); listener(warning); return () => { listeners.delete(listener); }; },
    flush,
    get(repo: string, file: string) { const value = records.get(keyFor(repo, file)); return value ? copy(value) : null; },
    list(repo: string) { return [...records.values()].filter(value => value.repo === repo).map(copy); },
    summaries(repo: string) { return [...records.values()].filter(value => value.repo === repo).map(draft => ({
      file: draft.snapshot.file_path, total: draft.state.choices.length,
      resolved: draft.state.choices.filter(choice => choice !== "Unresolved").length,
      whole: draft.state.whole, pending: Boolean(draft.pending), recoveryCount: draft.recovery.length,
    })); },
    open(repo: string, snapshot: ConflictSnapshot): ConflictDraft {
      const key = keyFor(repo, snapshot.file_path);
      const previous = records.get(key);
      if (previous && (previous.snapshot.revision === snapshot.revision || previous.pending?.revision === snapshot.revision)) return copy(previous);
      if (!previous && records.size >= MAX_RECORDS) {
        for (const [key, draft] of records) if (!hasResolution(draft) && !draft.recovery.length && !draft.pending) records.delete(key);
        if (records.size >= MAX_RECORDS) throw new Error("64 conflict files have retained drafts. Save or discard a retained draft before opening another file.");
      }
      const recovery = previous ? [...previous.recovery] : [];
      if (previous && hasResolution(previous)) {
        if (recovery.length >= 8) throw new Error("This file has 8 retained source versions. Copy or discard the older recovery before loading another version.");
        recovery.push(copy({ snapshot: previous.snapshot, state: previous.state }));
      }
      const draft: ConflictDraft = { repo, snapshot: copy(snapshot), state: initialResolution(snapshot), undo: [], redo: [], recovery, pending: null, group: null, editedAt: 0 };
      storeDraft(key, draft); changed(repo); return copy(draft);
    },
    edit(repo: string, file: string, state: ResolutionState, group: string | null = null): ConflictDraft {
      const draft = records.get(keyFor(repo, file));
      if (!draft) throw new Error("Load this conflict before editing");
      if (!isState(state) || state.choices.length !== draft.state.choices.length || stateSize(state) > 4 * 1024 * 1024) throw new Error("Replacement state exceeds the conflict editor limit or does not match this source");
      if (JSON.stringify(draft.state) === JSON.stringify(state)) return copy(draft);
      const undo = [...draft.undo];
      if (!(group && draft.group === group && Date.now() - draft.editedAt < 750)) undo.push(draft.state);
      while (undo.length > 50 || JSON.stringify(undo).length > MAX_HISTORY_CHARS) undo.shift();
      const next = { ...draft, undo, state: copy(state), redo: [], group, editedAt: Date.now(), pending: null };
      storeDraft(keyFor(repo, file), next); changed(repo); return copy(next);
    },
    travel(repo: string, file: string, direction: "undo" | "redo"): ConflictDraft | null {
      const draft = records.get(keyFor(repo, file)); if (!draft) return null;
      const source = draft[direction]; const next = source.pop(); if (!next) return copy(draft);
      draft[direction === "undo" ? "redo" : "undo"].push(copy(draft.state));
      draft.state = next; draft.group = null; draft.pending = null; changed(repo); return copy(draft);
    },
    pending(repo: string, file: string, snapshot: ConflictSnapshot, revision?: string, savedState?: ResolutionState) {
      const draft = records.get(keyFor(repo, file)); if (!draft) return;
      if (revision && draft.snapshot.revision !== revision && draft.pending?.revision !== revision) return;
      if (savedState && JSON.stringify(draft.state) !== JSON.stringify(savedState)) return;
      if (snapshot.file_path !== file) throw new Error("Staging receipt belongs to a different file");
      storeDraft(keyFor(repo, file), { ...draft, pending: copy(snapshot) }); changed(repo); flush();
    },
    complete(repo: string, file: string, revision: string, savedState?: ResolutionState) {
      const key = keyFor(repo, file); const draft = records.get(key);
      if (draft && savedState && JSON.stringify(draft.state) !== JSON.stringify(savedState)) return;
      if (draft && (draft.snapshot.revision === revision || draft.pending?.revision === revision)) {
        if (draft.recovery.length) { draft.state = initialResolution(draft.snapshot); draft.pending = null; draft.undo = []; draft.redo = []; }
        else records.delete(key);
        changed(repo); flush();
      }
    },
    discard(repo: string, file: string) { records.delete(keyFor(repo, file)); changed(repo); flush(); },
    discardRecovery(repo: string, file: string) {
      const draft = records.get(keyFor(repo, file)); if (!draft) return;
      draft.recovery = []; changed(repo); flush();
    },
    clear() { const repos = new Set([...records.values()].map(draft => draft.repo)); records.clear(); for (const repo of repos) syncRegistry(repo); flush(); },
  };
}

function recoveryStorage(): Storage | undefined {
  try { return typeof window === "undefined" ? undefined : window.localStorage; } catch { return undefined; }
}
export const conflictSessions = createConflictSessions(recoveryStorage());
if (typeof window !== "undefined") window.addEventListener("pagehide", () => conflictSessions.flush());
