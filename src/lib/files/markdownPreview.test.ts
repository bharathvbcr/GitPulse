import { describe, expect, it } from "vitest";
import { escapeHtml, renderMarkdown } from "./markdownPreview";

describe("markdownPreview", () => {
  it("escapes HTML before applying markdown so a file cannot inject markup", () => {
    expect(escapeHtml(`<img src=x onerror="alert(1)">`)).toBe(
      "&lt;img src=x onerror=&quot;alert(1)&quot;&gt;",
    );
    const html = renderMarkdown(`# Title\n\n<script>alert(1)</script>`);
    expect(html).toContain("&lt;script&gt;");
    expect(html).not.toContain("<script>");
    expect(html).toContain("<h1");
    expect(html).toContain("Title");
  });

  it("renders fenced code, inline code, and lists", () => {
    const html = renderMarkdown("```ts\nconst x = 1\n```\n\n- item\n\n`code`");
    expect(html).toContain("<pre");
    expect(html).toContain("const x = 1");
    expect(html).toContain("<li");
    expect(html).toContain("<code");
  });

  it("returns empty string for empty input", () => {
    expect(renderMarkdown("")).toBe("");
  });
});
