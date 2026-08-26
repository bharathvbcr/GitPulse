import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import TerminalPanel from "./TerminalPanel.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "TerminalPanel.svelte"),
  "utf8",
);

describe("TerminalPanel source contracts & safety hygiene", () => {
  it("invokes cmd_terminal_run with repoPath and args", () => {
    expect(source).toContain('invoke<TerminalRunResponse>("cmd_terminal_run"');
    expect(source).toContain("repoPath,");
    expect(source).toContain("args: tokenized.argv,");
  });

  it("invokes PTY session endpoints for interactive shell", () => {
    expect(source).toContain('"cmd_terminal_spawn"');
    expect(source).toContain('"cmd_terminal_write"');
    expect(source).toContain('"cmd_terminal_resize"');
    expect(source).toContain('"cmd_terminal_kill"');
  });

  it("tokenizes commands and checks for safety before execution in Console mode", () => {
    expect(source).toContain("tokenizeCommand(textToRun)");
    expect(source).toContain("validationError = tokenized.error");
  });

  it("journals executed terminal actions into harnessStore", () => {
    expect(source).toContain("harnessStore.recordAction({");
  });

  it("provides copy and clear output affordances in Console mode", () => {
    expect(source).toContain("clearOutput");
    expect(source).toContain("copyOutput");
  });
});

describe("TerminalPanel rendering", () => {
  it("renders the header with mode switcher", () => {
    const { body } = render(TerminalPanel);
    expect(body).toContain("Terminal");
    expect(body).toContain("Shell");
    expect(body).toContain("Console");
  });
});
