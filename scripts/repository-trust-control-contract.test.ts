import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * The trust refusal must name a control that exists, by the name it wears.
 *
 * This message is the only instruction most readers ever get: it is what the
 * MCP tools return, what `gitpulse-hook` prints into a session brief, and what
 * the desktop shows when a repository command is refused. It told people to
 * use "Extend Trust" while the button was labelled "Extend trust to every
 * worktree" — close enough to look right in review, far enough that searching
 * the UI for the words you were handed finds nothing, and the reader concludes
 * the control was removed.
 *
 * Rust owns the string and Svelte owns the button. Neither compiler sees the
 * other, and no runtime path compares them, so only a test that reads both
 * files can hold them together. Matching on the literals is the point — a
 * helper that derived one from the other would pass while the screen and the
 * message disagreed.
 */
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const TRUST_RS = path.join(ROOT, "src-tauri", "src", "repository_trust.rs");
// The button lives in the banner, not the panel it started in: it was hoisted
// above the branch list so it cannot scroll out of sight. This pin followed it
// — and caught the move, which is the point of naming a file rather than
// grepping the tree for the words.
const BANNER = path.join(ROOT, "src", "lib", "components", "TrustExtensionBanner.svelte");
const SIDEBAR = path.join(ROOT, "src", "lib", "components", "Sidebar.svelte");

function read(file: string): string {
  return readFileSync(file, "utf8");
}

/**
 * Rejoin Rust's `\`-at-end-of-line string continuations.
 *
 * The message is one literal wrapped over several source lines, so matching
 * the raw file asserts on where rustfmt happened to break it — a test that
 * fails on a reflow and says nothing about the message. Collapsing the
 * continuations reconstructs what the string actually contains. `\"` is
 * untouched: the pattern needs whitespace after the backslash.
 */
function unwrapped(source: string): string {
  return source.replace(/\\[ \t]*\n[ \t]*/g, "");
}

/** The literal assigned to the Rust constant. */
function rustControlLabel(source: string): string {
  const match = source.match(/pub const EXTEND_TRUST_CONTROL: &str = "([^"]+)";/);
  expect(match, "repository_trust.rs must declare EXTEND_TRUST_CONTROL").not.toBeNull();
  return match![1];
}

/** Every button label the banner renders. */
function bannerButtonLabels(source: string): string[] {
  return [...source.matchAll(/>([^<>{}]+)<\/button>/g)].map((m) => m[1].trim());
}

describe("the trust refusal names a real control", () => {
  const rust = unwrapped(read(TRUST_RS));
  const label = rustControlLabel(rust);

  it("spells the control exactly as the button is labelled", () => {
    const labels = bannerButtonLabels(read(BANNER));
    expect(
      labels,
      `TrustExtensionBanner has no button labelled ${JSON.stringify(label)}; it renders ${JSON.stringify(labels)}`,
    ).toContain(label);
  });

  it("uses the constant in the refusal rather than retyping the words", () => {
    // A second copy of the label inside the message would satisfy the test
    // above and still drift the next time the button is renamed.
    //
    // Only the interpolation itself is asserted. An earlier version pinned the
    // word that happened to follow it, and rewording the sentence around the
    // control broke a test whose subject is not the sentence.
    expect(rust).toContain('\\"{EXTEND_TRUST_CONTROL}\\"');
    const retyped = rust.split(`pub const EXTEND_TRUST_CONTROL: &str = "${label}";`).join("");
    expect(
      retyped.includes(`"${label}"`),
      "the label is written out a second time; interpolate the constant instead",
    ).toBe(false);
  });

  it("leads with the path that needs no hunting", () => {
    // Approving the worktree itself writes a repository-scoped grant, so it
    // covers every other worktree: one dialog, no navigation. The panel is
    // the fallback for a worktree already gone by the time this is read. A
    // message that offered only the panel sent every reader the long way
    // round, which is what it used to do.
    const openThisOne = rust.indexOf("and trust it before running repository commands");
    const covers = rust.indexOf("approving any working tree of the repository at");
    const fallback = rust.indexOf("If this worktree is already gone");
    expect(openThisOne, "the refusal must open with this checkout").toBeGreaterThan(-1);
    expect(covers, "and say that one approval covers the family").toBeGreaterThan(-1);
    expect(fallback, "the panel stays as the fallback").toBeGreaterThan(-1);
    expect(openThisOne).toBeLessThan(covers);
    expect(covers).toBeLessThan(fallback);
  });

  it("says why an already-approved repository is being asked again", () => {
    // The dead end this message exists to avoid: someone who approved this
    // repository months ago, told to approve it, having followed that advice
    // and watched it fail.
    expect(rust).toContain("which is why you are asked again here");
  });

  it("does not say the same thing twice", () => {
    // The fix for the wrong control name made the message longer; a second
    // sentence restating "one approval covers them all" was the cost, and a
    // refusal nobody finishes reading helps nobody.
    const restatements = [
      ...rust.matchAll(/covers (all of them|the whole repository and every other worktree)/g),
    ];
    expect(
      restatements.length,
      `the coverage rule is stated ${restatements.length} times; say it once`,
    ).toBe(1);
  });

  it("puts the control where the refusal says it is", () => {
    // Directions go stale the moment something moves, and this pair already
    // did: the message said "the Worktrees section" while the banner was being
    // hoisted out of that panel to the top of the sidebar. Checking only that
    // the sidebar mounts it somewhere was too weak to notice — so the position
    // the words claim is what gets asserted.
    expect(rust).toContain("at the top of the left sidebar");
    const sidebarSource = read(SIDEBAR);
    const banner = sidebarSource.indexOf("<TrustExtensionBanner />");
    expect(banner, "Sidebar must mount the banner").toBeGreaterThan(-1);
    for (const below of ["<BranchList />", "<WorktreesPanel />"]) {
      const index = sidebarSource.indexOf(below);
      expect(index, `Sidebar must still mount ${below}`).toBeGreaterThan(-1);
      expect(banner, `the banner must precede ${below} to be "at the top"`).toBeLessThan(index);
    }
  });
});
