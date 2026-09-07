import { describe, expect, it } from "vitest";
import {
  firstScriptBlock,
  scriptBlocks,
  stripMarkupComments,
  stripMarkupTags,
} from "./markupText";

describe("stripMarkupComments", () => {
  it("removes comments that contain newlines", () => {
    // `/<!--.*?-->/` without the dotall flag leaves this whole comment in place.
    expect(stripMarkupComments("a<!--\nsecret\n-->b")).toBe("ab");
  });

  it("drops an unclosed comment rather than leaving the opener", () => {
    expect(stripMarkupComments("visible<!-- still secret")).toBe("visible");
  });
});

describe("stripMarkupTags", () => {
  it("drops an unclosed tag that a single replace(/<[^>]*>/) would keep", () => {
    expect(stripMarkupTags("ok<script")).toBe("ok");
    expect(stripMarkupTags("ok<script").includes("<script")).toBe(false);
  });

  it("strips nested-looking tags until no markup delimiters remain", () => {
    expect(stripMarkupTags("hello<b>world</b>")).toBe("helloworld");
    expect(stripMarkupTags("x<tspan>y</tspan>z")).toBe("xyz");
  });
});

describe("scriptBlocks", () => {
  it("finds upper-case SCRIPT tags the HTML-filter regexp missed", () => {
    const block = firstScriptBlock("<SCRIPT lang='ts'>const x = 1;</SCRIPT>template");
    expect(block?.inner).toBe("const x = 1;");
    expect(block?.after).toBe("template");
  });

  it("does not treat scription as a script tag", () => {
    expect(scriptBlocks("<scription>nope</scription>")).toEqual([]);
  });

  it("returns every script block in a Svelte-shaped file", () => {
    const src = "<script module>a</script>\n<script>b</script>\n<div/>";
    expect(scriptBlocks(src).map((block) => block.inner)).toEqual(["a", "b"]);
  });
});
