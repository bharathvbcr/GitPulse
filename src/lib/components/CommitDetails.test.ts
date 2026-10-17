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
