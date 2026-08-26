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
  it("mounts the explorer beside the blame pane with a toggle", () => {
    expect(source).toContain('import FileExplorer from "./FileExplorer.svelte"');
    expect(source).toContain("{#if explorerOpen}");
    expect(source).toContain("<FileExplorer />");
    expect(source).toContain("explorerOpen = !explorerOpen");
  });

  it("routes manual path entry through the store's single selection site", () => {
    // New values must go through selectFilePath (which also persists across
    // tab switches); only an identical resubmit may load directly.
    expect(source).toContain("repoStore.selectFilePath(path)");
    const directBranch = source.indexOf("$repoStore.selectedFilePath === path");
    expect(directBranch).toBeGreaterThan(-1);
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
