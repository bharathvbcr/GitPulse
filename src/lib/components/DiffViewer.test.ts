import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "DiffViewer.svelte"),
  "utf8"
);

describe("DiffViewer row chrome", () => {
  it("does not draw a horizontal rule under every diff line", () => {
    expect(source).not.toContain("divide-y");
    expect(source).not.toMatch(/border-y\s+border-border/);
  });

  it("lets long diff lines scroll instead of clipping silently", () => {
    // Unwrapped rows scroll horizontally, and nothing hard-clips a row: an
    // `overflow-hidden` row with a pinned height throws the remainder away.
    expect(source).not.toMatch(/overflow-hidden[^"']*whitespace-pre/);
    // Scoped to rows: the pane, its body and the minimap rail legitimately
    // clip, and a blanket ban would fail on those without saying anything
    // about a diff line.
    expect(source).not.toMatch(/wrapping \? 'overflow-hidden'/);
    expect(source).toContain("'overflow-x-auto'");
  });

  it("never pins a row's height while it is wrapping", () => {
    // The regression this guards: every row carried `height: ROW_HEIGHT` while
    // wrap set `whitespace-pre-wrap`, so a line wrapping to three visual rows
    // was squeezed into 20px — drawn over its neighbours, centred so only the
    // middle slice showed, with the start of the line invisible. Wrapping made
    // code disappear rather than reflow.
    expect(source).toContain("min-height: ${ROW_HEIGHT}px");
    expect(source).not.toContain('style="height: {ROW_HEIGHT}px;"');
  });

  it("stops windowing while wrapping, because rows are no longer uniform", () => {
    // VirtualList places row n at n * rowHeight. That is only true while every
    // row is that tall, which wrapping breaks.
    expect(source).toContain("virtualize={!wrapping}");
  });

  it("bounds how much it will render unwrapped, and says why when it will not", () => {
    // Rendering a wrapped diff in full is what makes it correct; the cap is
    // what keeps that from meaning a hundred thousand rows. A toggle that
    // silently does nothing is a control users keep pressing.
    expect(source).toContain("WRAP_MAX_LINES");
    expect(source).toContain("disabled={!wrapAvailable}");
    expect(source).toContain("Wrapping is unavailable above");
  });

  it("top-aligns a wrapped row instead of centring it", () => {
    // Centring is right for one line and wrong for three: it shows the middle
    // and cuts the rest.
    expect(source).toContain("wrapping ? 'items-start' : 'items-center'");
  });

  it("renders commit metadata and binary notices as their own row kinds", () => {
    expect(source).toContain('line.type === "meta"');
    expect(source).toContain('line.type === "binary"');
    expect(source).toMatch(/binary.*<\/span>\s*<span class="whitespace-pre">\{line\.content\}<\/span>/s);
  });

  it("drives the empty state from emptyDiffCopy so merges get merge copy", () => {
    expect(source).toContain("emptyDiffCopy(");
    expect(source).toContain("title={emptyCopy.title}");
    expect(source).toContain('hint={emptyCopy.hint}');
  });

  it("excludes metadata from the visible-lines stat", () => {
    expect(source).toContain("contentLineCount");
    expect(source.match(/lines\.length\.toLocaleString\(\)/)).toBeNull();
  });

  it("offers hunk and line-level staging only for working-tree files", () => {
    expect(source).toContain("Stage Hunk");
    expect(source).toContain("Unstage Hunk");
    expect(source).toContain("Stage Selected");
    expect(source).toContain("Unstage Selected");
    expect(source).toContain("isWorkingTreeFile");
    expect(source).toContain("statuses.some");
  });

  it("supports click/drag range selection on add and del lines", () => {
    expect(source).toContain("onLinePointerDown");
    expect(source).toContain("onLinePointerEnter");
    expect(source).toContain("selectRange");
    expect(source).toContain("onpointerdown");
  });

  it("annotates multi-line replacement blocks, not just adjacent pairs", () => {
    expect(source).toContain("replacementBlockBounds");
    expect(source).toContain("annotateRange(lines, bounds[0], bounds[1])");
  });
});

describe("DiffViewer store-emission memo guards", () => {
  it("clears selections and scroll via ONE effect keyed on the composite view key", () => {
    // repoStore republishes fresh objects every ~6s status poll; resetting
    // per emission erased selections mid-drag and yanked scroll to the top.
    // The single guard keys on path + commit + staged side + whitespace
    // mode, which are exactly the inputs that swap the fetched contents.
    const effectIdx = source.indexOf('join("\\u0000")');
    expect(effectIdx).toBeGreaterThan(-1);
    for (const dep of [
      "$repoStore.selectedFilePath",
      "$repoStore.selectedCommitId",
      "$repoStore.selectedIsStaged",
      "$repoStore.selectedIgnoreWhitespace",
    ]) {
      expect(source.indexOf(dep, 0)).toBeGreaterThan(-1);
    }
    const resetIdx = source.indexOf("selectedLines = new Set();", effectIdx);
    expect(resetIdx).toBeGreaterThan(effectIdx);
    expect(source.indexOf("unifiedScroll = 0;", resetIdx)).toBeGreaterThan(resetIdx);
    expect(source.indexOf("splitScroll = 0;", resetIdx)).toBeGreaterThan(resetIdx);
    expect(source.indexOf("isDragging = false;", resetIdx)).toBeGreaterThan(resetIdx);
    expect(source.indexOf("dragAnchor = null;", resetIdx)).toBeGreaterThan(resetIdx);
  });

  it("has no unconditional reset effects left over from the per-effect era", () => {
    // The old selection-wipe and scroll-reset effects ran on EVERY publish.
    expect(source).not.toContain("// Reset selection when switching files");
    expect(source).not.toContain("void $repoStore.selectedFilePath;");
    expect(source.match(/prevSelPath|prevScrollPath/g)).toBeNull();
  });

  it("refetches image blobs only when repo, path, or commit change", () => {
    expect(source).toContain("imageBlobKey");
    expect(source).toContain(
      'showingImage && path && repo ? `${repo}\\u0000${path}\\u0000${commitId ?? ""}` : null',
    );
    // Stale responses are dropped by request key, not by effect teardown
    // (which Svelte runs before every re-run, memo hits included).
    expect(source).toContain("if (imageBlobKey === requestKey)");
    expect(source.match(/return \(\) => \{\s*cancelled = true;\s*\};/g)).toBeNull();
  });
});

describe("DiffViewer parse caching and store-owned whitespace toggle", () => {
  it("parses through a module-scope reference-identity cache", () => {
    // Re-parsing up to 300k lines on every background publish — and with it
    // discarding the word-diff segments cache — was the main jank engine.
    expect(source).toContain("const parseCache = createParseCache();");
    expect(source).toContain("$derived(parseCache.parse($repoStore.selectedDiff))");
    expect(source).not.toContain("parseUnifiedDiff($repoStore.selectedDiff || \"\")");
  });

  it("delegates the whitespace toggle to the store instead of local state", () => {
    // The prev*/decideWhitespaceRefetch block double-fetched and showed
    // stale content after staged flips; the store owns refetching now.
    expect(source).not.toContain("whitespaceToggle");
    expect(source).not.toContain("decideWhitespaceRefetch");
    expect(source).not.toMatch(/let ignoreWhitespace = \$state/);
    expect(source).toContain("checked={$repoStore.selectedIgnoreWhitespace}");
    expect(source).toContain("repoStore.setIgnoreWhitespace(e.currentTarget.checked)");
  });

  it("imports replacementBlockBounds from wordDiff instead of redefining it", () => {
    expect(source).toMatch(/import \{[^}]*replacementBlockBounds[^}]*\} from "\.\.\/diff\/wordDiff"/s);
    expect(source).not.toContain("function replacementBlockBounds(index: number)");
  });
});

describe("DiffViewer staging safety and stats", () => {
  it("disables hunk staging when the diff is truncated", () => {
    expect(source).toContain("disabled={truncatedSource}");
    expect(source).toContain("Partial data");
  });

  it("counts only add/del/ctx rows as content lines", () => {
    const idx = source.indexOf("contentLineCount = $derived");
    const body = source.slice(idx, idx + 400);
    for (const kind of ['"add"', '"del"', '"ctx"']) {
      expect(body).toContain(kind);
    }
    // hdr rows are chrome: a hunk header must not inflate the line stat.
    expect(body).not.toContain('"hdr"');
  });

  it("keeps the EmptyState branch reachable on zero parsed rows", () => {
    expect(source).toContain("{#if lines.length === 0}");
    expect(source).toContain("<EmptyState");
  });

  it("renders LanguageLogo in header when a file path is selected", () => {
    expect(source).toContain("LanguageLogo");
    expect(source).toContain("filePath={$repoStore.selectedFilePath}");
  });
});
