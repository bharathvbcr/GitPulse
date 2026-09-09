import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import {
  cancelExternalToolInstall,
  classifyMapFailure,
  cloneToolSource,
  getExternalToolsStatus,
  getToolConfig,
  getToolLadder,
  getToolPreflight,
  installButtonLabel,
  installExternalTool,
  mapFailureMessage,
  refreshToolCapability,
  saveToolConfig,
  toolStatusSummary,
  verifyTool,
  type ToolStatus,
} from "./externalTools";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

function status(partial: Partial<ToolStatus>): ToolStatus {
  return {
    tool: "devmap",
    installed: false,
    path: null,
    lookup: "missing",
    version: null,
    reason: null,
    source_checkout: null,
    install_ready: false,
    install_block: null,
    install_command: "cargo install --path …",
    ...partial,
  };
}

beforeEach(() => {
  vi.mocked(invoke).mockReset();
});

describe("toolStatusSummary", () => {
  it("names a broken explicit path rather than 'not installed'", () => {
    expect(
      toolStatusSummary(
        status({
          lookup: "explicit_missing",
          reason: "GITPULSE_DEVMAP_BIN is set to /x, but that path is not a file",
        }),
      ),
    ).toContain("GITPULSE_DEVMAP_BIN");
  });

  it("shows the path that answered when installed", () => {
    expect(
      toolStatusSummary(
        status({
          installed: true,
          path: "/Users/me/.cargo/bin/devmap",
          lookup: "path_search",
          version: "devmap 1.2.3",
        }),
      ),
    ).toContain("/Users/me/.cargo/bin/devmap");
  });

  it("labels a saved_config lookup", () => {
    expect(
      toolStatusSummary(
        status({
          installed: true,
          path: "/app/bin/devmap",
          lookup: "saved_config",
        }),
      ),
    ).toContain("saved config");
  });

  it("surfaces stale config rather than silent drop", () => {
    expect(
      toolStatusSummary(
        status({
          stale_config: "/old/devmap no longer exists",
        }),
      ),
    ).toContain("Stale saved path");
  });

  it("falls back to a not-installed reason", () => {
    expect(toolStatusSummary(status({ reason: null }))).toContain("not installed");
  });
});

describe("installButtonLabel", () => {
  it("says Update when already installed", () => {
    expect(installButtonLabel(status({ installed: true }))).toBe("Update devmap");
  });

  it("says Install when missing", () => {
    expect(installButtonLabel(status({ tool: "manvi" }))).toBe("Install manvi");
  });
});

describe("classifyMapFailure", () => {
  it("separates no CLI from missing sqlite and missing JSON", () => {
    expect(
      classifyMapFailure({
        cliAvailable: false,
        mapAvailable: false,
        mapReason: "whatever",
      }),
    ).toBe("no_cli");

    expect(
      classifyMapFailure({
        cliAvailable: true,
        mapAvailable: false,
        mapReason: "no map at .devcouncil/codeintel/devmap.sqlite",
      }),
    ).toBe("no_sqlite_store");

    expect(
      classifyMapFailure({
        cliAvailable: true,
        mapAvailable: false,
        mapReason: "index has not been built",
      }),
    ).toBe("no_sqlite_store");

    expect(
      classifyMapFailure({
        cliAvailable: true,
        mapAvailable: false,
        mapReason: "no repo map at /repo/.devcouncil/repo_map.json; run Build Map first",
        mapPath: "/repo/.devcouncil/repo_map.json",
      }),
    ).toBe("no_json_artifact");

    expect(
      classifyMapFailure({
        cliAvailable: true,
        mapAvailable: false,
        mapReason: "something else",
        mapPath: "/repo/.devcouncil/repo_map.json",
      }),
    ).toBe("no_json_artifact");

    expect(
      classifyMapFailure({
        cliAvailable: true,
        mapAvailable: false,
        mapReason: "unknown",
      }),
    ).toBe("other");

    expect(
      classifyMapFailure({
        cliAvailable: true,
        mapAvailable: true,
      }),
    ).toBe("other");
  });

  it("mapFailureMessage names the action for each mode", () => {
    expect(mapFailureMessage("no_cli")).toContain("Setup");
    expect(mapFailureMessage("no_sqlite_store")).toContain("Build");
    expect(mapFailureMessage("no_json_artifact")).toContain("repo_map.json");
    expect(mapFailureMessage("other", "custom")).toBe("custom");
    expect(mapFailureMessage("other")).toBe("Map unavailable");
  });
});

describe("IPC wrappers", () => {
  it("invokes status, install, cancel, config, ladder, preflight, verify, clone, refresh", async () => {
    vi.mocked(invoke).mockResolvedValue({});

    await getExternalToolsStatus();
    expect(invoke).toHaveBeenCalledWith("cmd_external_tools_status");

    await installExternalTool("devmap", "toolchain_remote");
    expect(invoke).toHaveBeenCalledWith("cmd_external_tool_install", {
      tool: "devmap",
      rung: "toolchain_remote",
    });

    await installExternalTool("manvi");
    expect(invoke).toHaveBeenCalledWith("cmd_external_tool_install", {
      tool: "manvi",
      rung: null,
    });

    await cancelExternalToolInstall();
    expect(invoke).toHaveBeenCalledWith("cmd_external_tool_install_cancel");

    await getToolConfig();
    expect(invoke).toHaveBeenCalledWith("cmd_tool_config_get");

    const cfg = {
      version: 1,
      devmap: {},
      manvi: {},
      onboarding: { skipped_tools: [] as string[], dismissed: false },
    };
    await saveToolConfig(cfg);
    expect(invoke).toHaveBeenCalledWith("cmd_tool_config_save", { config: cfg });

    await getToolLadder("devmap");
    expect(invoke).toHaveBeenCalledWith("cmd_tool_ladder", { tool: "devmap" });

    await getToolPreflight("manvi");
    expect(invoke).toHaveBeenCalledWith("cmd_tool_preflight", { tool: "manvi" });

    await verifyTool("devmap");
    expect(invoke).toHaveBeenCalledWith("cmd_tool_verify", { tool: "devmap" });

    await cloneToolSource("devmap", "/tmp");
    expect(invoke).toHaveBeenCalledWith("cmd_onboarding_clone_source", {
      tool: "devmap",
      parentDir: "/tmp",
    });

    await refreshToolCapability();
    expect(invoke).toHaveBeenCalledWith("cmd_tool_capability_refresh");
  });
});
