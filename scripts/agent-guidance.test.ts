import { readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { expect, it } from "vitest";

it.each(["AGENTS.md", "CLAUDE.md"])("%s exposes DevMap before the generated GitNexus block", (name) => {
  const guide = readFileSync(new URL(`../${name}`, import.meta.url), "utf8");
  const boundary = guide.indexOf("<!-- gitnexus:start -->");
  expect(boundary).toBeGreaterThan(0);
  const workflow = guide.slice(0, boundary);
  expect(workflow).toContain("devmap status --json");
  expect(workflow).toContain("gitpulse_codeintel_search");
  expect(workflow).toContain("devmap impact");
  expect(workflow).toContain("devmap paths --json");
  expect(workflow).toContain("devmap build --manifest");
  expect(workflow).toContain("Never copy another worktree");
  expect(workflow).toContain("unavailable");
  expect(workflow).toContain("truncated");
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
