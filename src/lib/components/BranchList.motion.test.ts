import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "BranchList.svelte"), "utf8");
const markup = source.slice(source.indexOf("</script>"));

/** The class attribute of the first element whose class list starts with `prefix`. */
function classOf(prefix: string): string {
  const at = markup.indexOf(`class="${prefix}`);
  expect(at, `no element with class starting "${prefix}"`).toBeGreaterThan(-1);
  return markup.slice(at, markup.indexOf('"', at + 7 + prefix.length));
}

function constant(name: string): string {
  const match = source.match(new RegExp(`const ${name} =\\s*"([^"]+)"`));
  expect(match, `${name} is not declared`).not.toBeNull();
  return match![1];
}

describe("BranchList hover motion", () => {
  it("animates what the gp-row primitive animates, and no paint-heavy property", () => {
    // The ring paints through box-shadow, which design-lint rule 9 keeps out
    // of transitions; it lands with the fill because the fill is fast-out.
    const props = constant("ROW_MOTION").match(/^transition-\[([^\]]+)\]/)![1].split(",");
    expect(props.sort()).toEqual(["background-color", "color"]);
  });

  it("runs on the house motion tokens, not Tailwind's 150ms slow-start default", () => {
    for (const name of ["ROW_MOTION", "REVEAL_MOTION"]) {
      const motion = constant(name);
      expect(motion, name).toContain("duration-(--gp-dur-instant)");
      expect(motion, name).toContain("ease-(--gp-ease-effect)");
      // The bracket form is what app.css's reduced-motion rules match;
      // `.transition-opacity` is in neither list.
      expect(motion, name).toMatch(/^transition-\[/);
    }
  });

  it("puts every row and every hover reveal on the shared timing", () => {
    // Branch, tag and folder rows; then the pin star, the status dots and the
    // two revealed action buttons, which crossfade in one spot.
    for (const prefix of [
      "gp-cv-row relative w-full",
      "gp-cv-row w-full px-2 rounded-full",
      "gp-cv-row gp-label w-full",
    ]) {
      expect(classOf(prefix), prefix).toContain("{ROW_MOTION}");
    }
    for (const prefix of [
      "p-0.5 rounded-full text-textMuted hover:text-warn",
      "absolute right-1.5 flex items-center gap-1",
      "rounded-full text-textMuted hover:text-accent",
      "p-0.5 rounded-full opacity-0 group-hover:opacity-100",
    ]) {
      expect(classOf(prefix), prefix).toContain("{REVEAL_MOTION}");
    }
  });

  it("gives every row state a hover step on both fill and ring", () => {
    // The checked-out, selected and keyboard rows painted no hover at all, so
    // the rows a reader hovers most never answered the pointer.
    const body = source.slice(source.indexOf("function rowSurface("), source.indexOf("let gapChips"));
    const tones = [...body.matchAll(/return "([^"]+)"/g)].map(m => m[1]);
    expect(tones).toHaveLength(4);
    for (const tone of tones) {
      expect(tone, tone).toMatch(/\bring-[a-z]/i);
      expect(tone, tone).toMatch(/\bhover:ring-/);
    }
    // The keyboard row already rests on the hover fill, so its step is the
    // ring; every other state also steps its fill.
    for (const tone of tones.filter(t => !t.startsWith("bg-surfaceHover"))) {
      expect(tone, tone).toMatch(/\bhover:bg-/);
    }
    // Branch and tag rows both take their surface from the one owner.
    expect(markup).toContain("{rowSurface(branch.is_current, selected, isRowSelected)}");
    expect(markup).toContain("{rowSurface(false, $filterStore.selectedBranch === row.tag.name, isRowSelected)}");
  });

  it("leaves no row or reveal on the bare Tailwind transitions", () => {
    // Every site above used one of these before; a new one would bring the
    // snapping ring or the slow start back.
    const rowsAndReveals = [
      classOf("gp-cv-row relative w-full"),
      classOf("gp-cv-row w-full px-2 rounded-full"),
      classOf("gp-cv-row gp-label w-full"),
      classOf("p-0.5 rounded-full text-textMuted hover:text-warn"),
      classOf("absolute right-1.5 flex items-center gap-1"),
      classOf("rounded-full text-textMuted hover:text-accent"),
      classOf("p-0.5 rounded-full opacity-0 group-hover:opacity-100"),
    ].join("\n");
    expect(rowsAndReveals).not.toMatch(/\btransition-(colors|opacity)\b/);
  });
});
