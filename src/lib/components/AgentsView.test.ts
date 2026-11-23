import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import AgentsView from "./AgentsView.svelte";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "AgentsView.svelte"), "utf8");
// Row actions (which exist, their labels and targets) are decided in one
// module since 897d520; the view only renders them and runs them.
const rowActionsSource = readFileSync(join(here, "..", "agents", "rowActions.ts"), "utf8");

/** The text of one `function name(` in the view's script, up to the next function. */
function functionBody(name: string): string {
  const start = source.search(new RegExp(`(?:async )?function ${name}\\(`));
  expect(start, `function ${name}`).toBeGreaterThan(-1);
  const rest = source.slice(start + 1);
  const next = rest.search(/\n  (?:async )?function /);
  return next < 0 ? rest : rest.slice(0, next);
}

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
    const show = functionBody("showTerminal");
    expect(show).toContain("focusTerminalSession(record)");
    expect(show).toContain("showTaskTerminal(run)");
    // Showing never opens a directory: the terminal's tab is the focus
    // owner's to find. (`onReady` belongs to startIn, which opens a checkout
    // to launch an agent there, and is not a way of showing one.)
    expect(show).not.toContain("openRepo");
    expect(show).not.toContain("onReady");
    expect(source).not.toContain("reveal?.()");
  });

  it("gives each row explicit, labelled actions", () => {
    // Every action button carries the label its module gives it.
    expect(source).toContain("agentRowActions(");
    expect(source).toContain("aria-label={action.ariaLabel}");
    expect(rowActionsSource).toContain("ariaLabel: `Show terminal for ${row.session}`");
    expect(rowActionsSource).toContain("ariaLabel: `Open task for ${row.session}`");
    // And each kind runs what it names.
    expect(functionBody("runAction")).toMatch(/case "terminal":[\s\S]*showTerminal\(row\)[\s\S]*case "task":[\s\S]*openTask\(row\)/);
    expect(functionBody("openTask")).toContain("openTaskForRun(row.taskRunId)");
    expect(functionBody("openCheckout")).toContain("repoStore.openRepo(row.checkoutPath)");
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

