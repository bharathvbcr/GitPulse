import { describe, expect, it } from "vitest";
import {
  dropElements,
  firstScriptBlock,
  scriptBlocks,
  stripMarkupComments,
  stripMarkupTags,
  visibleMarkupText,
} from "./markupText";

describe("stripMarkupComments", () => {
  it("removes comments that contain newlines", () => {
    // `/<!--.*?-->/` without the dotall flag leaves this whole comment in place.
    expect(stripMarkupComments("a<!--\nsecret\n-->b")).toBe("ab");
  });

  it("drops an unclosed comment rather than leaving the opener", () => {
    expect(stripMarkupComments("visible<!-- still secret")).toBe("visible");
  });

  it("repeats when removing one comment exposes another opener", () => {
    expect(stripMarkupComments("<!<!---->-->")).toBe("");
    expect(stripMarkupComments("<!<!---->--> kept")).toBe(" kept");
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
    expect(stripMarkupTags("<scr<script>ipt>")).not.toContain("<script");
  });

  it("can leave a gap where a tag was so neighbouring words stay apart", () => {
    expect(stripMarkupTags("<span>Net</span><span>+100</span>", " ").replace(/\s+/g, " ").trim()).toBe(
      "Net +100",
    );
  });
});

describe("visibleMarkupText", () => {
  it("drops a comment that spans lines and the tags around the words", () => {
    expect(visibleMarkupText("<!--\nhidden\n--><b>Net</b> <i>+100</i>")).toBe("Net +100");
  });
});

describe("dropElements", () => {
  it("removes an svg element without leaving its contents as text", () => {
    expect(dropElements('<button><svg><path d="M0"/></svg>Locate</button>', "svg")).toBe(
      "<button>Locate</button>",
    );
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
