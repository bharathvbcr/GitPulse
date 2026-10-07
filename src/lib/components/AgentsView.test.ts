import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import AgentsView from "./AgentsView.svelte";

const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "AgentsView.svelte"), "utf8");

describe("AgentsView", () => {
  it("does not call an unread task board an empty workspace", () => {
    const html = render(AgentsView).body;
    expect(html).toContain('data-testid="agents-view"');
    expect(html).toContain("Task attempts have not been read yet");
    expect(html).toContain("Still reading");
    expect(html).not.toContain("No agent sessions");
    expect(html).not.toContain("could not be read");
  });

  it("keeps gaps on the page when the attention column is hidden", () => {
    const gaps = source.indexOf('data-testid="agents-gaps"');
    const table = source.indexOf("<table");
    expect(gaps).toBeGreaterThan(-1);
    expect(table).toBeGreaterThan(gaps);
    expect(source).toContain("Sessions could not be read");
    expect(source).toContain("Still reading");
  });

  it("shows a terminal through the focus owner, in the tab that hosts it, not by revealing it by hand", () => {
    // focusTerminalSession owns surface → hosting tab → dock → reveal. A
    // hand-rolled reveal after opening the row's directory skipped the dock
    // and opened the cwd, which is not the tab the terminal lives in.
    expect(source).toContain("focusTerminalSession(record)");
    expect(source).toContain("showTaskTerminal(run)");
    expect(source).not.toContain("reveal?.()");
    expect(source).not.toContain("onReady");
  });

  it("gives each row explicit, labelled actions", () => {
    expect(source).toContain('aria-label="Show terminal for {row.session}"');
    expect(source).toContain('aria-label="Open task for {row.session}"');
    expect(source).toContain("openTaskForRun(row.taskRunId)");
    expect(source).toContain("repoStore.openRepo(row.checkoutPath)");
    expect(source).toContain("kindLabel(row.kind)");
  });

  it("watches task attempts only while this surface is showing", () => {
    expect(source).toContain("createBoardAgents");
    expect(source).toContain("board.start()");
    expect(source).toContain("board.stop()");
    expect(source).toContain('globalSurface === "agents"');
  });

  it("uses the workspace plate so native text keeps its color", () => {
    expect(source).toContain("gp-workspace");
    expect(source).toContain("bg-background");
  });

  it("asks the terminal where it is, and does not invent a directory", () => {
    expect(source).toContain("readAgentCwds");
    expect(source).toContain("$agentDirectories.get(record.sessionId)");
    expect(source).not.toContain("cwd: null");
  });

  it("re-reads directories when the set of sessions changes, not on every registry update", () => {
    const effect = source.slice(source.indexOf("readAgentCwds(ids)") - 400, source.indexOf("readAgentCwds(ids)"));
    expect(source).toContain("const cwdKey = $derived(agentCwdTargets($terminalSessions)");
    expect(effect).toContain("cwdKey");
    expect(effect).not.toContain("$terminalSessions");
  });
});
