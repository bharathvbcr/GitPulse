import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

vi.mock("../ipc/invoke", () => ({ invoke: vi.fn() }));

import { invoke } from "../ipc/invoke";
import { clearEditorDraftRegistryForTests, unsavedEditorDrafts } from "./editorDraftRegistry";
import { UnbasedDraft } from "./fileWrite";
import {
  claimDocuments,
  clearOpenDocumentsForTests,
  discardDraft,
  documentConflict,
  documentIn,
  documentKey,
  draftPaths,
  editDocument,
  getDocument,
  holdDocument,
  isHeldElsewhere,
  loadDocument,
  normalizeDocumentPath,
  openDocuments,
  saveDocument,
} from "./openDocuments";
import { editorFileSaveQueue } from "./serialSave";
import type { FileBlob } from "./types";

const invokeMock = vi.mocked(invoke);
const REPO = "/work/repo";

interface Disk {
  text: string;
  base: string;
}

/** A host that answers reads and writes from per-checkout, per-path files. */
function fakeHost(files: Record<string, Disk>) {
  const writes: Record<string, unknown>[] = [];
  const reads: string[] = [];
  let writeGate: Promise<void> | null = null;
  let readGate: Promise<void> | null = null;
  let nextBase = 1;
  invokeMock.mockImplementation((async (cmd: string, args: Record<string, unknown>) => {
    const id = `${args.repoPath}:${args.filePath}`;
    if (cmd === "cmd_get_file_blob") {
      reads.push(id);
      if (readGate) await readGate;
      const disk = files[id];
      if (!disk) throw new Error(`no such file ${id}`);
      const blob: FileBlob = {
        path: String(args.filePath),
        is_binary: false,
        is_image: false,
        mime: "text/plain",
        text: disk.text,
        invalid_utf8_bytes: 0,
        eol: "lf",
        disk_base: disk.base,
      };
      return blob;
    }
    if (cmd === "cmd_write_file_content") {
      writes.push(args);
      if (writeGate) await writeGate;
      const disk = files[id];
      if (args.expectedBase !== null && disk && args.expectedBase !== disk.base) {
        throw {
          code: "changed_on_disk",
          message: "changed on disk",
          path: args.filePath,
          expected: String(args.expectedBase),
          actual: disk.base,
        };
      }
      const base = `w${nextBase++}`;
      files[id] = { text: String(args.content), base };
      return { policy: null, output: { disk_base: base } };
    }
    throw new Error(`unexpected command ${cmd}`);
  }) as unknown as typeof invoke);
  return {
    writes,
    reads,
    gateWrites() {
      let open!: () => void;
      writeGate = new Promise<void>((resolve) => (open = resolve));
      return () => {
        writeGate = null;
        open();
      };
    },
    gateReads() {
      let open!: () => void;
      readGate = new Promise<void>((resolve) => (open = resolve));
      return () => {
        readGate = null;
        open();
      };
    },
  };
}

const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

beforeEach(() => {
  invokeMock.mockReset();
  clearOpenDocumentsForTests();
  clearEditorDraftRegistryForTests();
});

afterEach(() => {
  clearOpenDocumentsForTests();
});

describe("document identity", () => {
  it("names one document per checkout and normalized path, and nothing looser", () => {
    expect(normalizeDocumentPath("./src//a.ts")).toBe("src/a.ts");
    expect(normalizeDocumentPath("src/./a.ts")).toBe("src/a.ts");
    // `..` needs the tree to resolve and a name's case is the host's to judge.
    expect(normalizeDocumentPath("src/../a.ts")).toBe("src/../a.ts");
    expect(documentKey(REPO, "./src/a.ts")).toBe(documentKey(REPO, "src/a.ts"));
    expect(documentKey(REPO, "src/A.ts")).not.toBe(documentKey(REPO, "src/a.ts"));
    expect(documentKey("/work/other", "src/a.ts")).not.toBe(documentKey(REPO, "src/a.ts"));
  });

  it("keeps the same path in two checkouts as two documents with their own drafts", async () => {
    fakeHost({
      [`${REPO}:a.ts`]: { text: "main\n", base: "m1" },
      ["/work/wt:a.ts"]: { text: "wt\n", base: "t1" },
    });
    holdDocument(REPO, "a.ts", "h1");
    holdDocument("/work/wt", "a.ts", "h2");
    await loadDocument(REPO, "a.ts");
    await loadDocument("/work/wt", "a.ts");
    editDocument(REPO, "a.ts", "main!\n", "main\n");
    const map = get(openDocuments);
    expect(documentIn(map, REPO, "a.ts")?.draft?.content).toBe("main!\n");
    expect(documentIn(map, "/work/wt", "a.ts")?.draft).toBeNull();
    expect(documentIn(map, "/work/wt", "a.ts")?.blob?.text).toBe("wt\n");
    expect(unsavedEditorDrafts()).toEqual([{ repo: REPO, paths: ["a.ts"] }]);
  });
});

describe("two hosts on one file", () => {
  it("holds one draft that either host's typing replaces, and one dirty entry for the quit guard", async () => {
    fakeHost({ [`${REPO}:a.ts`]: { text: "one\n", base: "b1" } });
    const code = holdDocument(REPO, "a.ts", "code");
    const split = holdDocument(REPO, "./a.ts", "terminal-file:1");
    await loadDocument(REPO, "a.ts");
    await loadDocument(REPO, "./a.ts");

    editDocument(REPO, "a.ts", "one\ntwo\n", "one\n");
    editDocument(REPO, "./a.ts", "one\ntwo\nthree\n", "one\n");

    const doc = getDocument(REPO, "a.ts");
    expect(get(openDocuments).size).toBe(1);
    expect(doc?.draft).toEqual({ content: "one\ntwo\nthree\n", sourceContent: "one\n", base: "b1" });
    expect(draftPaths(REPO)).toEqual(["a.ts"]);
    expect(unsavedEditorDrafts()).toEqual([{ repo: REPO, paths: ["a.ts"] }]);
    code();
    split();
  });

  it("saves once from either host and leaves both clean", async () => {
    const host = fakeHost({ [`${REPO}:a.ts`]: { text: "one\n", base: "b1" } });
    holdDocument(REPO, "a.ts", "code");
    holdDocument(REPO, "a.ts", "terminal-file:1");
    await loadDocument(REPO, "a.ts");
    editDocument(REPO, "a.ts", "one\ntwo\n", "one\n");

    await saveDocument(REPO, "a.ts", "one\ntwo\n");

    expect(host.writes).toHaveLength(1);
    expect(host.writes[0]).toMatchObject({ filePath: "a.ts", content: "one\ntwo\n", expectedBase: "b1" });
    // Only Overwrite asks the host to skip its encoder check.
    expect(host.writes[0]).not.toHaveProperty("overwrite");
    const doc = getDocument(REPO, "a.ts");
    expect(doc?.draft).toBeNull();
    expect(doc?.blob).toMatchObject({ text: "one\ntwo\n", disk_base: "w1" });
    expect(unsavedEditorDrafts()).toEqual([]);
  });

  it("raises one conflict for both hosts and lets Overwrite from either clear it", async () => {
    const files = { [`${REPO}:a.ts`]: { text: "one\n", base: "b1" } };
    const host = fakeHost(files);
    holdDocument(REPO, "a.ts", "code");
    holdDocument(REPO, "a.ts", "terminal-file:1");
    await loadDocument(REPO, "a.ts");
    editDocument(REPO, "a.ts", "mine\n", "one\n");
    files[`${REPO}:a.ts`] = { text: "agent\n", base: "b2" };

    await expect(saveDocument(REPO, "a.ts", "mine\n")).rejects.toMatchObject({ code: "changed_on_disk" });
    await flush();

    const doc = getDocument(REPO, "a.ts");
    expect(doc?.draft?.content).toBe("mine\n");
    expect(documentConflict(doc)).toEqual({ actual: "b2" });
    // The refusal re-read the file, so a comparison shows what is there now.
    expect(doc?.blob?.text).toBe("agent\n");

    await saveDocument(REPO, "a.ts", "mine\n", { overrideBase: "b2", overwrite: true });
    expect(host.writes.at(-1)).toMatchObject({ expectedBase: "b2", overwrite: true, content: "mine\n" });
    expect(documentConflict(getDocument(REPO, "a.ts"))).toBeNull();
    expect(getDocument(REPO, "a.ts")?.draft).toBeNull();
  });
});

describe("save lifecycle", () => {
  it("keeps text typed while a write was in flight as a draft re-based on what was saved", async () => {
    const host = fakeHost({ [`${REPO}:a.ts`]: { text: "one\n", base: "b1" } });
    holdDocument(REPO, "a.ts", "code");
    await loadDocument(REPO, "a.ts");
    editDocument(REPO, "a.ts", "one\ntwo\n", "one\n");
    const open = host.gateWrites();
    const saving = saveDocument(REPO, "a.ts", "one\ntwo\n");
    await flush();
    editDocument(REPO, "a.ts", "one\ntwo\nthree\n", "one\n");
    open();
    await saving;

    expect(getDocument(REPO, "a.ts")?.draft).toEqual({
      content: "one\ntwo\nthree\n",
      sourceContent: "one\ntwo\n",
      base: "w1",
    });
    expect(unsavedEditorDrafts()).toEqual([{ repo: REPO, paths: ["a.ts"] }]);
    // The next save is judged against the version the first one produced.
    await saveDocument(REPO, "a.ts", "one\ntwo\nthree\n");
    expect(host.writes.at(-1)).toMatchObject({ expectedBase: "w1" });
    expect(getDocument(REPO, "a.ts")?.draft).toBeNull();
  });

  it("serialises saves of one file and judges the second against the first's result", async () => {
    const host = fakeHost({ [`${REPO}:a.ts`]: { text: "one\n", base: "b1" } });
    holdDocument(REPO, "a.ts", "code");
    holdDocument(REPO, "a.ts", "terminal-file:1");
    await loadDocument(REPO, "a.ts");
    editDocument(REPO, "a.ts", "a\n", "one\n");
    const open = host.gateWrites();
    const first = saveDocument(REPO, "a.ts", "a\n");
    const second = saveDocument(REPO, "a.ts", "a\n");
    await flush();
    expect(host.writes).toHaveLength(1);
    open();
    await Promise.all([first, second]);
    // The second run found no draft, so it saved against the blob the first left.
    expect(host.writes).toHaveLength(2);
    expect(host.writes[1]).toMatchObject({ expectedBase: "w1" });
  });

  it("refuses a draft with no recorded base and writes nothing until Overwrite names one", async () => {
    const host = fakeHost({ [`${REPO}:a.ts`]: { text: "one\n", base: "b1" } });
    holdDocument(REPO, "a.ts", "code");
    // Typed before any read landed: the draft has no version to be judged by.
    editDocument(REPO, "a.ts", "mine\n", "one\n");
    expect(getDocument(REPO, "a.ts")?.draft?.base).toBeNull();

    await expect(saveDocument(REPO, "a.ts", "mine\n")).rejects.toBeInstanceOf(UnbasedDraft);
    expect(host.writes).toHaveLength(0);
    expect(documentConflict(getDocument(REPO, "a.ts"))).toEqual({ actual: null });

    await saveDocument(REPO, "a.ts", "mine\n", { overrideBase: null, overwrite: true });
    expect(host.writes).toHaveLength(1);
    expect(host.writes[0]).toMatchObject({ expectedBase: null, overwrite: true });
    expect(getDocument(REPO, "a.ts")?.draft).toBeNull();
  });

  it("keeps the draft when the write fails for any other reason, and records no conflict", async () => {
    fakeHost({ [`${REPO}:a.ts`]: { text: "one\n", base: "b1" } });
    holdDocument(REPO, "a.ts", "code");
    await loadDocument(REPO, "a.ts");
    editDocument(REPO, "a.ts", "two\n", "one\n");
    invokeMock.mockImplementationOnce((async () => {
      throw new Error("disk full");
    }) as unknown as typeof invoke);
    await expect(saveDocument(REPO, "a.ts", "two\n")).rejects.toThrow("disk full");
    expect(getDocument(REPO, "a.ts")?.draft?.content).toBe("two\n");
    expect(documentConflict(getDocument(REPO, "a.ts"))).toBeNull();
    expect(unsavedEditorDrafts()).toEqual([{ repo: REPO, paths: ["a.ts"] }]);
  });

  it("does not write when the host's precondition refuses, and leaves the draft", async () => {
    const host = fakeHost({ [`${REPO}:a.ts`]: { text: "one\n", base: "b1" } });
    holdDocument(REPO, "a.ts", "code");
    await loadDocument(REPO, "a.ts");
    editDocument(REPO, "a.ts", "two\n", "one\n");
    await expect(
      saveDocument(REPO, "a.ts", "two\n", {
        beforeWrite: () => {
          throw new Error("Repository changed before save; the editor draft was kept");
        },
      }),
    ).rejects.toThrow("Repository changed");
    expect(host.writes).toHaveLength(0);
    expect(getDocument(REPO, "a.ts")?.draft?.content).toBe("two\n");
  });

  it("runs the after-save hook inside the queue, after the draft is completed", async () => {
    fakeHost({ [`${REPO}:a.ts`]: { text: "one\n", base: "b1" } });
    holdDocument(REPO, "a.ts", "code");
    await loadDocument(REPO, "a.ts");
    editDocument(REPO, "a.ts", "two\n", "one\n");
    const seen: Array<{ draft: unknown; pending: number }> = [];
    await saveDocument(REPO, "a.ts", "two\n", {
      afterSave: async () => {
        seen.push({ draft: getDocument(REPO, "a.ts")?.draft, pending: editorFileSaveQueue.pending });
      },
    });
    expect(seen).toEqual([{ draft: null, pending: 1 }]);
  });

  it("marks a draft conflicted as soon as a reloaded blob carries a different base", async () => {
    const files = { [`${REPO}:a.ts`]: { text: "one\n", base: "b1" } };
    const host = fakeHost(files);
    holdDocument(REPO, "a.ts", "code");
    await loadDocument(REPO, "a.ts");
    editDocument(REPO, "a.ts", "mine\n", "one\n");
    expect(documentConflict(getDocument(REPO, "a.ts"))).toBeNull();
    files[`${REPO}:a.ts`] = { text: "agent\n", base: "b2" };
    await loadDocument(REPO, "a.ts", { force: true });
    expect(documentConflict(getDocument(REPO, "a.ts"))).toEqual({ actual: "b2" });
    // The draft's own base is untouched by a later keystroke.
    editDocument(REPO, "a.ts", "mine!\n", "agent\n");
    expect(getDocument(REPO, "a.ts")?.draft).toEqual({ content: "mine!\n", sourceContent: "one\n", base: "b1" });
    expect(host.writes).toHaveLength(0);
  });
});

describe("drafts", () => {
  it("is not a draft when the input is back to the baseline", async () => {
    fakeHost({ [`${REPO}:a.ts`]: { text: "one\n", base: "b1" } });
    holdDocument(REPO, "a.ts", "code");
    await loadDocument(REPO, "a.ts");
    editDocument(REPO, "a.ts", "two\n", "one\n");
    editDocument(REPO, "a.ts", "one\n", "one\n");
    expect(getDocument(REPO, "a.ts")?.draft).toBeNull();
    expect(unsavedEditorDrafts()).toEqual([]);
  });

  it("drops the draft when it is edited back to the text it was made from, even after a reload", async () => {
    const files = { [`${REPO}:a.ts`]: { text: "v1 text", base: "3:v1" } };
    fakeHost(files);
    holdDocument(REPO, "a.ts", "code");
    await loadDocument(REPO, "a.ts");
    editDocument(REPO, "a.ts", "mine", "v1 text");
    files[`${REPO}:a.ts`] = { text: "agent text", base: "9:v2" };
    await loadDocument(REPO, "a.ts", { force: true });
    // The editor now reports the reloaded text as its baseline with every keystroke.
    editDocument(REPO, "a.ts", "v1 text", "agent text");
    expect(getDocument(REPO, "a.ts")?.draft).toBeNull();
  });

  it("compares disk versions as tokens: only a different one on both sides is a conflict", async () => {
    fakeHost({ [`${REPO}:a.ts`]: { text: "v1 text", base: "3:v1" } });
    holdDocument(REPO, "a.ts", "code");
    await loadDocument(REPO, "a.ts");
    editDocument(REPO, "a.ts", "mine", "v1 text");
    const doc = getDocument(REPO, "a.ts");
    if (!doc?.blob) throw new Error("document not loaded");
    const reloaded = (disk_base: string | null | undefined) => ({ ...doc, blob: { ...doc.blob!, disk_base } });
    expect(documentConflict(reloaded("9:v2"))).toEqual({ actual: "9:v2" });
    expect(documentConflict(reloaded("3:v1"))).toBeNull();
    expect(documentConflict(reloaded(null))).toBeNull();
    expect(documentConflict(reloaded(undefined))).toBeNull();
    expect(documentConflict(undefined)).toBeNull();
    expect(documentConflict({ ...doc, draft: null })).toBeNull();
    // A draft with no recorded base has nothing to compare, until a save refuses it.
    expect(documentConflict({ ...reloaded("9:v2"), draft: { ...doc.draft!, base: null } })).toBeNull();
  });

  it("goes only when discarded, with its refusal", async () => {
    const files = { [`${REPO}:a.ts`]: { text: "one\n", base: "b1" } };
    fakeHost(files);
    holdDocument(REPO, "a.ts", "code");
    await loadDocument(REPO, "a.ts");
    editDocument(REPO, "a.ts", "mine\n", "one\n");
    files[`${REPO}:a.ts`] = { text: "agent\n", base: "b2" };
    await expect(saveDocument(REPO, "a.ts", "mine\n")).rejects.toBeDefined();
    discardDraft(REPO, "a.ts");
    expect(getDocument(REPO, "a.ts")?.draft).toBeNull();
    expect(getDocument(REPO, "a.ts")?.refusal).toBeNull();
    // A new draft starts clean, not under the old refusal.
    await loadDocument(REPO, "a.ts", { force: true });
    editDocument(REPO, "a.ts", "again\n", "agent\n");
    expect(documentConflict(getDocument(REPO, "a.ts"))).toBeNull();
    expect(unsavedEditorDrafts()).toEqual([{ repo: REPO, paths: ["a.ts"] }]);
  });
});

describe("loading", () => {
  it("answers a loaded document from memory and joins a read already in flight", async () => {
    const host = fakeHost({ [`${REPO}:a.ts`]: { text: "one\n", base: "b1" } });
    holdDocument(REPO, "a.ts", "code");
    const open = host.gateReads();
    const first = loadDocument(REPO, "a.ts");
    const joined = loadDocument(REPO, "./a.ts");
    open();
    await Promise.all([first, joined]);
    expect(host.reads).toHaveLength(1);
    await loadDocument(REPO, "a.ts");
    expect(host.reads).toHaveLength(1);
    await loadDocument(REPO, "a.ts", { force: true });
    expect(host.reads).toHaveLength(2);
  });

  it("keeps the blob on screen through a forced read and blanks it only when asked to", async () => {
    const host = fakeHost({ [`${REPO}:a.ts`]: { text: "one\n", base: "b1" } });
    holdDocument(REPO, "a.ts", "code");
    await loadDocument(REPO, "a.ts");
    const open = host.gateReads();
    const reload = loadDocument(REPO, "a.ts", { force: true });
    expect(getDocument(REPO, "a.ts")).toMatchObject({ loading: true, blob: { text: "one\n" } });
    open();
    await reload;
    const remount = host.gateReads();
    const blanked = loadDocument(REPO, "a.ts", { force: true, remount: true });
    expect(getDocument(REPO, "a.ts")).toMatchObject({ loading: true, blob: null });
    remount();
    await blanked;
    expect(getDocument(REPO, "a.ts")?.blob?.text).toBe("one\n");
  });

  it("lets only the newest read answer", async () => {
    const files = { [`${REPO}:a.ts`]: { text: "old\n", base: "b1" } };
    const host = fakeHost(files);
    holdDocument(REPO, "a.ts", "code");
    const open = host.gateReads();
    const stale = loadDocument(REPO, "a.ts", { force: true });
    files[`${REPO}:a.ts`] = { text: "new\n", base: "b2" };
    const fresh = loadDocument(REPO, "a.ts", { force: true });
    open();
    expect(await stale).toEqual({ status: "superseded" });
    expect(await fresh).toMatchObject({ status: "loaded" });
    expect(getDocument(REPO, "a.ts")?.blob?.text).toBe("new\n");
  });

  it("reports a failed read on the document and leaves no stale text", async () => {
    fakeHost({});
    holdDocument(REPO, "missing.ts", "code");
    const result = await loadDocument(REPO, "missing.ts");
    expect(result).toMatchObject({ status: "failed" });
    expect(getDocument(REPO, "missing.ts")).toMatchObject({ blob: null, loading: false });
    expect(getDocument(REPO, "missing.ts")?.error).toContain("no such file");
  });
});

describe("unloading", () => {
  it("drops a document when its last holder leaves and it has no draft", async () => {
    fakeHost({ [`${REPO}:a.ts`]: { text: "one\n", base: "b1" } });
    const code = holdDocument(REPO, "a.ts", "code");
    const split = holdDocument(REPO, "a.ts", "terminal-file:1");
    await loadDocument(REPO, "a.ts");
    code();
    expect(getDocument(REPO, "a.ts")).toBeDefined();
    split();
    expect(getDocument(REPO, "a.ts")).toBeUndefined();
    expect(get(openDocuments).size).toBe(0);
  });

  it("keeps a document that carries a draft after every host has left, until it is discarded", async () => {
    fakeHost({ [`${REPO}:a.ts`]: { text: "one\n", base: "b1" } });
    const release = holdDocument(REPO, "a.ts", "code");
    await loadDocument(REPO, "a.ts");
    editDocument(REPO, "a.ts", "two\n", "one\n");
    release();
    expect(getDocument(REPO, "a.ts")?.draft?.content).toBe("two\n");
    expect(unsavedEditorDrafts()).toEqual([{ repo: REPO, paths: ["a.ts"] }]);
    discardDraft(REPO, "a.ts");
    expect(getDocument(REPO, "a.ts")).toBeUndefined();
    expect(unsavedEditorDrafts()).toEqual([]);
  });

  it("does not bring a document back from a read that lands after it was unloaded", async () => {
    const host = fakeHost({ [`${REPO}:a.ts`]: { text: "one\n", base: "b1" } });
    const release = holdDocument(REPO, "a.ts", "code");
    const open = host.gateReads();
    const read = loadDocument(REPO, "a.ts");
    release();
    open();
    expect(await read).toEqual({ status: "superseded" });
    expect(getDocument(REPO, "a.ts")).toBeUndefined();
  });

  it("counts a host's open-but-unshown file as held, per checkout, until it stops claiming it", async () => {
    fakeHost({ [`${REPO}:a.ts`]: { text: "one\n", base: "b1" } });
    holdDocument(REPO, "a.ts", "terminal-file:1");
    // The Code page has a.ts as a background tab; nothing of it is on screen.
    claimDocuments(REPO, "code-page", ["a.ts", "b.ts"]);
    expect(isHeldElsewhere(REPO, "a.ts", "terminal-file:1")).toBe(true);
    expect(isHeldElsewhere(REPO, "./b.ts", "terminal-file:1")).toBe(true);
    expect(isHeldElsewhere(REPO, "c.ts", "terminal-file:1")).toBe(false);
    expect(isHeldElsewhere("/work/other", "a.ts", "terminal-file:1")).toBe(false);
    // A host's own claim is not "elsewhere" to it.
    expect(isHeldElsewhere(REPO, "a.ts", "code-page")).toBe(true);
    expect(isHeldElsewhere(REPO, "b.ts", "code-page")).toBe(false);
    // A claim keeps no text: once the split leaves, nothing is loaded.
    expect(getDocument(REPO, "b.ts")).toBeUndefined();
    // A new claim replaces the old one.
    claimDocuments(REPO, "code-page", ["b.ts"]);
    expect(isHeldElsewhere(REPO, "a.ts", "terminal-file:1")).toBe(false);
    claimDocuments(REPO, "code-page", []);
    expect(isHeldElsewhere(REPO, "b.ts", "terminal-file:1")).toBe(false);
  });

  it("tells a host whether another still shows the file, and a release twice is one release", async () => {
    fakeHost({ [`${REPO}:a.ts`]: { text: "one\n", base: "b1" } });
    const code = holdDocument(REPO, "a.ts", "code");
    holdDocument(REPO, "a.ts", "terminal-file:1");
    expect(isHeldElsewhere(REPO, "a.ts", "code")).toBe(true);
    expect(isHeldElsewhere(REPO, "./a.ts", "terminal-file:1")).toBe(true);
    code();
    code();
    expect(isHeldElsewhere(REPO, "a.ts", "code")).toBe(true);
    expect(isHeldElsewhere(REPO, "a.ts", "terminal-file:1")).toBe(false);
  });
});
