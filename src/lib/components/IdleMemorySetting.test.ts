import { readFileSync } from "node:fs";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { compile } from "svelte/compiler";
import { render } from "svelte/server";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
vi.mock("../platform", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../platform")>();
  return {
    ...actual,
    isTauri: () => true,
  };
});

const { loadIdleMemoryMode, resetIdleMemoryStateForTests, saveIdleMemoryMode } = await import(
  "../runtime/idleShedderWiring"
);
const { default: IdleMemorySetting } = await import("./IdleMemorySetting.svelte");

const source = readFileSync(new URL("./IdleMemorySetting.svelte", import.meta.url), "utf8");

beforeEach(() => {
  invoke.mockReset();
  resetIdleMemoryStateForTests();
});

describe("IdleMemorySetting", () => {
  it("compiles cleanly without Svelte warnings and uses SettingSegment", () => {
    const { warnings } = compile(source, { generate: "client", filename: "IdleMemorySetting.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
    expect(source).toContain("SettingSegment");
    expect(source).toContain("gitpulse-ptyd");
  });

  it("renders Off, Balanced (default selected), and Aggressive options with clear copy", () => {
    const { body } = render(IdleMemorySetting, { props: { active: false } });
    expect(body).toContain("Idle memory efficiency");
    expect(body).toContain('aria-label="Idle memory mode"');
    expect(body).toContain(">Off<");
    expect(body).toContain(">Balanced<");
    expect(body).toContain(">Aggressive<");
    expect(body).toMatch(/aria-pressed="true"[^>]*>\s*Balanced\s*</);
    expect(body).toContain("Stage 1");
    expect(body).toContain("Stage 2");
    expect(body).toContain("gitpulse-ptyd");
  });

  it("reflects loaded and saved idle memory mode values", async () => {
    invoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "cmd_idle_memory_settings") return { mode: "aggressive" };
      if (cmd === "cmd_idle_memory_settings_save") {
        return { mode: (args?.settings as { mode: string })?.mode ?? "balanced" };
      }
      return undefined;
    });

    await loadIdleMemoryMode();
    const loaded = render(IdleMemorySetting, { props: { active: false } }).body;
    expect(loaded).toMatch(/aria-pressed="true"[^>]*>\s*Aggressive\s*</);

    await saveIdleMemoryMode("off");
    const saved = render(IdleMemorySetting, { props: { active: false } }).body;
    expect(saved).toMatch(/aria-pressed="true"[^>]*>\s*Off\s*</);
  });
});
