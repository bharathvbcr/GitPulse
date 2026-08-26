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
  it("wraps computeWindow with clampScrollTop + ensureNonEmptyWindow using live geometry", () => {
    expect(source).toContain("ensureNonEmptyWindow(");
    expect(source).toContain("clampScrollTop(");
    const ensureIdx = source.indexOf("ensureNonEmptyWindow(");
    const computeIdx = source.indexOf("computeWindow(clamped");
    expect(computeIdx).toBeGreaterThan(-1);
    expect(computeIdx).toBeGreaterThan(ensureIdx - 200);
    // The guard receives the same geometry as the window itself.
    const block = source.slice(ensureIdx, ensureIdx + 400);
    expect(block).toContain("allRows.length");
    expect(block).toContain("ROW_HEIGHT");
    expect(block).toContain("viewportHeight");
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

describe("BranchList context menu hardening", () => {
  it("positions via measured size through clampMenuPosition, not guesses", () => {
    expect(source).toContain('from "../branches/menuPosition"');
    expect(source).toContain("clampMenuPosition(");
    // The old hardcoded viewport margins are gone.
    expect(source).not.toContain("innerWidth - 200");
    expect(source).not.toContain("innerHeight - 280");
  });

  it("binds the portaled node and measures its real rendered size after mount", () => {
    expect(source).toContain("bind:this={menuEl}");
    expect(source).toContain("el.offsetWidth");
    expect(source).toContain("el.offsetHeight");
  });

  it("closes the menu on window resize", () => {
    expect(source).toContain('window.addEventListener("resize"');
    expect(source).toContain('window.removeEventListener("resize"');
  });

  it("dismisses a stale menu on right-clicks elsewhere via a window contextmenu listener", () => {
    expect(source).toContain('window.addEventListener("contextmenu"');
    expect(source).toContain('window.removeEventListener("contextmenu"');
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
    // Escape and Tab restore focus to the element that opened the menu.
    expect(handler).toContain("closeMenu({ restoreFocus: true })");
    // Arrow cycling wraps rather than dead-ending at the ends.
    expect(handler).toContain("% items.length");
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

  it("drives every row's height and content-visibility hint inline from ROW_HEIGHT", () => {
    const occurrences = (source.match(/contain-intrinsic-size: auto \{ROW_HEIGHT\}px/g) ?? []).length;
    // Branch, tag, folder-header, and section-header rows all carry it.
    expect(occurrences).toBe(4);
    expect((source.match(/height: \{ROW_HEIGHT\}px/g) ?? []).length).toBe(4);
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
      'title="{branch.commits_ahead_of_base} commits ahead of {branch.compared_to || \'base\'}"'
    );
  });

  it("gives upstream arrows descriptive titles", () => {
    expect(source).toContain('"{branch.ahead_count} ahead of upstream"');
    expect(source).toContain('"{branch.behind_count} behind upstream"');
  });
});
