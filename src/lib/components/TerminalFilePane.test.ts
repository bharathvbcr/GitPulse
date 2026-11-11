import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import TerminalFilePane from "./TerminalFilePane.svelte";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "TerminalFilePane.svelte"), "utf8");
const mediaSource = readFileSync(join(here, "files", "MediaViewer.svelte"), "utf8");
const railSource = readFileSync(join(here, "files", "ExplorerRail.svelte"), "utf8");

describe("TerminalFilePane contracts", () => {
  it("reuses ExplorerRail, MediaViewer, and EditorConflictBanner with shared serialSave + draft registry", () => {
    expect(source).toContain('import ExplorerRail from "./files/ExplorerRail.svelte"');
    expect(source).toContain('import MediaViewer from "./files/MediaViewer.svelte"');
    expect(source).toContain('import EditorConflictBanner from "./files/EditorConflictBanner.svelte"');
    expect(source).toContain("editorFileSaveQueue.run(fileSaveKey(repo, path)");
    expect(source).toContain("recordEditorDrafts(repoPath");
  });

  it("threads explicit repoPath into ExplorerRail, MediaViewer, and MarkDevViewer, and retries fallbackPath on subdir false-hits", () => {
    expect(source).toContain("fallbackPath = undefined");
    expect(source).toContain("onResolvedPath");
    expect(source).toContain("<ExplorerRail");
    expect(source).toContain("{repoPath}");
    expect(railSource).toContain("repoPath = null");
    expect(mediaSource).toContain("repoPathOverride: effectiveRepoPath");
    expect(mediaSource).toContain("revealTarget,");
    expect(mediaSource).toContain("onSendSelectionToAgent,");
  });

  it("renders empty prompt when opened without a file path and exposes explorer toggle", () => {
    const { body } = render(TerminalFilePane, {
      props: {
        tabId: "file-1",
        repoPath: "/repo",
      },
    });
    expect(body).toContain("data-terminal-file-pane=\"file-1\"");
    expect(body).toContain("data-testid=\"terminal-file-pane-explorer-toggle\"");
    expect(body).toContain("Choose a file from the explorer");
  });
});
