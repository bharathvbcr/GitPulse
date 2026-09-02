import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "../FileViewer.svelte"), "utf8");

describe("FileViewer", () => {
  it("imports and orchestrates FileTreePanel, MediaViewer, and LivePulseDashboard", () => {
    expect(source).toContain("FileTreePanel");
    expect(source).toContain("MediaViewer");
    expect(source).toContain("LivePulseDashboard");
  });

  it("loads file content via cmd_get_file_blob with async protection", () => {
    // FileBlobPayload was a component-local copy of the canonical FileBlob,
    // field-for-field identical and unable to fail on drift. Asserting the
    // shared type keeps a local re-declaration from creeping back.
    expect(source).toContain('invoke<FileBlob>("cmd_get_file_blob"');
    expect(source).toContain('import type { FileBlob } from "../files/types"');
    expect(source).toContain("createAsyncGuard");
    expect(source).toContain("guard.isLive()");
  });

  it("keeps editor tabs across view remounts with preview vs pin semantics", () => {
    expect(source).toContain("createRepoPanelCache");
    expect(source).toContain("openPreview");
    expect(source).toContain("openPinned");
    expect(source).toContain("activateEditorTab");
    expect(source).toContain("onPinFile");
    expect(source).toContain("closeTab");
    expect(source).toContain("closeAllTabs");
    expect(source).toContain("closeOtherTabs");
    expect(source).toContain("pathSegments");
  });

  it("toggles explorer and dashboard from the keyboard without stealing Cmd+W", () => {
    expect(source).toContain("explorerOpen");
    expect(source).toContain("dashboardOpen");
    expect(source).toContain('"b"');
    expect(source).toContain('"d"');
    expect(source).not.toContain('"w"');
  });

  it("writes file content via cmd_write_file_content and opens paths through joinWorktreePath", () => {
    expect(source).toContain('invoke("cmd_write_file_content"');
    expect(source).toContain("joinWorktreePath");
    expect(source).toContain("formatError");
  });

  it("renders LanguageLogo for open editor tabs", () => {
    expect(source).toContain("LanguageLogo");
    expect(source).toContain("filePath={tab.path}");
  });
});
