import { readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { expect, it } from "vitest";

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

it("keeps the bundled DevMap skills and plugin marketplace available to Git", () => {
  const published = ["devmap", "devmap-debugging", "devmap-exploring", "devmap-impact", "devmap-refactoring"]
    .map((skill) => `.agents/skills/${skill}/SKILL.md`);
  published.push(".agents/plugins/marketplace.json");
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

it.each(["AGENTS.md", "CLAUDE.md"])("%s makes generated GitNexus rules conditional", (name) => {
  const guide = readFileSync(new URL(`../${name}`, import.meta.url), "utf8");
  const workflow = guide.slice(0, guide.indexOf("<!-- gitnexus:start -->"));
  expect(workflow).toContain("takes precedence over the generated GitNexus block");
  expect(workflow).toContain("GitNexus is optional");
  expect(workflow).not.toContain("Run both tools' applicable checks");
  for (const skill of ["devmap", "devmap-debugging", "devmap-exploring", "devmap-impact", "devmap-refactoring"]) {
    const path = `.agents/skills/${skill}/SKILL.md`;
    expect(workflow).toContain(path);
    const contents = readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
    expect(contents).toContain(`name: ${skill}\n`);
    expect(contents).toContain("devmap paths --json");
    expect(contents).not.toContain("Do not use GitNexus");
  }
});
