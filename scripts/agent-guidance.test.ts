import { readFileSync } from "node:fs";
import { expect, it } from "vitest";

it.each(["AGENTS.md", "CLAUDE.md"])("%s exposes DevMap before the generated GitNexus block", (name) => {
  const guide = readFileSync(new URL(`../${name}`, import.meta.url), "utf8");
  const boundary = guide.indexOf("<!-- gitnexus:start -->");
  expect(boundary).toBeGreaterThan(0);
  const workflow = guide.slice(0, boundary);
  expect(workflow).toContain("devmap status --json");
  expect(workflow).toContain("gitpulse_codeintel_search");
  expect(workflow).toContain("devmap impact");
  expect(workflow).toContain(".devcouncil/repo_map.json");
  expect(workflow).toContain("unavailable");
  expect(workflow).toContain("truncated");
});
