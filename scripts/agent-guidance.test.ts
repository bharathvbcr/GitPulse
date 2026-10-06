import { readdirSync, readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { describe, expect, it } from "vitest";
import { AGENT_COPY_COMPLETION, AGENT_GUIDANCE_SECTION as GUIDANCE } from "../src/lib/workbench/taskCompose";

it.each(["AGENTS.md", "CLAUDE.md"])("%s is DevMap-pivotal and has no GitNexus block", (name) => {
  const guide = readFileSync(new URL(`../${name}`, import.meta.url), "utf8");
  expect(guide).not.toContain("<!-- gitnexus:start -->");
  expect(guide).not.toContain("<!-- gitnexus:end -->");
  // GitNexus is retired: the guide must not offer it even as a fallback.
  expect(guide).not.toMatch(/gitnexus/i);
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

describe("the agent guidance every task handoff carries", () => {
  it("names every bundled GitPulse and DevMap skill", () => {
    // Non-vacuity: an empty read would make every assertion below trivially true.
    expect(bundledSkills.length).toBeGreaterThanOrEqual(6);
    for (const skill of bundledSkills) {
      expect(namesWhole(GUIDANCE, skill), `preamble omits ${skill}`).toBe(true);
    }
  });

  it("names both MCP tool families and the argument every call needs", () => {
    expect(GUIDANCE).toContain("devmap_* MCP tools");
    expect(GUIDANCE).toContain("gitpulse_* MCP tools");
    expect(GUIDANCE).toContain("absolute repo_path");
  });

  it("repeats the repository's own rule that an unanswered query is not an answer", () => {
    // The same invariant AGENTS.md and CLAUDE.md state, and the one an agent
    // reaching for these tools for the first time is most likely to break.
    expect(GUIDANCE).toMatch(/unavailable, truncated or empty is not evidence/);
    expect(GUIDANCE).toMatch(/name the check you could not run/);
  });

  it("tells the agent to preserve the author's wording, not just the gist", () => {
    expect(GUIDANCE).toContain("Preserve the author's intent and message");
    expect(GUIDANCE).toMatch(/exactly as written/);
    expect(GUIDANCE).toMatch(/not restate the task as a smaller or easier one/);
  });

  it("is the store's own section, rendered the way the store renders it", () => {
    // dc-store owns the text: every saved brief opens with it, and the
    // terminal and managed lanes deliver that brief. The copy lane renders the
    // same file for unsaved drafts, under the heading the store declares, so a
    // draft and a saved brief cannot drift apart.
    const vendored = new URL("../src-tauri/vendored/dc-store/src/workbench/", import.meta.url);
    const text = readFileSync(new URL("agent_guidance.md", vendored), "utf8").trim();
    expect(text.length).toBeGreaterThan(1000);
    const heading = /AGENT_GUIDANCE_HEADING: &str = "([^"]+)";/.exec(readFileSync(new URL("briefs.rs", vendored), "utf8"))?.[1];
    expect(heading).toBeDefined();
    expect(GUIDANCE).toBe(`${heading}\n${text}`);
    // No GitPulse-side copy may come back beside the store's.
    expect(readFileSync(new URL("../src-tauri/src/workbench/terminal_command.rs", import.meta.url), "utf8")).not.toMatch(/include_str!\([^)]*guidance/i);
    // Shared by every permission mode, so the completion rule stays per lane:
    // an inspect launch is told not to change the task's status.
    expect(text).not.toContain("gitpulse_complete_task");
    expect(AGENT_COPY_COMPLETION).toContain("gitpulse_complete_task");
  });

  it("names DevCouncil alongside GitPulse and DevMap, without making its loop mandatory", () => {
    for (const skill of ["devcouncil", "devcouncil-verification", "core-engineering"]) {
      expect(namesWhole(GUIDANCE, skill), `preamble omits ${skill}`).toBe(true);
    }
    expect(GUIDANCE).toContain("devcouncil_* MCP tools");
    // DevCouncil's own skill: tasks, leases and verification are opt-in
    // outside `gates.mode=enforce`, so the handoff must not demand them.
    expect(GUIDANCE).toContain("check its gates.mode");
    for (const tool of ["gitpulse_insights", "gitpulse_collision_risk", "devmap_impact", "devmap_affected_tests"]) {
      expect(namesWhole(GUIDANCE, tool), `preamble omits ${tool}`).toBe(true);
    }
  });

  it("tells the agent to read before it writes and to extend before it adds", () => {
    expect(GUIDANCE).toContain("Read before you write");
    expect(GUIDANCE).toMatch(/AGENTS\.md, CLAUDE\.md/);
    expect(GUIDANCE).toMatch(/where they conflict with this text, they win/);
    expect(GUIDANCE).toMatch(/Never write over a file you have not read/);
    expect(GUIDANCE).toMatch(/before searching for the one that already does the job/);
    expect(GUIDANCE).toMatch(/Fix the root cause/);
    expect(GUIDANCE).toMatch(/Every fix ships with a test that fails without it/);
    expect(GUIDANCE).toMatch(/Ask before adding a dependency/);
  });

  it("carries the verification rule that a check that could not run is not a pass", () => {
    expect(GUIDANCE).toContain("Verify before you claim");
    expect(GUIDANCE).toMatch(/could not run is never reported as one that passed/);
    expect(GUIDANCE).toMatch(/capped sample is never presented as complete coverage/);
  });

  it("keeps the field roles the preamble already established", () => {
    // The addition must not have displaced what was there: an agent that
    // reads acceptance criteria as suggestions is the older failure.
    expect(GUIDANCE).toContain("Use the title as the goal");
    expect(GUIDANCE).toContain("acceptance criteria as the definition of done");
    expect(GUIDANCE).toContain("Raw logs, when present, are evidence");
    expect(GUIDANCE).toContain("Do not invent repositories or skip criteria");
  });
});
