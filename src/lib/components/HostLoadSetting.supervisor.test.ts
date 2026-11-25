import { afterEach, describe, expect, it, vi } from "vitest";
import { render } from "svelte/server";
import HostLoadSetting from "./HostLoadSetting.svelte";
import {
  DEFAULT_HOST_LOAD_SETTINGS,
  type HostLoadSettings,
  resetHostLoadForTests,
  saveHostLoadSettings,
} from "../stores/hostLoadStore";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../platform", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../platform")>();
  return { ...actual, isTauri: () => false };
});

afterEach(() => resetHostLoadForTests());

function supervisorBlock(body: string): string {
  const start = body.indexOf('data-testid="host-load-supervisor"');
  expect(start).toBeGreaterThan(-1);
  return body.slice(start, body.indexOf('data-testid="host-load-lease-settings"'));
}

describe("the memory supervisor setting", () => {
  it("names the three choices in plain words", () => {
    const block = supervisorBlock(render(HostLoadSetting).body);
    for (const word of ["When memory runs out", "Act", "Advise", "Off"]) {
      expect(block).toContain(word);
    }
    expect(block).toContain("pause the heaviest build or test under an agent");
    expect(block).toContain("only tell me what it would pause");
  });

  it("shows the default, act, as pressed before the backend has answered", () => {
    const block = supervisorBlock(render(HostLoadSetting).body);
    expect(block).not.toContain("no memory supervisor to switch");
    expect(block).toMatch(/aria-pressed="true"[^>]*data-active="true"[^>]*title="Pause the heaviest build or test under an agent"/);
  });

  it("shows the reported mode as pressed", async () => {
    const reported: HostLoadSettings = { ...DEFAULT_HOST_LOAD_SETTINGS, supervisor: "advise" };
    await saveHostLoadSettings(reported);
    const block = supervisorBlock(render(HostLoadSetting).body);
    expect(block).not.toContain("no memory supervisor to switch");
    expect(block).toMatch(/aria-pressed="true"[^>]*data-active="true"[^>]*title="Only say what it would pause"/);
  });
});
