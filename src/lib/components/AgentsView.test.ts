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
    expect(html).not.toContain("No agent checkouts");
    expect(html).not.toContain("could not be read");
  });

  it("keeps gaps on the page when the attention column is hidden", () => {
    const gaps = source.indexOf('data-testid="agents-gaps"');
    const table = source.indexOf("<table");
    expect(gaps).toBeGreaterThan(-1);
    expect(table).toBeGreaterThan(gaps);
    expect(source).toContain("Repositories could not be read");
    expect(source).toContain("Still reading");
  });

  it("opens a checkout in the repository surface and reveals a live terminal after it is ready", () => {
    expect(source).toContain('interfaceStore.setGlobalSurface("repository")');
    expect(source).toContain("repoStore.openRepo");
    expect(source).toContain("onReady");
    expect(source).toContain("reveal?.()");
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

  it("reads directories again only when the set of sessions changes", () => {
    const effect = source.slice(source.indexOf("readAgentCwds(ids)") - 400, source.indexOf("readAgentCwds(ids)"));
    expect(source).toContain("const cwdTargets = $derived(agentCwdTargets($terminalSessions)");
    expect(effect).toContain("cwdTargets");
    // Reading the whole registry here would re-run the sweep on every title or status change.
    expect(effect).not.toContain("$terminalSessions");
    expect(source).toContain("agentDirectories.set(found)");
  });

  it("names a task's repository from its registered id", () => {
    expect(source).toContain("repositoryPath: (id) => repoPaths.get(id) ?? null");
    expect(source).toContain("registered.want($board.runs.map((run) => run.repository_id))");
  });
});
