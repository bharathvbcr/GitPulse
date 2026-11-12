import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "BranchCleanupModal.svelte"),
  "utf8",
);

describe("BranchCleanupModal backups read failure", () => {
  const start = source.indexOf("async function loadBackups()");
  const load = source.slice(start, source.indexOf("\n  }", start));

  it("records why the list could not be read and clears it on a good read", () => {
    expect(source).toContain("let backupsError = $state<string | null>(null);");
    const [ok, failed] = load.split("} catch");
    expect(ok).toContain("backupsError = null;");
    expect(failed).toContain('backupsError = reportPanelError("branch-cleanup", err);');
  });

  it("records the failure in diagnostics, redacted, under its own name", () => {
    // formatError would show the text but leave no record and skip the
    // credential redaction the diagnostics seam applies. The modal opens from
    // several surfaces (the Ops pane that named it is retired), so it reports
    // as itself.
    expect(source).toContain('import { reportPanelError } from "../diagnostics/report";');
    expect(source).not.toContain("formatError");
  });

  it("says a retry is running, so a repeated identical failure is not mistaken for no reaction", () => {
    const [, failed] = load.split("} catch");
    expect(load).toContain("backupsLoading = true;");
    expect(failed).toContain("finally {");
    expect(failed).toContain("if (seq === backupsSeq) backupsLoading = false;");
    const view = source.slice(source.indexOf("{#if backupsError}"));
    const errorView = view.slice(0, view.indexOf("{:else if backups.length === 0}"));
    expect(errorView).toContain("disabled={backupsLoading}");
    expect(errorView).toContain('{backupsLoading ? "Trying again…" : "Try again"}');
  });

  it("never lets the empty-state sentence stand in for a failed read", () => {
    const view = source.slice(source.indexOf("<!-- Backups List View -->"));
    const errorBranch = view.indexOf("{#if backupsError}");
    const emptyBranch = view.indexOf("{:else if backups.length === 0}");
    expect(errorBranch).toBeGreaterThan(-1);
    // The error branch is the first one, so the empty state is its else.
    expect(emptyBranch).toBeGreaterThan(errorBranch);
    expect(view.indexOf("No branch backups found")).toBeGreaterThan(emptyBranch);
    const errorView = view.slice(errorBranch, emptyBranch);
    expect(errorView).toContain('role="alert"');
    expect(errorView).toContain("Couldn't read branch backups.");
    expect(errorView).toContain("{backupsError}");
    expect(errorView).toContain("onclick={() => void loadBackups()}");
    expect(errorView).toContain("Try again");
  });

  it("does not print a count for a list it could not read", () => {
    expect(source).toContain('Backups ({backupsError ? "?" : backups.length})');
  });
});

describe("BranchCleanupModal scan trigger", () => {
  it("rescans only when the modal opens or the repository changes", () => {
    // repoStore publishes a fresh object per keystroke and per status poll;
    // reading `$repoStore.currentPath` in the effect rescanned on each one
    // and reset the reader's selection to the defaults.
    expect(source).toContain("const scanRepoPath = $derived($currentRepoPath);");
    const call = source.indexOf("void loadBackups();");
    expect(call).toBeGreaterThan(-1);
    const effect = source.slice(source.lastIndexOf("$effect(() => {", call), call);
    expect(effect).toContain("if (isOpen && scanRepoPath)");
    expect(effect).not.toContain("$repoStore");
    // runScan reads the filters before its first await; untracked, so a
    // filter edit rescans through its own onchange and not a second time here.
    expect(effect).toContain("untrack(() => {");
    expect(effect.indexOf("untrack(() => {")).toBeLessThan(effect.indexOf("void runScan();"));
  });
});
