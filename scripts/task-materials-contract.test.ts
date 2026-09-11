import { readFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";
import { compile } from "svelte/compiler";
import { render } from "svelte/server";
import TaskBoard from "../src/lib/components/TaskBoard.svelte";

const panels = ["TaskEditor", "WorkspaceEditor", "AutomaticEnhancements", "AttentionInbox", "TaskManviAssist", "TaskRuns", "AgentDecisions", "NativeNotificationSettings", "NativeNotificationBridge"];
const read = (name: string) => readFileSync(new URL(`../src/lib/components/${name}.svelte`, import.meta.url), "utf8");

describe("Tasks material coverage", () => {
  it.each(panels)("%s participates in the shared materials, including its fields and nested panels", (name) => {
    const source = read(name);
    const style = source.slice(source.indexOf("<style>"));
    expect(style).not.toMatch(/background(?:-color)?:\s*rgb\(var\(--c-(?:bg|surface|surface-hover)\)\)/);
    expect(compile(source, { generate: "client", filename: `${name}.svelte` }).warnings).toEqual([]);
  });

  it("blurs the actual automation popover and sheet headers", () => {
    expect(read("AutomaticEnhancements")).toContain('class:shadow-float={compact}');
    for (const name of ["TaskEditor", "WorkspaceEditor"]) {
      expect(read(name)).toContain('class="gp-glass shadow-float"');
    }
  });

  it("keeps the workspace save bar in sheet chrome instead of a sticky overlay", () => {
    const source = read("WorkspaceEditor");
    const header = source.slice(source.indexOf("<style>")).match(/(?:^|})\s*header\{([^}]*)\}/)?.[1];
    const footer = source.slice(source.indexOf("<style>")).match(/(?:^|})\s*footer\{([^}]*)\}/)?.[1];
    expect(header).toBeDefined();
    expect(footer).toBeDefined();
    expect(header).not.toContain("position:sticky");
    expect(footer).not.toContain("position:sticky");
    expect(footer).not.toMatch(/background(?:-color)?:/);
    expect(source).toContain('class="sheet-body"');
    expect(source).toMatch(/<\/div>\s*<footer>/);
  });

  it("opens native notification review on the shared glass card, not an opaque plate", () => {
    const source = read("NativeNotificationBridge");
    expect(source).toContain('class="native-activation gp-card gp-glass shadow-float"');
    expect(source).toContain('class="activation-error gp-card shadow-float"');
    expect(source).toContain('class="gp-btn"');
  });

  it("keeps the task save bar in sheet chrome instead of an opaque sticky overlay", () => {
    const source = read("TaskEditor");
    const footer = source.slice(source.indexOf("<style>")).match(/(?:^|})\s*footer\{([^}]*)\}/)?.[1];
    expect(footer).toBeDefined();
    expect(footer).not.toContain("position:sticky");
    expect(footer).not.toMatch(/background(?:-color)?:/);
    expect(source).toContain('form="task-editor-form-{id}"');
    expect(source).toMatch(/\{#if current\}<TaskRuns[\s\S]*?<\/div>\s*<footer>/);
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
