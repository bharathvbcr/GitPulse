import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import CommitDetails from "./CommitDetails.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "CommitDetails.svelte"),
  "utf8",
);

describe("CommitDetails", () => {
  it("renders empty state when no commit is selected", () => {
    const { body } = render(CommitDetails);
    expect(body).toBeDefined();
  });

  it("integrates LanguageLogo and formatPathParts for changed files", () => {
    expect(source).toContain("LanguageLogo");
    expect(source).toContain("formatPathParts");
    expect(source).toContain("filePath={f.path}");
  });

  it("labels the signature from the one-commit verification, not from the details", () => {
    expect(source).toContain("signatureLabel(details?.signature_check, details != null && details.signature_check === undefined)");
    expect(source).not.toContain("gpg_status");
    expect(source).not.toContain('"Unsigned"');
  });

  it("renders the cut-output notice and marks partial totals from the shared summary", () => {
    expect(source).toContain("summarizeCommitFiles(details)");
    expect(source).toContain('data-testid="commit-files-note"');
    expect(source).toContain('filesSummary.partial ? "+" : ""');
    expect(source).not.toContain("changedFileCount");
  });

  it("only announces clipboard success after the shared copy seam succeeds", () => {
    const shaCopy = source.slice(source.indexOf("async function copySha"), source.indexOf("async function copyMessage"));
    const messageCopy = source.slice(source.indexOf("async function copyMessage"), source.indexOf("async function explainCommit"));
    expect(shaCopy).toContain("if (!(await copyText(id)))");
    expect(messageCopy).toContain("if (!(await copyText(msg)))");
    expect(shaCopy).toContain("toastStore.error");
    expect(messageCopy).toContain("toastStore.error");
  });


  /**
   * The provenance effect tracked the whole repo store, so every draft
   * keystroke and status poll blanked the badge and re-issued the IPC for the
   * commit already on screen. It is keyed on repository + commit now, and only
   * a new key blanks the badge or outdates an in-flight request.
   */
  it("fetches provenance once per repository and commit, not per store publish", () => {
    // Keyed on the shared repository path and the derived commit id, both
    // compared by value: the effect itself runs only on a new target, so it
    // needs no hand-kept key in front of the fetch.
    const start = source.indexOf("$effect(() => {\n    const repo = $currentRepoPath;\n    const id = currentCommitId;");
    expect(start).toBeGreaterThan(-1);
    const effect = source.slice(start, source.indexOf("// The label comes from the separate one-commit verification"));
    expect(effect).not.toContain("$repoStore");
    expect(source).not.toContain("provenanceKey");
    expect(effect.indexOf("++provenanceToken")).toBeLessThan(effect.indexOf("provenance = null"));
    expect(effect).toContain('"cmd_provenance_freshness"');
  });
});

describe("CommitDetails copy controls keyboard access", () => {
  it("shows the copy-message button on focus and gives both copy buttons accessible names", () => {
    const start = source.indexOf("onclick={() => copyMessage(headerSummary)}");
    expect(start).toBeGreaterThan(-1);
    const button = source.slice(start, source.indexOf("</button>", start));
    expect(button).toContain("group-focus-within/msg:opacity-100");
    expect(button).toContain("focus-visible:opacity-100");
    expect(button).toContain('aria-label="Copy commit message"');
    expect(source).toContain('aria-label="Copy commit SHA"');
    expect(source).toContain("group-hover/sha:opacity-100 group-focus-within/sha:opacity-100");
  });
});

describe("CommitDetails explain error scope", () => {
  it("clears a failed explanation when the commit or repository changes", () => {
    const start = source.indexOf("// A new commit or repository outdates an explanation");
    expect(start).toBeGreaterThan(-1);
    const untrackStart = source.indexOf("untrack(() => {", start);
    const block = source.slice(untrackStart, source.indexOf("});\n  });", untrackStart));
    expect(block).toContain("explainGuard?.cancel();");
    expect(block).toContain("explainError = null;");
  });

  it("holds the copied-flash timers and clears them when the pane unmounts", () => {
    expect(source).toContain("copiedShaTimer = setTimeout(");
    expect(source).toContain("copiedMessageTimer = setTimeout(");
    expect(source).toContain("clearTimeout(copiedShaTimer);");
    expect(source).toContain("clearTimeout(copiedMessageTimer);");
  });
});

describe("CommitDetails single current commit", () => {
  it("takes its commit id from the same resolver as the commit list", () => {
    expect(source).toContain('import { resolveCurrentCommitId } from "../graph/currentCommit"');
    expect(source).toContain("let currentCommitId = $derived(\n    resolveCurrentCommitId(");
    expect(source).not.toContain("$repoStore.selectedCommitId || $graphStore.selectedCommit?.id");
  });

  it("never renders a zero file count for details that are not loaded", () => {
    expect(source).toContain('Changed files ({details ? filesSummary.count : "--"})');
    expect(source).not.toContain("Changed files ({filesSummary.count})");
  });

  it("tells a failed details read apart from an empty commit and offers Try again", () => {
    expect(source).toContain('{#if detailsStatus === "error"}');
    expect(source).toContain("Couldn't load commit details. {detailsError}");
    expect(source).toContain("Try again</button>");
    expect(source).toContain("onclick={retryDetails}");
    expect(source).toContain("graphStore.selectCommit(selectedCommit)");
  });

  it("orders the preview states: reading, failed, text, empty commit, no commit", () => {
    const start = source.indexOf("{#if diffLoading}");
    expect(start).toBeGreaterThan(-1);
    const preview = source.slice(start);
    const order = [
      "{#if diffLoading}",
      "{:else if diffError}",
      "{:else if previewLines.length > 0}",
      "{:else if currentCommitId !== null}",
      "{:else}",
    ].map((marker) => preview.indexOf(marker));
    expect(order.every((i) => i > -1)).toBe(true);
    expect([...order].sort((a, b) => a - b)).toEqual(order);
    expect(preview).toContain("Reading the diff…");
    expect(preview).toContain("Couldn't read this diff. {diffError}");
    expect(preview).toContain("No changes in this commit");
    expect(preview).toContain("No changes in this file");
    // "No commit selected" is reserved for having no commit at all.
    expect(preview.indexOf("No commit selected")).toBeGreaterThan(preview.indexOf("{:else if currentCommitId !== null}"));
    expect(source).toContain("$repoStore.selectedDiffPending");
    expect(source).toContain("repoStore.selectCommitFileDiff(id, filePath)");
    expect(source).toContain("repoStore.selectCommitDiff(id)");
  });

  it("derives the header commit from that id, never from a different commit", () => {
    const start = source.indexOf("let selectedCommit = $derived(");
    const block = source.slice(start, source.indexOf("let details = $state"));
    expect(block).toContain("currentCommitId === null");
    expect(block).toContain("$graphStore.selectedCommit?.id === currentCommitId");
  });
});
