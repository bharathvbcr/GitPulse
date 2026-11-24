import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { compile } from "svelte/compiler";
import { render } from "svelte/server";
import { describe, expect, it } from "vitest";
import FindField from "./FindField.svelte";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "FindField.svelte"), "utf8");

describe("FindField", () => {
  it("compiles with zero accessibility warnings and opens inline without a wrapper element", () => {
    const compiled = compile(source, { generate: "client" });
    const a11yWarnings = compiled.warnings.filter((w) => w.code.startsWith("a11y_"));
    expect(a11yWarnings).toEqual([]);

    // Opening in place requires `{#if open}` at the component root so the open pill
    // sits directly in the parent toolbar's flex slot.
    expect(source).toMatch(/<\/script>\s*\n\s*\{#if open\}/);
  });

  it("renders only the Find pill when closed without ongotoline", () => {
    const { body } = render(FindField, {
      props: {
        label: "Find in this diff",
        placeholder: "Find in diff…",
        shortcut: "⌘F",
        status: "",
        hasMatches: false,
        onstep: () => {},
      },
    });
    expect(body).toContain("data-find-trigger");
    expect(body).toContain(">⌘F<");
    expect(body).not.toContain("data-goto-line-trigger");
    expect(body).not.toContain("data-find-field");
  });

  it("renders both Find and Go to line pills in the same slot when closed with ongotoline", () => {
    const { body } = render(FindField, {
      props: {
        label: "Find in file",
        placeholder: "Find in file…",
        shortcut: "⌘F",
        goToShortcut: "⌘G",
        lineCount: 42,
        status: "",
        hasMatches: false,
        onstep: () => {},
        ongotoline: () => {},
      },
    });
    expect(body).toContain("data-find-trigger");
    expect(body).toContain("data-goto-line-trigger");
    expect(body).toContain(">Go to line<");
    expect(body).toContain(">⌘G<");
    expect(body).not.toMatch(/data-goto-line-trigger[^>]*disabled/);
  });

  it("disables Go to line when the file is empty", () => {
    const { body } = render(FindField, {
      props: {
        label: "Find in file",
        placeholder: "Find in file…",
        shortcut: "⌘F",
        goToShortcut: "⌘G",
        lineCount: 0,
        status: "",
        hasMatches: false,
        onstep: () => {},
        ongotoline: () => {},
      },
    });
    expect(body).toContain("data-goto-line-trigger");
    expect(body).toContain("disabled");
  });

  it("renders the open Find pill with match status, colon-line preview, and Go-to-line toggle", () => {
    const { body: matchBody } = render(FindField, {
      props: {
        open: true,
        mode: "find",
        query: "needle",
        label: "Find in file",
        placeholder: "Find in file…",
        shortcut: "⌘F",
        goToShortcut: "⌘G",
        lineCount: 100,
        status: "2 of 5",
        hasMatches: true,
        onstep: () => {},
        ongotoline: () => {},
      },
    });
    expect(matchBody).toContain("data-find-field");
    expect(matchBody).toContain('data-find-mode="find"');
    expect(matchBody).toContain('data-goto-mode-toggle');
    expect(matchBody).toContain("2 of 5");

    const { body: colonBody } = render(FindField, {
      props: {
        open: true,
        mode: "find",
        query: ":42:10",
        label: "Find in file",
        placeholder: "Find in file…",
        shortcut: "⌘F",
        lineCount: 100,
        status: "no matches",
        hasMatches: false,
        onstep: () => {},
        ongotoline: () => {},
      },
    });
    expect(colonBody).toContain("Line 42, Col 10");
  });

  it("renders the unified Go-to-line mode inside the same pill and falls back to find when unsupported", () => {
    const { body } = render(FindField, {
      props: {
        open: true,
        mode: "line",
        label: "Find in file",
        placeholder: "Find in file…",
        shortcut: "⌘F",
        lineCount: 120,
        status: "",
        hasMatches: false,
        onstep: () => {},
        ongotoline: () => {},
      },
    });
    expect(body).toContain('data-find-mode="line"');
    expect(body).toContain("data-goto-line-input");
    expect(body).toContain("data-goto-submit");
    expect(body).toContain("data-find-mode-toggle");
    expect(body).toContain('placeholder="Line (1–120)…"');

    const { body: unsupportedBody } = render(FindField, {
      props: {
        open: true,
        mode: "line",
        label: "Find in this diff",
        placeholder: "Find in diff…",
        shortcut: "⌘F",
        status: "",
        hasMatches: false,
        onstep: () => {},
      },
    });
    expect(unsupportedBody).toContain('data-find-mode="find"');
    expect(unsupportedBody).not.toContain("data-goto-line-input");
  });
});
