import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import {
  cancelInstall,
  closeSetupWizard,
  dismissFirstRun,
  markOnboardingComplete,
  onboardingStore,
  openSetupWizard,
  refreshToolsStatus,
  runInstall,
  runVerify,
  setWizardStep,
  setWizardTool,
  setWizardPreset,
} from "./onboardingStore";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_event: string, handler: (e: { payload: { line: string } }) => void) => {
    handler({ payload: { line: "compiling…" } });
    return () => {};
  }),
}));

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  closeSetupWizard();
});

describe("onboardingStore", () => {
  it("opens and closes the wizard", () => {
    openSetupWizard("manvi");
    expect(get(onboardingStore.wizard).open).toBe(true);
    expect(get(onboardingStore.wizard).tool).toBe("manvi");
    closeSetupWizard();
    expect(get(onboardingStore.wizard).open).toBe(false);
  });

  it("opens with the DevMap-only preset", () => {
    openSetupWizard("devmap");
    expect(get(onboardingStore.wizard).preset).toBe("devmap");
    setWizardPreset("analysis");
    expect(get(onboardingStore.wizard).preset).toBe("analysis");
  });

  it("advances steps and switches tools", async () => {
    vi.mocked(invoke).mockResolvedValue({
      tool: "devmap",
      selected: null,
      rungs: [],
      ok: true,
      requirements: [],
      estimate: "seconds",
    });
    openSetupWizard("devmap");
    setWizardStep("preflight");
    expect(get(onboardingStore.wizard).step).toBe("preflight");
    setWizardTool("manvi");
    expect(get(onboardingStore.wizard).tool).toBe("manvi");
    await Promise.resolve();
  });

  it("refreshes status and runs verify", async () => {
    vi.mocked(invoke).mockResolvedValue({
      available: true,
      ok: true,
      tool: "devmap",
      binary: "/bin/devmap",
      detail: "doctor ok",
      devmap: { tool: "devmap", installed: true },
      manvi: { tool: "manvi", installed: false },
    });
    await refreshToolsStatus();
    expect(invoke).toHaveBeenCalledWith("cmd_external_tools_status");
    await runVerify("devmap");
    expect(invoke).toHaveBeenCalledWith("cmd_tool_verify", { tool: "devmap" });
    expect(get(onboardingStore.verify)?.ok).toBe(true);
  });

  it("runs install and records outcome", async () => {
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "cmd_external_tool_install") {
        return {
          tool: "devmap",
          ok: true,
          binary: "/bin/devmap",
          lookup: "path_search",
          version: "1.2.3",
          source_used: null,
          command: "cargo install",
          exit_code: 0,
          stdout: "",
          stderr: "",
          timed_out: false,
          cancelled: false,
          reason: null,
        };
      }
      if (cmd === "cmd_tool_verify") {
        return { tool: "devmap", ok: true, binary: "/bin/devmap", detail: "ok" };
      }
      if (cmd === "cmd_external_tools_status") {
        return {
          devmap: { tool: "devmap", installed: true },
          manvi: { tool: "manvi", installed: false },
        };
      }
      return {};
    });
    await runInstall("devmap", "toolchain_remote");
    expect(get(onboardingStore.lastOutcome)?.ok).toBe(true);
    expect(get(onboardingStore.installing)).toBeNull();
  });

  it("records install failure without throwing", async () => {
    vi.mocked(invoke).mockRejectedValue(new Error("boom"));
    await runInstall("manvi");
    expect(get(onboardingStore.lastOutcome)?.ok).toBe(false);
    expect(get(onboardingStore.lastOutcome)?.reason).toContain("boom");
  });

  it("cancels an in-flight install", async () => {
    vi.mocked(invoke).mockResolvedValue(undefined);
    await cancelInstall();
    expect(invoke).toHaveBeenCalledWith("cmd_external_tool_install_cancel");
  });

  it("loads tool config into the store", async () => {
    const { refreshToolConfig } = await import("./onboardingStore");
    vi.mocked(invoke).mockResolvedValue({
      config: {
        version: 1,
        devmap: {},
        manvi: {},
        onboarding: { skipped_tools: [], dismissed: false },
      },
      path: "/tmp/tools.json",
      stale: [],
    });
    const view = await refreshToolConfig();
    expect(view?.path).toBe("/tmp/tools.json");
    expect(get(onboardingStore.config)?.path).toBe("/tmp/tools.json");
  });

  it("dismisses first-run and marks onboarding complete", async () => {
    const saved = {
      config: {
        version: 1,
        devmap: {},
        manvi: {},
        onboarding: { skipped_tools: [] as string[], dismissed: false },
      },
      path: "/tmp/tools.json",
      stale: [] as Array<{ field: string; path: string; reason: string; detail: string }>,
    };
    vi.mocked(invoke).mockResolvedValue(saved);
    await dismissFirstRun();
    expect(invoke).toHaveBeenCalledWith(
      "cmd_tool_config_save",
      expect.objectContaining({
        config: expect.objectContaining({
          onboarding: expect.objectContaining({ dismissed: true }),
        }),
      }),
    );

    vi.mocked(invoke).mockResolvedValue({
      ...saved,
      config: {
        ...saved.config,
        onboarding: {
          skipped_tools: [],
          dismissed: true,
          completed_at: "2026-01-01T00:00:00.000Z",
        },
      },
    });
    await markOnboardingComplete();
    expect(invoke).toHaveBeenCalledWith(
      "cmd_tool_config_save",
      expect.objectContaining({
        config: expect.objectContaining({
          onboarding: expect.objectContaining({
            dismissed: true,
            completed_at: expect.any(String),
          }),
        }),
      }),
    );
  });
});
