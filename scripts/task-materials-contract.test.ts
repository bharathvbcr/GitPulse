import { readFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";
import { compile } from "svelte/compiler";
import { render } from "svelte/server";
import TaskBoard from "../src/lib/components/TaskBoard.svelte";

const panels = ["TaskEditor", "WorkspaceEditor", "AutomaticEnhancements", "AttentionInbox", "TaskEnhancements", "TaskRuns", "AgentDecisions", "NativeNotificationSettings"];
const read = (name: string) => readFileSync(new URL(`../src/lib/components/${name}.svelte`, import.meta.url), "utf8");

describe("Tasks material coverage", () => {
  it.each(panels)("%s participates in the shared materials, including its fields and nested panels", (name) => {
    const source = read(name);
    const style = source.slice(source.indexOf("<style>"));
    expect(style).not.toMatch(/background(?:-color)?:\s*rgb\(var\(--c-(?:bg|surface|surface-hover)\)\)/);
    expect(compile(source, { generate: "client", filename: `${name}.svelte` }).warnings).toEqual([]);
  });

  it("blurs the actual automation popover and sticky editor headers", () => {
    expect(read("AutomaticEnhancements")).toContain('class:shadow-float={compact}');
    for (const name of ["TaskEditor", "WorkspaceEditor"]) {
      expect(read(name)).toContain('class="gp-glass shadow-float"');
      expect(read(name)).toContain("position:sticky");
    }
  });

  it("renders one decorative liquid scope selection with an accessible selected state", () => {
    vi.stubGlobal("navigator", { platform: "MacIntel", maxTouchPoints: 0 });
    try {
      const body = render(TaskBoard).body;
      expect(body.match(/<span class="gp-liquid-selection gp-gpu(?: [^"]*)?" aria-hidden="true"/g)).toHaveLength(1);
      expect(body).toContain('aria-pressed="true"');
      expect(body).toContain('aria-hidden="true"');
      expect(read("TaskBoard")).toContain("crossfade(liquidSelection())");
    } finally { vi.unstubAllGlobals(); }
  });
});
