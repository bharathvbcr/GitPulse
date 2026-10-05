import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import AgentCommitMenu from "./AgentCommitMenu.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "AgentCommitMenu.svelte"),
  "utf8",
);

describe("AgentCommitMenu", () => {
  it("renders regular button by default", () => {
    const { body } = render(AgentCommitMenu, { props: { compact: false } });
    expect(body).toContain("Ask Agent to Commit");
    expect(body).toContain('aria-haspopup="dialog"');
    expect(body).toContain('data-agent-commit-menu');
  });

  it("renders compact button when compact is true", () => {
    const { body } = render(AgentCommitMenu, { props: { compact: true } });
    expect(body).toContain("Ask Agent");
    expect(body).toContain('rounded-full');
    expect(body).toContain('data-agent-commit-menu');
  });

  it("includes Antigravity and Claude Code launch options in source", () => {
    expect(source).toContain("handleLaunch");
    expect(source).toContain('"agy"');
    expect(source).toContain('"claude"');
    expect(source).toContain("Ask Antigravity (agy)");
    expect(source).toContain("Ask Claude Code");
  });

  it("provides Developer CLI and Python SDK inspection and copying", () => {
    expect(source).toContain("getAgentCommitCliCommand");
    expect(source).toContain("getAgentCommitSdkSnippet");
    expect(source).toContain("CLI & Developer SDK");
    expect(source).toContain("handleCopy");
    expect(source).toContain("copyText");
  });

  it("binds popover dismissal to the menu root", () => {
    expect(source).toContain("use:popover={dismissal}");
    expect(source).toContain("inside: \"[data-agent-commit-menu]\"");
    expect(source).toContain("LAYERS.MENU");
  });

  it("handles staging scope toggle and custom note", () => {
    expect(source).toContain("scope === \"staged\"");
    expect(source).toContain("userNote");
    expect(source).toContain("agent-commit-note");
  });

  it("fails closed on merge conflicts and clean working trees", () => {
    expect(source).toContain("AGENT_COMMIT_CONFLICTS");
    expect(source).toContain("AGENT_COMMIT_CLEAN");
    expect(source).toContain("AGENT_COMMIT_NO_REPO");
    expect(source).toContain("conflictedCount > 0");
    expect(source).toContain("disabledReason");
  });
});
