import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import Sidebar, { workingTreeSegments } from "./Sidebar.svelte";
import { summarizeStatuses, type StatusLike } from "../files/fileStatus";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "Sidebar.svelte"),
  "utf8"
);

describe("Sidebar", () => {
  it("labels its icon-only controls for screen readers", () => {
    const { body } = render(Sidebar);
    // Gold standard (FilterBar): every icon-only button carries an explicit
    // aria-label alongside its title.
    expect(body).toContain('aria-label="Open Repository"');
  });

  it("keeps the per-file stage/unstage affordances titled", () => {
    // File rows render from backend state (absent under SSR), so these are
    // asserted at source level like DiffViewer.test.ts does.
    expect(source).toContain('title="Unstage file"');
    expect(source).toContain('title="Stage file"');
  });
});

describe("Sidebar batch staging", () => {
  it("offers stage all and unstage all in the change-list headers", () => {
    expect(source).toContain("stageAll");
    expect(source).toContain("unstageAll");
    expect(source).toContain("stage all");
    expect(source).toContain("unstage all");
  });
});

describe("Sidebar resize shell", () => {
  it("renders an accessible vertical separator with live value metadata", () => {
    expect(source).toContain('role="separator"');
    expect(source).toContain('aria-orientation="vertical"');
    expect(source).toContain('aria-label="Resize sidebar"');
    expect(source).toContain("aria-valuenow=");
    expect(source).toContain("aria-valuemin={SIDEBAR_MIN_WIDTH}");
    expect(source).toContain("aria-valuemax={SIDEBAR_MAX_WIDTH}");
    expect(source).toContain("tabindex=\"0\"");
  });

  it("resizes via pointer capture and keyboard steps, resetting on double-click", () => {
    expect(source).toContain("setPointerCapture");
    expect(source).toContain("SIDEBAR_RESIZE_STEP");
    expect(source).toContain('"ArrowLeft"');
    expect(source).toContain('"ArrowRight"');
    expect(source).toContain('case "Home"');
    expect(source).toContain('case "End"');
    // Double-click resets — but NOT within the post-drag suppression window:
    // a two-click fine-tune must never snap back to the default mid-gesture.
    expect(source).toContain("lastDragEndAt");
    expect(source).toMatch(/ondblclick=\{\(\) => \{\s*if \(Date\.now\(\) - lastDragEndAt > 250\)\s*layoutStore\.setWidth\(SIDEBAR_DEFAULT_WIDTH\);/);
  });

  it("drives width from the persisted layout store, not a hardcoded class", () => {
    expect(source).not.toContain("w-80 ");
    expect(source).toMatch(/style="width:\{\$layoutStore/);
    // No width transition mid-drag; smooth otherwise.
    expect(source).toContain("transition-[width] duration-(--gp-dur-fast)");
    expect(source).toContain("if (!dragging) return;");
  });

  it("hides the resizer while collapsed", () => {
    const resizer = source.slice(source.indexOf('role="separator"'));
    const railStart = source.indexOf("Collapsed rail");
    expect(railStart).toBeGreaterThan(-1);
    expect(resizer.length).toBeGreaterThan(0);
    // The separator block is gated behind the same !collapsed condition as the
    // expanded shell (the only {#if} wrapping it).
    expect(source.match(/\{#if !\$layoutStore\.collapsed\}/g)?.length).toBe(2);
  });
});

describe("Sidebar collapse toggle", () => {
  it("toggles and persists collapse state from the header and the rail", () => {
    expect((source.match(/layoutStore\.toggleCollapsed\(\)/g) ?? []).length).toBe(2);
    expect(source).toContain("PanelLeftClose");
    expect(source).toContain("PanelLeftOpen");
    expect(source).toContain('aria-label="Collapse sidebar"');
    expect(source).toContain('aria-label="Expand sidebar"');
    expect(source).toContain("SIDEBAR_COLLAPSED_WIDTH");
  });

  it("shows the dirty-file badge with warn emphasis on the collapsed rail", () => {
    expect(source).toContain("{dirtyCount}");
    expect(source).toMatch(/dirtyCount > 0\s*\?\s*'bg-warn\/15 text-warn/);
  });
});

describe("Sidebar repo pulse strip", () => {
  it("guards every derivation against a missing current branch", () => {
    // Detached HEAD must not crash the strip or leak remote counts.
    expect(source).toContain('$repoStore.currentBranch ?? "detached"');
    expect(source).toContain("b.is_current && !b.is_remote");
    expect(source).toContain("currentBranchInfo?.ahead_count ?? 0");
    expect(source).toContain("currentBranchInfo?.behind_count ?? 0");
  });

  it("shows upstream sync chips and the default marker for the current branch only", () => {
    expect(source).toContain("{#if aheadCount > 0}");
    expect(source).toContain("{#if behindCount > 0}");
    expect(source).toContain("{#if currentBranchInfo?.is_default}");
    // Chip styling matches BranchList's ok-up / warn-down pills.
    expect(source).toContain("bg-ok/15 text-ok border border-ok/35");
    expect(source).toContain("bg-warn/15 text-warn border border-warn/35");
  });

  it("summarizes the working tree with additions, deletions, conflicts, or clean state", () => {
    expect(source).toContain("+{totalAdditions}");
    expect(source).toContain("−{totalDeletions}");
    expect(source).toContain("{dirtyCount} changed file{dirtyCount === 1 ? \"\" : \"s\"}");
    expect(source).toContain("{#if conflictedCount > 0}");
    expect(source).toContain("<AlertTriangle");
    expect(source).toContain("Working tree clean");
    expect(source).toContain("<CheckCircle2");
  });

  it("lays a clean working tree out as a title row plus an empty change bar", () => {
    const card = source.slice(
      source.indexOf("At-a-glance repo pulse"),
      source.indexOf("Main Scrollable Section"),
    );
    const title = card.indexOf(">Working tree clean<");
    const bar = card.indexOf("data-working-tree-bar");
    // The old status was one 16px line. The title has to sit above a track.
    expect(title).toBeGreaterThan(-1);
    expect(bar).toBeGreaterThan(title);
    expect(card).toContain('class="flex h-1.5 w-full overflow-hidden rounded-full');
    expect(card).toContain("{#each changeSegments as segment (segment.kind)}");
    expect(card).toContain('style:flex="{segment.count} 0 0px"');
    expect(card).toContain('$repoStore.currentBranch ?? "detached"');
  });

  it("sizes the change bar from staged, unstaged, untracked, and conflict counts", () => {
    const row = (partial: Partial<StatusLike> & Pick<StatusLike, "path" | "status_code">): StatusLike => ({
      is_staged: false,
      is_conflicted: false,
      additions: 0,
      deletions: 0,
      ...partial,
    });
    const summary = summarizeStatuses([
      row({ path: "a.ts", status_code: "M ", is_staged: true }),
      row({ path: "b.ts", status_code: "M ", is_staged: true }),
      row({ path: "c.ts", status_code: " M" }),
      row({ path: "d.ts", status_code: "??" }),
      row({ path: "e.ts", status_code: "UU", is_conflicted: true }),
    ]);
    expect(summary).toMatchObject({ staged: 2, unstaged: 1, untracked: 1, conflicted: 1, dirty: 5 });
    expect(workingTreeSegments(summary)).toEqual([
      { kind: "staged", count: 2, percent: 40 },
      { kind: "unstaged", count: 1, percent: 20 },
      { kind: "untracked", count: 1, percent: 20 },
      { kind: "conflict", count: 1, percent: 20 },
    ]);
    // A file staged and unstaged at once counts in both segments, once as a dirty file.
    const mixed = summarizeStatuses([
      row({ path: "mixed.ts", status_code: "MM", is_staged: true, additions: 3, deletions: 2 }),
    ]);
    expect(mixed.dirty).toBe(1);
    expect(workingTreeSegments(mixed)).toEqual([
      { kind: "staged", count: 1, percent: 50 },
      { kind: "unstaged", count: 1, percent: 50 },
    ]);
    expect(workingTreeSegments({ staged: 0, unstaged: 0, untracked: 0, conflicted: 0 })).toEqual([]);
  });

  it("stays hidden when no repository is open", () => {
    expect(source).toContain('{#if $repoStore.currentPath}');
  });
});

describe("Sidebar quick actions", () => {
  it("wires fetch/pull/push/stash as fire-and-forget actions disabled while loading", () => {
    expect(source).toContain("void repoStore.fetch()");
    expect(source).toContain("void repoStore.pull()");
    expect(source).toContain("void repoStore.push()");
    expect(source).toContain("void repoStore.stashSave()");
    expect((source.match(/disabled={\$repoStore\.isLoading}/g) ?? []).length).toBeGreaterThanOrEqual(4);
    expect(source).toContain('aria-label="Fetch from remote"');
    expect(source).toContain('aria-label="Pull from upstream"');
    expect(source).toContain('aria-label="Push to upstream"');
    expect(source).toContain('aria-label="Stash changes"');
  });

  it("shows fetch, pull, push, and stash as pills that become icons before they overflow", () => {
    const start = source.indexOf("<!-- Quick actions -->");
    const row = source.slice(start, source.indexOf("</div>", start));
    expect(row).toContain("gp-btn");
    expect(row).toContain("flex-1");
    expect(row).toContain("min-w-0");
    expect(row).toContain("@container/actions");
    expect(row).not.toContain("gp-icon-btn");
    // Visible words. Stash is the short label; the tooltip stays the full one.
    expect(row).toContain(">Fetch</span>");
    expect(row).toContain(">Pull</span>");
    expect(row).toContain(">Push</span>");
    expect(row).toContain(">Stash</span>");
    expect(row).not.toContain(">Stash changes</span>");
    expect(row).toContain('title="Stash changes"');
    expect(row).toContain('aria-label="Fetch from remote"');
    expect(row).toContain('aria-label="Pull from upstream"');
    expect(row).toContain('aria-label="Push to upstream"');
    expect(row).toContain('aria-label="Stash changes"');
    // The row's own width, not the collapsed rail, hides the words together.
    expect(row).toContain("@max-[279px]/actions:hidden");
    expect(row.match(/@max-\[279px\]\/actions:hidden/g)?.length).toBe(4);
  });
});

describe("Sidebar change lists", () => {
  it("filters both lists through one shared case-insensitive input", () => {
    expect(source).toContain('placeholder="Filter files…"');
    expect(source).toContain('aria-label="Filter files"');
    expect(source).toContain("path.toLowerCase().includes(query)");
    expect(source).toContain("${filteredStaged.length} of ${stagedFiles.length}");
    expect(source).toContain("${filteredUnstaged.length} of ${unstagedFiles.length}");
    expect(source).toMatch(/isFiltering \? `No matches for '\$\{fileFilter\}'` : /);
  });

  it("keeps stage/unstage batch actions out of collapsed sections", () => {
    expect(source).toContain("!sections.staged && stagedFiles.length > 0");
    expect(source).toContain("!sections.unstaged && unstagedFiles.length > 0");
    expect(source).toContain("aria-expanded={!sections.staged}");
    expect(source).toContain("aria-expanded={!sections.unstaged}");
  });

  it("grows result windows stepwise instead of jumping to full length", () => {
    expect(source).toContain("FILE_LIST_STEP * 2");
    expect(source).toContain("`Show ${remaining.toLocaleString()} more`");
    expect(source).toContain("`Show all ${total.toLocaleString()}`");
    expect(source).toContain("limit + FILE_LIST_STEP");
  });

  it("styles conflicted paths warn-semibold in both lists via one helper", () => {
    const uses = source.match(/pathClass\(f\.is_conflicted\)/g) ?? [];
    expect(uses.length).toBe(2);
    expect(source).toContain('return conflicted ? "text-warn font-semibold" : "text-textPrimary";');
  });

  it("no longer nests interactive buttons inside a role=button row", () => {
    expect(source).not.toContain('role="button"');
    expect(source).not.toContain("onkeydown={(e) =>");
    // The path itself is now a real button that opens the diff.
    expect((source.match(/repoStore\.selectFileDiff\(f\.path, true\)/g) ?? []).length).toBe(1);
    expect((source.match(/repoStore\.selectFileDiff\(f\.path, false\)/g) ?? []).length).toBe(1);
    expect(source).toContain('aria-label="Stage file"');
    expect(source).toContain('aria-label="Unstage file"');
  });

  it("uses roomier spacing per the density pass", () => {
    expect(source).toContain("overflow-y-auto p-3 space-y-5");
    expect(source).toContain("py-1.5 rounded-full flex items-center gap-1");
    expect(source).toContain("py-1.5");
  });

  it("persists section collapse flags under the sections storage key", async () => {
    expect(source).toContain('from "../sidebar/layoutStore"');
    const layoutStoreSource = readFileSync(
      join(dirname(fileURLToPath(import.meta.url)), "../sidebar/layoutStore.ts"),
      "utf8"
    );
    expect(layoutStoreSource).toContain('"gitpulse_sidebar_sections"');
    expect(layoutStoreSource).toContain('"gitpulse_sidebar_layout"');
    expect(source).toContain("saveSections(sections)");
    expect(source).toContain("loadSections(readSectionsRaw())");
  });

  it("renders LanguageLogo and formatPathParts for staged and unstaged files", () => {
    expect(source).toContain("LanguageLogo");
    expect(source).toContain("formatPathParts");
    expect(source).toContain("filePath={f.path}");
  });
});
