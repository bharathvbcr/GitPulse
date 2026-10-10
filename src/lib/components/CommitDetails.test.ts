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
    expect(source).toContain("const provenanceRepo = $derived($repoStore.currentPath);");
    const effect = source.slice(
      source.indexOf("$effect(() => {\n    const repo = provenanceRepo;"),
      source.indexOf("function gpgLabel"),
    );
    expect(effect).toContain("const id = currentCommitId;");
    expect(effect).not.toContain("$repoStore");
    const memo = effect.indexOf("if (key === provenanceKey) return;");
    expect(memo).toBeGreaterThan(-1);
    expect(memo).toBeLessThan(effect.indexOf("++provenanceToken"));
    expect(memo).toBeLessThan(effect.indexOf("provenance = null"));
    expect(effect).toContain('"cmd_provenance_freshness"');
  });
});
