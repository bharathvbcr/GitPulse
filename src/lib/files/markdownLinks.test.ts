import { describe, expect, it, vi } from "vitest";
import {
  findFragmentTarget,
  handleMarkdownClick,
  MARKDOWN_ID_PREFIX,
  resolveMarkdownLink,
  type MarkdownClickContext,
  type MarkdownNote,
} from "./markdownLinks";

const note: MarkdownNote = { repoPath: "/work/repo", path: "docs/guide/README.md" };

describe("resolveMarkdownLink", () => {
  it("jumps within the note for a fragment, decoded", () => {
    expect(resolveMarkdownLink("#tables", note)).toEqual({ kind: "fragment", id: "tables" });
    expect(resolveMarkdownLink("#caf%C3%A9", note)).toEqual({ kind: "fragment", id: "café" });
    expect(resolveMarkdownLink("#1", null)).toEqual({ kind: "fragment", id: "1" });
  });

  it("refuses the bare # the renderer leaves where it refused a destination", () => {
    // `[x](javascript:alert(1))` renders as `href="#"`: a jump to the top
    // would read as the link having worked.
    expect(resolveMarkdownLink("#", note).kind).toBe("refused");
    expect(resolveMarkdownLink("", note).kind).toBe("refused");
    expect(resolveMarkdownLink("   ", note).kind).toBe("refused");
  });

  it("hands http, https and mailto to the OS, and nothing else", () => {
    expect(resolveMarkdownLink("https://example.com/a?b=1#c", note)).toEqual({
      kind: "external",
      url: "https://example.com/a?b=1#c",
    });
    expect(resolveMarkdownLink("http://example.com", null).kind).toBe("external");
    expect(resolveMarkdownLink("mailto:someone@example.com", null).kind).toBe("external");
    for (const href of [
      "javascript:alert(1)",
      "JaVaScRiPt:alert(1)",
      "data:text/html,<script>alert(1)</script>",
      "file:///etc/passwd",
      "vbscript:x",
      "tauri://localhost/index.html",
      "ftp://example.com",
      "//evil.example/x",
      "\\\\server\\share",
      "C:/Windows/system.ini",
    ]) {
      expect(resolveMarkdownLink(href, note).kind, href).toBe("refused");
    }
  });

  it("resolves relative paths against the note's folder, and /paths against the repository", () => {
    expect(resolveMarkdownLink("setup.md", note)).toEqual({ kind: "file", path: "docs/guide/setup.md", fragment: null });
    expect(resolveMarkdownLink("./setup.md#install", note)).toEqual({
      kind: "file",
      path: "docs/guide/setup.md",
      fragment: "install",
    });
    expect(resolveMarkdownLink("../api.md", note)).toEqual({ kind: "file", path: "docs/api.md", fragment: null });
    expect(resolveMarkdownLink("/README.md", note)).toEqual({ kind: "file", path: "README.md", fragment: null });
    expect(resolveMarkdownLink("Wiki%20Link.md", note)).toEqual({
      kind: "file",
      path: "docs/guide/Wiki Link.md",
      fragment: null,
    });
    expect(resolveMarkdownLink("a.md?plain=1#L3", note)).toEqual({ kind: "file", path: "docs/guide/a.md", fragment: "L3" });
    const atRoot = { repoPath: "/work/repo", path: "README.md" };
    expect(resolveMarkdownLink("docs/a.md", atRoot)).toEqual({ kind: "file", path: "docs/a.md", fragment: null });
  });

  it("refuses a path that climbs out of the repository, however it is spelled", () => {
    for (const href of [
      "../../../outside.md",
      "/../outside.md",
      "%2E%2E/%2E%2E/%2E%2E/outside.md",
      "..%2F..%2F..%2Foutside.md",
      "../../../../../../etc/passwd",
    ]) {
      expect(resolveMarkdownLink(href, note).kind, href).toBe("refused");
    }
  });

  it("refuses folders, malformed escapes, NULs and over-long links", () => {
    expect(resolveMarkdownLink("docs/", note).kind).toBe("refused");
    expect(resolveMarkdownLink("%E0%A4%A.md", note).kind).toBe("refused");
    expect(resolveMarkdownLink("a%00.md", note).kind).toBe("refused");
    expect(resolveMarkdownLink(`https://example.com/${"a".repeat(5000)}`, note).kind).toBe("refused");
    expect(resolveMarkdownLink(`${"a/".repeat(3000)}x.md`, note).kind).toBe("refused");
  });

  it("refuses relative links when the text is not a file", () => {
    expect(resolveMarkdownLink("setup.md", null).kind).toBe("refused");
    expect(resolveMarkdownLink("?x#install", note)).toEqual({ kind: "fragment", id: "install" });
  });

  it("says why every refusal happened", () => {
    for (const href of ["#", "javascript:x", "../../../x", "docs/", "a%00", "x.md"]) {
      const action = resolveMarkdownLink(href, href === "x.md" ? null : note);
      expect(action.kind).toBe("refused");
      if (action.kind === "refused") expect(action.reason.length, href).toBeGreaterThan(10);
    }
  });
});

/** The slice of `Element` the delegate reads, without a DOM. */
function element(attrs: Record<string, string>, matches: string[] = []) {
  return {
    id: attrs.id ?? "",
    getAttribute: (name: string) => attrs[name] ?? null,
    closest(selector: string) {
      return matches.includes(selector) ? this : null;
    },
    scrollIntoView: vi.fn(),
  };
}

function container(children: ReturnType<typeof element>[]) {
  return { querySelectorAll: () => children } as unknown as ParentNode;
}

function event(target: unknown) {
  return { target, preventDefault: vi.fn() } as unknown as MouseEvent & { preventDefault: ReturnType<typeof vi.fn> };
}

function context(overrides: Partial<MarkdownClickContext> = {}): MarkdownClickContext {
  return {
    container: container([]),
    note,
    openExternal: vi.fn(async () => {}),
    openFile: vi.fn(),
    copy: vi.fn(async () => true),
    report: vi.fn(),
    ...overrides,
  };
}

describe("handleMarkdownClick", () => {
  it("never lets the webview follow a link, whatever the link is", () => {
    for (const href of ["https://example.com", "#x", "a.md", "javascript:x", "#", "../../../x"]) {
      const click = event(element({ href }, ["a[href]"]));
      handleMarkdownClick(click, context());
      expect(click.preventDefault, href).toHaveBeenCalled();
    }
  });

  it("opens external links through the OS opener and reports a failure", async () => {
    const failing = context({ openExternal: vi.fn(async () => Promise.reject(new Error("denied"))) });
    handleMarkdownClick(event(element({ href: "https://example.com" }, ["a[href]"])), failing);
    expect(failing.openExternal).toHaveBeenCalledWith("https://example.com");
    await vi.waitFor(() => expect(failing.report).toHaveBeenCalledWith(expect.stringContaining("denied")));
  });

  it("scrolls to a fragment inside this note only, by its prefixed id", () => {
    const heading = element({ id: `${MARKDOWN_ID_PREFIX}tables` });
    const ctx = context({ container: container([element({ id: "tables" }), heading]) });
    handleMarkdownClick(event(element({ href: "#tables" }, ["a[href]"])), ctx);
    expect(heading.scrollIntoView).toHaveBeenCalled();
    expect(ctx.report).not.toHaveBeenCalled();
  });

  it("reports a fragment the note does not have", () => {
    const ctx = context();
    handleMarkdownClick(event(element({ href: "#nowhere" }, ["a[href]"])), ctx);
    expect(ctx.report).toHaveBeenCalledWith(expect.stringContaining("nowhere"));
  });

  it("opens repository files and reports refusals", () => {
    const ctx = context();
    handleMarkdownClick(event(element({ href: "../api.md#x" }, ["a[href]"])), ctx);
    expect(ctx.openFile).toHaveBeenCalledWith("docs/api.md", "x");
    handleMarkdownClick(event(element({ href: "javascript:x" }, ["a[href]"])), ctx);
    expect(ctx.report).toHaveBeenCalledWith(expect.stringContaining("GitPulse opens"));
    expect(ctx.openExternal).not.toHaveBeenCalled();
  });

  it("leaves clicks that are not on a link alone", () => {
    const click = event(element({}, []));
    handleMarkdownClick(click, context());
    expect(click.preventDefault).not.toHaveBeenCalled();
    const textNode = event({});
    handleMarkdownClick(textNode, context());
    expect(textNode.preventDefault).not.toHaveBeenCalled();
  });
});

describe("findFragmentTarget", () => {
  it("matches ids exactly, including ones no CSS selector accepts unescaped", () => {
    const footnote = element({ id: `${MARKDOWN_ID_PREFIX}1` });
    const odd = element({ id: `${MARKDOWN_ID_PREFIX}a"b]c` });
    const root = container([footnote, odd]);
    expect(findFragmentTarget(root, "1")).toBe(footnote);
    expect(findFragmentTarget(root, 'a"b]c')).toBe(odd);
    expect(findFragmentTarget(root, "a")).toBeNull();
  });
});
