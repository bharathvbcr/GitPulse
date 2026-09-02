import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import LanguageLogo from "./LanguageLogo.svelte";

describe("LanguageLogo", () => {
  it("renders SVG logo for given language name", () => {
    const { body } = render(LanguageLogo, {
      props: { language: "Rust", size: 16 },
    });
    expect(body).toContain("<svg");
    expect(body).toContain('width="16"');
    expect(body).toContain('height="16"');
    expect(body).toContain('title="Rust"');
  });

  it("resolves language logo from filePath", () => {
    const { body } = render(LanguageLogo, {
      props: { filePath: "src/App.svelte" },
    });
    expect(body).toContain("<svg");
    expect(body).toContain('title="Svelte"');
  });

  it("resolves TypeScript from .ts and .tsx files", () => {
    const ts = render(LanguageLogo, {
      props: { filePath: "main.ts" },
    });
    expect(ts.body).toContain('title="TypeScript"');

    const tsx = render(LanguageLogo, {
      props: { filePath: "Component.tsx" },
    });
    expect(tsx.body).toContain('title="TypeScript"');
  });

  it("handles fallback gracefully for unknown files", () => {
    const { body } = render(LanguageLogo, {
      props: { filePath: "unknown.xyz" },
    });
    expect(body).toContain("<svg");
    expect(body).toContain('title="File"');
  });

  it("applies custom class and title overrides", () => {
    const { body } = render(LanguageLogo, {
      props: { language: "Go", class: "my-custom-class", title: "Custom Go Tooltip" },
    });
    expect(body).toContain("my-custom-class");
    expect(body).toContain('title="Custom Go Tooltip"');
  });
});
