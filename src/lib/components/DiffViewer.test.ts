import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "DiffViewer.svelte"),
  "utf8"
);

describe("DiffViewer truncation honesty", () => {
  /**
   * The backend caps what it reads, so a diff can be a prefix without the
   * viewer being able to tell from the rows alone: the last hunk on screen
   * looks exactly like the last hunk in the commit.
   */
  it("treats a backend-truncated diff as truncated, not merely a short one", () => {
    expect(source).toContain("selectedDiffTruncated");
    expect(source).toMatch(/cutByBackend\s*\|\|\s*cutByRenderer/);
  });

  /**
   * The staging lockout is the part that matters for correctness: building a
   * patch from a prefix stages less than the rows on screen imply, silently.
   * It must key off the combined flag, never off the renderer's cut alone.
   */
  it("disables staging on any truncation, including the backend's", () => {
    expect(source).toContain("disabled={truncatedSource}");
    // The render slice is the only thing allowed to key off the renderer cut;
    // if `lines` were sliced by the combined flag, a backend-truncated diff
    // would render nothing at all.
    expect(source).toMatch(/lines = \$derived\(cutByRenderer \?/);
  });

  it("locks the line-selection actions too, not only the hunk button", () => {
    // A selection made before the notice appeared would otherwise still stage
    // from a prefix.
    const bar = source.slice(source.indexOf("Selection bar"));
    expect(bar).toContain("Stage Selected");
    expect((bar.match(/disabled=\{truncatedSource\}/g) ?? []).length).toBeGreaterThanOrEqual(2);
  });

  it("tells the user which cut happened rather than one generic message", () => {
    expect(source).toContain("{#if cutByBackend}");
    expect(source).toMatch(/larger than GitPulse reads/i);
    expect(source).toMatch(/Staging is disabled/i);
  });
});

describe("DiffViewer identity comes from the diff, not from the selection", () => {
  /**
   * The regression: the header printed `selectedFilePath` whatever the body
   * held, so a commit-wide or worktree-wide diff was labelled with the last
   * file the reader had clicked — its name, its language icon and its line
   * count over a body showing something else entirely.
   */
  it("titles the pane from the parsed outline", () => {
    expect(source).toContain("buildOutline(lines)");
    expect(source).toMatch(/const title = \$derived\(\s*outlineTitle\(/);
    expect(source).not.toMatch(/\{\s*\$repoStore\.selectedFilePath \|\| \$repoStore\.selectedCommitId \|\| "Diff View"\s*\}/);
  });

  it("picks the language icon from the outline before the selection", () => {
    expect(source).toContain("outlineLanguagePath(outline, $repoStore.selectedFilePath)");
    expect(source).toContain("<LanguageLogo filePath={languagePath}");
  });

  it("colours each row in ITS file's language, not the header's", () => {
    // A commit diff is a stack of files in different languages; one global
    // language painted a 200-file commit's Rust with the JSON tokenizer,
    // because the reader's last click had landed on a `.json`.
    expect(source).toContain("languagePathForLine(outline, index, $repoStore.selectedFilePath)");
    expect(source).toContain("spansFor(line, text, languageForLine(index), syntaxActive, ranges)");
    // And no single `language` is left for a row to fall back to.
    expect(source).not.toMatch(/const language = \$derived\(detectLanguageFromPath/);
  });

  it("shows the diff's own churn and status rather than only a line count", () => {
    expect(source).toContain("churnSummary(outline.additions, outline.deletions)");
    expect(source).toContain("sectionStatus(singleSection)");
  });

  it("keeps the content-line stat free of chrome rows", () => {
    const idx = source.indexOf("contentLineCount = $derived");
    const body = source.slice(idx, idx + 400);
    for (const kind of ['"add"', '"del"', '"ctx"']) {
      expect(body).toContain(kind);
    }
    // hdr rows are chrome: a hunk header must not inflate the line stat.
    expect(body).not.toContain('"hdr"');
    // And the header prints that stat, not the raw row count.
    expect(source).toContain("{contentLineCount.toLocaleString()} lines");
  });
});

describe("DiffViewer row chrome", () => {
  it("does not draw a horizontal rule under every diff line", () => {
    expect(source).not.toContain("divide-y");
    expect(source).not.toMatch(/border-y\s+border-border/);
  });

  it("gives the surface one horizontal scrollbar instead of one per row", () => {
    // The regression: every row carried `overflow-x-auto`, so a long line
    // scrolled alone, grew its own scrollbar, and took its line numbers with
    // it. The scroller is now the list, and rows size to its content width.
    expect(source).not.toContain("overflow-x-auto");
    expect(source).toContain("contentWidth={!wrapping}");
  });

  it("pins the line-number gutter over an opaque background", () => {
    // Sticky is what keeps the numbers on screen while a long line scrolls;
    // opaque is what keeps the code from scrolling visibly underneath them.
    expect(source).toMatch(/sticky left-0 z-10[^"]*bg-background/);
  });

  it("numbers both sides of a change, not one column meaning two things", () => {
    // One column printing oldNo for deletions and newNo for additions is not
    // monotonic and cannot be read as either file's line numbers.
    const gutter = source.slice(source.indexOf("snippet unifiedLine"));
    expect(gutter).toContain('{line.oldNo ?? ""}');
    expect(gutter).toContain('{line.newNo ?? ""}');
  });

  it("sizes the gutter to the file's widest line number", () => {
    // A fixed 40px column silently clipped six-digit numbers.
    expect(source).toContain("gutterDigits");
    expect(source).toContain("width: {gutterDigits}ch");
  });

  it("never pins a row's height while it is wrapping", () => {
    // The regression this guards: every row carried `height: ROW_HEIGHT` while
    // wrap set `whitespace-pre-wrap`, so a line wrapping to three visual rows
    // was squeezed into 20px — drawn over its neighbours, centred so only the
    // middle slice showed, with the start of the line invisible.
    expect(source).toContain("min-height: ${ROW_HEIGHT}px");
    expect(source).not.toContain('style="height: {ROW_HEIGHT}px;"');
  });

  it("stops windowing while wrapping, because rows are no longer uniform", () => {
    expect(source).toContain("virtualize={!wrapping}");
  });

  it("bounds how much it will render unwrapped, and says why when it will not", () => {
    expect(source).toContain("WRAP_MAX_LINES");
    expect(source).toContain("disabled={!wrapAvailable}");
    expect(source).toContain("Wrapping is unavailable above");
  });

  it("counts wrap availability against the rows on screen, not the line list", () => {
    // Split rows pair lines and are fewer than the unified lines they came
    // from, so the cap has to be measured against whichever list is drawn.
    expect(source).toContain("wrapAvailable = $derived(renderedRowCount <= WRAP_MAX_LINES)");
  });

  it("top-aligns a wrapped row instead of centring it", () => {
    expect(source).toContain("wrapping ? 'items-start' : 'items-center'");
  });

  it("does not tie unrelated layout to the wrap flag", () => {
    // The toolbar, the truncation banner and the selection bar all carried
    // `wrapping ? 'items-start' : 'items-center'`, so toggling Wrap visibly
    // jumped chrome that does not wrap.
    const chrome = source.slice(0, source.indexOf("Row snippets"));
    const toolbars = chrome.slice(chrome.indexOf("<div class=\"flex h-full"));
    expect(toolbars).not.toContain("{wrapping ? 'items-start' : 'items-center'}");
  });

  it("renders commit metadata and binary notices as their own row kinds", () => {
    expect(source).toContain('line.type === "meta"');
    expect(source).toContain('line.type === "binary"');
  });

  it("renders a file header as a named section rather than a raw git line", () => {
    expect(source).toContain('line.content.startsWith("diff --git ")');
    expect(source).toContain("sectionAt(outline, index)");
  });

  it("lets the reader select and copy the code", () => {
    // `select-none` on the diff root made a diff viewer that could not be
    // copied out of. Chrome still opts out; the code does not.
    const surface = source.slice(source.indexOf("Row snippets"));
    expect(surface).not.toContain("select-none whitespace");
    expect(source).not.toMatch(/class="flex h-full flex-1 flex-col[^"]*select-none/);
    expect(source).toContain("gp-diff-surface");
  });
});

describe("DiffViewer split view", () => {
  it("builds its rows from the shared model instead of its own pairing", () => {
    // The old builder held ONE pending deletion, so a block of D deletions
    // and A additions came out offset by the size of the block, and its
    // intra-line pairing disagreed with the unified view's.
    expect(source).toContain("buildSplitRows(lines)");
    expect(source).not.toContain("annotateSplitPair");
    expect(source).not.toContain("let pendingDel");
  });

  it("annotates through one function both views call", () => {
    expect(source).toContain("function annotateAround(");
    expect(source).toContain("replacementBlockBounds");
    expect(source).toContain("annotateRange(lines, bounds[0], bounds[1])");
  });

  it("spans chrome across both columns instead of leaving one blank", () => {
    expect(source).toContain('row.kind === "span"');
    expect(source).toMatch(/flex w-full items-center gap-2 bg-surfaceHover/);
  });

  it("offers staging from split rows, not only from unified ones", () => {
    const split = source.slice(source.indexOf("snippet splitSide"));
    expect(split).toContain("stageBox(index)");
    expect(split).toContain("onLinePointerDown");
  });

  it("keeps the reader's place when the layout changes", () => {
    expect(source).toContain("function setViewMode(");
    expect(source).toContain("splitRowForLine(splitModel, anchor)");
  });
});

describe("DiffViewer navigation", () => {
  it("offers keyboard stepping that does not fight the diff's own scrolling", () => {
    expect(source).toContain("event.altKey");
    expect(source).toContain("ArrowDown");
    expect(source).not.toContain('event.key === "ArrowDown" && !event.altKey');
  });

  it("leaves typing targets alone", () => {
    expect(source).toContain('tag === "INPUT"');
    expect(source).toContain("isContentEditable");
  });

  it("steps between blocks of change, not between changed lines", () => {
    expect(source).toContain("nextChangeRow(tones,");
    expect(source).toContain('event.key === "PageDown"');
  });

  it("searches the diff on the chord every editor uses", () => {
    expect(source).toMatch(/event\.key\.toLowerCase\(\) === "f"/);
    expect(source).toContain("findMatches(");
    expect(source).toContain('aria-label="Find in this diff"');
  });

  it("searches the text a row shows, not the raw marker column", () => {
    // A query for `+ foo` must not match the marker of every added line, and
    // a hit at raw column 4 belongs at rendered column 3.
    expect(source).toContain("function renderedText(");
    expect(source).toContain("shiftMatches(hits, markerOffset(line), text.length)");
  });

  it("closes the find bar on Escape rather than trapping it open", () => {
    expect(source).toMatch(/event\.key === "Escape" && searchOpen/);
  });
});

describe("DiffViewer minimap", () => {
  it("projects from the list actually on screen", () => {
    // Ticks were built from the unified line list and the resulting offset
    // written into whichever pane was drawn, so split view was mis-scrolled
    // by the ratio between the two lists.
    expect(source).toContain("const tones = $derived(viewMode === \"split\" ? splitToneList : unifiedTones)");
    expect(source).toContain("buildTicks(tones)");
    expect(source).toContain("scrollForRatio(ratio, renderedRowCount, ROW_HEIGHT, viewportHeight)");
  });

  it("marks where the reader is and where each file starts", () => {
    expect(source).toContain("viewportBand(");
    expect(source).toContain("fileMarks(");
  });
});

describe("DiffViewer store-emission memo guards", () => {
  it("resets scroll only when the reader changed what they are looking at", () => {
    // repoStore republishes fresh objects every ~6s status poll; resetting
    // per emission erased selections mid-drag and yanked scroll to the top.
    const effectIdx = source.indexOf("if (key === viewKey) return;");
    expect(effectIdx).toBeGreaterThan(-1);
    for (const dep of [
      "$repoStore.selectedFilePath",
      "$repoStore.selectedCommitId",
      "$repoStore.selectedIsStaged",
      "$repoStore.selectedIgnoreWhitespace",
    ]) {
      expect(source.indexOf(dep)).toBeGreaterThan(-1);
    }
    const resetIdx = source.indexOf("unifiedScroll = 0;", effectIdx);
    expect(resetIdx).toBeGreaterThan(effectIdx);
    expect(source.indexOf("splitScroll = 0;", resetIdx)).toBeGreaterThan(resetIdx);
  });

  it("clears a stale line selection when the diff text changes under it", () => {
    // Selections are indices into the parsed lines, and a watcher-driven or
    // post-mutation refetch replaces those lines without changing the path,
    // the commit or the staged side. Staging from stale indices stages lines
    // the reader never picked.
    const idx = source.indexOf("if (key === contentKey) return;");
    expect(idx).toBeGreaterThan(-1);
    expect(source.indexOf("selectedLines = new Set();", idx)).toBeGreaterThan(idx);
    // …and leaves the scroll alone, because the reader has not gone anywhere.
    const tail = source.slice(idx, idx + 400);
    expect(tail).not.toContain("unifiedScroll = 0;");
  });

  it("has no unconditional reset effects left over from the per-effect era", () => {
    expect(source).not.toContain("// Reset selection when switching files");
    expect(source).not.toContain("void $repoStore.selectedFilePath;");
    expect(source.match(/prevSelPath|prevScrollPath/g)).toBeNull();
  });

  it("refetches image blobs only when repo, path, or commit change", () => {
    expect(source).toContain("imageBlobKey");
    expect(source).toContain("if (imageBlobKey === requestKey)");
    expect(source.match(/return \(\) => \{\s*cancelled = true;\s*\};/g)).toBeNull();
  });

  it("drops a stale impact answer instead of letting it overwrite the current file", () => {
    const idx = source.indexOf("getImpact(repoPath, filePath");
    expect(idx).toBeGreaterThan(-1);
    expect(source.slice(idx - 400, idx)).toContain("createAsyncGuard()");
    expect(source.slice(idx, idx + 300)).toContain("guard.isLive()");
  });
});

describe("DiffViewer parse caching and store-owned whitespace toggle", () => {
  it("parses through a module-scope reference-identity cache", () => {
    expect(source).toContain("const parseCache = createParseCache();");
    expect(source).toContain("$derived(parseCache.parse($repoStore.selectedDiff))");
    expect(source).not.toContain("parseUnifiedDiff($repoStore.selectedDiff || \"\")");
  });

  it("memoizes composed spans per line so a scroll frame is not a re-tokenize", () => {
    expect(source).toContain("const spanCache = new WeakMap<");
    expect(source).toContain("spanCache.set(line,");
  });

  it("delegates the whitespace toggle to the store instead of local state", () => {
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

  it("bounds syntax colouring rather than tokenizing a hundred-thousand-line diff", () => {
    expect(source).toContain("SYNTAX_MAX_LINES");
    expect(source).toContain("disabled={!syntaxAvailable}");
  });
});

describe("DiffViewer staging safety and stats", () => {
  it("disables hunk staging when the diff is truncated", () => {
    expect(source).toContain("disabled={truncatedSource}");
    expect(source).toContain("Partial data");
  });

  it("offers hunk and line-level staging only for working-tree files", () => {
    expect(source).toContain("Stage Hunk");
    expect(source).toContain("Unstage Hunk");
    expect(source).toContain("Stage Selected");
    expect(source).toContain("Unstage Selected");
    expect(source).toContain("isWorkingTreeFile");
    expect(source).toContain("statuses.some");
  });

  it("offers only the action that applies to this side of the index", () => {
    // Both buttons used to be shown at once, so "Unstage Selected" sat there
    // on an unstaged file doing nothing.
    expect(source).toMatch(/\{#if isStaged\}[\s\S]*Unstage Selected[\s\S]*\{:else\}[\s\S]*Stage Selected[\s\S]*\{\/if\}/);
  });

  it("supports click/drag range selection on add and del lines", () => {
    expect(source).toContain("onLinePointerDown");
    expect(source).toContain("onLinePointerEnter");
    expect(source).toContain("selectRange");
    expect(source).toContain("onpointerdown");
  });
});

describe("DiffViewer keeps its frame when there is nothing to show", () => {
  it("keeps the file list beside an empty diff", () => {
    // `{#if lines.length === 0}` used to replace the rail as well as the
    // rows, so a clean merge left no way to reach another file.
    expect(source).toContain("const showEmpty = $derived(!pending && lines.length === 0)");
    const frame = source.indexOf("{#if railOpen && hasRail}");
    const empty = source.indexOf("{:else if showEmpty}");
    expect(frame).toBeGreaterThan(-1);
    expect(empty).toBeGreaterThan(frame);
  });

  it("keeps the file list beside an image diff", () => {
    // The image viewer used to be returned at the top level, taking the
    // toolbar, the rail and the file stepper with it.
    expect(source).not.toMatch(/^\{#if showingImage\}/m);
    // Deferred behind LazyMount — only an image diff reaches this pane — so
    // the mount point is what has to sit after the rail, not a static tag.
    const imageIdx = source.indexOf("load={loadImageDiffViewer}");
    const railIdx = source.indexOf("<DiffFileRail");
    expect(railIdx).toBeGreaterThan(-1);
    expect(imageIdx).toBeGreaterThan(railIdx);
  });

  it("says it is reading rather than showing the previous file's rows", () => {
    expect(source).toContain("$repoStore.selectedDiffPending");
    expect(source).toContain("Reading the diff…");
  });

  it("announces the wait instead of only spinning a glyph", () => {
    // A spinner is a picture of nothing to a screen reader, and a reader
    // already inside the diff has to be told the rows under it are being
    // replaced — so the region reports busy and the message is a live status.
    expect(source).toContain("aria-busy={pending}");
    expect(source).toMatch(/role="status"[\s\S]{0,200}Reading the diff…/);
  });

  it("drives the empty state from emptyDiffCopy so merges get merge copy", () => {
    expect(source).toContain("emptyDiffCopy(");
    expect(source).toContain("title={emptyCopy.title}");
    expect(source).toContain("hint={emptyCopy.hint}");
  });
});

describe("DiffViewer accessibility", () => {
  it("names the whitespace checkbox even when its label is hidden", () => {
    // `hidden sm:inline` on the only text left the checkbox with no
    // accessible name below that breakpoint.
    expect(source).toContain('aria-label="Ignore whitespace-only changes"');
  });

  it("names every icon-only control", () => {
    for (const label of [
      'aria-label="Previous file"',
      'aria-label="Next file"',
      'aria-label="Previous change"',
      'aria-label="Next change"',
      'aria-label="Previous match"',
      'aria-label="Next match"',
      'aria-label="Close find"',
      'aria-label="Toggle word wrap"',
      'aria-label="Toggle syntax colouring"',
    ]) {
      expect(source, label).toContain(label);
    }
  });

  it("names the line-selection checkbox, which has no text at all", () => {
    expect(source).toMatch(/aria-label=\{selectedLines\.has\(\s*index,?\s*\)\s*\? "Deselect line" : "Select line for patch staging"\}/);
  });
});
