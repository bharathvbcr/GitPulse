import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));

/**
 * Component behavior is pinned at the source level (house convention: the
 * vitest environment is node, so nothing mounts here). These assertions lock
 * the blame page's audit fixes in place: the explorer wiring, single-load-site
 * selection write-back, the zero-OID guard, coverage-failure surfacing, and
 * the freshness fingerprint.
 */
const source = readFileSync(join(here, "BlameViewer.svelte"), "utf8");

describe("BlameViewer file explorer integration", () => {
  it("mounts the explorer beside the blame pane with a toggle and unified w-72 styling", () => {
    expect(source).toContain('import FileTreePanel from "./files/FileTreePanel.svelte"');
    expect(source).toContain("{#if explorerOpen}");
    expect(source).toContain("<FileTreePanel />");
    expect(source).toContain("explorerOpen = !explorerOpen");
    expect(source).toContain('class="w-72 shrink-0 h-full overflow-hidden"');
    expect(source).toContain('key.toLowerCase() === "b"');
  });

  it("reads the selection instead of offering a second way to name a file", () => {
    // The path box is gone. It existed because Blame was a destination you
    // could arrive at with nothing selected; Blame is a section of Code now,
    // and Explorer — one click away, sharing `selectedFilePath` — is where a
    // file is chosen. A second picker here would be a second source of truth
    // for one subject, and the one that could disagree.
    expect(source).not.toContain("bind:value={filePath}");
    expect(source).not.toContain('placeholder="Path to file in repo..."');
    expect(source).not.toContain("repoStore.selectFilePath(");
    // The selection is still what drives the load, and it is still the only
    // thing that does.
    expect(source).toContain("const selected = $repoStore.selectedFilePath;");
  });

  it("keeps a way to ask for the same file again after a failure", () => {
    // Retry used to mean re-typing the path and pressing Enter — deleting the
    // box without replacing that would have removed the only recovery from a
    // failed blame. The store does not notify for an unchanged value, so the
    // retry calls the loader directly.
    expect(source).toContain("function retryBlame()");
    expect(source).toContain("void loadBlameFor(repo, path);");
    expect(source).toContain("onclick={retryBlame}");
  });
});

describe("BlameViewer audit fixes", () => {
  it("renders uncommitted (zero-OID) lines as plain text, not commit links", () => {
    expect(source).toContain("const ZERO_OID_RE = /^0+$/;");
    expect(source).toContain("ZERO_OID_RE.test(line.commit_id)");
    // The guarded branch renders a span, not a button that would inspect a
    // nonexistent commit.
    expect(source).toContain('title="Not committed yet"');
  });

  it("surfaces coverage fetch failure instead of swallowing it", () => {
    expect(source).toContain("coverageFailed");
    expect(source).not.toContain(".catch(() => new Map<number, number>())");
    expect(source).toContain("Coverage unavailable");
  });

  it("re-blames when blame inputs move, memoized on a fingerprint", () => {
    // Status of the blamed file + current branch tip participate in the key,
    // so watcher refreshes after external edits/commits refresh stale blame.
    expect(source).toContain("s.path === selected");
    expect(source).toContain("tip_commit_id");
    expect(source).toContain("if (key === prevKey) return;");
  });

  it("keeps every rejection routed through the diagnostics reporting seam", () => {
    // reportPanelError formats via formatError AND lands the failure in the
    // persistent diagnostics ring; the banner text contract is unchanged.
    expect(source).toContain('from "../diagnostics/report"');
    expect(source).toContain('reportPanelError("blame", err)');
    expect(source).not.toMatch(/String\(\s*(err|reason|error|e)\s*\)/);
  });
});
