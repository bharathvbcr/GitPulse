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
    expect(html).toContain("Task runs have not been read yet");
    expect(html).toContain("Still reading");
    expect(html).not.toContain("No agent sessions");
    expect(html).not.toContain("couldn't be read");
  });

  it("keeps gaps on the page when the attention column is hidden", () => {
    const gaps = source.indexOf('data-testid="agents-gaps"');
    const table = source.indexOf("<table");
    expect(gaps).toBeGreaterThan(-1);
    expect(table).toBeGreaterThan(gaps);
    expect(source).toContain("Sessions couldn't be read");
    expect(source).toContain("Still reading");
  });

  it("shows a terminal through the focus owner, in the tab that hosts it, not by revealing it by hand", () => {
    // focusTerminalSession owns surface → hosting tab → dock → reveal. A
    // hand-rolled reveal after opening the row's directory skipped the dock
    // and opened the cwd, which is not the tab the terminal lives in.
    const show = source.slice(source.indexOf("async function showTerminal"), source.indexOf("async function openTask"));
    expect(show).toContain("focusTerminalSession(record)");
    expect(show).toContain("showTaskTerminal(run)");
    expect(show).not.toContain("onReady");
    expect(show).not.toContain("openRepo");
    expect(source).not.toContain("reveal?.()");
    // Starting an agent in a checkout is a launch, not a reveal: like the
    // Worktrees fork it waits for the opened tab's canonical path (`onReady`)
    // and hands the launch to that tab's dock.
    const start = source.slice(source.indexOf("async function startIn"), source.indexOf("async function reviewChanges"));
    expect(start).toContain("onReady");
    expect(start).toContain("terminalLaunchRequests.request(at, launcher)");
  });

  it("gives each row explicit, labelled actions", () => {
    // The actions and their labels are decided in agents/rowActions.ts
    // ("Show terminal for …", "Open task for …"; pinned in rowActions.test.ts);
    // the view renders each with its own label and routes it to its handler.
    expect(source).toContain("agentRowActions(row, ");
    expect(source).toContain("aria-label={action.ariaLabel}");
    expect(source).toContain("onclick={() => runAction(action)}");
    expect(source).toMatch(/case "terminal":\s*return void showTerminal\(row\);/);
    expect(source).toMatch(/case "task":\s*return void openTask\(row\);/);
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
    // The mapping lives in agents/repoAttention.ts (`planeTerminals`), which
    // the header shares; it leaves an unread directory null (repoAttention.test.ts).
    expect(source).toContain("planeTerminals($terminalSessions, $sessionActivity, $agentDirectories)");
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

  it("ticks its clock on the visibility-aware timer, not a raw interval", () => {
    // ft-0ff882a6: a raw one-second setInterval kept waking the renderer
    // while the window was hidden.
    expect(source).not.toMatch(/\bsetInterval\(/);
    expect(source).toContain("const stopClock = createAdaptiveTimer(() => {");
  });

  it("wires onopenTask on the embedded AgentInbox to requestTaskOpen", () => {
    expect(source).toContain("requestTaskOpen");
    expect(source).toMatch(/<AgentInbox[^>]*onopenTask=\{openInboxTask\}/);
  });
});

