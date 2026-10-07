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
    expect(source).toContain("agentKindLabel(row.kind)");
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

  it("takes directories from the sweep the tab chip also reads, and does not invent one", () => {
    expect(source).toContain("$agentDirectories.get(record.sessionId)");
    expect(source).not.toContain("cwd: null");
    // The sweep (cwd.ts, createAgentDirectorySweep) owns when to read, and
    // re-reads only when the set of sessions changes (cwd.test.ts). A second
    // reader here would re-run on every registry change.
    expect(source).not.toContain("readAgentCwds");
    expect(source).toContain("agentDirectories.refresh()");
  });

  it("names a task's repository from its registered id", () => {
    expect(source).toContain("repositoryPath: (id) => repoPaths.get(id) ?? null");
    expect(source).toContain("registered.want($board.runs.map((run) => run.repository_id))");
    expect(source).toContain("registered.refresh()");
  });

  it("says when registered repositories could not be read or were capped", () => {
    expect(source).toContain("$registeredStatus.error");
    expect(source).toContain("!$registeredStatus.complete");
  });
});
