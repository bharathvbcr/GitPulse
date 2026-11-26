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
  it("reuses ExplorerRail, MediaViewer, and EditorConflictBanner, and reads, drafts and saves through the shared open-file store", () => {
    expect(source).toContain('import ExplorerRail from "./files/ExplorerRail.svelte"');
    expect(source).toContain('import MediaViewer from "./files/MediaViewer.svelte"');
    expect(source).toContain('import EditorConflictBanner from "./files/EditorConflictBanner.svelte"');
    // One document per (checkout, path), shared with the Code page: no draft,
    // save queue run or registry write of the pane's own.
    expect(source).toContain('from "../files/openDocuments"');
    expect(source).toContain("saveDocument(repoPath, filePath, newContent, { overrideBase, overwrite })");
    expect(source).toContain("editDocument(repoPath, filePath, newContent, sourceContent)");
    expect(source).toContain("holdDocument(repo, path, holder)");
    expect(source).not.toContain("editorFileSaveQueue.run");
    expect(source).not.toContain("recordEditorDrafts");
    expect(source).not.toContain("let drafts");
  });

  it("answers for the drafts of the files it showed, except those another host still shows", () => {
    const own = source.slice(source.indexOf("function ownDirtyPaths"), source.indexOf("$effect(() => {", source.indexOf("function ownDirtyPaths")));
    expect(own).toContain("hasDraft(v.repo, v.path)");
    expect(own).toContain("!isHeldElsewhere(v.repo, v.path, holder)");
    expect(source).toContain("const dirtyPaths = ownDirtyPaths();");
    expect(source).toContain("for (const path of ownDirtyPaths()) discardDraft(repoPath, path);");
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

  it("supports inline Code | Diff toggle via cmd_get_file_diff + parseUnifiedDiff and exposes reloadIfClean for Follow Agent", () => {
    expect(source).toContain('data-testid="terminal-file-code-toggle"');
    expect(source).toContain('data-testid="terminal-file-diff-toggle"');
    expect(source).toContain('invoke<DiffPayload>("cmd_get_file_diff"');
    expect(source).toContain("parseUnifiedDiff(");
    expect(source).toContain("export function reloadIfClean()");

    const { body } = render(TerminalFilePane, {
      props: {
        tabId: "file-2",
        repoPath: "/repo",
        filePath: "src/main.rs",
      },
    });
    expect(body).toContain('data-testid="terminal-file-code-toggle"');
    expect(body).toContain('data-testid="terminal-file-diff-toggle"');
  });

  it("queries DevMap symbolsForFile + getNeighbors for stack-trace Quick-Lens callers when revealLine is set", async () => {
    const lens = await import("../terminal/symbolQuickLens");
    const picked = lens.pickEnclosingSymbol(
      [
        {
          symbol_name: "OuterModule",
          qualified_name: "src/app.ts::OuterModule",
          kind: "Module",
          span_start_line: 1,
          span_end_line: 120,
        },
        {
          symbol_name: "parseHeader",
          qualified_name: "src/app.ts::parseHeader",
          kind: "Function",
          span_start_line: 25,
          span_end_line: 55,
        },
      ],
      38,
    );
    expect(picked?.symbol_name).toBe("parseHeader");
    expect(source).toContain('data-testid="terminal-file-quick-lens"');
    expect(source).toContain('data-testid="terminal-file-send-symbol-lens"');
    expect(source).toContain("symbolsForFile(");
    expect(source).toContain("getNeighbors(");
  });

  it("respects defaultExplorerOpen when docked TerminalPanel sidebar is already showing ExplorerRail and exposes dock-to-sidebar action", () => {
    const { body } = render(TerminalFilePane, {
      props: {
        tabId: "file-no-dup-tree",
        repoPath: "/repo",
        defaultExplorerOpen: false,
        onscreen: true,
        onDockToSidebar: () => {},
      },
    });
    expect(body).not.toContain("data-terminal-file-explorer-card");
    expect(body).toContain('aria-label="Show file explorer"');
    expect(body).toContain('data-testid="terminal-file-dock-sidebar"');
    expect(source).toContain("explorerTouchedByUser");
    expect(source).toContain("prevDefaultExplorerOpen");
    expect(source).toContain("if (desired === false && prev !== false)");
  });

  it("routes Cmd/Ctrl+F and Cmd/Ctrl+G chords to the scoped file viewer via triggerFileSearch", () => {
    const chord = source.slice(
      source.indexOf("export function handleViewChord(event: KeyboardEvent): boolean"),
      source.indexOf("onDestroy(() =>"),
    );
    expect(chord).toContain('isCommitSearchChord(event) && triggerFileSearch(paneRoot, "find", extractSelectionSeed(null, paneRoot))');
    expect(chord).toContain('triggerFileSearch(paneRoot, "line")');
    expect(chord).toContain('el.addEventListener("keydown", onKeydown)');
  });

  it("opens the Explorer page from a double-click on its Explorer card's empty space, and from a toolbar button", () => {
    const panelSource = readFileSync(join(here, "TerminalPanel.svelte"), "utf8");
    const card = source.slice(source.lastIndexOf("<div", source.indexOf("data-terminal-file-explorer-card")), source.indexOf("<ExplorerRail"));
    expect(source).toContain('import { opensExplorerPage } from "../files/explorerPageGesture"');
    expect(source).toContain("onOpenExplorerPage?: () => void;");
    expect(card).toMatch(/ondblclick=\{\(event\) => \{\s*if \(onOpenExplorerPage && opensExplorerPage\(event\)\) onOpenExplorerPage\(\);\s*\}\}/);
    const button = source.indexOf('data-testid="terminal-file-open-explorer-page"');
    expect(button).toBeGreaterThan(-1);
    expect(source.slice(source.lastIndexOf("{#if onOpenExplorerPage}", button), button)).not.toContain("{/if}");
    expect(source.slice(button, source.indexOf("</button>", button))).toContain("onclick={onOpenExplorerPage}");
    // The terminal hands its host's page owner straight through.
    expect(panelSource).toContain("onOpenExplorerPage={onOpenExplorerPage}");
  });

  it("exits diff mode once per HTML preview or split open request nonce without locking Diff out", () => {
    expect(source).toContain("htmlViewModeRequest");
    expect(source).toContain("matchHtmlViewMode(req, repo, target)");
    expect(source).toContain("matched.nonce > lastHandledHtmlModeNonce");
  });

  it("exits diff mode once per SVG preview or split open request nonce without locking Diff out", () => {
    expect(source).toContain("svgViewModeRequest");
    expect(source).toContain("matchSvgViewMode(req, repo, target)");
    expect(source).toContain("matched.nonce > lastHandledSvgModeNonce");
  });
});