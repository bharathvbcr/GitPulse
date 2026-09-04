import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import type {
  ConflictChunk,
  ConflictDocument,
  ConflictResolutionChoice,
} from "../diff/conflict";
import { render } from "svelte/server";
import ConflictEditor, {
  adoptResolutions,
  canFinalizeConflictSave,
  clearCustomDraftsForDocument,
  conflictDraftKey,
  flushCustomDrafts,
  type ConflictCustomDrafts,
} from "./ConflictEditor.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "ConflictEditor.svelte"),
  "utf8",
);

function openingTagContaining(tagName: string, marker: string): string {
  const markerIndex = source.indexOf(marker);
  expect(markerIndex, `missing ${marker}`).toBeGreaterThan(-1);
  const start = source.lastIndexOf(`<${tagName}`, markerIndex);
  const end = source.indexOf(">", markerIndex);
  expect(start, `missing <${tagName}> for ${marker}`).toBeGreaterThan(-1);
  expect(end, `unterminated <${tagName}> for ${marker}`).toBeGreaterThan(start);
  return source.slice(start, end + 1);
}

/**
 * The real wire types, not structural mirrors. The mirrors could not fail on
 * drift: they were assignable to whatever the component declared, so both
 * sides could go stale together.
 */
function chunk(index: number, resolution: ConflictResolutionChoice): ConflictChunk {
  return {
    chunk_index: index,
    start_line: index * 10,
    end_line: index * 10 + 3,
    ours_label: "HEAD",
    ours_content: `ours ${index}`,
    theirs_label: "feature",
    theirs_content: `theirs ${index}`,
    resolution,
    // Rust has always sent these; the structural mirror this file used to
    // declare simply left them out, and nothing could say so.
    ours_crlf: [],
    theirs_crlf: [],
    local_crlf: false,
  };
}

function doc(path: string, chunks: ConflictChunk[]): ConflictDocument {
  return {
    file_path: path,
    segments: chunks.map((Conflict) => ({ Conflict })),
    total_conflicts: chunks.length,
    crlf: false,
    trailing_newline: true,
    final_crlf: false,
    normal_crlf_flags: [],
  };
}

describe("adoptResolutions (stale-parse protection)", () => {
  it("carries same-file resolutions onto a fresh parse by chunk index", () => {
    const current = doc("a.txt", [chunk(0, "Unresolved"), chunk(1, "AcceptTheirs"), chunk(2, { Custom: "merged line" })]);
    const next = doc("a.txt", [chunk(0, "Unresolved"), chunk(1, "Unresolved"), chunk(2, "Unresolved")]);
    const merged = adoptResolutions(next, current);
    expect(merged.segments.map((seg) => seg.Conflict?.resolution)).toEqual([
      "Unresolved",
      "AcceptTheirs",
      { Custom: "merged line" },
    ]);
  });

  it("leaves a different file's parse untouched", () => {
    const current = doc("a.txt", [chunk(0, "AcceptOurs")]);
    const next = doc("b.txt", [chunk(0, "Unresolved")]);
    expect(adoptResolutions(next, current).segments[0].Conflict?.resolution).toBe("Unresolved");
  });

  it("returns the next parse unchanged when nothing was resolved yet", () => {
    const current = doc("a.txt", [chunk(0, "Unresolved")]);
    const next = doc("a.txt", [chunk(0, "AcceptOurs")]);
    expect(adoptResolutions(next, current).segments[0].Conflict?.resolution).toBe("AcceptOurs");
  });

  it("handles a null current doc and leaves the previous doc unmutated", () => {
    const next = doc("a.txt", [chunk(0, "AcceptTheirs")]);
    expect(adoptResolutions(next, null)).toBe(next);
    const current = doc("a.txt", [chunk(0, "AcceptOurs")]);
    adoptResolutions(next, current);
    expect(current.segments[0].Conflict?.resolution).toBe("AcceptOurs");
  });
});

describe("ConflictEditor load-effect memo guard", () => {
  it("reloads only when the conflicted file or repo actually changes", () => {
    // repoStore republishes fresh objects every ~6s status poll; reloading
    // per emission would churn IPC and replace parsedDoc mid-edit.
    expect(source).toContain("if (key === loadedKey) return;");
    expect(source).toContain("loadedKey = key;");
    // The memo key derives only from repo path + conflicted file.
    expect(source).toContain("`${repo}\\u0000${file}`");
  });

  it("does not cancel an in-flight load via effect teardown on memo hits", () => {
    // Svelte runs an effect's previous teardown before EVERY re-run, so the
    // load guard must be cancelled explicitly in the body instead.
    const loadIdx = source.indexOf("const guard = createAsyncGuard();");
    const explicitCancel = source.indexOf("loadGuard?.cancel();", source.indexOf("let loadedKey"));
    expect(explicitCancel).toBeGreaterThan(-1);
    expect(explicitCancel).toBeLessThan(loadIdx);
    expect(source.match(/return \(\) => \{\s*guard\.cancel\(\);\s*\};/g)).toBeNull();
  });

  it("routes a landed parse through adoptResolutions instead of assigning raw", () => {
    expect(source).toContain("const adopted = adoptResolutions(doc, parsedDoc);");
    expect(source).toContain("parsedDoc = adopted;");
  });
});

describe("ConflictEditor journal completeness", () => {
  it("records the completed write before stale UI checks", () => {
    const body = source.slice(
      source.indexOf("async function saveResolved"),
      source.indexOf("</script>", source.indexOf("async function saveResolved")),
    );
    const write = body.indexOf('await invoke("cmd_write_file_content"');
    const successJournal = body.indexOf("harnessStore.recordAction", write);
    const nextGuard = body.indexOf("if (!guard.isLive()", write);
    expect(successJournal).toBeGreaterThan(write);
    expect(successJournal).toBeLessThan(nextGuard);

    const caught = body.indexOf("} catch", nextGuard);
    const failureJournal = body.indexOf("harnessStore.recordAction", caught);
    const failureGuard = body.indexOf("if (!guard.isLive()", caught);
    expect(failureJournal).toBeGreaterThan(caught);
    expect(failureJournal).toBeLessThan(failureGuard);
  });
});

describe("ConflictEditor custom draft lifecycle", () => {
  it("keeps chunk-zero drafts isolated when switching between files", () => {
    const drafts: ConflictCustomDrafts = {
      [conflictDraftKey("/repo", "a.txt", 0)]: { value: "draft for a", active: true },
      [conflictDraftKey("/repo", "b.txt", 0)]: { value: "draft for b", active: true },
    };
    const first = doc("a.txt", [chunk(0, "Unresolved")]);
    const second = doc("b.txt", [chunk(0, "Unresolved")]);

    flushCustomDrafts(first, "/repo", "a.txt", drafts);
    flushCustomDrafts(second, "/repo", "b.txt", drafts);

    expect(first.segments[0].Conflict?.resolution).toEqual({ Custom: "draft for a" });
    expect(second.segments[0].Conflict?.resolution).toEqual({ Custom: "draft for b" });
    expect(conflictDraftKey("/repo", "a.txt", 0)).not.toBe(
      conflictDraftKey("/repo", "b.txt", 0),
    );
    expect(source).toContain("const customTimers = new Map<string");
    expect(source).toContain("flushCustomDrafts(adopted, repo, file, customDrafts);");
  });

  it("refuses to save while a newly selected file is still showing the prior parse", () => {
    const save = source.slice(
      source.indexOf("async function saveResolved"),
      source.indexOf("</script>", source.indexOf("async function saveResolved")),
    );
    expect(save).toContain("if (document.file_path !== file) return;");
    expect(source).toContain("parsedDoc?.file_path !== selectedFile");
  });

  it("flushes the latest custom value before an immediate save resolves the document", () => {
    const document = doc("a.txt", [chunk(0, { Custom: "previous value" })]);
    const drafts: ConflictCustomDrafts = {
      [conflictDraftKey("/repo", "a.txt", 0)]: { value: "latest keystroke", active: true },
    };

    flushCustomDrafts(document, "/repo", "a.txt", drafts);

    expect(document.segments[0].Conflict?.resolution).toEqual({ Custom: "latest keystroke" });
    const save = source.slice(
      source.indexOf("async function saveResolved"),
      source.indexOf("</script>", source.indexOf("async function saveResolved")),
    );
    expect(save.indexOf("flushCustomDrafts(")).toBeGreaterThan(-1);
    expect(save.indexOf("flushCustomDrafts(")).toBeLessThan(
      save.indexOf('invoke<string>("cmd_resolve_conflict"'),
    );
  });

  it("preserves drafts after a failed save and clears only the saved target", () => {
    const firstKey = conflictDraftKey("/repo", "a.txt", 0);
    const secondKey = conflictDraftKey("/repo", "b.txt", 0);
    const drafts: ConflictCustomDrafts = {
      [firstKey]: { value: "draft for a", active: true },
      [secondKey]: { value: "draft for b", active: true },
    };
    const first = doc("a.txt", [chunk(0, { Custom: "draft for a" })]);

    // Flush is the last synchronous draft operation before resolve/write. If
    // either backend call fails, the component takes its catch path without
    // invoking the success-only clearing helper.
    flushCustomDrafts(first, "/repo", "a.txt", drafts);
    expect(drafts[firstKey]).toEqual({ value: "draft for a", active: true });
    expect(drafts[secondKey]).toEqual({ value: "draft for b", active: true });

    clearCustomDraftsForDocument(drafts, "/repo", "a.txt", first);
    expect(drafts[firstKey]).toBeUndefined();
    expect(drafts[secondKey]).toEqual({ value: "draft for b", active: true });

    const save = source.slice(
      source.indexOf("async function saveResolved"),
      source.indexOf("</script>", source.indexOf("async function saveResolved")),
    );
    const write = save.indexOf('await invoke("cmd_write_file_content"');
    const clear = save.indexOf("clearCustomDraftsForDocument(");
    const failedSave = save.slice(save.indexOf("} catch (err)"), save.indexOf("} finally"));
    expect(clear).toBeGreaterThan(write);
    expect(failedSave).not.toContain("clearCustomDraftsForDocument(");
  });

  it("does not clear or stage a newer resolution edit after an older save returns", () => {
    expect(canFinalizeConflictSave(7, 7)).toBe(true);
    expect(canFinalizeConflictSave(7, 8)).toBe(false);

    const save = source.slice(
      source.indexOf("async function saveResolved"),
      source.indexOf("</script>", source.indexOf("async function saveResolved")),
    );
    const write = save.indexOf('await invoke("cmd_write_file_content"');
    const superseded = save.indexOf("canFinalizeConflictSave(saveRevision, editRevision)");
    const clear = save.indexOf("clearCustomDraftsForDocument(");
    const stage = save.indexOf("repoStore.stageFile(");
    expect(superseded).toBeGreaterThan(write);
    expect(clear).toBeGreaterThan(superseded);
    expect(stage).toBeGreaterThan(superseded);
    expect(save.slice(superseded, clear)).toContain("return");
  });

  it("locks every resolution mutator until writing and staging have both settled", () => {
    expect(openingTagContaining("select", "bind:value={selectedFile}"))
      .toContain("disabled={isSaving}");
    for (const label of [
      "Accept All Current (Ours)",
      "Accept All Incoming (Theirs)",
      "Accept Ours",
      "Both (Ours First)",
      "Accept Theirs",
      "Both (Theirs First)",
    ]) {
      expect(openingTagContaining("button", label), label).toContain("disabled={isSaving}");
    }
    expect(openingTagContaining("textarea", "Type the exact content this conflict"))
      .toContain("disabled={isSaving}");

    const save = source.slice(
      source.indexOf("async function saveResolved"),
      source.indexOf("</script>", source.indexOf("async function saveResolved")),
    );
    expect(save.indexOf("isSaving = true")).toBeLessThan(
      save.indexOf('invoke<string>("cmd_resolve_conflict"'),
    );
    expect(save.indexOf("isSaving = false")).toBeGreaterThan(
      save.indexOf("await repoStore.stageFile(file)"),
    );
  });

  it("updates the document synchronously on input and only debounces preview work", () => {
    const input = source.slice(
      source.indexOf("function onCustomInput"),
      source.indexOf("async function updatePreview"),
    );
    expect(input.indexOf("setDocumentChunkResolution(")).toBeGreaterThan(-1);
    expect(input.indexOf("setDocumentChunkResolution(")).toBeLessThan(
      input.indexOf("setTimeout("),
    );
  });
});

describe("ConflictEditor rendering", () => {
  it("renders the empty state when no conflicts exist", () => {
    const { body } = render(ConflictEditor);
    expect(body).toContain("No merge conflicts");
  });
});
