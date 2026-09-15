import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import BranchList from "./BranchList.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "BranchList.svelte"),
  "utf8"
);

describe("BranchList", () => {
  it("offers a visible merge picker without opening a branch menu", () => {
    const { body } = render(BranchList);
    expect(body).toContain('aria-label="Merge branches"');
  });

  it("labels the create-branch button for screen readers", () => {
    const { body } = render(BranchList);
    expect(body).toContain('aria-label="Create branch"');
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

describe("BranchList tag rows", () => {
  it("shows how far a tag is ahead of the base, but only when it was compared", () => {
    // A tag holding unmerged work is on no branch, so this chip is the only
    // place the sidebar can say so. Both halves of the guard matter: without
    // `compared_to` the counts are zeros meaning "not asked", and a release
    // tag sitting on the base has nothing to report.
    expect(source).toContain(
      "{#if row.tag.compared_to && row.tag.commits_ahead_of_base > 0}",
    );
    expect(source).toContain(
      'title="{row.tag.commits_ahead_of_base} commits not in {row.tag.compared_to}"',
    );
  });
});

describe("BranchList delete escalation", () => {
  it("attempts the safe non-forced delete first", () => {
    const safeIdx = source.indexOf("repoStore.deleteBranch(branch.name, false)");
    expect(safeIdx).toBeGreaterThan(-1);
    const forceIdx = source.indexOf("repoStore.deleteBranch(branch.name, true)");
    expect(forceIdx).toBeGreaterThan(safeIdx);
  });

  it("escalates to force only through an explicit confirm fed by the decision helper", () => {
    const decisionIdx = source.indexOf("escalateDeleteDecision(outcome.error ?? \"\", branch)");
    expect(decisionIdx).toBeGreaterThan(-1);
    const confirmIdx = source.indexOf('title: "Force-delete branch"');
    expect(confirmIdx).toBeGreaterThan(decisionIdx);
    // The retry is gated on both a positive decision and an explicit confirm.
    expect(source).toContain("!decision.canRetryForce || !decision.message");
    expect(source.indexOf("if (!forceOk) return;")).toBeGreaterThan(confirmIdx);
  });

  it("no longer deletes with a bare unconditional force", () => {
    // The only `force=true` call sits after the escalation confirm block.
    const confirmIdx = source.indexOf('title: "Force-delete branch"');
    const forceIdx = source.indexOf("repoStore.deleteBranch(branch.name, true)");
    expect(forceIdx).toBeGreaterThan(confirmIdx);
    expect(source.indexOf("if (!ok) return;")).toBeLessThan(
      source.indexOf("repoStore.deleteBranch(branch.name, false)")
    );
  });
});

describe("BranchList create-form safety", () => {
  it("keeps the typed name when creation fails (F14)", () => {
    const outcomeCheck = source.indexOf("if (!outcome.ok) return;");
    const clear = source.indexOf("createName = \"\";", source.indexOf("async function submitCreate"));
    expect(outcomeCheck).toBeGreaterThan(-1);
    expect(clear).toBeGreaterThan(outcomeCheck);
  });

  it("bails out of suggestName when the repo changed mid-flight (race)", () => {
    const fn = source.slice(source.indexOf("async function suggestName"), source.indexOf("function openBranchMenu"));
    expect(fn).toContain("const repo = $repoStore.currentPath");
    expect(fn.match(/\$repoStore\.currentPath !== repo/g)?.length).toBeGreaterThanOrEqual(2);
  });
});

describe("BranchList tags", () => {
  it("says when the tag list failed or was capped, instead of looking complete", () => {
    expect(source).toContain("$repoStore.tagsFailed");
    expect(source).toContain("$repoStore.tagsTruncated");
    expect(source).toContain("The tag list could not be read, so this may not be complete.");
    expect(source).toContain("Older tags exist and are not listed.");
  });

  it("creates and deletes tags through the store, not a missing UI", () => {
    expect(source).toContain("repoStore.createTag");
    expect(source).toContain("repoStore.deleteTag");
    expect(source).toContain('aria-label="Create tag"');
    expect(source).toContain("Checkout");
    expect(source).toContain("Delete…");
  });

  it("confirms tag deletion before invoking it", () => {
    const confirmIdx = source.indexOf('title: "Delete tag"');
    const deleteIdx = source.indexOf("repoStore.deleteTag(tag.name)");
    expect(confirmIdx).toBeGreaterThan(-1);
    expect(deleteIdx).toBeGreaterThan(confirmIdx);
    expect(source).toContain("if (!ok) return;");
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
    expect(source).toContain("scrollOffsetToReveal(rowOffsets, index, scrollTop, viewportHeight)");
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
    const twoLineBranch = row.indexOf("{#if twoLine}");
    const oneLineBranch = row.indexOf("{:else}", twoLineBranch);
    const twoLineBody = row.slice(twoLineBranch, oneLineBranch);
    // Line one renders identity only; the numbers snippet is on line two.
    const numbersInTwoLine = twoLineBody.indexOf("branchNumbers(");
    const identityInTwoLine = twoLineBody.indexOf("branchIdentity(");
    expect(identityInTwoLine).toBeGreaterThan(-1);
    expect(numbersInTwoLine).toBeGreaterThan(identityInTwoLine);
  });

  it("renders the same number facts in both layouts from one owner", () => {
    // Two copies of this markup is how the dense list comes to show a count
    // the two-line list does not, or the other way round.
    expect((source.match(/\{@render branchNumbers\(/g) ?? []).length).toBe(2);
    expect((source.match(/\{@render branchIdentity\(/g) ?? []).length).toBe(2);
    expect((source.match(/\{@render branchActions\(/g) ?? []).length).toBe(2);
    expect((source.match(/\{#snippet branchNumbers\(/g) ?? []).length).toBe(1);
  });

  it("clips line two instead of letting counts escape the row", () => {
    // Every item on line two is shrink-0, so without this the numbers paint
    // through the row's edge and under the hover actions on a narrow sidebar.
    const line2 = source.slice(source.indexOf("{@render branchNumbers(branch, statsMissing)}") - 900);
    expect(line2).toContain("overflow-hidden");
    // The fade only consumes pixels when content reaches them, so a clipped
    // row looks clipped rather than looking like a smaller number.
    expect(source).toContain("mask-image: linear-gradient(to right, #000 calc(100% - 14px), transparent)");
    expect(source).toContain("-webkit-mask-image: linear-gradient(to right, #000 calc(100% - 14px), transparent)");
  });

  it("reserves the hover actions' width so they never overlap the text", () => {
    // The rail is absolutely positioned; pr-11 is the space it sits in.
    // Measured: 20px copy + 16px menu + 2px gap + 4px inset = 42px < 44px.
    expect(source).toContain('"flex-1 min-w-0 flex items-center gap-1.5 text-left pr-11"');
    expect(source).toContain('class="absolute right-1 top-0 h-full flex items-center gap-0.5"');
  });

  it("drops the stale chip only where the tinted age replaces it", () => {
    // Two-line prints the real age and tints it; one-line has no age to
    // tint, so it keeps the chip. Passing the flag both ways is the point —
    // a single hardcoded choice would either duplicate or lose the signal.
    expect(source).toContain("{@render branchIdentity(branch, leaf, false, false)}");
    expect(source).toContain("{@render branchIdentity(branch, leaf, true, true)}");
    expect(source).toContain("showStale && isStaleBranch(branch.last_commit_timestamp)");
    expect(source).toContain("stale ? 'text-amber-500/90' : 'text-textMuted/80'");
  });

  it("keeps the separator with the author, which is what clipping removes", () => {
    // A separator owned by the age survives the author's removal and reads
    // as a dot joining the time to nothing.
    expect(source).toContain('{age ? "· " : ""}{author}');
  });

  it("shows only measured file counts, never a zero standing in for unknown", () => {
    expect(source).toContain("{#if branch.files_changed > 0}");
  });

  it("offers the dense layout rather than forcing two lines on everyone", () => {
    expect(source).toContain('$interfaceStore.branchRowLayout');
    expect(source).toContain('rowLayout === "two-line"');
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
    expect(source).toContain("candidate.tabIndex >= 0");
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

describe("BranchList at-a-glance tooltips", () => {
  it("explains the commits-ahead-of-base counter", () => {
    expect(source).toContain(
      "title=\"{branch.commits_ahead_of_base} commits ahead of {branch.compared_to || 'base'}"
    );
  });

  it("also reports how far behind the base the branch is, which no chip shows", () => {
    // commits_behind_base rode on BranchInfo unread. A branch 2 ahead and 90
    // behind reads identically to one 2 ahead and current without it.
    expect(source).toContain("branch.commits_behind_base > 0");
    expect(source).toContain("${branch.commits_behind_base} behind");
  });

  it("gives upstream arrows descriptive titles", () => {
    expect(source).toContain('"{branch.ahead_count} ahead of upstream"');
    expect(source).toContain('"{branch.behind_count} behind upstream"');
  });
});
