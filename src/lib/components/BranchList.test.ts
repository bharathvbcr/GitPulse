import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { functionBody } from "../stores/sourceScan";
import { render } from "svelte/server";
import { folderToggleAction } from "../branches/flattenRows";
import { groupBranches } from "../branches/groupBranches";
import type { BranchInfo, BranchSection } from "../branches/types";
import BranchList from "./BranchList.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "BranchList.svelte"),
  "utf8"
);

/** Header action: the visible title, the label it was shortened from, and the click handler. */
const HEADER_ACTIONS = [
  {
    aria: "Locate checked-out branch",
    title: "Locate",
    handler: "onclick={locateCurrentBranch}",
  },
  {
    aria: "Clean stale branches",
    title: "Clean stale",
    handler: 'onclick={() => window.dispatchEvent(new CustomEvent("gitpulse:branch-cleanup"))}',
  },
] as const;

function sourceButton(header: string, aria: string): string {
  const at = header.indexOf(`aria-label="${aria}"`);
  expect(at, aria).toBeGreaterThan(-1);
  const open = header.lastIndexOf("<button", at);
  const close = header.indexOf("</button>", at);
  expect(open).toBeGreaterThan(-1);
  expect(close).toBeGreaterThan(at);
  return header.slice(open, close + "</button>".length);
}

/** Text a reader sees. Attributes, icon markup, and `{...}` expressions are not the title. */
function visibleLabel(buttonHtml: string): string {
  return buttonHtml
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/<svg[\s\S]*?<\/svg>/gi, "")
    .replace(/\{[\s\S]*?\}/g, "")
    .replace(/<[^>]*>/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

describe("BranchList header", () => {
  it("puts the branch title on its own row and shows each action title", () => {
    const start = source.indexOf("<!-- Header Bar -->");
    const end = source.indexOf('aria-label="Merge branches"');
    expect(start).toBeGreaterThan(-1);
    expect(end).toBeGreaterThan(start);
    const header = source.slice(start, end);

    // The old header was one justify-between row: the count and the icon
    // buttons shared it. The title element now closes before any action.
    expect(header).not.toContain("justify-between");
    expect(header).toContain("flex-col");
    expect(header).toContain('class="flex flex-nowrap gap-1 min-w-0"');
    expect(header).not.toContain("flex-wrap");
    expect(header).not.toContain("grid-cols-2");
    // The section label is the sentence-case gp-label (design-lint rules 1 and
    // 2 refuse the uppercase + tracking + text-[10px] spelling).
    const titleRow = header.match(
      /<div class="[^"]*\bgp-label\b[^"]*">\s*Branches \(\{\$repoStore\.branches\.length\}\)\s*<\/div>/
    );
    expect(header).not.toContain("uppercase");
    expect(titleRow).not.toBeNull();
    const titleMarkup = titleRow?.[0] ?? "";
    expect(titleMarkup).not.toContain("<button");
    const afterTitle = header.slice(header.indexOf(titleMarkup) + titleMarkup.length);
    expect(afterTitle).toContain("<button");
    expect(header).not.toContain("+All");
    expect(header).not.toContain("-All");

    const actionClass = source.slice(
      source.indexOf("const headerActionClass"),
      source.indexOf("let gapChips"),
    );
    expect(actionClass).toContain("h-6");
    expect(actionClass).toContain("flex-1");
    expect(actionClass).toContain("basis-0");
    expect(actionClass).toContain("min-w-0");
    expect(actionClass).not.toContain("w-full");
    expect(actionClass).not.toContain("shrink-0");
    expect(actionClass).not.toContain("min-h-6");
    const chipRowAt = header.indexOf('<div class="flex flex-nowrap gap-1 min-w-0">');
    expect(chipRowAt).toBeGreaterThan(-1);
    const chipRow = header.slice(chipRowAt, header.indexOf("</div>", chipRowAt) + "</div>".length);
    expect(chipRow.match(/<button/g)).toHaveLength(4);
    // Words on the chips fail this. Names stay on aria-label and title.
    expect(visibleLabel(chipRow)).toBe("");

    const { body } = render(BranchList);
    const mergeAt = body.indexOf('aria-label="Merge branches"');
    const rendered = body.slice(0, mergeAt);
    expect(rendered).toMatch(/Branches \(\d+\)/);

    for (const action of HEADER_ACTIONS) {
      const markup = sourceButton(header, action.aria);
      expect(markup).toContain(action.handler);
      expect(markup).toContain(`title="`);
      expect(visibleLabel(markup)).toBe("");
      expect(action.aria.startsWith(action.title)).toBe(true);

      const renderedButton = sourceButton(rendered, action.aria);
      expect(visibleLabel(renderedButton)).toBe("");
    }

    // Expand and Collapse are one chip. Two static chips fail this.
    expect(header).not.toContain('aria-label="Expand all folders"');
    expect(header).not.toContain('aria-label="Collapse all folders"');
    expect(header).not.toContain("onclick={expandAll}");
    expect(header).not.toContain("onclick={collapseAll}");
    const foldAt = header.indexOf("offerExpand ? expandAll() : collapseAll()");
    expect(foldAt).toBeGreaterThan(-1);
    const foldOpen = header.lastIndexOf("<button", foldAt);
    const foldClose = header.indexOf("</button>", foldAt);
    const fold = header.slice(foldOpen, foldClose);
    expect(fold).toContain('title={offerExpand ? "Expand all folders" : "Collapse all folders"}');
    expect(fold).toContain('aria-label={offerExpand ? "Expand all folders" : "Collapse all folders"}');
    expect(fold).toContain("disabled={!folderToggle.enabled}");
    expect(visibleLabel(fold)).toBe("");
    expect(fold).not.toContain('aria-haspopup="menu"');

    // Empty tree: nothing to toggle, so the chip stays and stays disabled.
    const renderedFold = sourceButton(rendered, "Expand all folders");
    expect(visibleLabel(renderedFold)).toBe("");
    expect(renderedFold).toMatch(/\sdisabled(?:=""|>|\s)/);
    expect(rendered).not.toContain('aria-label="Collapse all folders"');

    // Create opens the menu. The pill is named, and it has no visible word.
    expect(header).toContain('aria-label="Create branch"');
    expect(header).not.toContain('aria-label="Create tag"');
    expect(header).not.toContain("onclick={() => (creating = !creating)}");
    expect(header).not.toContain("onclick={() => void runCreateTag()}");
    const createPill = sourceButton(header, "Create branch");
    expect(createPill).toContain("onclick={toggleCreateMenu}");
    expect(createPill).toContain('aria-haspopup="menu"');
    expect(createPill).toContain('title="Create branch"');
    expect(visibleLabel(createPill)).toBe("");
    expect(visibleLabel(sourceButton(rendered, "Create branch"))).toBe("");

    const branchAt = source.indexOf("onclick={chooseCreateBranch}");
    expect(branchAt).toBeGreaterThan(-1);
    const branchItem = source.slice(
      source.lastIndexOf("<button", branchAt),
      source.indexOf("</button>", branchAt) + "</button>".length,
    );
    const tagItem = sourceButton(source, "Create tag");
    expect(branchItem).toContain('role="menuitem"');
    expect(branchItem).toContain('title="Create branch"');
    expect(branchItem).toContain("onclick={chooseCreateBranch}");
    expect(visibleLabel(branchItem)).toBe("Branch");
    expect(tagItem).toContain('role="menuitem"');
    expect(tagItem).toContain('title="Create tag at HEAD"');
    expect(tagItem).toContain("onclick={chooseCreateTag}");
    expect(visibleLabel(tagItem)).toBe("Tag");
    expect(functionBody(source, "chooseCreateBranch")).toContain("creating = !creating");
    expect(functionBody(source, "chooseCreateBranch")).not.toContain("runCreateTag");
    expect(functionBody(source, "chooseCreateTag")).toContain("void runCreateTag()");
    expect(functionBody(source, "chooseCreateTag")).not.toContain("creating = !creating");
    expect(functionBody(source, "toggleCreateMenu")).not.toContain("creating = !creating");
    expect(functionBody(source, "toggleCreateMenu")).not.toContain("runCreateTag");
    expect(source).toContain('escape: "capture"');
  });

  it("offers Expand while a folder is closed and Collapse only when every folder is open", () => {
    const lookup = (record: Record<string, boolean>) => (id: string, kind: BranchSection["kind"]) =>
      id in record ? record[id] : kind === "remote" || kind === "tags";
    const ref = (partial: Partial<BranchInfo> & Pick<BranchInfo, "name">): BranchInfo => ({
      is_current: false,
      is_remote: false,
      tip_commit_id: "abc",
      ahead_count: 0,
      behind_count: 0,
      is_default: false,
      is_gone: false,
      last_commit_timestamp: 1,
      last_author: "Ada",
      last_summary: "feat: work",
      commits_ahead_of_base: 0,
      commits_behind_base: 0,
      additions: 0,
      deletions: 0,
      files_changed: 0,
      ...partial,
    });

    const empty = folderToggleAction([], lookup({}));
    expect(empty.enabled).toBe(false);
    expect(empty.action).toBe("expand");

    const local = groupBranches([ref({ name: "main" })]);
    expect(folderToggleAction(local, lookup({})).enabled).toBe(true);
    expect(folderToggleAction(local, lookup({})).action).toBe("collapse");

    const nested = groupBranches([ref({ name: "feat/auth" })]);
    expect(folderToggleAction(nested, lookup({})).action).toBe("collapse");
    expect(folderToggleAction(nested, lookup({ "local/feat": true })).action).toBe("expand");

    const withRemote = groupBranches([
      ref({ name: "main" }),
      ref({ name: "origin/main", is_remote: true, remote_name: "origin" }),
    ]);
    expect(folderToggleAction(withRemote, lookup({})).action).toBe("expand");
    expect(
      folderToggleAction(withRemote, lookup({ local: false, "remote:origin": false })).action,
    ).toBe("collapse");
  });
});

describe("BranchList", () => {
  it("offers a visible merge picker without opening a branch menu", () => {
    const { body } = render(BranchList);
    expect(body).toContain('aria-label="Merge branches"');
  });

  it("labels the create-branch menu item for screen readers", () => {
    const at = source.indexOf("onclick={chooseCreateBranch}");
    const item = source.slice(source.lastIndexOf("<button", at), source.indexOf("</button>", at));
    expect(item).toContain('role="menuitem"');
    expect(item).toContain('aria-label="Create branch"');
    const { body } = render(BranchList);
    // The header control keeps the name; the menu item is not mounted until it opens.
    expect(body).toContain('aria-label="Create branch"');
    expect(body).not.toContain('role="menuitem"');
  });

  it("labels the sparkles button, which only mounts once the create form opens", () => {
    expect(source).toContain('aria-label="Suggest branch name"');
    // The button stays disabled while a suggestion is pending…
    expect(source).toContain("disabled={suggesting}");
    // …and the handler re-checks in flight: two same-tick clicks cannot fire
    // two racing AI invokes before Svelte flushes the disabled attribute.
    expect(source).toContain("if (suggesting) return;");
    expect(source).toContain("const repo = $repoStore.currentPath;");
  });

  it("routes menu copies through the shared fallback and exposes failures", () => {
    expect(source).toContain('from "../desktop/clipboard"');
    expect(source).toContain("if (!(await copyToClipboard(value)))");
    expect(source).not.toContain("navigator.clipboard");
  });
});

/**
 * Rendered in harness/branches.html ("delete, tags and create"): delete asks
 * first and declining writes nothing; a confirmed delete is the safe one;
 * git's unmerged refusal opens a separate force prompt naming the commits
 * lost, and declining it never forces; a refusal force cannot fix offers no
 * force prompt; a failed create keeps the form and the typed name; the
 * upstream arrows and base counter titles say what they count.
 */
describe("BranchList create-form safety", () => {
  it("bails out of suggestName when the repo changed mid-flight (race)", () => {
    const fn = source.slice(source.indexOf("async function suggestName"), source.indexOf("function openBranchMenu"));
    expect(fn).toContain("const repo = $repoStore.currentPath");
    // The guard counts switches rather than comparing the endpoints: a path
    // comparison lets A -> B -> A through, and BranchList outlives the switch.
    expect(fn).toContain("const epoch = tabScope.epoch");
    expect(fn.match(/!tabScope\.isCurrent\(epoch\)/g)?.length).toBeGreaterThanOrEqual(2);
    expect(fn).not.toMatch(/\$repoStore\.currentPath !== repo/);
  });
});

describe("BranchList tags", () => {
  // Rendered in harness/branches.html: the failed and capped tag-list notes,
  // the ahead chip shown only for a compared tag, and the delete prompt
  // answered no (nothing deleted) and yes (exactly that tag deleted).
  it("offers tag creation from the sidebar", () => {
    expect(source).toContain("repoStore.createTag");
    expect(source).toContain('aria-label="Create tag"');
  });
});

describe("BranchList pin persistence via branches/pins", () => {
  it("routes storage through the pure pins helpers", () => {
    expect(source).toContain('from "../branches/pins"');
    expect(source).toContain("parsePinned");
    expect(source).toContain("serializePinned");
    expect(source).toContain("pinnedKey(path)");
    // No hand-rolled JSON.parse or ad-hoc key strings left behind.
    expect(source).not.toContain("JSON.parse(raw)");
    expect(source).not.toContain("`gitpulse:pinned:${path}`");
  });

  it("applies the parsed result unconditionally so pins never leak across repos", () => {
    // Regression: when localStorage had NO entry for the current repo, the
    // previous repo's pin set survived loadPinned and was later persisted
    // into the new repo's key. The parse must run even when raw is null,
    // yielding an empty set that overwrites stale state.
    const fn = source.slice(
      source.indexOf("function loadPinned"),
      source.indexOf("function savePinned")
    );
    expect(fn).toContain("const names = parsePinned(raw);");
    expect(fn).not.toContain("if (raw)");
    // Empty-set overwrite happens BEFORE the identity check can skip it.
    const parseIdx = fn.indexOf("parsePinned(raw)");
    const applyIdx = fn.indexOf("if (signature !== pinnedSignature)");
    expect(parseIdx).toBeGreaterThan(-1);
    expect(applyIdx).toBeGreaterThan(parseIdx);
  });

  it("rebuilds pinnedNames only when the stored pin list value changes", () => {
    // repoStore republishes fresh objects every ~6s status poll; rebuilding
    // the Set per emission recomputes grouping and the virtual window on
    // every tick even when nothing changed.
    expect(source).toContain("pinnedSignature");
    const compareIdx = source.indexOf("if (signature !== pinnedSignature)");
    expect(compareIdx).toBeGreaterThan(-1);
    const rebuildIdx = source.indexOf("pinnedNames = new Set(names);");
    expect(rebuildIdx).toBeGreaterThan(compareIdx);
  });
});

describe("BranchList virtual window tail guarantee", () => {
  it("clamps the anchor before computing the window, using live geometry", () => {
    // Same property the fixed-height version guarded: a deep anchor over a
    // list that just shrank must not paint a frame of nothing. The tail
    // guarantee itself now lives inside windowFromOffsets (see
    // sidebar/rowWindow.test.ts) because no caller wants the empty band.
    expect(source).toContain("windowFromOffsets(");
    expect(source).toContain("clampScrollTopToOffsets(scrollTop, rowOffsets, viewportHeight)");
    const winIdx = source.indexOf("windowFromOffsets(");
    const block = source.slice(winIdx, winIdx + 300);
    expect(block).toContain("rowOffsets");
    expect(block).toContain("viewportHeight");
    expect(block).toContain("BRANCH_OVERSCAN");
  });

  it("answers every scroll-position question from the one offsets array", () => {
    // Rows are no longer all one height, so `index * height` is wrong
    // everywhere. It does not throw — it silently scrolls to another row —
    // so the guard is that no such arithmetic survives and that all four
    // position consumers read rowOffsets.
    expect(source).toContain("let rowOffsets = $derived(buildRowOffsets(rowHeights));");
    expect(source).toContain("totalRowHeight(rowOffsets)");
    expect(source).toContain("rowTop(rowOffsets, win.start)");
    expect(source).toContain("scrollOffsetToReveal(rowOffsets, index, containerEl.scrollTop, containerEl.clientHeight, CUE_CLEAR_PX)");
    expect(source).toContain("scrollOffsetToCenter(rowOffsets, idx, viewportHeight)");
    // No surviving uniform-height arithmetic.
    expect(source).not.toMatch(/idx \* [A-Z_]*HEIGHT/);
    expect(source).not.toMatch(/index \* [A-Z_]*HEIGHT/);
    expect(source).not.toMatch(/win\.start \* [A-Z_]*HEIGHT/);
    expect(source).not.toMatch(/allRows\.length \* [A-Z_]*HEIGHT/);
  });

  it("sizes each row by its own kind, so the window and the markup agree", () => {
    // One helper call feeds both the offsets array and the inline heights.
    // Two independent height decisions is how a variable-height list drifts
    // into blank bands and overlapping rows.
    expect(source).toContain(
      "allRows.map((row) => sidebarRowHeight(row.kind, $densityStore, rowLayout))",
    );
  });

  it("takes overscan from the shared metrics module, not a local constant", () => {
    expect(source).toContain('from "../sidebar/metrics"');
    expect(source).toContain("BRANCH_OVERSCAN");
    expect(source).not.toContain("const OVERSCAN");
  });

  it("derives ROW_HEIGHT from the density store instead of hardcoding 26", () => {
    expect(source).toContain("branchRowHeight($densityStore)");
    expect(source).not.toContain("const ROW_HEIGHT = 26");
    expect(source).toContain("$derived");
  });
});

describe("BranchList two-line branch rows", () => {
  // The rendered geometry — row heights matching the window math, neither
  // line painting under the hover actions, the clipping ladder — is asserted
  // against real layout in harness/branches.html. These guard the structure
  // that geometry depends on, which a source read can see and a jsdom render
  // of an empty branch list cannot.

  it("gives the name a line of its own, with the numbers on the second", () => {
    const row = source.slice(
      source.indexOf("{#snippet branchRow("),
      source.indexOf("{#snippet tagRow("),
    );
    const multiLineBranch = row.indexOf("{#if multiLine}");
    const oneLineBranch = row.indexOf("{:else}", multiLineBranch);
    const multiLineBody = row.slice(multiLineBranch, oneLineBranch);
    // Line one renders identity only; the numbers snippet is on line two.
    const numbersInMultiLine = multiLineBody.indexOf("branchNumbers(");
    const identityInMultiLine = multiLineBody.indexOf("branchIdentity(");
    expect(identityInMultiLine).toBeGreaterThan(-1);
    expect(numbersInMultiLine).toBeGreaterThan(identityInMultiLine);
  });

  it("renders the same number facts in every layout from one owner", () => {
    // Two copies of this markup is how the dense list comes to show a count
    // the other lists do not, or the other way round. The multi-line rows
    // share one render of each; only the age and author are placed per
    // layout, from one snippet.
    expect((source.match(/\{@render branchNumbers\(/g) ?? []).length).toBe(2);
    expect((source.match(/\{@render branchIdentity\(/g) ?? []).length).toBe(2);
    expect((source.match(/\{@render branchActions\(/g) ?? []).length).toBe(2);
    expect((source.match(/\{#snippet branchNumbers\(/g) ?? []).length).toBe(1);
    expect((source.match(/\{#snippet branchAge\(/g) ?? []).length).toBe(1);
    expect((source.match(/\{@render branchAge\(/g) ?? []).length).toBe(2);
  });

  it("puts the age and author on line three of the three-line row, line two of the two-line row", () => {
    const row = source.slice(
      source.indexOf("{#snippet branchRow("),
      source.indexOf("{#snippet tagRow("),
    );
    const lineTwo = row.indexOf("{@render branchNumbers(branch, statsMissing)}");
    const twoLineAge = row.indexOf("{#if rowLines === 2}", lineTwo);
    const threeLineAge = row.indexOf("{#if rowLines === 3}", twoLineAge);
    expect(twoLineAge).toBeGreaterThan(lineTwo);
    expect(threeLineAge).toBeGreaterThan(twoLineAge);
    // Each placement renders the one snippet, and nothing else sits between
    // the guard and its render but whitespace and the line's own wrapper.
    expect(row.slice(twoLineAge, threeLineAge)).toContain("{@render branchAge(branch)}");
    expect(row.slice(threeLineAge)).toContain("{@render branchAge(branch)}");
  });

  it("reads one line count for the markup and the height, not two booleans", () => {
    // `multiLine` and `rowLines` both come from `branchRowLines`, the same
    // function `sidebarRowHeight` asks, so the markup and the window math
    // cannot disagree about how many lines a layout has.
    expect(source).toContain("let rowLines = $derived(branchRowLines(rowLayout));");
    expect(source).toContain("let multiLine = $derived(rowLines > 1);");
    expect(source).not.toContain("twoLine");
  });

  it("keeps line three's height when a branch has nothing to put there", () => {
    expect(source).toContain(
      '<span class="w-full flex items-center gap-1 min-w-0 min-h-lh text-micro overflow-hidden pr-2.5">',
    );
  });

  it("clips line two instead of letting counts escape the row", () => {
    // Every item on line two is shrink-0, so without this the numbers paint
    // through the row's edge and under the hover actions on a narrow sidebar.
    const line2 = source.slice(source.indexOf("{@render branchNumbers(branch, statsMissing)}") - 900);
    expect(line2).toContain("overflow-hidden");
    // The fade covers exactly the line's right padding (pr-2.5 = 10px), so it
    // only ever touches content that has already overflowed: the author's
    // ellipsis ends before it and stays crisp, a clipped number still fades.
    expect(line2).toContain("overflow-hidden pr-2.5");
    expect(source).toContain("mask-image: linear-gradient(to right, black calc(100% - 10px), transparent)");
    expect(source).toContain("-webkit-mask-image: linear-gradient(to right, black calc(100% - 10px), transparent)");
  });

  it("reserves the hover actions' width on line one only, so line two gets the full row", () => {
    // The rail is absolutely positioned level with the name; pr-11 on line
    // one is the space it sits in. Measured: 16px copy + 16px menu + 2px gap
    // + 4px inset = 38px < 44px (the copy button is p-0.5 in the rail, 16px,
    // so the rail ends above line two). Line two runs under nothing.
    expect(source).toContain('<span class="w-full flex items-center gap-1.5 min-w-0 leading-none pr-11">');
    expect(source).toContain('"flex-1 min-w-0 flex items-center gap-1.5 text-left pr-1"');
    expect(source).toContain('class="absolute right-1 top-[5px] h-4 flex items-center"');
    // At rest the status dots hold the row's right edge; they yield to the
    // actions on hover and focus rather than sitting beside them.
    expect(source).toContain("group-hover:opacity-0 group-focus-within:opacity-0");
  });

  it("drops the stale chip only where the tinted age replaces it", () => {
    // Two-line prints the real age and tints it; one-line has no age to
    // tint, so it keeps the chip. Passing the flag both ways is the point —
    // a single hardcoded choice would either duplicate or lose the signal.
    expect(source).toContain("{@render branchIdentity(branch, leaf, false, false)}");
    expect(source).toContain("{@render branchIdentity(branch, leaf, true, true)}");
    expect(source).toContain("showStale && isStaleBranch(branch.last_commit_timestamp)");
    expect(source).toContain("stale ? 'text-warn' : 'text-textMuted'");
  });

  it("keeps the separator with the author, which is what clipping removes", () => {
    // A separator owned by the age survives the author's removal and reads
    // as a dot joining the time to nothing.
    expect(source).toContain('{age ? "· " : ""}{author}');
  });

  it("shows only measured file counts, never a zero standing in for unknown", () => {
    expect(source).toContain("{#if branch.files_changed > 0}");
  });

  it("offers the denser layouts rather than forcing three lines on everyone", () => {
    expect(source).toContain('$interfaceStore.branchRowLayout');
    expect(source).toContain("branchRowLines(rowLayout)");
    expect(source).not.toContain('rowLayout === "two-line"');
  });
});

describe("BranchList context menu hardening", () => {
  it("positions via measured size through the shared popover owner, not guesses", () => {
    // Measuring is the owner's job now and popover.test.ts proves the
    // measured box beats the estimate; this holds the estimate to being only
    // a flash-length placeholder, and the old hardcoded margins to staying
    // gone.
    expect(source).toContain('from "../ui/popover"');
    expect(source).toContain("estimate: { width: MENU_ESTIMATED_W, height: MENU_ESTIMATED_H }");
    expect(source).not.toContain("innerWidth - 200");
    expect(source).not.toContain("innerHeight - 280");
    // Replaced, not accumulated: no second clamp beside the owner's.
    expect(source).not.toContain("clampMenuPosition");
  });

  it("binds the portaled node and re-measures when its items change", () => {
    // A background refresh can flip is_current/is_remote while the menu is
    // up, changing its height under an already-clamped position.
    expect(source).toContain("bind:this={menuEl}");
    expect(source).toContain("revision: menuShape");
    expect(source).toMatch(/use:portal=\{"body"\}\s*\n\s*use:popover=\{dismissal\}/);
  });

  it("closes the menu on window resize", () => {
    expect(source).toContain("resize: true");
  });

  it("dismisses a stale menu on right-clicks elsewhere", () => {
    expect(source).toContain("contextmenu: true");
    // Bubble-phase clicks, not capture: the menu container stops propagation
    // on its own clicks, and that is what keeps "Copy name" from closing the
    // menu it was invoked from.
    expect(source).toContain('pointer: "click"');
    expect(source).toContain("onclick={(e) => e.stopPropagation()}");
    // Every close funnels through one path so the opener ref is dropped too.
    expect(source.match(/closeMenu\(/g)?.length ?? 0).toBeGreaterThanOrEqual(10);
  });

  it("captures the opener element at open time for focus restoration", () => {
    expect(source).toContain("document.activeElement instanceof HTMLElement");
    expect(source).toContain("opener.focus()");
  });
});

describe("BranchList menu keyboard accessibility", () => {
  it("marks every action as a menuitem inside the role=menu container", () => {
    const menuBlock = source.slice(source.indexOf("{#if menu}"));
    expect(menuBlock).toContain('role="menu"');
    expect((menuBlock.match(/role="menuitem"/g) ?? []).length).toBeGreaterThanOrEqual(11);
  });

  it("implements the WAI-ARIA key set on the menu container", () => {
    const handler = source.slice(
      source.indexOf("function handleMenuKeydown"),
      source.indexOf("// A background refresh")
    );
    for (const key of ['"Escape"', '"Tab"', '"ArrowDown"', '"ArrowUp"', '"Home"', '"End"']) {
      expect(handler).toContain(key);
    }
    // Escape restores focus to the element that opened the menu.
    expect(handler).toContain("closeMenu({ restoreFocus: true })");
    // Arrow cycling wraps rather than dead-ending at the ends.
    expect(handler).toContain("% items.length");
  });

  it("lets Tab and Shift+Tab leave the menu in document order", () => {
    const handler = source.slice(
      source.indexOf("function handleMenuKeydown"),
      source.indexOf("// A background refresh"),
    );
    const tabBranch = handler.slice(
      handler.indexOf('e.key === "Tab"'),
      handler.indexOf('e.key === "Escape"'),
    );

    expect(tabBranch).toContain("focusAdjacentToMenuOpener(");
    expect(tabBranch).toContain("e.shiftKey");
    expect(tabBranch).not.toContain("restoreFocus");
    expect(source).toMatch(/focusAdjacentToMenuOpener[^}]*\} from "[./]+\/ui\/focusTrap"/);
    expect(source).not.toContain("function focusAdjacentToMenuOpener");
  });

  it("declares popup semantics on the kebab trigger", () => {
    expect(source).toContain('aria-haspopup="menu"');
    expect(source).toMatch(/aria-expanded=\{menu\?\.branch/);
  });
});

describe("BranchList tree container a11y", () => {
  it("labels the role=tree region while keeping the deliberate design comment", () => {
    expect(source).toMatch(/role="tree"\s*aria-label="Branches"/s);
    // The non-ARIA-tree rationale must survive the change.
    expect(source).toContain("Deliberately NOT a strict ARIA tree");
  });
});

describe("BranchList density-aware rows", () => {
  it("has no hardcoded h-[26px] row classes left", () => {
    expect(source).not.toContain("h-[26px]");
  });

  it("drives every row's height and content-visibility hint from the metrics module", () => {
    // Branch, tag, folder-header and section-header rows all carry both.
    // A row kind added without them is a hole in the windowing, so the count
    // is a deliberate tripwire rather than a derived number.
    const declared = [...source.matchAll(/height: \{([A-Z_]+)\}px/g)].map((m) => m[1]);
    expect(declared).toHaveLength(4);
    // Only the two metrics-derived heights exist, and each row's
    // contain-intrinsic-size hint names the SAME variable as its height —
    // a mismatched pair is exactly the drift this module was created to stop.
    for (const name of declared) {
      expect(["HEADER_HEIGHT", "BRANCH_HEIGHT"]).toContain(name);
    }
    for (const name of new Set(declared)) {
      const heights = (source.match(new RegExp(`height: \\{${name}\\}px`, "g")) ?? []).length;
      const hints = (
        source.match(new RegExp(`contain-intrinsic-size: auto \\{${name}\\}px`, "g")) ?? []
      ).length;
      expect(hints).toBe(heights);
    }
    // Branch rows are the only kind that grows; the other three stay headers.
    expect((source.match(/height: \{BRANCH_HEIGHT\}px/g) ?? []).length).toBe(1);
    expect((source.match(/height: \{HEADER_HEIGHT\}px/g) ?? []).length).toBe(3);
    expect(source).toContain('sidebarRowHeight("branch", $densityStore, rowLayout)');
    expect(source).toContain("branchRowHeight($densityStore)");
  });

  it("varies chrome spacing by density without dynamic class fragments", () => {
    expect(source).toContain('{gapBand}');
    expect(source).toContain('{gapChips}');
    expect(source).toContain('{scrollerAir}');
    // Tailwind scans literal class strings; interpolated fragments would be purged.
    for (const cls of ["mb-1.5", "mb-2", "pb-1"]) {
      expect(source).toContain(`"${cls}"`);
    }
  });
});

describe("BranchList chip strip scrollbar", () => {
  it("hides the scrollbar locally now that the dead .no-scrollbar class is gone", () => {
    expect(source).not.toContain("no-scrollbar");
    expect(source).toContain("chip-strip");
    expect(source).toContain("scrollbar-width: none");
    expect(source).toContain("::-webkit-scrollbar");
  });
});

/** The `$effect` block that encloses `needle`, from its opening line to its close. */
function effectAround(text: string, needle: string): string {
  const at = text.indexOf(needle);
  expect(at, needle).toBeGreaterThan(-1);
  const start = text.lastIndexOf("$effect(() => {", at);
  const end = text.indexOf("\n  });", at);
  expect(start).toBeGreaterThan(-1);
  return text.slice(start, end);
}

describe("BranchList freshness badges", () => {
  it("asks for a batch only when the repository, the tips or the base change", () => {
    // Reading `$repoStore` inside an effect re-runs it on every store
    // emission; each re-run here was a freshness batch (~54 in the first
    // 30 s of one launch). The effects read the shared repository key and
    // derived values instead, and the reset depends on the repository alone.
    const load = effectAround(source, "freshness.load(");
    expect(load).not.toContain("$repoStore");
    expect(load).toContain("const path = $currentRepoPath;");
    const repo = effectAround(source, "freshnessStore.forRepo(");
    expect(repo).not.toContain("$repoStore");
    expect(repo).toContain("freshnessStore.forRepo($currentRepoPath);");
    expect(source).toContain('const freshness = freshnessStore.consumer("branches");');
    expect(source).toContain("const branchInfos = $derived($repoStore.branches);");
  });

  it("never resets the shared store unconditionally", () => {
    // An unconditional reset dropped the pull-request list's loads too.
    expect(source).not.toContain("freshnessStore.reset(");
  });
});

describe("BranchList mutation contract", () => {
  const firstPrompt = (body: string) => Math.min(...["await askConfirm(", "await askText("].map((p) => body.indexOf(p)).filter((i) => i >= 0));

  it.each([
    ["runCreateTag", "repoStore.createTag(trimmed, at?.commitId, undefined, target)"],
    ["runDeleteTag", "repoStore.deleteTag(tag.name, target)"],
    ["runRename", "repoStore.renameBranch(branch.name, trimmed, target)"],
    ["runDelete", "repoStore.deleteBranch(branch.name, false, target)"],
  ])("%s names its repository before the prompt and keeps the outcome", (name, call) => {
    const body = functionBody(source, name);
    expect(body.indexOf("repoStore.captureTarget()")).toBeGreaterThan(-1);
    expect(body.indexOf("repoStore.captureTarget()")).toBeLessThan(firstPrompt(body));
    expect(body).toContain(call);
    expect(body).toContain("reportFailure(");
  });

  it("force-deletes against the same repository", () => {
    expect(functionBody(source, "runDelete")).toContain("repoStore.deleteBranch(branch.name, true, target)");
  });

  it("offers Undo through the one owner, bound to the repository it deleted from", () => {
    const body = functionBody(source, "runDelete");
    expect(body.match(/offerBranchUndo\(/g)).toHaveLength(2);
    expect(body).not.toContain("repoStore.createBranch(");
    expect(body).not.toContain('toastStore.success(`Restored');
  });
});

describe("BranchList keyboard reveal", () => {
  it("measures the reveal against the scroller itself, not the scroll-event state", () => {
    // `scrollTop` state only follows scroll events; under load a late event let
    // ArrowUp compute from a stale offset and leave the row clipped at the edge.
    const body = functionBody(source, "ensureVisible");
    expect(body).toContain("containerEl.scrollTop");
    expect(body).toContain("containerEl.clientHeight");
    expect(body).not.toContain("viewportHeight");
  });
});
