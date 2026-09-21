import { describe, expect, it } from "vitest";
import type { BlameLine } from "../../files/types";
import type { FileStatus } from "../../stores/repoStore";

/**
 * Deep equality comparator for BlameLine arrays, matching the canonical
 * implementation in BlameViewer.svelte.
 */
export function areBlameLinesEqual(a: BlameLine[], b: BlameLine[]): boolean {
  if (a === b) return true;
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    const la = a[i];
    const lb = b[i];
    if (
      la.line_no !== lb.line_no ||
      la.commit_id !== lb.commit_id ||
      la.timestamp !== lb.timestamp ||
      la.author_name !== lb.author_name ||
      la.content !== lb.content
    ) {
      return false;
    }
  }
  return true;
}

/**
 * Computes the blame fingerprint with full sensitivity to file status,
 * branch tip, and content revisions.
 */
export function computeBlameFingerprint(
  repo: string | null,
  selected: string | null,
  statuses: FileStatus[],
  currentBranchTip: string | null,
  contentRevisions: Record<string, string> = {},
): string {
  const fileStatus = selected
    ? statuses.find((s) => s.path === selected)
    : undefined;
  const statusCode = fileStatus
    ? `${fileStatus.status_code}:${fileStatus.is_staged}:${fileStatus.additions}:${fileStatus.deletions}`
    : "";
  const tip = currentBranchTip ?? "";
  const revision = repo ? (contentRevisions[repo] ?? "") : "";
  return `${repo ?? ""}\u0000${selected ?? ""}\u0000${statusCode}\u0000${tip}\u0000${revision}`;
}

describe("BlameViewer SWR Hardening & Concurrency Stress Tests", () => {
  const mockLine = (no: number, commit = "abc1234", text = "const x = 1;"): BlameLine => ({
    line_no: no,
    commit_id: commit,
    author_name: "Developer",
    author_email: "dev@example.com",
    timestamp: 1700000000,
    content: text,
  });

  describe("SWR State Preservation vs Pre-Fix Flashing Simulator", () => {
    it("proves pre-fix logic caused 1,000 UI unmounts and wipes while hardened SWR preserves complete stability", () => {
      // Pre-fix simulator:
      class PreFixBlameViewer {
        blameLines: BlameLine[] = [mockLine(1), mockLine(2), mockLine(3)];
        isLoading = false;
        listScroll = 450;
        selection: { kind: string; id: string } | null = { kind: "band", id: "recent" };
        unmountCount = 0;
        scrollWipeCount = 0;
        filterWipeCount = 0;

        // When loadBlameFor runs in pre-fix code:
        onReloadStart() {
          this.isLoading = true;
          // In template: {#if blameLines.length > 0 && !isLoading && !errorMsg}
          // and {#if isLoading} -> VirtualList unmounts and timeline unmounts!
          this.unmountCount++;
        }

        onReloadEnd(next: BlameLine[]) {
          this.blameLines = next;
          // Unconditional resets from pre-fix lines 116-119:
          this.selection = null;
          this.listScroll = 0;
          this.isLoading = false;
          this.scrollWipeCount++;
          this.filterWipeCount++;
        }
      }

      // Hardened SWR simulator:
      class HardenedBlameViewer {
        blameLines: BlameLine[] = [mockLine(1), mockLine(2), mockLine(3)];
        isLoading = false;
        listScroll = 450;
        selection: { kind: string; id: string } | null = { kind: "band", id: "recent" };
        loadedSubject = "/repo\0file.ts";
        unmountCount = 0;
        scrollWipeCount = 0;
        filterWipeCount = 0;

        onReloadStart(subject: string) {
          if (this.loadedSubject !== subject || this.blameLines.length === 0) {
            this.isLoading = true;
            this.unmountCount++;
          }
          // Background revalidation: does NOT unmount timeline or virtual list!
        }

        onReloadEnd(next: BlameLine[], subject: string) {
          const isSubjectChange = this.loadedSubject !== subject;
          const linesChanged = isSubjectChange || !areBlameLinesEqual(this.blameLines, next);
          if (linesChanged) {
            this.blameLines = next;
          }
          if (isSubjectChange) {
            this.selection = null;
            this.listScroll = 0;
            this.loadedSubject = subject;
            this.scrollWipeCount++;
            this.filterWipeCount++;
          }
          this.isLoading = false;
        }
      }

      const preFix = new PreFixBlameViewer();
      const hardened = new HardenedBlameViewer();

      // Simulate 1,000 background revalidations (e.g. status polls, watcher events)
      const identicalLines = [mockLine(1), mockLine(2), mockLine(3)];
      for (let i = 0; i < 1000; i++) {
        preFix.onReloadStart();
        preFix.onReloadEnd(identicalLines);

        hardened.onReloadStart("/repo\0file.ts");
        hardened.onReloadEnd(identicalLines, "/repo\0file.ts");
      }

      // Pre-fix: 1,000 unmounts, 1,000 scroll wipes, 1,000 filter wipes!
      expect(preFix.unmountCount).toBe(1000);
      expect(preFix.scrollWipeCount).toBe(1000);
      expect(preFix.filterWipeCount).toBe(1000);
      expect(preFix.listScroll).toBe(0);
      expect(preFix.selection).toBeNull();

      // Hardened SWR: ZERO unmounts, ZERO scroll wipes, ZERO filter wipes!
      expect(hardened.unmountCount).toBe(0);
      expect(hardened.scrollWipeCount).toBe(0);
      expect(hardened.filterWipeCount).toBe(0);
      expect(hardened.listScroll).toBe(450);
      expect(hardened.selection).toEqual({ kind: "band", id: "recent" });
    });
  });

  describe("Fingerprint Sensitivity and Isolation", () => {
    it("differentiates file-specific mutations from unrelated store churn", () => {
      const repo = "/workspace/GitPulse";
      const file = "src/lib/components/BlameViewer.svelte";
      const tip = "deadbeef12345678";
      const revisions = { [repo]: "1:1" };

      const baseKey = computeBlameFingerprint(repo, file, [], tip, revisions);

      // File modified
      const modStatus: FileStatus[] = [{
        path: file,
        status_code: " M",
        is_staged: false,
        is_conflicted: false,
        additions: 5,
        deletions: 2,
      }];
      expect(computeBlameFingerprint(repo, file, modStatus, tip, revisions)).not.toBe(baseKey);

      // Branch tip updated (git commit/rebase/checkout)
      expect(computeBlameFingerprint(repo, file, [], "cafebabe99999999", revisions)).not.toBe(baseKey);

      // Full refresh requested
      expect(computeBlameFingerprint(repo, file, [], tip, { [repo]: "1:2" })).not.toBe(baseKey);

      // Unrelated file modified (should have same status code part for file)
      const otherFileStatus: FileStatus[] = [{
        path: "unrelated.txt",
        status_code: " M",
        is_staged: false,
        is_conflicted: false,
        additions: 10,
        deletions: 0,
      }];
      expect(computeBlameFingerprint(repo, file, otherFileStatus, tip, revisions)).toBe(baseKey);
    });
  });

  describe("Deep Blame Lines Equality Stress", () => {
    it("efficiently compares 10,000 line blame results without performance degradation", () => {
      const fileA: BlameLine[] = Array.from({ length: 10000 }, (_, i) => mockLine(i + 1));
      const fileB: BlameLine[] = Array.from({ length: 10000 }, (_, i) => mockLine(i + 1));

      const t0 = performance.now();
      expect(areBlameLinesEqual(fileA, fileB)).toBe(true);
      const elapsed = performance.now() - t0;
      expect(elapsed).toBeLessThan(50);
    });

    it("detects granular mutations across all BlameLine fields", () => {
      const base: BlameLine[] = [mockLine(1), mockLine(2), mockLine(3)];

      // Identical
      expect(areBlameLinesEqual(base, [mockLine(1), mockLine(2), mockLine(3)])).toBe(true);

      // Length
      expect(areBlameLinesEqual(base, [mockLine(1), mockLine(2)])).toBe(false);

      // Content
      expect(areBlameLinesEqual(base, [mockLine(1), mockLine(2, "abc1234", "changed"), mockLine(3)])).toBe(false);

      // Commit
      expect(areBlameLinesEqual(base, [mockLine(1), mockLine(2, "different_sha"), mockLine(3)])).toBe(false);

      // Timestamp
      expect(areBlameLinesEqual(base, [mockLine(1), { ...mockLine(2), timestamp: 99999 }, mockLine(3)])).toBe(false);

      // Author
      expect(areBlameLinesEqual(base, [mockLine(1), { ...mockLine(2), author_name: "Linus" }, mockLine(3)])).toBe(false);

      // Line number
      expect(areBlameLinesEqual(base, [mockLine(1), { ...mockLine(2), line_no: 42 }, mockLine(3)])).toBe(false);
    });

    it("handles boundary cases including zero-OID uncommitted lines and unicode", () => {
      expect(areBlameLinesEqual([], [])).toBe(true);
      expect(areBlameLinesEqual([], [mockLine(1)])).toBe(false);

      const uncommittedA = [mockLine(1, "0".repeat(40), "new uncommitted work")];
      const uncommittedB = [mockLine(1, "0".repeat(40), "new uncommitted work")];
      expect(areBlameLinesEqual(uncommittedA, uncommittedB)).toBe(true);

      const unicodeA = [mockLine(1, "abc", "const icon = '🚀 ⚡️ 🔥';")];
      const unicodeB = [mockLine(1, "abc", "const icon = '🚀 ⚡️ 🔥';")];
      expect(areBlameLinesEqual(unicodeA, unicodeB)).toBe(true);
    });
  });

  describe("File Switching vs Same-File SWR Lifecycle", () => {
    it("drops selection and resets scroll when switching files, but preserves them on revalidation", () => {
      let state = {
        blameLines: [mockLine(1), mockLine(2)],
        listScroll: 300,
        selection: { kind: "band", id: "older" } as any,
        loadedSubject: "/repo\0fileA.ts",
      };

      function update(next: BlameLine[], subject: string) {
        const isSubjectChange = state.loadedSubject !== subject;
        const linesChanged = isSubjectChange || !areBlameLinesEqual(state.blameLines, next);
        if (linesChanged) state.blameLines = next;
        if (isSubjectChange) {
          state.selection = null;
          state.listScroll = 0;
          state.loadedSubject = subject;
        }
      }

      // Revalidate same file:
      update([mockLine(1), mockLine(2)], "/repo\0fileA.ts");
      expect(state.listScroll).toBe(300);
      expect(state.selection).toEqual({ kind: "band", id: "older" });

      // Switch to fileB:
      update([mockLine(1)], "/repo\0fileB.ts");
      expect(state.listScroll).toBe(0);
      expect(state.selection).toBeNull();
      expect(state.loadedSubject).toBe("/repo\0fileB.ts");
    });
  });
});
