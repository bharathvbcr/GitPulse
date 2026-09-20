import { readdirSync, readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { describe, expect, it } from "vitest";
import { AGENT_COPY_PREAMBLE } from "../src/lib/workbench/taskCompose";

it.each(["AGENTS.md", "CLAUDE.md"])("%s is DevMap-pivotal and has no GitNexus block", (name) => {
  const guide = readFileSync(new URL(`../${name}`, import.meta.url), "utf8");
  expect(guide).not.toContain("<!-- gitnexus:start -->");
  expect(guide).not.toContain("<!-- gitnexus:end -->");
  expect(guide).not.toContain("MUST run impact analysis before editing");
  expect(guide).toContain("devmap status --json");
  expect(guide).toContain("devmap_search");
  expect(guide).toContain("gitpulse_codeintel_search");
  expect(guide).toContain("devmap impact");
  expect(guide).toContain("devmap paths --json");
  expect(guide).toContain("devmap build --manifest");
  expect(guide).toContain("Never copy another worktree");
  expect(guide).toContain("unavailable");
  expect(guide).toContain("truncated");
  expect(guide).toContain("DevMap is the primary index");
  expect(guide).toContain("components and modules");
  expect(guide).toContain("Manvi wraps them");
  expect(guide).toContain("GitPulse uses");
});

it("keeps the bundled DevMap skills and plugin marketplace available to Git", () => {
  const published = ["devmap", "devmap-debugging", "devmap-exploring", "devmap-impact", "devmap-refactoring"]
    .map((skill) => `.agents/skills/${skill}/SKILL.md`);
  published.push(".agents/plugins/marketplace.json");
  published.push(".cursor/rules/devmap.mdc");
  for (const skill of ["devmap", "devmap-debugging", "devmap-exploring", "devmap-impact", "devmap-refactoring"]) {
    published.push(`.cursor/skills/${skill}/SKILL.md`);
  }
  for (const path of published) {
    const result = spawnSync("git", ["check-ignore", "--no-index", "-q", path], {
      cwd: new URL("..", import.meta.url),
    });
    expect(result.error).toBeUndefined();
    expect(result.status, path).toBe(1);
  }
  for (const path of [".agents/session.json", ".agents/skills/private/SKILL.md", ".agents/skills/devmap/local.txt"]) {
    const result = spawnSync("git", ["check-ignore", "--no-index", "-q", path], {
      cwd: new URL("..", import.meta.url),
    });
    expect(result.error).toBeUndefined();
    expect(result.status, path).toBe(0);
  }
});

it.each(["AGENTS.md", "CLAUDE.md"])("%s lists the five DevMap skills", (name) => {
  const guide = readFileSync(new URL(`../${name}`, import.meta.url), "utf8");
  for (const skill of ["devmap", "devmap-debugging", "devmap-exploring", "devmap-impact", "devmap-refactoring"]) {
    const path = `.agents/skills/${skill}/SKILL.md`;
    expect(guide).toContain(path);
    const contents = readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
    expect(contents).toContain(`name: ${skill}\n`);
    expect(contents).toContain("devmap paths --json");
  }
});

it("does not ship GitNexus skills under .claude/skills", () => {
  const result = spawnSync("test", ["!", "-e", ".claude/skills/gitnexus"], {
    cwd: new URL("..", import.meta.url),
  });
  expect(result.status).toBe(0);
});

/**
 * The guidance an agent gets when it is handed a *task*, rather than when it
 * opens the repository.
 *
 * `AGENTS.md` and `CLAUDE.md` reach an agent that is already working in this
 * checkout. A copied task reaches one that is not: the handoff sends only
 * `{id, request_id, expected_revision}` to Manvi, and a clipboard copy is
 * pasted into a session that has never seen this repository. Everything the
 * two guides say about DevMap and GitPulse was therefore unavailable on
 * exactly the path where an agent is least oriented, and the preamble said
 * nothing about either.
 *
 * The skill names are read from the directories that ship them rather than
 * listed here, so adding or renaming a skill fails this test instead of
 * quietly leaving the preamble a version behind.
 */
const skillNames = (dir: string): string[] =>
  readdirSync(new URL(`../${dir}`, import.meta.url), { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort();
const bundledSkills = [...skillNames(".agents/skills"), ...skillNames("plugins/gitpulse/skills")];
/** "devmap" must match on its own, not inside "devmap-impact". */
const namesWhole = (haystack: string, needle: string): boolean =>
  new RegExp(`(?<![\\w-])${needle.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?![\\w-])`).test(haystack);

describe("the task handoff preamble carries this project's own guidance", () => {
  it("names every bundled GitPulse and DevMap skill", () => {
    // Non-vacuity: an empty read would make every assertion below trivially true.
    expect(bundledSkills.length).toBeGreaterThanOrEqual(6);
    for (const skill of bundledSkills) {
      expect(namesWhole(AGENT_COPY_PREAMBLE, skill), `preamble omits ${skill}`).toBe(true);
    }
  });

  it("names both MCP tool families and the argument every call needs", () => {
    expect(AGENT_COPY_PREAMBLE).toContain("devmap_* MCP tools");
    expect(AGENT_COPY_PREAMBLE).toContain("gitpulse_* MCP tools");
    expect(AGENT_COPY_PREAMBLE).toContain("absolute repo_path");
  });

  it("repeats the repository's own rule that an unanswered query is not an answer", () => {
    // The same invariant AGENTS.md and CLAUDE.md state, and the one an agent
    // reaching for these tools for the first time is most likely to break.
    expect(AGENT_COPY_PREAMBLE).toMatch(/unavailable, truncated or empty is not evidence/);
    expect(AGENT_COPY_PREAMBLE).toMatch(/name the check you could not run/);
  });

  it("tells the agent to preserve the author's wording, not just the gist", () => {
    expect(AGENT_COPY_PREAMBLE).toContain("Preserve the author's intent and message");
    expect(AGENT_COPY_PREAMBLE).toMatch(/exactly as written/);
    expect(AGENT_COPY_PREAMBLE).toMatch(/not restate the task as a smaller or easier one/);
  });

  it("keeps the field roles the preamble already established", () => {
    // The addition must not have displaced what was there: an agent that
    // reads acceptance criteria as suggestions is the older failure.
    expect(AGENT_COPY_PREAMBLE).toContain("Use the title as the goal");
    expect(AGENT_COPY_PREAMBLE).toContain("acceptance criteria as the definition of done");
    expect(AGENT_COPY_PREAMBLE).toContain("Raw logs, when present, are evidence");
    expect(AGENT_COPY_PREAMBLE).toContain("Do not invent repositories or skip criteria");
  });
});
