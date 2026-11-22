/**
 * One owner of an open file's state.
 *
 * The Code page and a terminal file split both show files, and both used to
 * load them, keep drafts for them and save them on their own. A file open in
 * both held two drafts that could diverge and be saved over each other. A
 * document is keyed by (checkout root, path); it holds the text last read from
 * disk and at most one draft, and every host reads and writes through here.
 * The hosts keep only what is theirs: which file is on screen, tabs, layout.
 *
 * Lifetime: a document exists while some host holds it or it carries a draft.
 * A draft is process-lifetime on purpose (it survives a view remount, never a
 * quit: `editorTabPersist` keeps only which files were open); a document with
 * neither a holder nor a draft is dropped, so closing a file frees its text.
 *
 * The save lifecycle is the one `editorFileSaveQueue` already serialises per
 * (checkout, path): the host call, the draft's completion, and the conflict a
 * refusal raises all happen here, once.
 */

import { writable, type Readable } from "svelte/store";
import { invoke } from "../ipc/invoke";
import { formatError } from "../ui/formatError";
import type { Guarded } from "../stores/harnessStore";
import { recordEditorDrafts } from "./editorDraftRegistry";
import { readFileWriteError, saveBase, UnbasedDraft } from "./fileWrite";
import { editorFileSaveQueue, fileSaveKey } from "./serialSave";
import type { FileBlob, SavedFile } from "./types";

/** Unsaved editor content for one document. */
export interface DocumentDraft {
  content: string;
  /**
   * File content against which this draft first became dirty. Kept for the
   * life of the draft: a file that changes on disk afterwards must not
   * quietly become the draft's new baseline.
   */
  sourceContent: string;
  /**
   * The disk version (`FileBlob.disk_base`) this draft was made from, sent
   * with the save so the backend refuses it when the file has moved on. Null
   * when the blob carried none: such a draft is refused on save (the editor
   * shows the conflict banner) and is written only by an explicit, confirmed
   * Overwrite.
   */
  base: string | null;
}

/** The disk version a refused save saw; null when the draft had none to judge by. */
export interface DocumentConflict {
  actual: string | null;
}

export interface OpenDocument {
  readonly repo: string;
  /** Normalized (see `normalizeDocumentPath`). */
  readonly path: string;
  /** What was last read from, or written to, disk. Null until a read lands, and after a failed one. */
  readonly blob: FileBlob | null;
  readonly loading: boolean;
  readonly error: string | null;
  readonly draft: DocumentDraft | null;
  /** Set by a refused save; belongs to the draft it refused and goes with it. */
  readonly refusal: DocumentConflict | null;
}

export type DocumentLoad =
  | { status: "loaded"; blob: FileBlob }
  | { status: "failed"; error: string }
  /** A newer read, or the document's unloading, took over before this one landed. */
  | { status: "superseded" };

export interface SaveOptions {
  /**
   * Undefined saves against the draft's own base (or, with no draft, the blob
   * on hand); a string saves against that version, which is how Overwrite
   * names the one it already saw; null is an unconditional write.
   */
  overrideBase?: string | null;
  /** The user's choice to keep the buffer: byte-exact even over a file the editor could not round-trip. */
  overwrite?: boolean;
  /** Runs inside the save queue just before the write; throwing refuses the save. */
  beforeWrite?: () => void;
  /** Runs inside the save queue once the draft is completed, so it is ordered with the write. */
  afterSave?: () => Promise<void>;
}

/**
 * Only what is provably the same file: empty and `.` segments go ("./a//b" is
 * "a/b"). `..` is left alone (resolving it needs the tree) and nothing is
 * case-folded, so two names the host might treat as different stay two.
 */
export function normalizeDocumentPath(path: string): string {
  return path
    .split("/")
    .filter((segment) => segment !== "" && segment !== ".")
    .join("/");
}

/** The key a document is stored under; the save queue's own key for it. */
export function documentKey(repo: string, path: string): string {
  return fileSaveKey(repo, normalizeDocumentPath(path));
}

const state = writable<ReadonlyMap<string, OpenDocument>>(new Map());
let documents: ReadonlyMap<string, OpenDocument> = new Map();
const holders = new Map<string, Set<string>>();
/** The read whose answer a document will take; an older one is stale. */
const latestRead = new Map<string, number>();
const inflight = new Map<string, Promise<DocumentLoad>>();
let readTokens = 0;

/** Every open document, replaced (never mutated) on each change. */
export const openDocuments: Readable<ReadonlyMap<string, OpenDocument>> = { subscribe: state.subscribe };

export function documentIn(
  map: ReadonlyMap<string, OpenDocument>,
  repo: string | null | undefined,
  path: string | null | undefined,
): OpenDocument | undefined {
  return repo && path ? map.get(documentKey(repo, path)) : undefined;
}

export function getDocument(repo: string, path: string): OpenDocument | undefined {
  return documents.get(documentKey(repo, path));
}

function commit(next: Map<string, OpenDocument>): void {
  documents = next;
  state.set(next);
}

/** The app-close guard reads one owner for editor drafts: this module. */
function publishDrafts(repo: string): void {
  const paths: string[] = [];
  for (const doc of documents.values()) {
    if (doc.repo === repo && doc.draft) paths.push(doc.path);
  }
  recordEditorDrafts(repo, paths, "documents");
}

function emptyDocument(repo: string, path: string): OpenDocument {
  return { repo, path, blob: null, loading: false, error: null, draft: null, refusal: null };
}

/** Creates the document if need be, then applies `change`. */
function patch(repo: string, path: string, change: (doc: OpenDocument) => OpenDocument): void {
  const key = documentKey(repo, path);
  const doc = documents.get(key) ?? emptyDocument(repo, normalizeDocumentPath(path));
  const next = change(doc);
  if (next === doc && documents.has(key)) return;
  commit(new Map(documents).set(key, next));
  if (next.draft !== doc.draft) publishDrafts(repo);
}

/** Drops a document nothing shows and nothing owes work to. */
function settle(repo: string, path: string): void {
  const key = documentKey(repo, path);
  const doc = documents.get(key);
  if (!doc || doc.draft || (holders.get(key)?.size ?? 0) > 0) return;
  const next = new Map(documents);
  next.delete(key);
  latestRead.delete(key);
  inflight.delete(key);
  commit(next);
}

/**
 * `holder` shows this document. Returns the release. One hold per holder per
 * document: a second hold under the same name is the same hold.
 */
export function holdDocument(repo: string, path: string, holder: string): () => void {
  const key = documentKey(repo, path);
  const set = holders.get(key) ?? new Set<string>();
  set.add(holder);
  holders.set(key, set);
  patch(repo, path, (doc) => doc);
  let released = false;
  return () => {
    if (released) return;
    released = true;
    const current = holders.get(key);
    current?.delete(holder);
    if (current && current.size === 0) holders.delete(key);
    settle(repo, path);
  };
}

/** True when a holder other than `holder` shows this document. */
export function isHeldElsewhere(repo: string, path: string, holder: string): boolean {
  for (const other of holders.get(documentKey(repo, path)) ?? []) {
    if (other !== holder) return true;
  }
  return false;
}

/**
 * Reads the file from disk into its document. Unforced, a loaded document is
 * answered as it stands and a read already in flight is joined. Forced, a new
 * read starts and the old blob stays on screen until it lands, so a viewer
 * keeps its scroll and selection; `remount` blanks it first for a reload that
 * must close the editor on the disk version.
 */
export function loadDocument(
  repo: string,
  path: string,
  options: { force?: boolean; remount?: boolean } = {},
): Promise<DocumentLoad> {
  const target = normalizeDocumentPath(path);
  const key = documentKey(repo, target);
  if (!options.force) {
    const doc = documents.get(key);
    if (doc?.blob && !doc.loading) return Promise.resolve({ status: "loaded", blob: doc.blob });
    const pending = inflight.get(key);
    if (pending) return pending;
  }
  const token = ++readTokens;
  latestRead.set(key, token);
  patch(repo, target, (doc) => ({
    ...doc,
    loading: true,
    error: null,
    blob: options.remount ? null : doc.blob,
  }));
  const read: Promise<DocumentLoad> = (async (): Promise<DocumentLoad> => {
    try {
      const blob = await invoke<FileBlob>("cmd_get_file_blob", {
        repoPath: repo,
        filePath: target,
        commitId: null,
      });
      if (latestRead.get(key) !== token) return { status: "superseded" };
      patch(repo, target, (doc) => ({ ...doc, blob, loading: false, error: null }));
      return { status: "loaded", blob };
    } catch (err: unknown) {
      if (latestRead.get(key) !== token) return { status: "superseded" };
      const error = formatError(err);
      patch(repo, target, (doc) => ({ ...doc, blob: null, loading: false, error }));
      return { status: "failed", error };
    } finally {
      if (inflight.get(key) === read) inflight.delete(key);
    }
  })();
  inflight.set(key, read);
  return read;
}

/**
 * The editor's latest input. Only the first dirtying keystroke fixes the
 * baseline and the base: later ones arrive carrying whatever blob is loaded
 * now, which after an external change is not the version this draft was made
 * from. Input that equals the baseline is not a draft.
 */
export function editDocument(repo: string, path: string, content: string, sourceContent: string): void {
  patch(repo, path, (doc) => {
    const existing = doc.draft;
    const source = existing ? existing.sourceContent : sourceContent;
    if (content === source) {
      return doc.draft === null && doc.refusal === null ? doc : { ...doc, draft: null, refusal: null };
    }
    // A refusal belongs to the draft it refused; a new draft starts clean.
    return {
      ...doc,
      draft: { content, sourceContent: source, base: existing ? existing.base : doc.blob?.disk_base ?? null },
      refusal: existing ? doc.refusal : null,
    };
  });
  settle(repo, path);
}

/** Called only after the user explicitly confirms discarding this draft. */
export function discardDraft(repo: string, path: string): void {
  patch(repo, path, (doc) => (doc.draft === null && doc.refusal === null ? doc : { ...doc, draft: null, refusal: null }));
  settle(repo, path);
}

export function hasDraft(repo: string, path: string): boolean {
  return getDocument(repo, path)?.draft != null;
}

/** Every path in `repo` that carries a draft. */
export function draftPaths(repo: string): string[] {
  const paths: string[] = [];
  for (const doc of documents.values()) {
    if (doc.repo === repo && doc.draft) paths.push(doc.path);
  }
  return paths.sort();
}

/**
 * Set while the draft was made from a version that is no longer on disk: a
 * save was refused, or the blob that reloaded carries a different base than
 * the draft. `actual` is the version Overwrite saves over. Tokens are compared,
 * never text, so a CRLF flip or a same-size rewrite counts and a touch does not.
 */
export function documentConflict(doc: OpenDocument | undefined): DocumentConflict | null {
  if (!doc?.draft) return null;
  if (doc.refusal) return doc.refusal;
  const diskBase = doc.blob?.disk_base;
  return doc.draft.base !== null && diskBase != null && diskBase !== doc.draft.base
    ? { actual: diskBase }
    : null;
}

/**
 * A successful write is the only thing allowed to clear dirty state. Input can
 * race a slow write, so a draft that has moved past what was saved stays
 * dirty; it is then re-based onto the saved text and version, because the disk
 * now holds what it saved and the next save is judged against that.
 */
function completeSave(repo: string, path: string, savedContent: string, savedBase: string): void {
  patch(repo, path, (doc) => {
    const draft = doc.draft;
    const kept = draft && draft.content !== savedContent
      ? { ...draft, sourceContent: savedContent, base: savedBase }
      : null;
    // The blob now is what was written, so its base is the saved one: left
    // stale it would read as an external change to the next keystroke.
    const blob = doc.blob ? { ...doc.blob, text: savedContent, disk_base: savedBase } : null;
    return { ...doc, draft: kept, blob, refusal: null };
  });
  settle(repo, path);
}

/**
 * Saves `content` through the per-file queue. A `changed_on_disk` refusal
 * keeps the draft, records the disk version it saw (the Overwrite base) and
 * reads the file again so a comparison shows what is there now; a draft with
 * no recorded version is refused the same way, with nothing to name. The
 * error is rethrown either way so an editor stays in edit mode.
 */
export async function saveDocument(
  repo: string,
  path: string,
  content: string,
  options: SaveOptions = {},
): Promise<void> {
  const target = normalizeDocumentPath(path);
  if (!repo || !target) throw new Error("No active file to save");
  try {
    await editorFileSaveQueue.run(fileSaveKey(repo, target), async () => {
      options.beforeWrite?.();
      const doc = documents.get(documentKey(repo, target));
      const judged = saveBase(doc?.draft ?? undefined, doc?.blob?.disk_base ?? null, options.overrideBase);
      if (judged === "unbased") throw new UnbasedDraft(target);
      const saved = await invoke<Guarded<SavedFile>>("cmd_write_file_content", {
        repoPath: repo,
        filePath: target,
        content,
        expectedBase: judged.base,
        // Sent only for Overwrite, so an ordinary save's payload is unchanged.
        ...(options.overwrite ? { overwrite: true } : {}),
      });
      completeSave(repo, target, content, saved.output.disk_base);
      await options.afterSave?.();
    });
  } catch (err: unknown) {
    const typed = readFileWriteError(err);
    if (typed?.code === "changed_on_disk") {
      refuse(repo, target, typed.actual);
      void loadDocument(repo, target, { force: true });
    } else if (err instanceof UnbasedDraft) {
      refuse(repo, target, null);
    }
    throw err;
  }
}

function refuse(repo: string, path: string, actual: string | null): void {
  patch(repo, path, (doc) => (doc.draft ? { ...doc, refusal: { actual } } : doc));
}

/** Test isolation for the intentionally process-lifetime store. */
export function clearOpenDocumentsForTests(): void {
  for (const doc of documents.values()) recordEditorDrafts(doc.repo, [], "documents");
  holders.clear();
  latestRead.clear();
  inflight.clear();
  commit(new Map());
}
