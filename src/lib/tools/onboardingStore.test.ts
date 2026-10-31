import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import {
  cancelInstall,
  cloneSource,
  closeSetupWizard,
  dismissFirstRun,
  finishSetup,
  markOnboardingComplete,
  onboardingStore,
  openSetupWizard,
  refreshToolsStatus,
  retryWizardLoad,
  runInstall,
  runVerify,
  setupOutcome,
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

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("failed reads and actions are shown, not swallowed (SO-2)", () => {
  beforeEach(() => {
    onboardingStore.config.set(null);
    onboardingStore.verify.set(null);
    onboardingStore.loadError.set(null);
    onboardingStore.actionError.set(null);
    onboardingStore.verifying.set(false);
  });

  it("a rejected ladder read sets loadError and leaves ladder and preflight empty", async () => {
    vi.mocked(invoke).mockRejectedValue("worker down");
    setWizardTool("devmap");
    await flush();
    expect(get(onboardingStore.loadError)).toContain("worker down");
    expect(get(onboardingStore.ladder)).toBeNull();
    expect(get(onboardingStore.preflight)).toBeNull();
  });

  it("retryWizardLoad clears the error and reloads when the read now works", async () => {
    vi.mocked(invoke).mockRejectedValue("worker down");
    setWizardTool("devmap");
    await flush();
    vi.mocked(invoke).mockImplementation(async (cmd: string) => {
      if (cmd === "cmd_tool_ladder") return { tool: "devmap", selected: null, rungs: [], ok: true, requirements: [], estimate: "s" };
      if (cmd === "cmd_tool_preflight") return { tool: "devmap", ok: true, requirements: [], estimate: "s" };
      return {};
    });
    retryWizardLoad("devmap");
    await flush();
    expect(get(onboardingStore.loadError)).toBeNull();
    expect(get(onboardingStore.ladder)).not.toBeNull();
    expect(get(onboardingStore.preflight)).not.toBeNull();
  });

  it("a verify that rejects sets actionError and returns verifying to false", async () => {
    vi.mocked(invoke).mockRejectedValue(new Error("no doctor"));
    await runVerify("devmap");
    expect(get(onboardingStore.actionError)).toContain("no doctor");
    expect(get(onboardingStore.verifying)).toBe(false);
    expect(get(onboardingStore.verify)).toBeNull();
  });

  it("verifying is true while the call is out, and a second press does not stack a run", async () => {
    let release!: (value: unknown) => void;
    vi.mocked(invoke).mockReturnValue(new Promise((resolve) => (release = resolve)) as never);
    const first = runVerify("devmap");
    expect(get(onboardingStore.verifying)).toBe(true);
    await runVerify("devmap");
    expect(invoke).toHaveBeenCalledTimes(1);
    release({ tool: "devmap", ok: true, binary: null, detail: "ok" });
    await first;
    expect(get(onboardingStore.verifying)).toBe(false);
    expect(get(onboardingStore.verify)?.ok).toBe(true);
  });

  it("cancelInstall reports a rejection", async () => {
    vi.mocked(invoke).mockRejectedValue(new Error("not running"));
    await cancelInstall();
    expect(get(onboardingStore.actionError)).toContain("not running");
  });

  it("markOnboardingComplete rejects when the configuration cannot be loaded", async () => {
    vi.mocked(invoke).mockRejectedValue(new Error("config unreadable"));
    await expect(markOnboardingComplete()).rejects.toThrow(
      "Tool configuration is not loaded yet, so setup completion could not be recorded.",
    );
  });

  it("finishSetup stays open and says why when completion cannot be recorded", async () => {
    vi.mocked(invoke).mockRejectedValue(new Error("config unreadable"));
    openSetupWizard("devmap");
    await flush();
    onboardingStore.actionError.set(null);
    await expect(finishSetup()).resolves.toBe(false);
    expect(get(onboardingStore.wizard).open).toBe(true);
    expect(get(onboardingStore.actionError)).toContain("could not be recorded");
  });

  it("finishSetup closes the wizard once completion is recorded", async () => {
    const view = {
      config: { version: 1, devmap: {}, manvi: {}, onboarding: { skipped_tools: [], dismissed: false } },
      path: "/tmp/tools.json",
      stale: [],
    };
    onboardingStore.config.set(view as never);
    vi.mocked(invoke).mockResolvedValue(view);
    openSetupWizard("devmap");
    await flush();
    await expect(finishSetup()).resolves.toBe(true);
    expect(get(onboardingStore.wizard).open).toBe(false);
    expect(get(onboardingStore.actionError)).toBeNull();
  });

  it("cloneSource answers a line for a rejection instead of leaving Cloning… up", async () => {
    vi.mocked(invoke).mockRejectedValue(new Error("no git"));
    await expect(cloneSource("devmap", "/tmp")).resolves.toBe("Clone failed: no git");
    vi.mocked(invoke).mockResolvedValue({ tool: "devmap", ok: true, path: "/tmp/devmap", reason: null });
    await expect(cloneSource("devmap", "/tmp")).resolves.toBe("Cloned to /tmp/devmap");
    vi.mocked(invoke).mockResolvedValue({ tool: "devmap", ok: false, path: null, reason: null });
    await expect(cloneSource("devmap", "/tmp")).resolves.toBe("Clone failed");
  });

  it("the last step only claims completion for a passing verify of this tool", () => {
    const report = (tool: "devmap" | "manvi", ok: boolean) => ({ tool, ok, binary: null, detail: "" }) as never;
    expect(setupOutcome(null, "devmap")).toBe("skipped");
    expect(setupOutcome(report("manvi", true), "devmap")).toBe("skipped");
    expect(setupOutcome(report("devmap", false), "devmap")).toBe("failed");
    expect(setupOutcome(report("devmap", true), "devmap")).toBe("passed");
  });
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
