import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createConflictSessions, initialResolution, materializeResolution, type ConflictSnapshot } from "./conflictSession";
import { clearEditorDraftRegistryForTests, unsavedEditorDrafts } from "../files/editorDraftRegistry";

function snapshot(revision = "a", file = "file"): ConflictSnapshot {
  return { file_path: file, revision, operation: "merge-a", worktree_mode: "100644", stages: [], reason: null,
    document: { file_path: file, diagnostics: [], marker_size: 7, total_conflicts: 1, crlf: false, trailing_newline: true, final_crlf: false, normal_crlf_flags: [], segments: [{ Conflict: { chunk_index: 0, start_line: 1, end_line: 5, ours_label: "main", theirs_label: "topic", ours_content: "ours", theirs_content: "theirs", base_content: null, resolution: "Unresolved", ours_crlf: [false], theirs_crlf: [false], base_crlf: null, local_crlf: false } }] } };
}
function storage() {
  let value: string | null = null;
  return { getItem: () => value, setItem: (_: string, next: string) => { value = next; }, removeItem: () => { value = null; } };
}
const choice = (snap = snapshot(), value = "draft") => ({ ...initialResolution(snap), choices: [{ Custom: value }], custom: { "0": value } });
beforeEach(() => { vi.useFakeTimers(); clearEditorDraftRegistryForTests(); });
afterEach(() => { vi.runOnlyPendingTimers(); vi.useRealTimers(); });

describe("conflict draft recovery", () => {
  it("retains presets and blank custom edits across view owners and process reload", () => {
    const disk = storage(); const sessions = createConflictSessions(disk);
    sessions.open("/repo", snapshot()); sessions.edit("/repo", "file", choice(snapshot(), "")); sessions.flush();
    expect(sessions.open("/repo", snapshot()).state.choices).toEqual([{ Custom: "" }]);
    const restored = createConflictSessions(disk);
    expect(restored.warning).toBeNull();
    expect(restored.open("/repo", snapshot()).state.choices).toEqual([{ Custom: "" }]);
    expect(unsavedEditorDrafts()).toEqual([{ repo: "/repo", paths: ["file"] }]);
  });
  it("retains stale source for comparison and never reapplies it automatically", () => {
    const sessions = createConflictSessions();
    sessions.open("/repo", snapshot()); sessions.edit("/repo", "file", choice());
    const next = sessions.open("/repo", snapshot("new-source"));
    expect(next.state.choices).toEqual(["Unresolved"]);
    expect(next.recovery[0]?.state.choices).toEqual([{ Custom: "draft" }]);
    expect(materializeResolution(next)?.segments[0].Conflict?.resolution).toBe("Unresolved");
  });
  it("keeps repository and unusual filename identities independent", () => {
    const sessions = createConflictSessions();
    for (const [repo, file] of [["/a", "b:c"], ["/a:b", "c"], ["/a", "x\ny"]]) {
      const snap = snapshot("a", file); sessions.open(repo, snap); sessions.edit(repo, file, choice(snap, repo + file));
    }
    expect(sessions.get("/a", "b:c")?.state.choices).toEqual([{ Custom: "/ab:c" }]);
    expect(sessions.list("/a")).toHaveLength(2);
  });
  it("undoes and redoes bulk choices and coalesces typing without losing the latest character", () => {
    const sessions = createConflictSessions(); sessions.open("/repo", snapshot());
    for (let index = 0; index < 1000; index++) sessions.edit("/repo", "file", choice(snapshot(), `edit ${index}`), "typing:0");
    expect(sessions.get("/repo", "file")?.undo).toHaveLength(1);
    expect(sessions.travel("/repo", "file", "undo")?.state.choices).toEqual(["Unresolved"]);
    expect(sessions.travel("/repo", "file", "redo")?.state.choices).toEqual([{ Custom: "edit 999" }]);
    sessions.travel("/repo", "file", "undo"); sessions.edit("/repo", "file", choice(snapshot(), "branch"));
    expect(sessions.get("/repo", "file")?.redo).toHaveLength(0);
  });
  it("bounds history while retaining current content and protects dirty files at capacity", () => {
    const sessions = createConflictSessions(); sessions.open("/repo", snapshot());
    for (let i = 0; i < 100; i++) sessions.edit("/repo", "file", choice(snapshot(), String(i)));
    expect(sessions.get("/repo", "file")?.undo).toHaveLength(50);
    for (let i = 1; i < 64; i++) { const snap = snapshot("a", `f${i}`); sessions.open("/repo", snap); sessions.edit("/repo", snap.file_path, choice(snap)); }
    expect(() => sessions.open("/repo", snapshot("a", "overflow"))).toThrow("64 conflict files");
    expect(sessions.get("/repo", "file")?.state.choices).toEqual([{ Custom: "99" }]);
  });
  it("reports storage failure and keeps the latest draft in memory", () => {
    const disk = { ...storage(), setItem: () => { throw new Error("QuotaExceeded"); } };
    const sessions = createConflictSessions(disk); sessions.open("/repo", snapshot()); sessions.edit("/repo", "file", choice()); sessions.flush();
    expect(sessions.warning).toContain("unavailable"); expect(sessions.get("/repo", "file")?.state.choices).toEqual([{ Custom: "draft" }]);
  });
  it.each(["{broken", '{"version":2,"drafts":[]}', '{"version":1,"drafts":[{}]}'])("preserves corrupt storage without silently replacing it: %s", raw => {
    const disk = storage(); disk.setItem("", raw); const sessions = createConflictSessions(disk);
    expect(sessions.warning).toContain("could not be read"); sessions.open("/repo", snapshot()); sessions.edit("/repo", "file", choice()); sessions.flush();
    expect(disk.getItem()).toBe(raw);
  });
  it("preserves a saved-but-unstaged receipt across a remount and clears only after completion", () => {
    const sessions = createConflictSessions(); sessions.open("/repo", snapshot()); sessions.edit("/repo", "file", choice()); sessions.pending("/repo", "file", snapshot("saved"));
    expect(sessions.open("/repo", snapshot("saved")).pending?.revision).toBe("saved");
    sessions.complete("/repo", "file", "obsolete"); expect(sessions.get("/repo", "file")).not.toBeNull();
    sessions.complete("/repo", "file", "saved"); expect(sessions.get("/repo", "file")).toBeNull(); expect(unsavedEditorDrafts()).toEqual([]);
  });
  it("does not clear newer choices when an older save completes", () => {
    const sessions = createConflictSessions(); sessions.open("/repo", snapshot()); const saved = choice(); sessions.edit("/repo", "file", saved);
    sessions.edit("/repo", "file", choice(snapshot(), "newer"));
    sessions.complete("/repo", "file", "a", saved);
    expect(sessions.get("/repo", "file")?.state.choices).toEqual([{ Custom: "newer" }]);
  });
  it("does not attach an older staging receipt to newer choices", () => {
    const sessions = createConflictSessions(); sessions.open("/repo", snapshot()); const saved = choice(); sessions.edit("/repo", "file", saved);
    sessions.edit("/repo", "file", choice(snapshot(), "newer"));
    sessions.pending("/repo", "file", snapshot("saved"), "a", saved);
    expect(sessions.get("/repo", "file")?.pending).toBeNull();
  });
  it("returns defensive copies instead of letting a remounted owner mutate another owner", () => {
    const sessions = createConflictSessions(); const view = sessions.open("/repo", snapshot()); view.state.choices[0] = "AcceptOurs";
    expect(sessions.get("/repo", "file")?.state.choices).toEqual(["Unresolved"]);
  });
  it("refuses oversized replacement states without mutating the retained draft", () => {
    const sessions = createConflictSessions(); sessions.open("/repo", snapshot()); sessions.edit("/repo", "file", choice());
    expect(() => sessions.edit("/repo", "file", choice(snapshot(), "x".repeat(1024 * 1024 + 1)))).toThrow("limit");
    expect(sessions.get("/repo", "file")?.state.choices).toEqual([{ Custom: "draft" }]);
  });
  it("treats duplicate persisted identities as corrupt rather than overwriting a draft", () => {
    const disk = storage(); const sessions = createConflictSessions(disk); sessions.open("/repo", snapshot()); sessions.edit("/repo", "file", choice()); sessions.flush();
    const payload = JSON.parse(disk.getItem()!); payload.drafts.push({ ...payload.drafts[0], state: choice(snapshot(), "duplicate") });
    disk.setItem("", JSON.stringify(payload)); const restored = createConflictSessions(disk);
    expect(restored.warning).toContain("could not be read"); expect(restored.get("/repo", "file")).toBeNull();
  });
  it("publishes debounced storage failures and clears the notice after recovery", () => {
    let fail = true; const disk = { ...storage(), setItem: () => { if (fail) throw Error("quota"); } };
    const sessions = createConflictSessions(disk); const notices: Array<string | null> = [];
    const unsubscribe = sessions.subscribeWarning(notice => notices.push(notice));
    sessions.open("/repo", snapshot()); sessions.edit("/repo", "file", choice()); vi.advanceTimersByTime(200);
    expect(notices.at(-1)).toContain("unavailable"); fail = false; sessions.flush(); expect(notices.at(-1)).toBeNull();
    unsubscribe(); const count = notices.length; fail = true; sessions.flush(); expect(notices).toHaveLength(count);
  });
  it("refuses invalid stored marker widths and mismatched receipts", () => {
    const disk = storage(); const sessions = createConflictSessions(disk); sessions.open("/repo", snapshot()); sessions.edit("/repo", "file", choice()); sessions.flush();
    expect(() => sessions.pending("/repo", "file", snapshot("a", "other"))).toThrow("different file");
    const payload = JSON.parse(disk.getItem()!); payload.drafts[0].snapshot.document.marker_size = 0; disk.setItem("", JSON.stringify(payload));
    expect(createConflictSessions(disk).warning).toContain("could not be read");
  });
  it("shows lightweight progress and discards only the selected recovery or file", () => {
    const sessions = createConflictSessions(); sessions.open("/repo", snapshot()); sessions.edit("/repo", "file", choice());
    sessions.open("/repo", snapshot("new")); sessions.edit("/repo", "file", choice(snapshot("new"), "current"));
    expect(sessions.summaries("/repo")).toEqual([{file:"file",total:1,resolved:1,whole:null,pending:false,recoveryCount:1}]);
    sessions.discardRecovery("/repo", "file"); expect(sessions.get("/repo", "file")?.state.choices).toEqual([{Custom:"current"}]);
    expect(sessions.get("/repo", "file")?.recovery).toEqual([]);
    sessions.open("/other", snapshot()); sessions.edit("/other", "file", choice());
    sessions.discard("/repo", "file"); expect(sessions.get("/repo", "file")).toBeNull(); expect(sessions.get("/other", "file")).not.toBeNull();
    sessions.clear(); expect(unsavedEditorDrafts()).toEqual([]);
  });
  it("restores EOL metadata and stage previews without dropping empty sides", () => {
    const disk = storage(); const snap = snapshot(); snap.stages = [{stage:2,mode:"100644",oid:"object",size:0,text:""}];
    snap.document!.segments.unshift({Normal:"context"}); snap.document!.normal_crlf_flags = [[true]];
    const sessions = createConflictSessions(disk); sessions.open("/repo", snap); sessions.edit("/repo", "file", choice(snap)); sessions.flush();
    const restored = createConflictSessions(disk); expect(restored.warning).toBeNull(); expect(restored.get("/repo", "file")?.snapshot).toEqual(snap);
  });
  it("reports the disk budget and refuses aggregate memory growth while preserving edits", () => {
    const disk = storage(); const sessions = createConflictSessions(disk); const text = "x".repeat(1024 * 1024);
    for(let index=0;index<15;index++){const snap=snapshot("a",`f${index}`);sessions.open("/repo",snap);sessions.edit("/repo",snap.file_path,choice(snap,text));}
    sessions.flush(); expect(sessions.warning).toContain("storage limit");
    const next=snapshot("a","overflow");sessions.open("/repo",next);
    expect(()=>sessions.edit("/repo","overflow",choice(next,text))).toThrow("memory limit");
    expect(sessions.get("/repo","overflow")?.state.choices).toEqual(["Unresolved"]);
    expect(sessions.get("/repo","f0")?.state.choices).toEqual([{Custom:text}]);
    sessions.clear(); sessions.flush(); expect(sessions.warning).toBeNull();
  });
});
